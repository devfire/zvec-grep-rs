use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use zg_core::ids::FileId;
use zg_core::service::workspace_index::{IndexMode, WorkspaceIndex, WorkspaceIndexOptions};
use zg_core::storage::{ListEntitiesOptions, StorageOptions, create_workspace_index_storage};

use zg_core::types::{
    CURRENT_INDEX_VERSION, FileFormat, FileIndexStatus, FileInfo, FileKind, LEGACY_TS_INDEX_VERSION,
    SearchMetric, UnixMillis, WorkspaceIndexEmbeddingSchema, WorkspaceIndexInfo,
};

fn template_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/ts-index/storage-template")
}

fn expected_doc() -> Value {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/ts-index/expected.json"),
    )
    .expect("expected.json");
    serde_json::from_str(&text).expect("expected.json parses")
}

/// Copies the golden snapshot to a fresh tempdir; returns `(tempdir, storage)`.
fn staged_copy() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let storage = dir.path().join("index-storage");
    copy_dir(&template_dir(), &storage);
    (dir, storage)
}

fn copy_dir(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).expect("read golden dir") {
        let entry = entry.expect("dir entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            std::fs::create_dir_all(&target).expect("mkdir");
            copy_dir(&entry.path(), &target);
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            std::fs::copy(entry.path(), &target).expect("copy golden file");
        }
    }
}

fn embedding_schema(expected: &Value) -> WorkspaceIndexEmbeddingSchema {
    let embedding = expected.get("embedding").expect("embedding");
    WorkspaceIndexEmbeddingSchema {
        provider: embedding
            .get("provider")
            .and_then(Value::as_str)
            .expect("provider")
            .to_owned(),
        model: embedding
            .get("model")
            .and_then(Value::as_str)
            .expect("model")
            .to_owned(),
        dimension: embedding
            .get("dimension")
            .and_then(Value::as_u64)
            .expect("dimension") as usize,
        metric: match embedding.get("metric").and_then(Value::as_str).expect("metric") {
            "cosine" => SearchMetric::Cosine,
            "dot" => SearchMetric::Dot,
            "euclidean" => SearchMetric::Euclidean,
            other => panic!("unknown metric {other}"),
        },
    }
}

/// The exact `FileInfo`s the TS side reported, straight from `expected.json`.
fn expected_infos(expected: &Value) -> Vec<FileInfo> {
    expected
        .get("files")
        .and_then(Value::as_array)
        .expect("files")
        .iter()
        .map(|file| {
            let status = file.get("indexStatus").expect("indexStatus");
            FileInfo {
                id: FileId::from_raw(
                    file.get("id").and_then(Value::as_str).expect("id").to_owned(),
                ),
                absolute_path: file
                    .get("absolutePath")
                    .and_then(Value::as_str)
                    .expect("absolutePath")
                    .to_owned(),
                relative_path: file
                    .get("relativePath")
                    .and_then(Value::as_str)
                    .expect("relativePath")
                    .to_owned(),
                root_path: file
                    .get("rootPath")
                    .and_then(Value::as_str)
                    .expect("rootPath")
                    .to_owned(),
                size_bytes: file
                    .get("sizeBytes")
                    .and_then(Value::as_u64)
                    .expect("sizeBytes"),
                last_modified_time: UnixMillis::from_millis(
                    file
                        .get("lastModifiedTime")
                        .and_then(Value::as_i64)
                        .expect("lastModifiedTime"),
                ),
                content_hash: file
                    .get("contentHash")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                kind: match file.get("kind").and_then(Value::as_str).expect("kind") {
                    "text" => FileKind::Text,
                    "code" => FileKind::Code,
                    "data" => FileKind::Data,
                    "image" => FileKind::Image,
                    other => panic!("unknown kind {other}"),
                },
                format: FileFormat::parse(
                    file.get("format").and_then(Value::as_str).expect("format"),
                ),
                index_status: Some(FileIndexStatus {
                    indexed_time: status
                        .get("indexedTime")
                        .and_then(Value::as_i64)
                        .map(UnixMillis::from_millis),
                    entity_count: status
                        .get("entityCount")
                        .and_then(Value::as_u64)
                        .expect("entityCount") as usize,
                    token_count: status.get("tokenCount").and_then(Value::as_u64),
                    truncated_fragment_count: status
                        .get("truncatedFragmentCount")
                        .and_then(Value::as_u64)
                        .map(|value| value as usize),
                    error: status
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                }),
            }
        })
        .collect()
}

/// TS `makeFileId`: `sha256_hex(workspaceIndexId + "\\0" + absolutePath)`.
fn ts_file_id(workspace_index_id: &str, absolute_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace_index_id.as_bytes());
    hasher.update([0]);
    hasher.update(absolute_path.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn workspace_info(
    storage: &Path,
    schema: &WorkspaceIndexEmbeddingSchema,
    version: i64,
) -> WorkspaceIndexInfo {
    WorkspaceIndexInfo {
        id: "compat-probe".to_owned(),
        name: "compat-probe".to_owned(),
        path: storage.to_string_lossy().into_owned(),
        root_paths: Vec::new(),
        index_policy: None,
        embedding: Some(Some(schema.clone())),
        index_version: Some(version),
        created_time: UnixMillis::from_millis(1700000000000),
        updated_time: UnixMillis::from_millis(1700000000000),
    }
}

#[test]
fn imports_real_ts_index_exactly_and_migrates_one_way() {
    let expected = expected_doc();
    let (_dir, storage) = staged_copy();
    let schema = embedding_schema(&expected);

    let storage_handle = create_workspace_index_storage(StorageOptions::ReadWrite {
        storage_path: &storage,
        embedding: &schema,
    })
    .expect("open TS-written storage");
    let imported = storage_handle.list_files();
    assert_eq!(imported, expected_infos(&expected));

    // The TS id scheme is pinned cross-language, not just echoed back.
    let workspace_index_id = expected
        .get("workspaceIndexId")
        .and_then(Value::as_str)
        .expect("workspaceIndexId");
    for info in &imported {
        assert_eq!(
            info.id.as_str(),
            ts_file_id(workspace_index_id, &info.absolute_path)
        );
    }

    // Stance: verified import migrates one way — legacy collection deleted,
    // JSON store written.
    assert!(
        !storage.join("files.zvec").exists(),
        "files.zvec must be deleted after a verified import"
    );
    assert!(storage.join("files.json").exists());

    // The shared entity collection opens under Rust and links to the
    // imported metadata.
    let counts = expected
        .get("entityCounts")
        .and_then(Value::as_object)
        .expect("entityCounts");
    for info in &imported {
        let entities = storage_handle.list_entities_by_file(
            &info.id,
            ListEntitiesOptions { limit: None, offset: None },
        );
        let want = counts
            .get(&info.relative_path)
            .and_then(Value::as_u64)
            .expect("entity count") as usize;
        assert_eq!(entities.len(), want, "{}", info.relative_path);
        assert_eq!(
            info.index_status.as_ref().expect("status").entity_count,
            want
        );
    }
}

#[test]
fn read_only_open_imports_in_memory_and_mutates_nothing() {
    let expected = expected_doc();
    let (_dir, storage) = staged_copy();

    let storage_handle = create_workspace_index_storage(StorageOptions::ReadOnly {
        storage_path: &storage,
    })
    .expect("read-only open of TS-written storage");
    assert_eq!(storage_handle.list_files(), expected_infos(&expected));
    assert!(
        storage.join("files.zvec").exists(),
        "read-only open must not delete the legacy collection"
    );
    assert!(
        !storage.join("files.json").exists(),
        "read-only open must not persist files.json"
    );
}

#[test]
fn corrupt_legacy_collection_falls_back_to_reindex_without_deleting() {
    for shape in ["garbage-file", "empty-dir"] {
        let (_dir, storage) = staged_copy();
        std::fs::remove_dir_all(storage.join("files.zvec")).expect("clear legacy");
        match shape {
            "garbage-file" => {
                std::fs::write(storage.join("files.zvec"), b"not a collection").expect("garbage");
            }
            _ => std::fs::create_dir(storage.join("files.zvec")).expect("empty dir"),
        }
        let expected = expected_doc();
        let schema = embedding_schema(&expected);
        let storage_handle = create_workspace_index_storage(StorageOptions::ReadWrite {
            storage_path: &storage,
            embedding: &schema,
        })
        .expect("corrupt legacy must not fail open");
        assert!(
            storage_handle.list_files().is_empty(),
            "{shape}: no files, so the caller reindexes"
        );
        // Delete nothing, persist nothing.
        assert!(storage.join("files.zvec").exists(), "{shape}: untouched");
        assert!(
            !storage.join("files.json").exists(),
            "{shape}: no files.json from an unverified import"
        );
    }
}

#[test]
fn newer_ts_reindex_triggers_reimport_but_rust_newer_wins() {
    let expected = expected_doc();
    let schema = embedding_schema(&expected);

    // A TS reindex after a Rust run: newer files.zvec, older files.json.
    let (_dir, storage) = staged_copy();
    let first = create_workspace_index_storage(StorageOptions::ReadWrite {
        storage_path: &storage,
        embedding: &schema,
    })
    .expect("initial import");
    assert_eq!(first.list_files().len(), 3);
    drop(first);
    pin_mtime(&storage.join("files.json"), 1000);
    copy_dir(&template_dir().join("files.zvec"), &storage.join("files.zvec"));
    let second = create_workspace_index_storage(StorageOptions::ReadWrite {
        storage_path: &storage,
        embedding: &schema,
    })
    .expect("re-import");
    assert_eq!(second.list_files(), expected_infos(&expected));
    drop(second);
    assert!(
        !storage.join("files.zvec").exists(),
        "re-imported legacy collection is deleted again"
    );

    // Rust newer: the legacy collection is left alone, files.json wins.
    let (_dir, storage) = staged_copy();
    let first = create_workspace_index_storage(StorageOptions::ReadWrite {
        storage_path: &storage,
        embedding: &schema,
    })
    .expect("initial import");
    assert_eq!(first.list_files().len(), 3);
    drop(first);
    copy_dir(&template_dir().join("files.zvec"), &storage.join("files.zvec"));
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(storage.join("files.json"))
        .expect("open files.json")
        .set_modified(future)
        .expect("pin files.json into the future");
    let third = create_workspace_index_storage(StorageOptions::ReadWrite {
        storage_path: &storage,
        embedding: &schema,
    })
    .expect("open with stale legacy collection");
    assert_eq!(third.list_files().len(), 3);
    drop(third);
    assert!(
        storage.join("files.zvec").exists(),
        "a legacy collection older than files.json is never stolen"
    );
}

#[test]
fn index_version_gates_ts_and_rust_generations() {
    assert_eq!(LEGACY_TS_INDEX_VERSION, 1);
    assert_eq!(CURRENT_INDEX_VERSION, 2);

    let expected = expected_doc();
    let schema = embedding_schema(&expected);
    let (_dir, storage) = staged_copy();

    // v1 opens: the legacy generation is importable, not foreign.
    match WorkspaceIndex::open(
        workspace_info(&storage, &schema, LEGACY_TS_INDEX_VERSION),
        WorkspaceIndexOptions { mode: IndexMode::Read, embedding_model: None },
    ) {
        Ok(_) => {}
        Err(error) => panic!("v1 index must open, got {error}"),
    }

    // Unknown future versions still fail closed with the frozen code string.
    match WorkspaceIndex::open(
        workspace_info(&storage, &schema, 3),
        WorkspaceIndexOptions { mode: IndexMode::Read, embedding_model: None },
    ) {
        Ok(_) => panic!("version 3 must be rejected"),
        Err(error) => assert_eq!(
            error.code().to_string(),
            "ZVEC_GREP.ENGINE.WORKSPACE_INDEX.VERSION_MISMATCH"
        ),
    }

    // The golden fixture really is a TS-generation index.
    assert_eq!(
        expected.get("indexVersion").and_then(Value::as_i64),
        Some(LEGACY_TS_INDEX_VERSION)
    );
}

fn pin_mtime(path: &Path, secs: u64) {
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open for mtime")
        .set_modified(old)
        .expect("pin mtime");
}
