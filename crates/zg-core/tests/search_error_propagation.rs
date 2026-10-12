//! Storage errors must surface from search instead of reading as empty results.
//!
//! Regression test for <https://github.com/devfire/zvec-grep-rs/issues/73>:
//! recall mapped `search_fts`/`search_vector` failures to
//! `unwrap_or_default()`, and `get_entity` mapped fetch failures to
//! `unwrap_or(None)`, so a closed collection (or an FTS syntax failure)
//! presented as "no results" with a success status. Upstream `querySync`
//! throws; the pipeline now propagates.
//!
//! The closed collection is the deterministic trigger: every recall and
//! entity read fails with `STORAGE.COLLECTION_CLOSED`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use tempfile::TempDir;

use zg_core::EngineErrorCode;
use zg_core::ids::{FileId, make_entity_id};
use zg_core::pipeline::search::{SearchContext, search_workspace_index};
use zg_core::storage::{
    IndexedFragment, StorageOptions, WorkspaceIndexStorage, create_workspace_index_storage,
};
use zg_core::types::{
    CURRENT_INDEX_VERSION, Content, Entity, EntityFragment, FileFormat, FileInfo, FileKind, Range,
    RootPath, SearchMetric, SearchPlan, SearchPlanRoute, SearchPlanRouteMode, UnixMillis,
    WorkspaceIndexEmbeddingSchema, WorkspaceIndexInfo,
};

fn dummy_schema() -> WorkspaceIndexEmbeddingSchema {
    WorkspaceIndexEmbeddingSchema {
        provider: "test".to_owned(),
        model: "dummy".to_owned(),
        dimension: 4,
        metric: SearchMetric::Cosine,
    }
}

fn test_info(storage_path: &Path) -> WorkspaceIndexInfo {
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
        embedding: Some(Some(dummy_schema())),
        index_version: Some(CURRENT_INDEX_VERSION),
        created_time: UnixMillis::now(),
        updated_time: UnixMillis::now(),
    }
}

fn file_info(id: &str, root: &Path, name: &str) -> FileInfo {
    FileInfo {
        id: FileId::from_raw(id.to_owned()),
        absolute_path: root.join(name).to_string_lossy().into_owned(),
        relative_path: name.to_owned(),
        root_path: root.to_string_lossy().into_owned(),
        size_bytes: 16,
        last_modified_time: UnixMillis::from_millis(1_700_000_000_000),
        content_hash: Some(format!("hash-{id}")),
        kind: FileKind::Text,
        format: FileFormat::parse("text"),
        index_status: None,
    }
}

fn fragment(file: &FileInfo, text: &str) -> IndexedFragment {
    IndexedFragment {
        fragment: EntityFragment {
            entity: Entity {
                id: make_entity_id(&file.id, 0),
                file_id: file.id.clone(),
                range: Range::Text {
                    start_line: 0,
                    end_line: 0,
                    start_offset: 0,
                    end_offset: text.len(),
                },
                content: Content::Text {
                    text: text.to_owned(),
                },
                metadata: None,
            },
            group: None,
        },
        vector: vec![0.25, 0.5, 0.75, 1.0],
    }
}

/// One seeded file, then the storage is closed: every subsequent recall and
/// entity read fails with `STORAGE.COLLECTION_CLOSED`.
fn closed_storage(dir: &TempDir) -> Box<dyn WorkspaceIndexStorage> {
    let storage_path = dir.path().join("storage");
    let schema = dummy_schema();
    let mut storage = create_workspace_index_storage(StorageOptions::ReadWrite {
        storage_path: &storage_path,
        embedding: &schema,
    })
    .expect("open read-write storage");
    let file = file_info("seed", dir.path(), "seed.txt");
    std::fs::write(&file.absolute_path, "hello world recall target\n").expect("write seed");
    storage
        .replace_file(&file, &[fragment(&file, "hello world recall target")], None)
        .expect("seed storage");
    storage.close();
    storage
}

fn fts_plan(query: &str) -> SearchPlan {
    SearchPlan {
        routes: vec![SearchPlanRoute {
            mode: SearchPlanRouteMode::Fts,
            query: query.to_owned(),
        }],
        ..SearchPlan::default()
    }
}

#[test]
fn closed_storage_search_returns_error_not_empty_results() {
    let dir = TempDir::new().expect("tempdir");
    let storage = closed_storage(&dir);
    let info = test_info(&dir.path().join("storage"));
    let ctx = SearchContext {
        workspace_index: info,
        storage: &*storage,
        embedding_model: None,
    };
    let error = search_workspace_index(&fts_plan("hello"), &ctx)
        .expect_err("search on closed storage must fail");
    assert_eq!(*error.code(), EngineErrorCode::StorageCollectionClosed);
}

#[test]
fn closed_storage_get_entity_returns_error_not_none() {
    let dir = TempDir::new().expect("tempdir");
    let storage = closed_storage(&dir);
    let entity_id = make_entity_id(&FileId::from_raw("seed".to_owned()), 0);
    let error = storage
        .get_entity(&entity_id)
        .expect_err("get_entity on closed storage must fail");
    assert_eq!(*error.code(), EngineErrorCode::StorageCollectionClosed);
}
