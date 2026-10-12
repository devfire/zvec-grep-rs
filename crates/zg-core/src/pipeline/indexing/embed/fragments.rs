//! Fragment-level embedding execution: batching, retry, one-by-one fallback.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::models::embeddings::EmbeddingResult;
use crate::models::{EmbeddingInput, EmbeddingModel, ModelLoadSink};

use super::super::context::{PreparedFragment, error_to_message};
use super::super::retry::{
    EmbeddingScheduler, content_to_input, embed_inputs_with_retry, should_fail_fast_embedding_error,
};
use super::super::scanner::CancelFlag;

pub(super) fn embed_fragments(
    fragments: &[PreparedFragment],
    model: &dyn EmbeddingModel,
    workspace_roots: &[String],
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<&ModelLoadSink>,
) -> EngineResult<EmbeddingResult> {
    let max_batch = model.max_batch_size().max(1);
    let mut vectors: Vec<Vec<f32>> = vec![Vec::new(); fragments.len()];
    let mut truncated: Vec<usize> = Vec::new();
    let mut first_error: Option<EngineError> = None;
    let mut start = 0usize;
    while start < fragments.len() {
        let end = (start + max_batch).min(fragments.len());
        let Some(batch) = fragments.get(start..end) else {
            break;
        };
        match embed_fragment_batch(
            batch,
            model,
            workspace_roots,
            start,
            scheduler,
            abort,
            cancel,
            on_model_progress,
            Some(abort),
        ) {
            Ok(result) => {
                for (offset, vector) in result.vectors.into_iter().enumerate() {
                    if let Some(slot) = vectors.get_mut(start + offset) {
                        *slot = vector;
                    }
                }
                truncated.extend(result.truncated.into_iter().map(|index| start + index));
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if abort.load(Ordering::Relaxed) {
            break;
        }
        start = end;
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    // A short model return (or an abort-shortened run) must never commit
    // zero-dim padding: fail exactly like the batch path instead.
    let filled = vectors.iter().filter(|vector| !vector.is_empty()).count();
    if filled != fragments.len() {
        return Err(EngineError::new(
            EngineErrorCode::StorageEntityVectorCountMismatch,
            "embedding returned mismatched entity/vector counts",
        )
        .with_context(format!(
            "fragmentCount={} vectorCount={filled}",
            fragments.len()
        )));
    }
    Ok(EmbeddingResult { vectors, truncated })
}

#[allow(clippy::too_many_arguments)]
fn embed_fragment_batch(
    fragments: &[PreparedFragment],
    model: &dyn EmbeddingModel,
    workspace_roots: &[String],
    start_index: usize,
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<&ModelLoadSink>,
    on_terminal_failure: Option<&AtomicBool>,
) -> EngineResult<EmbeddingResult> {
    // Borrowed views over the already-prepared fragments: no per-item
    // `Content` clone, only reference-sized `EmbeddingInput` descriptors.
    let inputs: Vec<EmbeddingInput<'_>> = fragments
        .iter()
        .map(|fragment| content_to_input(&fragment.embedding_content))
        .collect();
    match embed_inputs_with_retry(
        &inputs,
        model,
        workspace_roots,
        scheduler,
        abort,
        cancel,
        on_model_progress.cloned(),
        on_terminal_failure,
    ) {
        Ok(result) => Ok(result),
        Err(error) => {
            if fragments.len() == 1 || should_fail_fast_embedding_error(&error, model) {
                return Err(error);
            }
            embed_fragment_batch_one_by_one(
                fragments,
                model,
                workspace_roots,
                start_index,
                scheduler,
                abort,
                cancel,
                on_model_progress,
                on_terminal_failure,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn embed_fragment_batch_one_by_one(
    fragments: &[PreparedFragment],
    model: &dyn EmbeddingModel,
    workspace_roots: &[String],
    start_index: usize,
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<&ModelLoadSink>,
    on_terminal_failure: Option<&AtomicBool>,
) -> EngineResult<EmbeddingResult> {
    let mut vectors = Vec::with_capacity(fragments.len());
    let mut truncated = Vec::new();
    for (index, fragment) in fragments.iter().enumerate() {
        let input = content_to_input(&fragment.embedding_content);
        match embed_inputs_with_retry(
            std::slice::from_ref(&input),
            model,
            workspace_roots,
            scheduler,
            abort,
            cancel,
            on_model_progress.cloned(),
            on_terminal_failure,
        ) {
            Ok(result) => {
                vectors.extend(result.vectors);
                if !result.truncated.is_empty() {
                    truncated.push(index);
                }
            }
            Err(error) => {
                if should_fail_fast_embedding_error(&error, model) {
                    return Err(error);
                }
                return Err(EngineError::new(
                    EngineErrorCode::IndexingEmbeddingFragmentFailed,
                    "embedding entity fragment failed after one-by-one fallback",
                )
                .with_context(format!(
                    "model={} fragmentId={} fragmentIndex={} cause={}",
                    model.info().reference,
                    fragment.fragment.entity.id.as_str(),
                    start_index + index,
                    error_to_message(&error)
                )));
            }
        }
    }
    Ok(EmbeddingResult { vectors, truncated })
}
