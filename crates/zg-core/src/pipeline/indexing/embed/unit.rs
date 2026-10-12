//! One embedding unit: batch embed with per-file fallback.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::models::{EmbeddingInput, EmbeddingModel, ModelLoadSink};

use super::super::context::{IndexProgressSink, IndexStats, PreparedFile, ProgressBase};
use super::super::prepare::describe_prepared_files;
use super::super::progress::{thread_progress_sink, thread_report};
use super::super::retry::{
    EmbeddingScheduler, content_to_input, embed_inputs_with_retry, should_fail_fast_embedding_error,
};
use super::super::scanner::CancelFlag;
use super::fragments::embed_fragments;
use super::{FileEmbed, FileOutcome, UnitOutcome};

/// Embeds one unit (batch or oversized single file) with the TS fallback
/// chain: batch → per-file → one-by-one (inside [`embed_fragments`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn embed_unit(
    unit: &[PreparedFile],
    model: &dyn EmbeddingModel,
    workspace_roots: &[String],
    scheduler: &Arc<EmbeddingScheduler>,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_progress: Option<&IndexProgressSink>,
    stats: &Arc<Mutex<IndexStats>>,
    progress_base: Option<ProgressBase>,
    total: usize,
) -> UnitOutcome {
    if let [prepared] = unit {
        thread_report(
            on_progress,
            scheduler,
            &format!("embedding {}", prepared.file.relative_path),
        );
        return match embed_fragments(
            &prepared.fragments,
            model,
            workspace_roots,
            scheduler,
            abort,
            cancel,
            thread_progress_sink(on_progress, scheduler, stats, progress_base, total).as_ref(),
        ) {
            Ok(result) => UnitOutcome::Embedded(vec![FileOutcome::Embedded(FileEmbed {
                vectors: result.vectors,
                truncated_fragment_count: result.truncated.len(),
            })]),
            Err(error) => {
                if should_fail_fast_embedding_error(&error, model) {
                    abort.store(true, Ordering::Relaxed);
                    UnitOutcome::Failed(error)
                } else {
                    UnitOutcome::Embedded(vec![FileOutcome::Failed(error)])
                }
            }
        };
    }
    thread_report(
        on_progress,
        scheduler,
        &format!("embedding {}", describe_prepared_files(unit)),
    );
    match embed_unit_contents(
        unit,
        model,
        workspace_roots,
        scheduler,
        abort,
        cancel,
        thread_progress_sink(on_progress, scheduler, stats, progress_base, total),
    ) {
        Ok(results) => UnitOutcome::Embedded(results),
        Err(error) => {
            if should_fail_fast_embedding_error(&error, model) {
                abort.store(true, Ordering::Relaxed);
                return UnitOutcome::Failed(error);
            }
            // Per-file fallback mirroring the TS batch catch block.
            let mut results = Vec::with_capacity(unit.len());
            for prepared in unit {
                if abort.load(Ordering::Relaxed) {
                    return UnitOutcome::Failed(error);
                }
                thread_report(
                    on_progress,
                    scheduler,
                    &format!("embedding {}", prepared.file.relative_path),
                );
                match embed_fragments(
                    &prepared.fragments,
                    model,
                    workspace_roots,
                    scheduler,
                    abort,
                    cancel,
                    thread_progress_sink(on_progress, scheduler, stats, progress_base, total)
                        .as_ref(),
                ) {
                    Ok(result) => results.push(FileOutcome::Embedded(FileEmbed {
                        vectors: result.vectors,
                        truncated_fragment_count: result.truncated.len(),
                    })),
                    Err(error) => {
                        if should_fail_fast_embedding_error(&error, model) {
                            abort.store(true, Ordering::Relaxed);
                            return UnitOutcome::Failed(error);
                        }
                        results.push(FileOutcome::Failed(error));
                    }
                }
            }
            UnitOutcome::Embedded(results)
        }
    }
}

fn embed_unit_contents(
    unit: &[PreparedFile],
    model: &dyn EmbeddingModel,
    workspace_roots: &[String],
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<ModelLoadSink>,
) -> EngineResult<Vec<FileOutcome>> {
    // Borrowed input views over the prepared fragments: only the small
    // `EmbeddingInput` descriptors (references) are collected — fragment text
    // and image bytes are never cloned, so peak memory stays at one copy.
    let inputs: Vec<EmbeddingInput<'_>> = unit
        .iter()
        .flat_map(|file| {
            file.fragments
                .iter()
                .map(|fragment| content_to_input(&fragment.embedding_content))
        })
        .collect();
    let result = embed_inputs_with_retry(
        &inputs,
        model,
        workspace_roots,
        scheduler,
        abort,
        cancel,
        on_model_progress,
        Some(abort),
    )?;
    let total_fragments: usize = unit.iter().map(|file| file.fragments.len()).sum();
    if result.vectors.len() != total_fragments {
        return Err(EngineError::new(
            EngineErrorCode::StorageEntityVectorCountMismatch,
            "embedding returned mismatched entity/vector counts",
        )
        .with_context(format!(
            "unitFiles={} fragmentCount={} vectorCount={}",
            unit.len(),
            total_fragments,
            result.vectors.len()
        )));
    }
    // Global input-order truncation indices; per-file attribution below.
    let truncated: HashSet<usize> = result.truncated.into_iter().collect();
    // Consume the vectors in input order without re-cloning per-file windows.
    let mut vectors = result.vectors.into_iter();
    let mut offset = 0usize;
    let mut out = Vec::with_capacity(unit.len());
    for file in unit {
        let end = offset + file.fragments.len();
        let file_vectors: Vec<Vec<f32>> = vectors.by_ref().take(file.fragments.len()).collect();
        let truncated_fragment_count = (offset..end)
            .filter(|index| truncated.contains(index))
            .count();
        offset = end;
        out.push(FileOutcome::Embedded(FileEmbed {
            vectors: file_vectors,
            truncated_fragment_count,
        }));
    }
    Ok(out)
}
