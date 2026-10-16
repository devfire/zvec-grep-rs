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

use zg_core::ids::{FileId, make_entity_id};
use zg_core::storage::layout::{delete_workspace_index_storage, has_workspace_index_storage};
use zg_core::storage::{IndexedFragment, StorageOptions, create_workspace_index_storage};
use zg_core::types::{
    Content, Entity, EntityFragment, FileFormat, FileInfo, FileKind, Range, SearchMetric,
    UnixMillis, WorkspaceIndexEmbeddingSchema,
};

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

fn indexed_file(id: &str, root: &std::path::Path, name: &str) -> (FileInfo, IndexedFragment) {
    let file = FileInfo {
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
    };
    let text = format!("text for {name}");
    let fragment = IndexedFragment {
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
                content: Content::Text { text },
                metadata: None,
            },
            group: None,
        },
        vector: vec![0.25, 0.5, 0.75, 1.0],
    };
    (file, fragment)
}

#[test]
fn quarantined_metadata_does_not_leave_orphan_documents() {
    // #75: corrupt files.json is quarantined and metadata restarts empty;
    // the old collection docs must not survive as un-deletable orphans.
    let dir = TempDir::new().expect("tempdir");
    let storage_path = dir.path().join("storage");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("repo root");
    let schema = dummy_schema();
    let open_rw = |path: &std::path::Path| {
        create_workspace_index_storage(StorageOptions::ReadWrite {
            storage_path: path,
            embedding: &schema,
        })
    };

    let (file_a, fragment_a) = indexed_file("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &root, "a.txt");
    {
        let mut storage = open_rw(&storage_path).expect("initial open");
        storage
            .replace_file(&file_a, &[fragment_a], None)
            .expect("index a");
        storage.finalize_writes().expect("finalize");
    }

    std::fs::write(storage_path.join("files.json"), "{ not json").expect("corrupt meta");
    assert!(open_rw(&storage_path).is_err(), "corrupt meta must surface");

    let (file_b, fragment_b) = indexed_file("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &root, "b.txt");
    let mut storage = open_rw(&storage_path).expect("reopen after quarantine");
    assert!(storage.list_files().is_empty());
    storage
        .replace_file(&file_b, &[fragment_b], None)
        .expect("index b");
    storage.finalize_writes().expect("finalize b");
    // Top-1 vector recall: an orphaned a.txt doc would take the slot and
    // be filtered out against the empty-then-b-only metadata.
    let hits = storage
        .search_vector(&[0.25, 0.5, 0.75, 1.0], 1, None)
        .expect("vector search");
    assert_eq!(hits.len(), 1, "orphan doc consumed the top-1 slot");
    assert_eq!(hits.first().expect("one hit").file.relative_path, "b.txt");
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
