//! Standalone stance: a TypeScript-generation `files.zvec` is foreign.
//!
//! This build never migrates TS indexes, so storage open must fail loudly
//! (not adopt, not delete) when one is present, and the legacy name must
//! not count as an index nor be removed by delete.

// Test targets exercise fallible fixtures directly: `unwrap`/`expect`/`panic!`
// refusal branches are the same class the crate roots allow under `cfg(test)`
// (integration tests are separate crates, so they carry their own allow).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
                embedding: &schema,
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

#[test]
fn read_only_open_verifies_embedding_fingerprint() {
    let dir = TempDir::new().expect("tempdir");
    let storage = dir.path().join("storage");
    let schema = dummy_schema();
    {
        let mut created = create_workspace_index_storage(StorageOptions::ReadWrite {
            storage_path: &storage,
            embedding: &schema,
        })
        .expect("create storage");
        created.close();
    }
    // Matching provenance: read-only open succeeds.
    create_workspace_index_storage(StorageOptions::ReadOnly {
        storage_path: &storage,
        embedding: &schema,
    })
    .expect("read-only open with matching fingerprint");

    let fingerprint_path = storage.join("embedding.json");
    let original = std::fs::read_to_string(&fingerprint_path).expect("read fingerprint");

    // Same dimension/metric, different model: silently serving this would
    // rank hits with the wrong model's vectors.
    std::fs::write(&fingerprint_path, original.replace("dummy", "other-model"))
        .expect("tamper model");
    let error = create_workspace_index_storage(StorageOptions::ReadOnly {
        storage_path: &storage,
        embedding: &schema,
    })
    .err()
    .expect("tampered model must refuse read-only open");
    assert_eq!(
        error.code().to_string(),
        "ZVEC_GREP.ENGINE.WORKSPACE_INDEX.EMBEDDING_MODEL_MISMATCH"
    );

    // Foreign (TS-generation `jieba`) tokenizer is refused, not adopted.
    std::fs::write(&fingerprint_path, original.replace("standard", "jieba"))
        .expect("tamper tokenizer");
    let error = create_workspace_index_storage(StorageOptions::ReadOnly {
        storage_path: &storage,
        embedding: &schema,
    })
    .err()
    .expect("foreign tokenizer must refuse read-only open");
    assert_eq!(
        error.code().to_string(),
        "ZVEC_GREP.ENGINE.STORAGE.FOREIGN_TS_INDEX_PRESENT"
    );

    // Missing sidecar: a collection this build did not stamp.
    std::fs::remove_file(&fingerprint_path).expect("remove fingerprint");
    let error = create_workspace_index_storage(StorageOptions::ReadOnly {
        storage_path: &storage,
        embedding: &schema,
    })
    .err()
    .expect("missing fingerprint must refuse read-only open");
    assert_eq!(
        error.code().to_string(),
        "ZVEC_GREP.ENGINE.STORAGE.FOREIGN_TS_INDEX_PRESENT"
    );
}
