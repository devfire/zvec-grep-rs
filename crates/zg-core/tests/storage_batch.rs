//! Batched storage writes: a multi-file delete preserves an unrelated sentinel.
//!
//! A filter-string assertion alone cannot prove this: the test indexes three
//! files, batch-deletes two, then confirms the sentinel's metadata record and
//! document contents both before and after reopening the storage (i.e. the
//! batched metadata snapshots actually persisted).

// Test targets exercise fallible fixtures directly: `unwrap`/`expect`/`panic!`
// refusal branches are the same class the crate roots allow under `cfg(test)`
// (integration tests are separate crates, so they carry their own allow).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use tempfile::TempDir;

use zg_core::ids::{FileId, make_entity_id};
use zg_core::storage::{
    IndexedFragment, StorageOptions, WorkspaceIndexStorage, create_workspace_index_storage,
};
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

fn open_rw(
    storage_path: &Path,
    schema: &WorkspaceIndexEmbeddingSchema,
) -> Box<dyn WorkspaceIndexStorage> {
    create_workspace_index_storage(StorageOptions::ReadWrite {
        storage_path,
        embedding: schema,
    })
    .expect("open read-write storage")
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

fn entity_texts(storage: &dyn WorkspaceIndexStorage, file: &FileInfo) -> Vec<String> {
    storage
        .list_entities_by_file(&file.id, zg_core::storage::ListEntitiesOptions::default())
        .into_iter()
        .map(|stored| match stored.entity.content {
            Content::Text { text } => text,
            Content::Image { .. } => panic!("expected text entity"),
        })
        .collect()
}

#[test]
fn multi_file_delete_batch_preserves_sentinel() {
    let dir = TempDir::new().expect("tempdir");
    let storage_path = dir.path().join("storage");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("repo root");
    let schema = dummy_schema();

    let file_a = file_info("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &root, "a.txt");
    let file_b = file_info("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &root, "b.txt");
    let sentinel = file_info("cccccccccccccccccccccccccccccccc", &root, "sentinel.txt");

    let mut storage = open_rw(&storage_path, &schema);

    // Empty batches are no-ops.
    storage.replace_files_batch(&[]).expect("empty replace");
    storage.delete_files_batch(&[]).expect("empty delete");

    // Batch-insert two files; single-insert the sentinel (both paths staged).
    storage
        .replace_files_batch(&[
            (file_a.clone(), vec![fragment(&file_a, "alpha")], None),
            (file_b.clone(), vec![fragment(&file_b, "beta")], None),
        ])
        .expect("batch replace");
    storage
        .replace_file(&sentinel, &[fragment(&sentinel, "sentinel text")], None)
        .expect("sentinel replace");
    storage.finalize_writes().expect("finalize");

    assert_eq!(entity_texts(&*storage, &sentinel), vec!["sentinel text"]);
    assert!(storage.get_file_by_path(&sentinel.absolute_path).is_some());

    // Batch-delete the other two files in one call.
    storage
        .delete_files_batch(&[file_a.id.clone(), file_b.id.clone()])
        .expect("batch delete");

    // The sentinel's record and document contents survive the batch delete.
    let remaining: Vec<String> = storage
        .list_file_refs()
        .iter()
        .map(|info| info.relative_path.clone())
        .collect();
    assert_eq!(remaining, vec!["sentinel.txt".to_owned()]);
    assert_eq!(entity_texts(&*storage, &sentinel), vec!["sentinel text"]);
    assert!(storage.get_file_by_path(&file_a.absolute_path).is_none());
    assert!(entity_texts(&*storage, &file_a).is_empty());

    // Snapshots persisted: the same holds after reopening.
    drop(storage);
    let storage = open_rw(&storage_path, &schema);
    let remaining: Vec<String> = storage
        .list_file_refs()
        .iter()
        .map(|info| info.relative_path.clone())
        .collect();
    assert_eq!(remaining, vec!["sentinel.txt".to_owned()]);
    assert_eq!(entity_texts(&*storage, &sentinel), vec!["sentinel text"]);
}
