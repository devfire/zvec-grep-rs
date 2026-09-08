//! Standalone stance: a TypeScript-generation `files.zvec` is foreign.
//!
//! This build never migrates TS indexes, so storage open must fail loudly
//! (not adopt, not delete) when one is present, and the legacy name must
//! not count as an index nor be removed by delete.

use tempfile::TempDir;

use zg_core::storage::layout::{delete_workspace_index_storage, has_workspace_index_storage};
use zg_core::storage::{StorageOptions, create_workspace_index_storage};
use zg_core::types::{SearchMetric, WorkspaceIndexEmbeddingSchema};

fn dummy_schema() -> WorkspaceIndexEmbeddingSchema {
    WorkspaceIndexEmbeddingSchema {
        provider: "test".to_owned(),
        model: "dummy".to_owned(),
        dimension: 4,
        metric: SearchMetric::Cosine,
    }
}

#[test]
fn foreign_ts_collection_aborts_open_and_survives_delete() {
    let dir = TempDir::new().expect("tempdir");
    let storage = dir.path().join("storage");
    std::fs::create_dir_all(storage.join("files.zvec")).expect("foreign dir");
    let schema = dummy_schema();

    for read_only in [false, true] {
        let result = if read_only {
            create_workspace_index_storage(StorageOptions::ReadOnly {
                storage_path: &storage,
            })
        } else {
            create_workspace_index_storage(StorageOptions::ReadWrite {
                storage_path: &storage,
                embedding: &schema,
            })
        };
        match result {
            Ok(_) => panic!("foreign files.zvec must abort open"),
            Err(error) => assert_eq!(
                error.code().to_string(),
                "ZVEC_GREP.ENGINE.STORAGE.FOREIGN_TS_INDEX_PRESENT"
            ),
        }
    }

    // Not ours: not an index, and delete leaves it alone.
    assert!(!has_workspace_index_storage(&storage));
    delete_workspace_index_storage(&storage).expect("delete");
    assert!(storage.join("files.zvec").exists());
}
