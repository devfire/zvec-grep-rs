//! Opened workspace index: version/embedding validation plus indexing,
//! status, and search over one storage handle.
//!
//! Port of `engine/service/workspace-index.ts` (`WorkspaceIndex`,
//! `isWorkspaceIndexed`).

use std::sync::Arc;

use crate::error::{
    DetailEntry, DetailValue, EngineError, EngineErrorCode, EngineResult, codes, error_details,
    workspace_index_detail,
};
use crate::models::EmbeddingModel;
use crate::pipeline::indexing::scanner::CancelFlag;
use crate::pipeline::indexing::{
    IndexContext, IndexProgressSink, get_workspace_index_status, index_workspace,
    index_workspace_paths,
};
use crate::pipeline::search::{SearchContext, search_workspace_index};
use crate::storage::{StorageOptions, WorkspaceIndexStorage, create_workspace_index_storage};
use crate::types::{
    CURRENT_INDEX_VERSION, IndexResult, SearchPlan, SearchPlanResult,
    WorkspaceIndexEmbeddingSchema, WorkspaceIndexInfo, WorkspaceIndexStatus,
};

/// Open mode (mirrors `WorkspaceIndexOptions["mode"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMode {
    Read,
    Write,
}

/// Options for [`WorkspaceIndex::open`].
pub struct WorkspaceIndexOptions {
    pub mode: IndexMode,
    pub embedding_model: Option<Arc<dyn EmbeddingModel>>,
}

/// Per-run indexing options (mirrors `IndexOptions`).
#[derive(Clone, Default)]
pub struct IndexOptions {
    pub embedding_concurrency: Option<usize>,
    pub on_progress: Option<IndexProgressSink>,
    pub changed_paths: Option<Vec<String>>,
    pub cancel: Option<CancelFlag>,
}

/// One opened workspace index.
///
/// Single-owner close: [`close`](WorkspaceIndex::close) consumes the handle, so a
/// `&mut` re-close is impossible at compile time. The explicit close is the only
/// owner of storage shutdown; [`Drop`] only backstops a handle dropped without
/// close. Every method rejects a closed handle with
/// `SERVICE.READ_SESSION_CLOSED` instead of touching closed storage.
pub struct WorkspaceIndex {
    info: WorkspaceIndexInfo,
    storage: Box<dyn WorkspaceIndexStorage>,
    embedding: WorkspaceIndexEmbeddingSchema,
    embedding_model: Option<Arc<dyn EmbeddingModel>>,
    closed: bool,
}

impl WorkspaceIndex {
    /// Opens (or creates, in write mode) the index described by `info`.
    ///
    /// # Errors
    ///
    /// Returns `WORKSPACE_INDEX.MISSING` when the index was never built, a version or embedding
    /// schema mismatch when the record disagrees with the current version or model, or a storage
    /// error when the collection cannot be opened.
    pub fn open(info: WorkspaceIndexInfo, options: WorkspaceIndexOptions) -> EngineResult<Self> {
        let embedding = require_workspace_index_embedding(&info, "open")?;
        validate_index_version(&info)?;
        if let Some(model) = &options.embedding_model {
            validate_embedding_schema(&info, &embedding, model.as_ref())?;
        }
        let storage_path = std::path::Path::new(&info.path);
        let storage = match options.mode {
            IndexMode::Write => create_workspace_index_storage(StorageOptions::ReadWrite {
                storage_path,
                embedding: &embedding,
            })?,
            IndexMode::Read => {
                create_workspace_index_storage(StorageOptions::ReadOnly { storage_path })?
            }
        };
        Ok(Self {
            info,
            storage,
            embedding,
            embedding_model: options.embedding_model,
            closed: false,
        })
    }

    /// Index display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.info.name
    }

    /// Index identity the handle was opened with.
    #[must_use]
    pub fn info(&self) -> &WorkspaceIndexInfo {
        &self.info
    }

    /// Recorded embedding schema the handle was opened with.
    #[must_use]
    pub fn embedding(&self) -> &WorkspaceIndexEmbeddingSchema {
        &self.embedding
    }

    /// Runs indexing, or changed-path indexing when requested (mirrors
    /// `WorkspaceIndex#index`).
    ///
    /// # Errors
    ///
    /// Returns `SERVICE.READ_SESSION_CLOSED` when the handle is closed,
    /// `WORKSPACE_INDEX.READ_ONLY` for a read-only handle,
    /// `WORKSPACE_INDEX.EMBEDDING_MODEL_REQUIRED` without a model, or an indexing error when the
    /// run fails.
    pub fn index(&mut self, options: &IndexOptions) -> EngineResult<IndexResult> {
        self.require_open()?;
        if self.storage.read_only() {
            return Err(EngineError::new(
                EngineErrorCode::WorkspaceIndexReadOnly,
                "cannot update a read-only workspace index",
            )
            .with_context(workspace_index_operation_details(&self.info.name, "index")));
        }
        let embedding_model = self.require_embedding_model("index")?;
        let mut ctx = IndexContext {
            workspace_index: self.info.clone(),
            storage: &mut *self.storage,
            embedding_model,
            embedding_concurrency: options.embedding_concurrency,
            on_progress: options.on_progress.clone(),
            cancel: options.cancel.clone(),
        };
        match &options.changed_paths {
            Some(paths) if !paths.is_empty() => index_workspace_paths(&mut ctx, paths),
            _ => index_workspace(&mut ctx),
        }
    }

    /// Derives status from stored files (mirrors `WorkspaceIndex#status`).
    ///
    /// # Errors
    ///
    /// Returns `SERVICE.READ_SESSION_CLOSED` when the handle is closed, or an error when the
    /// workspace root paths cannot be scanned.
    pub fn status(&self) -> EngineResult<WorkspaceIndexStatus> {
        self.require_open()?;
        get_workspace_index_status(&self.info, &self.storage.list_files(), None)
    }

    /// Executes a hybrid search plan (mirrors `WorkspaceIndex#searchPlan`).
    ///
    /// # Errors
    ///
    /// Returns `SERVICE.READ_SESSION_CLOSED` when the handle is closed, or an error when the
    /// plan is invalid or embedding recall or storage search fails.
    pub fn search_plan(&self, plan: &SearchPlan) -> EngineResult<SearchPlanResult> {
        self.require_open()?;
        search_workspace_index(
            plan,
            &SearchContext {
                workspace_index: self.info.clone(),
                storage: &*self.storage,
                embedding_model: self.embedding_model.clone(),
            },
        )
    }

    /// Rejects use after close with `SERVICE.READ_SESSION_CLOSED`.
    fn require_open(&self) -> EngineResult<()> {
        if self.closed {
            return Err(EngineError::new(
                codes::service_read_session_closed(),
                "workspace index is already closed",
            ));
        }
        Ok(())
    }

    /// Closes the underlying storage handle (mirrors `close`).
    ///
    /// Consuming close is the single-owner primitive: the one owner calls this
    /// once, and [`Drop`] skips the already-closed handle. A second close is a
    /// compile-time move error, not a runtime state.
    pub fn close(mut self) {
        if !self.closed {
            self.storage.close();
            self.closed = true;
        }
    }

    fn require_embedding_model(&self, operation: &str) -> EngineResult<Arc<dyn EmbeddingModel>> {
        self.embedding_model.clone().ok_or_else(|| {
            EngineError::new(
                EngineErrorCode::WorkspaceIndexEmbeddingModelRequired,
                "workspace index operation requires an embedding model",
            )
            .with_context(workspace_index_operation_details(
                &self.info.name,
                operation,
            ))
        })
    }
}

impl Drop for WorkspaceIndex {
    /// Backstop for handles dropped without [`close`](WorkspaceIndex::close):
    /// shuts storage down once. Explicit close sets `closed`, so this is a
    /// no-op on the single-owner path.
    fn drop(&mut self) {
        if !self.closed {
            self.storage.close();
        }
    }
}

fn validate_index_version(info: &WorkspaceIndexInfo) -> EngineResult<()> {
    if info.index_version == Some(CURRENT_INDEX_VERSION) {
        return Ok(());
    }
    let actual = info
        .index_version
        .map(|version| version.to_string())
        .unwrap_or_else(|| "null".to_owned());
    let detail = error_details(vec![
        DetailEntry::Line(&workspace_index_detail(&info.name)),
        DetailEntry::Pair("expected", DetailValue::Int(CURRENT_INDEX_VERSION)),
        DetailEntry::Pair("actual", DetailValue::Str(&actual)),
        DetailEntry::Pair(
            "hint",
            DetailValue::Str(
                "Recreate the index with the current zvec-grep version; run \"zg index --rebuild\" for a workspace index.",
            ),
        ),
    ])
    .unwrap_or_default();
    Err(EngineError::new(
        EngineErrorCode::WorkspaceIndexVersionMismatch,
        "workspace index version is not supported",
    )
    .with_context(detail))
}

fn validate_embedding_schema(
    info: &WorkspaceIndexInfo,
    expected: &WorkspaceIndexEmbeddingSchema,
    current: &dyn EmbeddingModel,
) -> EngineResult<()> {
    validate_index_version(info)?;
    let actual = current.info();
    if expected.provider != actual.provider {
        return Err(schema_mismatch(
            &info.name,
            "provider",
            EngineErrorCode::WorkspaceIndexEmbeddingProviderMismatch,
            "workspace index embedding provider does not match current model",
            &expected.provider,
            &actual.provider,
        ));
    }
    if expected.model != actual.model {
        return Err(schema_mismatch(
            &info.name,
            "model",
            EngineErrorCode::WorkspaceIndexEmbeddingModelMismatch,
            "workspace index embedding model does not match current model",
            &expected.model,
            &actual.model,
        ));
    }
    if expected.dimension != actual.dimension {
        return Err(schema_mismatch(
            &info.name,
            "dimension",
            EngineErrorCode::WorkspaceIndexEmbeddingDimensionMismatch,
            "workspace index embedding dimension does not match current model",
            &expected.dimension.to_string(),
            &actual.dimension.to_string(),
        ));
    }
    if expected.metric != actual.metric {
        return Err(schema_mismatch(
            &info.name,
            "metric",
            EngineErrorCode::WorkspaceIndexEmbeddingMetricMismatch,
            "workspace index embedding metric does not match current model",
            &format!("{:?}", expected.metric),
            &format!("{:?}", actual.metric),
        ));
    }
    Ok(())
}

fn schema_mismatch(
    name: &str,
    _field: &str,
    code: EngineErrorCode,
    message: &str,
    expected: &str,
    actual: &str,
) -> EngineError {
    let detail = error_details(vec![
        DetailEntry::Line(&workspace_index_detail(name)),
        DetailEntry::Pair("expected", DetailValue::Str(expected)),
        DetailEntry::Pair("actual", DetailValue::Str(actual)),
    ])
    .unwrap_or_default();
    EngineError::new(code, message).with_context(detail)
}

fn workspace_index_operation_details(name: &str, operation: &str) -> String {
    error_details(vec![
        DetailEntry::Line(&workspace_index_detail(name)),
        DetailEntry::Pair("operation", DetailValue::Str(operation)),
    ])
    .unwrap_or_default()
}

/// True when the manifest record describes a built index (mirrors
/// `isWorkspaceIndexed`).
#[must_use]
pub fn is_workspace_indexed(info: &WorkspaceIndexInfo) -> bool {
    matches!(info.embedding, Some(Some(_))) && info.index_version.is_some()
}

fn require_workspace_index_embedding(
    info: &WorkspaceIndexInfo,
    operation: &str,
) -> EngineResult<WorkspaceIndexEmbeddingSchema> {
    if let Some(Some(embedding)) = &info.embedding
        && info.index_version.is_some()
    {
        return Ok(embedding.clone());
    }
    let detail = error_details(vec![
        DetailEntry::Line(&workspace_index_detail(&info.name)),
        DetailEntry::Pair("operation", DetailValue::Str(operation)),
        DetailEntry::Pair(
            "hint",
            DetailValue::Str("Run zg index to build this index."),
        ),
    ])
    .unwrap_or_default();
    Err(EngineError::new(
        EngineErrorCode::WorkspaceIndexMissing,
        "workspace index has not been built",
    )
    .with_context(detail))
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::ids::FileId;
    use crate::types::{FileFormat, FileInfo, FileKind, RootPath, SearchMetric, UnixMillis};

    fn test_schema() -> WorkspaceIndexEmbeddingSchema {
        WorkspaceIndexEmbeddingSchema {
            provider: "test".to_owned(),
            model: "dummy".to_owned(),
            dimension: 4,
            metric: SearchMetric::Cosine,
        }
    }

    fn test_info(storage_path: &std::path::Path) -> WorkspaceIndexInfo {
        WorkspaceIndexInfo {
            id: "test".to_owned(),
            name: "test".to_owned(),
            path: storage_path.to_string_lossy().into_owned(),
            root_paths: vec![RootPath {
                absolute_path: storage_path.to_string_lossy().into_owned(),
                recursive: true,
                ..RootPath::default()
            }],
            index_policy: None,
            embedding: Some(Some(test_schema())),
            index_version: Some(CURRENT_INDEX_VERSION),
            created_time: UnixMillis::now(),
            updated_time: UnixMillis::now(),
        }
    }

    /// One indexed file record so `files.json` exists: `close`/`drop` only
    /// checkpoint staged writes, so a never-written store has no metadata
    /// file and a read-only reopen correctly fails with FILE_META_MISSING.
    fn seed_file_info(storage_path: &std::path::Path) -> FileInfo {
        let absolute_path = storage_path.join("seed.txt").to_string_lossy().into_owned();
        std::fs::write(&absolute_path, "0123456789abcdef").expect("write seed file");
        FileInfo {
            id: FileId::from_raw("seed".to_owned()),
            absolute_path,
            relative_path: "seed.txt".to_owned(),
            root_path: storage_path.to_string_lossy().into_owned(),
            size_bytes: 16,
            last_modified_time: UnixMillis::from_millis(1_700_000_000_000),
            content_hash: Some("hash-seed".to_owned()),
            kind: FileKind::Text,
            format: FileFormat::parse("text"),
            index_status: None,
        }
    }

    /// A handle already in the post-close state: consuming `close(self)` makes
    /// this unreachable at runtime, so tests construct it to prove the guard.
    fn closed_index(dir: &tempfile::TempDir) -> WorkspaceIndex {
        let storage_path = dir.path().join("storage");
        let schema = test_schema();
        let storage = create_workspace_index_storage(StorageOptions::ReadWrite {
            storage_path: &storage_path,
            embedding: &schema,
        })
        .expect("open test storage");
        WorkspaceIndex {
            info: test_info(&storage_path),
            storage,
            embedding: schema,
            embedding_model: None,
            closed: true,
        }
    }

    #[test]
    fn close_then_index_errors() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let mut index = closed_index(&dir);
        let error = index
            .index(&IndexOptions::default())
            .expect_err("index after close errors");
        assert_eq!(*error.code(), EngineErrorCode::ServiceReadSessionClosed);
    }

    #[test]
    fn close_then_status_errors() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let index = closed_index(&dir);
        let error = index.status().expect_err("status after close errors");
        assert_eq!(*error.code(), EngineErrorCode::ServiceReadSessionClosed);
    }

    #[test]
    fn double_close_is_noop() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        closed_index(&dir).close();
    }
    #[test]
    fn in_flight_manifest_reads_as_not_indexed() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // Pre-run shape: embedding recorded, version withheld until the run
        // commits. A kill in that window must read as incomplete.
        let mut info = test_info(dir.path());
        info.index_version = None;
        assert!(!is_workspace_indexed(&info));
        info.index_version = Some(CURRENT_INDEX_VERSION);
        assert!(is_workspace_indexed(&info));
    }

    #[test]
    fn drop_without_close_leaves_storage_reusable() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let storage_path = dir.path().join("storage");
        let info = test_info(&storage_path);
        let mut index = WorkspaceIndex::open(
            info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Write,
                embedding_model: None,
            },
        )
        .expect("open test index");
        // Checkpoint one file record so `files.json` exists for the
        // read-only reopen below.
        index
            .storage
            .replace_file(&seed_file_info(&storage_path), &[], None)
            .expect("seed test storage");
        drop(index);
        let reopened = WorkspaceIndex::open(
            info,
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: None,
            },
        )
        .expect("reopen after drop");
        reopened.status().expect("status after reopen");
    }
}
