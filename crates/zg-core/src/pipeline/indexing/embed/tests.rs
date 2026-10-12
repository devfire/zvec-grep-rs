//! Regression tests for short embedding returns and embed-failure marking.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::ids::{EntityId, FileId, make_entity_id};
use crate::models::embeddings::EmbeddingResult;
use crate::models::{
    EmbeddingInput, EmbeddingInputKind, EmbeddingModel, EmbeddingModelInfo, EmbeddingPurpose,
};
use crate::storage::{
    IndexedFragment, ListEntitiesOptions, StorageOptions, create_workspace_index_storage,
};
use crate::types::{
    CURRENT_INDEX_VERSION, Content, Entity, EntityFragment, FileFormat, FileInfo, FileKind, Range,
    RootPath, SearchMetric, UnixMillis, WorkspaceIndexEmbeddingSchema, WorkspaceIndexInfo,
};
use crate::utils::timing::TimingCollector;

use super::super::context::{IndexContext, IndexStats, PreparedFile, PreparedFragment};
use super::super::retry::{
    ConcurrencyPolicy, EmbeddingScheduler, resolve_embedding_concurrency_policy,
};
use super::fragments::embed_fragments;
use super::wave::embed_and_commit_wave;

/// Model returning one vector fewer than requested.
struct ShortModel;
impl EmbeddingModel for ShortModel {
    fn info(&self) -> &EmbeddingModelInfo {
        static INFO: std::sync::LazyLock<EmbeddingModelInfo> =
            std::sync::LazyLock::new(|| EmbeddingModelInfo {
                reference: "test/short".to_owned(),
                provider: "test".to_owned(),
                model: "short".to_owned(),
                dimension: 4,
                metric: SearchMetric::Cosine,
                supports_images: false,
                max_input_tokens: None,
                input_kinds: vec![EmbeddingInputKind::Text],
                endpoint: None,
                default_concurrency: None,
            });
        &INFO
    }
    fn max_batch_size(&self) -> usize {
        8
    }
    fn embed(
        &self,
        _purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        Ok(EmbeddingResult {
            vectors: vec![vec![0.0; 4]; inputs.len().saturating_sub(1)],
            truncated: Vec::new(),
        })
    }
}

fn fragment(index: usize) -> PreparedFragment {
    let file_id = FileId::from_raw("file".to_owned());
    PreparedFragment {
        fragment: EntityFragment {
            entity: Entity {
                id: EntityId::from_raw(format!("entity-{index}")),
                file_id,
                range: Range::File,
                content: Content::Text {
                    text: format!("fragment {index}"),
                },
                metadata: None,
            },
            group: None,
        },
        embedding_content: Content::Text {
            text: format!("fragment {index}"),
        },
    }
}

#[test]
fn short_embed_return_is_error_never_padding() {
    let model = ShortModel;
    let scheduler = EmbeddingScheduler::new(ConcurrencyPolicy {
        initial: 1,
        min: 1,
        max: 1,
        adaptive: false,
    });
    let abort = AtomicBool::new(false);
    let fragments = vec![fragment(0), fragment(1), fragment(2)];
    let error = embed_fragments(&fragments, &model, &[], &scheduler, &abort, None, None)
        .expect_err("short model return must be Err");
    assert_eq!(
        *error.code(),
        EngineErrorCode::StorageEntityVectorCountMismatch
    );
}

const FAIL_MARKER: &str = "FAIL_MARKER";

/// Local test model: embeds normally unless an input carries the fail
/// marker, in which case it returns a non-fail-fast error (local
/// provider, no retryable markers) so the wave takes the per-file
/// fallback and the marked file lands in `WaveEntry::EmbedFailed`.
struct FailMarkerModel;
impl EmbeddingModel for FailMarkerModel {
    fn info(&self) -> &EmbeddingModelInfo {
        static INFO: std::sync::LazyLock<EmbeddingModelInfo> =
            std::sync::LazyLock::new(|| EmbeddingModelInfo {
                reference: "test/fail-marker".to_owned(),
                provider: "local".to_owned(),
                model: "fail-marker".to_owned(),
                dimension: 4,
                metric: SearchMetric::Cosine,
                supports_images: false,
                max_input_tokens: None,
                input_kinds: vec![EmbeddingInputKind::Text],
                endpoint: None,
                default_concurrency: None,
            });
        &INFO
    }
    fn max_batch_size(&self) -> usize {
        64
    }
    fn embed(
        &self,
        _purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        let poisoned = inputs.iter().any(|input| match input {
            EmbeddingInput::Text { text } => text.contains(FAIL_MARKER),
            EmbeddingInput::Image { .. } => false,
        });
        if poisoned {
            return Err(EngineError::new(
                EngineErrorCode::ModelsTransformersJsEmbedFailed,
                "embed failed",
            )
            .with_context("test fail marker"));
        }
        Ok(EmbeddingResult {
            vectors: vec![vec![0.0; 4]; inputs.len()],
            truncated: Vec::new(),
        })
    }
}

fn test_file_info(root: &std::path::Path, id: &str, name: &str) -> FileInfo {
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

fn prepared_fragment(file_id: &FileId, index: usize, text: &str) -> PreparedFragment {
    PreparedFragment {
        fragment: EntityFragment {
            entity: Entity {
                id: make_entity_id(file_id, index),
                file_id: file_id.clone(),
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
        embedding_content: Content::Text {
            text: text.to_owned(),
        },
    }
}

fn indexed_fragment(file: &FileInfo, text: &str) -> IndexedFragment {
    let prepared = prepared_fragment(&file.id, 0, text);
    IndexedFragment {
        fragment: prepared.fragment,
        vector: vec![0.25, 0.5, 0.75, 1.0],
    }
}

fn test_schema() -> WorkspaceIndexEmbeddingSchema {
    WorkspaceIndexEmbeddingSchema {
        provider: "local".to_owned(),
        model: "fail-marker".to_owned(),
        dimension: 4,
        metric: SearchMetric::Cosine,
    }
}

/// Regression test for #55: a modified file whose embedding fails must
/// not keep stale fragments searchable. The wave must persist the
/// failure mark (deleting old documents, storing the reason) and report
/// `files_failed = 1` — not just bump the in-memory counter.
// Seed-run-wave-assert is one linear scenario; splitting it would scatter
// the arrange/act/assert without reducing complexity.
#[allow(clippy::too_many_lines)]
#[test]
fn embed_failure_marks_file_failed_and_drops_stale_fragments() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let storage_path = dir.path().join("storage");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("repo root");
    let schema = test_schema();
    let mut storage = create_workspace_index_storage(StorageOptions::ReadWrite {
        storage_path: &storage_path,
        embedding: &schema,
    })
    .expect("open read-write storage");

    // Seed file B with stale indexed content, as a previous successful run
    // would have left it; file A is new.
    let file_a = test_file_info(&root, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "a.txt");
    let file_b = test_file_info(&root, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "b.txt");
    storage
        .replace_file(&file_b, &[indexed_fragment(&file_b, "stale content")], None)
        .expect("seed stale fragments");
    storage.finalize_writes().expect("finalize seed");
    assert_eq!(
        storage
            .list_entities_by_file(&file_b.id, ListEntitiesOptions::default())
            .len(),
        1,
        "seed precondition: stale fragment present"
    );

    // One unit holding both files: the batch embed fails on B's marker,
    // the per-file fallback embeds A and reports B failed.
    let prepared_a = PreparedFile {
        file: file_a.clone(),
        fragments: vec![prepared_fragment(&file_a.id, 0, "hello world")],
    };
    let prepared_b = PreparedFile {
        file: FileInfo {
            content_hash: Some("hash-new-b".to_owned()),
            ..file_b.clone()
        },
        fragments: vec![prepared_fragment(
            &file_b.id,
            0,
            "FAIL_MARKER modified content",
        )],
    };

    let model: Arc<dyn EmbeddingModel> = Arc::new(FailMarkerModel);
    let scheduler = Arc::new(EmbeddingScheduler::new(
        resolve_embedding_concurrency_policy(None, &*model),
    ));
    let stats = Arc::new(Mutex::new(IndexStats::default()));
    let mut timings = TimingCollector::new();
    let mut embed_elapsed_ms = 0.0;
    let info = WorkspaceIndexInfo {
        id: "test".to_owned(),
        name: "test".to_owned(),
        path: storage_path.to_string_lossy().into_owned(),
        root_paths: vec![RootPath {
            absolute_path: root.to_string_lossy().into_owned(),
            recursive: true,
            ..RootPath::default()
        }],
        index_policy: None,
        embedding: Some(Some(schema)),
        index_version: Some(CURRENT_INDEX_VERSION),
        created_time: UnixMillis::now(),
        updated_time: UnixMillis::now(),
    };
    let mut ctx = IndexContext {
        workspace_index: info,
        storage: storage.as_mut(),
        embedding_model: model,
        embedding_concurrency: None,
        on_progress: None,
        cancel: None,
    };
    embed_and_commit_wave(
        vec![vec![prepared_a, prepared_b]],
        &mut ctx,
        &stats,
        &scheduler,
        &AtomicBool::new(false),
        &mut timings,
        None,
        2,
        &mut embed_elapsed_ms,
    )
    .expect("wave commits around one failed file");

    let guard = stats.lock().expect("stats lock");
    assert_eq!(guard.files_failed, 1);
    assert_eq!(guard.failed_files, vec!["b.txt".to_owned()]);
    assert_eq!(guard.failed_file_reasons.len(), 1);
    let reason = guard
        .failed_file_reasons
        .first()
        .expect("one failure reason");
    assert!(
        reason.starts_with("b.txt: embed:"),
        "unexpected reason: {reason}"
    );
    drop(guard);

    // Stale fragments are gone and the failure is persisted on metadata.
    assert!(
        ctx.storage
            .list_entities_by_file(&file_b.id, ListEntitiesOptions::default())
            .is_empty(),
        "stale fragments must be deleted on embed failure"
    );
    let failed = ctx
        .storage
        .get_file_by_path(&file_b.absolute_path)
        .expect("failed file metadata");
    let error = failed
        .index_status
        .as_ref()
        .and_then(|status| status.error.as_ref())
        .expect("stored failure reason");
    assert!(error.starts_with("embed:"), "unexpected error: {error}");
    // The healthy file in the same wave still commits.
    assert_eq!(
        ctx.storage
            .list_entities_by_file(&file_a.id, ListEntitiesOptions::default())
            .len(),
        1
    );
}
