//! ZvecWorkspaceIndexStorage: open/retry/lock, trait implementation.
//!
//! Ports `engine/storage/zvec.ts`. Entity fragments live in the `index.zvec`
//! collection; file metadata lives in `files.json` (see [`store`]) rather
//! than the second zvec collection TypeScript uses.

pub mod codec;
pub mod filter;
pub(crate) mod legacy_import;
pub mod schema;
pub mod search;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use zvec_rust::{Collection, CollectionOptions, Doc};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::ids::EntityId;
use crate::ids::FileId;
use crate::storage::layout::{
    WorkspaceIndexStoragePaths, normalize_absolute_path, path_has_prefix,
    resolve_workspace_index_storage_paths,
};
use crate::storage::{
    FileIndexDiagnostics, IndexedFragment, ListEntitiesOptions, StorageOptions,
    StorageSearchFilter, StorageSearchHit, StorageSearchPath, StoredEntity, WorkspaceIndexStorage,
};
use crate::types::{FileIndexStatus, FileInfo, UnixMillis};
use crate::utils::lock::{Guard, LockMode, LockOptions, acquire_read_write_lock};

use codec::{
    StoredFragment, doc_to_stored_fragment, fragment_to_doc, fragment_to_entity, public_entity_ids,
    validate_fragment_groups,
};
use filter::{build_filter, quote_filter_string};
use legacy_import::{LegacyImportOutcome, legacy_files_newer_than, try_import_legacy_files};
use schema::create_entities_schema;
use search::{search_fts as run_fts, search_vector as run_vector};
use store::{FileMetaStore, FileRecord};

/// Batch size for entity document upserts.
const ZVEC_UPSERT_BATCH_SIZE: usize = 1024;
/// Name of the zvec-internal lock file probed before open retries.
const ZVEC_LOCK_FILE: &str = "LOCK";
/// Open attempts before surfacing the last zvec error.
const ZVEC_OPEN_RETRY_ATTEMPTS: u32 = 8;
const ZVEC_OPEN_RETRY_BASE_DELAY_MS: u64 = 100;
const ZVEC_OPEN_RETRY_MAX_DELAY_MS: u64 = 1000;

/// Full [`WorkspaceIndexStorage`] implementation over zvec-rust.
pub struct ZvecWorkspaceIndexStorage {
    read_only: bool,
    collection: Option<Collection>,
    meta: FileMetaStore,
    file_ids_by_path: HashMap<String, String>,
    needs_optimize: bool,
    _lock: Guard,
}

impl ZvecWorkspaceIndexStorage {
    pub fn open(options: StorageOptions<'_>) -> EngineResult<Box<dyn WorkspaceIndexStorage>> {
        let read_only = options.read_only();
        let storage_path = options.storage_path().to_path_buf();
        let embedding = match &options {
            StorageOptions::ReadOnly { .. } => None,
            StorageOptions::ReadWrite { embedding, .. } => Some(*embedding),
        };
        initialize_zvec()?;
        let paths = resolve_workspace_index_storage_paths(&storage_path);
        if !read_only {
            std::fs::create_dir_all(&paths.storage_path).map_err(|error| {
                EngineError::new(
                    EngineErrorCode::from_static("STORAGE.CREATE_FAILED"),
                    "failed to create workspace index storage directory",
                )
                .with_context(format!(
                    "path={} error={error}",
                    paths.storage_path.display()
                ))
            })?;
        }
        let lock_mode = if read_only {
            LockMode::Read
        } else {
            LockMode::Write
        };
        let guard = acquire_read_write_lock(
            &paths.storage_path.join("LOCK"),
            lock_mode,
            &LockOptions::new("storage.open"),
        )?;
        let meta = open_file_meta_store(&paths, read_only)?;
        let mut file_ids_by_path = HashMap::new();
        for record in meta.list() {
            file_ids_by_path.insert(
                normalize_absolute_path(&record.info.absolute_path),
                record.info.id.as_str().to_owned(),
            );
        }
        let index_path = paths.index_path.to_string_lossy().into_owned();
        let collection = if paths.index_path.exists() {
            let mut open_options =
                CollectionOptions::new().map_err(|error| zvec_error_open(&error.to_string()))?;
            open_options
                .set_read_only(read_only)
                .map_err(|error| zvec_error_open(&error.to_string()))?;
            open_zvec_collection(&index_path, read_only, "open", || {
                Collection::open(&index_path, Some(&open_options))
            })?
        } else if read_only {
            return Err(EngineError::new(
                EngineErrorCode::from_static("STORAGE.ZVEC_COLLECTION_MISSING"),
                "zvec collection storage does not exist",
            )
            .with_context(format!("path={index_path}")));
        } else {
            let embedding = embedding.ok_or_else(|| {
                EngineError::new(
                    EngineErrorCode::from_static("STORAGE.MISSING_EMBEDDING_SCHEMA"),
                    "embedding schema is required to create workspace index storage",
                )
                .with_context(format!("path={index_path}"))
            })?;
            let collection_schema = create_entities_schema(embedding)?;
            open_zvec_collection(&index_path, read_only, "create", || {
                Collection::create_and_open(&index_path, &collection_schema, None)
            })?
        };
        Ok(Box::new(Self {
            read_only,
            collection: Some(collection),
            meta,
            file_ids_by_path,
            needs_optimize: false,
            _lock: guard,
        }))
    }

    fn require_collection(&self, operation: &str) -> EngineResult<&Collection> {
        self.collection.as_ref().ok_or_else(|| {
            EngineError::new(
                EngineErrorCode::from_static("STORAGE.COLLECTION_CLOSED"),
                "workspace index storage is closed",
            )
            .with_context(format!("operation={operation}"))
        })
    }

    fn assert_writable(&self, operation: &str) -> EngineResult<()> {
        if self.read_only {
            return Err(EngineError::new(
                EngineErrorCode::from_static("STORAGE.READ_ONLY"),
                "cannot update read-only workspace index storage",
            )
            .with_context(format!("operation={operation}")));
        }
        Ok(())
    }

    fn file_infos(&self) -> HashMap<String, FileInfo> {
        self.meta
            .list()
            .into_iter()
            .map(|record| (record.info.id.as_str().to_owned(), record.info))
            .collect()
    }

    fn stored_entity(&self, pk: &str, operation: &str) -> EngineResult<Option<StoredEntity>> {
        let collection = self.require_collection(operation)?;
        let docs = collection
            .fetch_with_options(&[pk], None, false)
            .map_err(|error| {
                zvec_error(
                    EngineErrorCode::from_static("STORAGE.ZVEC_FETCH_FAILED"),
                    "zvec fetch failed",
                    format!("fragmentId={pk} error={error}"),
                )
            })?;
        let doc = docs
            .iter()
            .find(|doc| doc.get_pk().unwrap_or_default() == pk);
        let Some(doc) = doc else {
            return Ok(None);
        };
        let files = self.file_infos();
        let Some(mut stored) = doc_to_stored_fragment(doc, &files)? else {
            return Ok(None);
        };
        loop {
            let group = stored.fragment.group.clone();
            match group {
                Some(group_id) if group_id != stored.fragment.entity.id.as_str() => {
                    let docs = collection
                        .fetch_with_options(&[group_id.as_str()], None, false)
                        .map_err(|error| {
                            zvec_error(
                                EngineErrorCode::from_static("STORAGE.ZVEC_FETCH_FAILED"),
                                "zvec fetch failed",
                                format!("fragmentId={group_id} error={error}"),
                            )
                        })?;
                    let major = docs
                        .iter()
                        .find(|doc| doc.get_pk().unwrap_or_default() == group_id);
                    let Some(major) = major else {
                        return Ok(None);
                    };
                    let Some(next) = doc_to_stored_fragment(major, &files)? else {
                        return Ok(None);
                    };
                    stored = next;
                }
                _ => {
                    return Ok(Some(StoredEntity {
                        entity: fragment_to_entity(&stored.fragment),
                        file: stored.file,
                    }));
                }
            }
        }
    }

    fn remember_file(&mut self, record: &FileRecord) {
        self.file_ids_by_path.insert(
            normalize_absolute_path(&record.info.absolute_path),
            record.info.id.as_str().to_owned(),
        );
    }

    fn delete_file_documents(&mut self, file_id: &FileId, operation: &str) -> EngineResult<()> {
        let collection = self.require_collection(operation)?;
        collection
            .delete_by_filter(&format!(
                "file_id = {}",
                quote_filter_string(file_id.as_str())
            ))
            .map_err(|error| {
                zvec_error(
                    EngineErrorCode::from_static("STORAGE.ZVEC_DELETE_FAILED"),
                    "zvec delete by filter failed",
                    format!("fileId={} error={error}", file_id.as_str()),
                )
            })?;
        self.needs_optimize = true;
        Ok(())
    }

    fn upsert_docs(&mut self, file_id: &FileId, docs: &[Doc]) -> EngineResult<()> {
        let collection = self.require_collection("replaceFile")?;
        for (batch_index, batch) in docs.chunks(ZVEC_UPSERT_BATCH_SIZE).enumerate() {
            let refs: Vec<&Doc> = batch.iter().collect();
            let result = collection.upsert(&refs).map_err(|error| {
                zvec_error(
                    EngineErrorCode::from_static("STORAGE.ZVEC_UPSERT_FAILED"),
                    "zvec failed to upsert entity documents",
                    format!("fileId={} error={error}", file_id.as_str()),
                )
            })?;
            let failed = result.results.iter().find(|status| !status.is_success());
            match failed {
                Some(status) => {
                    return Err(zvec_error(
                        EngineErrorCode::from_static("STORAGE.ZVEC_UPSERT_FAILED"),
                        "zvec failed to upsert entity documents",
                        format!(
                            "fileId={} batchStart={} batchSize={} code={} message={}",
                            file_id.as_str(),
                            batch_index * ZVEC_UPSERT_BATCH_SIZE,
                            batch.len(),
                            status.code,
                            status.message
                        ),
                    ));
                }
                None if result.error_count > 0 => {
                    return Err(zvec_error(
                        EngineErrorCode::from_static("STORAGE.ZVEC_UPSERT_FAILED"),
                        "zvec failed to upsert entity documents",
                        format!(
                            "fileId={} batchStart={} batchSize={} errorCount={}",
                            file_id.as_str(),
                            batch_index * ZVEC_UPSERT_BATCH_SIZE,
                            batch.len(),
                            result.error_count
                        ),
                    ));
                }
                None => {}
            }
        }
        Ok(())
    }

    fn docs_to_hits(&self, docs: &[Doc], path: StorageSearchPath) -> Vec<StorageSearchHit> {
        let files = self.file_infos();
        let mut hits = Vec::with_capacity(docs.len());
        for doc in docs {
            let stored = match doc_to_stored_fragment(doc, &files) {
                Ok(stored) => stored,
                Err(_) => continue,
            };
            if let Some(StoredFragment { fragment, file }) = stored {
                hits.push(StorageSearchHit {
                    fragment,
                    file,
                    path,
                    score: f64::from(doc.get_score()),
                });
            }
        }
        hits
    }
}

impl WorkspaceIndexStorage for ZvecWorkspaceIndexStorage {
    fn read_only(&self) -> bool {
        self.read_only
    }

    fn get_file_by_path(&self, absolute_path: &str) -> Option<FileInfo> {
        let normalized = normalize_absolute_path(absolute_path);
        let file_id = self.file_ids_by_path.get(&normalized)?;
        self.meta.get(file_id).map(FileRecord::file_info)
    }

    fn list_files_by_path_prefix(&self, absolute_path: &str) -> Vec<FileInfo> {
        self.list_files_by_path_prefixes(std::slice::from_ref(&absolute_path.to_owned()))
    }

    fn list_files_by_path_prefixes(&self, absolute_paths: &[String]) -> Vec<FileInfo> {
        let prefixes: HashSet<String> = absolute_paths
            .iter()
            .map(|path| normalize_absolute_path(path))
            .collect();
        if prefixes.is_empty() {
            return Vec::new();
        }
        self.meta
            .list()
            .into_iter()
            .filter(|record| {
                path_has_prefix(
                    &normalize_absolute_path(&record.info.absolute_path),
                    &prefixes,
                )
            })
            .map(|record| record.info)
            .collect()
    }

    fn list_files(&self) -> Vec<FileInfo> {
        self.meta
            .list()
            .into_iter()
            .map(|record| record.info)
            .collect()
    }

    fn list_entities_by_file(
        &self,
        file_id: &FileId,
        options: ListEntitiesOptions,
    ) -> Vec<StoredEntity> {
        let Some(record) = self.meta.get(file_id.as_str()) else {
            return Vec::new();
        };
        let offset = options.offset.unwrap_or(0);
        let limit = options.limit.unwrap_or(record.entity_ids.len());
        let ids: Vec<String> = record
            .entity_ids
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect();
        let mut entities = Vec::with_capacity(ids.len());
        for id in &ids {
            match self.stored_entity(id, "listEntitiesByFile") {
                Ok(Some(entity)) => entities.push(entity),
                Ok(None) => {}
                Err(_) => {}
            }
        }
        entities
    }

    fn get_entity(&self, entity_id: &EntityId) -> Option<StoredEntity> {
        self.stored_entity(entity_id.as_str(), "getEntity")
            .unwrap_or(None)
    }

    fn search_fts(
        &self,
        query: &str,
        limit: usize,
        filter: Option<&StorageSearchFilter>,
    ) -> EngineResult<Vec<StorageSearchHit>> {
        let _ = build_filter(filter).as_deref();
        let collection = self.require_collection("searchFts")?;
        let docs = run_fts(collection, query, limit, filter)?;
        Ok(self.docs_to_hits(&docs, StorageSearchPath::Fts))
    }

    fn search_vector(
        &self,
        vector: &[f32],
        limit: usize,
        filter: Option<&StorageSearchFilter>,
    ) -> EngineResult<Vec<StorageSearchHit>> {
        let collection = self.require_collection("searchVector")?;
        let docs = run_vector(collection, vector, limit, filter)?;
        Ok(self.docs_to_hits(&docs, StorageSearchPath::Vector))
    }

    fn replace_file(
        &mut self,
        file: &FileInfo,
        entries: &[IndexedFragment],
        diagnostics: Option<&FileIndexDiagnostics>,
    ) -> EngineResult<()> {
        self.assert_writable("replaceFile")?;
        validate_fragment_groups(&file.id, entries.iter().map(|entry| &entry.fragment))?;
        let dirty = FileRecord {
            info: FileInfo {
                absolute_path: normalize_absolute_path(&file.absolute_path),
                index_status: Some(FileIndexStatus {
                    indexed_time: None,
                    entity_count: 0,
                    ..FileIndexStatus::default()
                }),
                ..file.clone()
            },
            entity_ids: Vec::new(),
        };
        self.remember_file(&dirty);
        self.meta.upsert(dirty)?;
        self.delete_file_documents(&file.id, "replaceFile")?;
        let entity_ids: Vec<String> =
            public_entity_ids(entries.iter().map(|entry| &entry.fragment));
        let indexed = FileRecord {
            info: FileInfo {
                absolute_path: normalize_absolute_path(&file.absolute_path),
                index_status: Some(FileIndexStatus {
                    indexed_time: Some(UnixMillis::now()),
                    entity_count: entity_ids.len(),
                    truncated_fragment_count: Some(
                        diagnostics
                            .map(|d| d.truncated_fragment_count.unwrap_or(0))
                            .unwrap_or(0),
                    ),
                    ..FileIndexStatus::default()
                }),
                ..file.clone()
            },
            entity_ids,
        };
        if !entries.is_empty() {
            let mut docs = Vec::with_capacity(entries.len());
            for (index, entry) in entries.iter().enumerate() {
                let fragment_index = i32::try_from(index).map_err(|_| {
                    EngineError::new(
                        EngineErrorCode::from_static("STORAGE.DOC_ENCODE_FAILED"),
                        "fragment index does not fit an i32",
                    )
                    .with_context(format!("fileId={}", file.id.as_str()))
                })?;
                docs.push(fragment_to_doc(
                    &indexed.info,
                    &entry.fragment,
                    &entry.vector,
                    fragment_index,
                )?);
            }
            self.upsert_docs(&file.id, &docs)?;
            self.needs_optimize = true;
        }
        self.remember_file(&indexed);
        self.meta.upsert(indexed)?;
        Ok(())
    }

    fn mark_file_failed(&mut self, file: &FileInfo, error: &str) -> EngineResult<()> {
        self.assert_writable("markFileFailed")?;
        self.delete_file_documents(&file.id, "markFileFailed")?;
        let failed = FileRecord {
            info: FileInfo {
                absolute_path: normalize_absolute_path(&file.absolute_path),
                index_status: Some(FileIndexStatus {
                    indexed_time: None,
                    entity_count: 0,
                    error: Some(error.to_owned()),
                    ..FileIndexStatus::default()
                }),
                ..file.clone()
            },
            entity_ids: Vec::new(),
        };
        self.remember_file(&failed);
        let persisted = self.meta.get(file.id.as_str()).cloned().unwrap_or(failed);
        self.meta.upsert(persisted)?;
        Ok(())
    }

    fn delete_file(&mut self, file_id: &FileId) -> EngineResult<()> {
        self.assert_writable("deleteFile")?;
        let existing = self.meta.get(file_id.as_str()).cloned();
        self.delete_file_documents(file_id, "deleteFile")?;
        if let Some(record) = existing {
            self.file_ids_by_path
                .remove(&normalize_absolute_path(&record.info.absolute_path));
        }
        self.meta.remove(file_id.as_str())?;
        Ok(())
    }

    fn finalize_writes(&mut self) -> EngineResult<()> {
        self.assert_writable("finalizeWrites")?;
        if self.needs_optimize {
            let collection = self.require_collection("finalizeWrites")?;
            collection.optimize().map_err(|error| {
                zvec_error(
                    EngineErrorCode::from_static("STORAGE.ZVEC_OPTIMIZE_FAILED"),
                    "zvec optimize failed",
                    format!("error={error}"),
                )
            })?;
            self.needs_optimize = false;
        }
        Ok(())
    }

    fn close(&mut self) {
        if self.needs_optimize {
            if let Some(collection) = self.collection.as_ref() {
                let _ = collection.optimize();
            }
            self.needs_optimize = false;
        }
        // Dropping the collection closes it; the lock guard releases on drop
        // of `self`.
        self.collection.take();
    }
}

/// Opens the JSON file-metadata store, importing TS-generation `files.zvec`
/// metadata when the JSON store is missing — or when a TS reindex left a
/// newer `files.zvec` behind a previously imported `files.json`.
/// Read-only opens never mutate: an import stays in memory and no
/// `files.zvec` is deleted. Read-write opens persist verified imports and
/// delete the legacy collection (explicit one-way migration); partial or
/// unreadable imports persist nothing and delete nothing, so the next open
/// retries and indexing repairs the gap.
fn open_file_meta_store(
    paths: &WorkspaceIndexStoragePaths,
    read_only: bool,
) -> EngineResult<FileMetaStore> {
    let meta_exists = paths.files_meta_path.exists();
    let legacy_exists = paths.files_path.exists();
    let stale_json = meta_exists
        && legacy_exists
        && legacy_files_newer_than(&paths.files_path, &paths.files_meta_path);
    if meta_exists && !stale_json {
        return FileMetaStore::open(&paths.files_meta_path, read_only);
    }
    if !legacy_exists {
        return FileMetaStore::open(&paths.files_meta_path, read_only);
    }
    if stale_json {
        tracing::info!(
            storage = %paths.storage_path.display(),
            "legacy files.zvec is newer than files.json; re-importing TS-generation metadata"
        );
    }
    match try_import_legacy_files(&paths.files_path) {
        LegacyImportOutcome::Absent => FileMetaStore::open(&paths.files_meta_path, read_only),
        LegacyImportOutcome::Unreadable { reason } => {
            tracing::warn!(
                storage = %paths.storage_path.display(),
                reason = %reason,
                "legacy files.zvec is unreadable; falling back to full reindex without deleting anything"
            );
            FileMetaStore::open(&paths.files_meta_path, read_only)
        }
        LegacyImportOutcome::Imported { records, verified } => {
            let count = records.len();
            let store = FileMetaStore::from_records(&paths.files_meta_path, read_only, records);
            if read_only {
                tracing::info!(
                    storage = %paths.storage_path.display(),
                    files = count,
                    "imported legacy files.zvec metadata in memory; read-only open mutates nothing"
                );
                return Ok(store);
            }
            if !verified {
                tracing::warn!(
                    storage = %paths.storage_path.display(),
                    files = count,
                    "legacy files.zvec partially imported; keeping it for the next open and letting indexing repair the gap"
                );
                return Ok(store);
            }
            store.persist()?;
            if let Err(error) = std::fs::remove_dir_all(&paths.files_path) {
                tracing::warn!(
                    storage = %paths.storage_path.display(),
                    error = %error,
                    "imported legacy files.zvec metadata but could not delete the legacy collection; the next open keeps files.json"
                );
            } else {
                tracing::info!(
                    storage = %paths.storage_path.display(),
                    files = count,
                    "migrated legacy files.zvec metadata to files.json and deleted the legacy collection"
                );
            }
            Ok(store)
        }
    }
}

fn zvec_error(code: EngineErrorCode, message: &str, detail: String) -> EngineError {
    EngineError::new(code, message).with_context(detail)
}

fn zvec_error_open(detail: &str) -> EngineError {
    zvec_error(
        EngineErrorCode::from_static("STORAGE.ZVEC_OPEN_FAILED"),
        "failed to prepare zvec collection open",
        detail.to_owned(),
    )
}

static ZVEC_INIT_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

pub(crate) fn initialize_zvec() -> EngineResult<()> {
    let mutex = ZVEC_INIT_MUTEX.get_or_init(|| std::sync::Mutex::new(()));
    let _held = mutex.lock().map_err(|_| {
        EngineError::new(
            EngineErrorCode::from_static("STORAGE.ZVEC_INIT_FAILED"),
            "zvec initialization lock was poisoned",
        )
    })?;
    if zvec_rust::is_initialized() {
        return Ok(());
    }
    zvec_rust::initialize(None).map_err(|error| {
        zvec_error(
            EngineErrorCode::from_static("STORAGE.ZVEC_INIT_FAILED"),
            "failed to initialize zvec",
            error.to_string(),
        )
    })
}

fn open_zvec_collection(
    zvec_path: &str,
    read_only: bool,
    action: &str,
    open: impl Fn() -> zvec_rust::Result<Collection>,
) -> EngineResult<Collection> {
    let lock_writable = can_touch_zvec_lock(Path::new(zvec_path));
    let mut last_error = String::new();
    for attempt in 0..ZVEC_OPEN_RETRY_ATTEMPTS {
        match open() {
            Ok(collection) => return Ok(collection),
            Err(error) => {
                last_error = error.to_string();
                if attempt + 1 >= ZVEC_OPEN_RETRY_ATTEMPTS
                    || !is_retryable_zvec_open_error(&last_error, lock_writable)
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(zvec_open_retry_delay_ms(attempt)));
            }
        }
    }
    let mut detail = format!(
        "path={zvec_path} action={action} readOnly={read_only} attempts={ZVEC_OPEN_RETRY_ATTEMPTS} error={last_error}"
    );
    if let Some(hint) = missing_jieba_dict_hint(&last_error) {
        detail.push('\n');
        detail.push_str(&hint);
    }
    Err(zvec_error(
        EngineErrorCode::from_static("STORAGE.ZVEC_OPEN_FAILED"),
        "failed to open zvec collection storage",
        detail,
    ))
}

/// Explains `open_fts_indexers` failures when the jieba dictionary is not
/// configured. TS-generation entity collections use the jieba FTS
/// tokenizer; the native library resolves its dictionary from
/// `ZVEC_JIEBA_DICT_DIR` only, and `zvec-rust` 0.7 exposes no API for it
/// (setting process environment from library code would require `unsafe`,
/// forbidden here). Reading the variable is side-effect free.
fn missing_jieba_dict_hint(message: &str) -> Option<String> {
    if !message.contains("open_fts_indexers") {
        return None;
    }
    let configured = std::env::var("ZVEC_JIEBA_DICT_DIR")
        .ok()
        .filter(|dir| Path::new(dir).is_dir());
    if configured.is_some() {
        return None;
    }
    Some(
        "hint=set ZVEC_JIEBA_DICT_DIR to a jieba_dict directory to open TS-written indexes (see docs/ts-divergence.md, Phase E)".to_owned(),
    )
}

fn can_touch_zvec_lock(zvec_path: &Path) -> bool {
    let lock_path = zvec_path.join(ZVEC_LOCK_FILE);
    if !lock_path.exists() {
        return true;
    }
    std::fs::OpenOptions::new()
        .append(true)
        .open(&lock_path)
        .is_ok()
}

fn is_retryable_zvec_open_error(message: &str, lock_writable: bool) -> bool {
    if !lock_writable {
        return false;
    }
    let lower = message.to_lowercase();
    if !lower.contains("lock") {
        return false;
    }
    !(lower.contains("permission")
        || lower.contains("access denied")
        || lower.contains("eacces")
        || lower.contains("eperm")
        || lower.contains("read-only")
        || lower.contains("readonly"))
}

fn zvec_open_retry_delay_ms(attempt: u32) -> u64 {
    100u64
        .saturating_mul(1u64 << attempt)
        .min(ZVEC_OPEN_RETRY_MAX_DELAY_MS)
}

#[allow(dead_code)]
fn zvec_base_delay_ms() -> u64 {
    ZVEC_OPEN_RETRY_BASE_DELAY_MS
}
