//! Parallel embed of one bounded wave, then serial in-order commit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::storage::IndexedFragment;
use crate::types::FileInfo;
use crate::utils::timing::TimingCollector;

use super::super::context::{
    IndexContext, IndexStats, PreparedFile, ProgressBase, throw_if_index_cancelled,
};
use super::super::prepare::{
    commit_files_batch, finished_file_detail, mark_file_failed, record_file_failed,
};
use super::super::progress::{lock_stats, lock_stats_mut, report_indexing};
use super::super::retry::{EmbeddingScheduler, should_fail_fast_embedding_error};
use super::unit::embed_unit;
use super::{FileOutcome, UnitOutcome};

/// One wave file's commit plan: successes batch into a single
/// `replace_files_batch` (two metadata snapshots per wave, not per file:
/// #56); failures stage marks replayed in input order with one flush.
enum WaveEntry {
    Commit {
        prepared: PreparedFile,
        vectors: Vec<Vec<f32>>,
        truncated_fragment_count: usize,
    },
    CommitFailed {
        prepared: PreparedFile,
        error: EngineError,
    },
    UnitFailed {
        prepared: PreparedFile,
        error: EngineError,
    },
    EmbedFailed {
        prepared: PreparedFile,
        error: EngineError,
    },
}

/// Embeds one bounded wave of complete units in parallel, then commits the
/// owned results serially in input order. Units are borrowed by workers and
/// moved into the commit phase after joining, so no `unit.clone()` ever
/// materializes; vectors and fragments move into storage payloads without
/// re-cloning. Embedding wall-clock accumulates into `embed_elapsed_ms`; the
/// caller records `index_embedding` once (count 1).
// `embed_and_commit_wave` keeps parallel embed plus serial in-order commit in
// one wave so unit ordering and fail-fast invariants stay local; splitting it
// would scatter the commit ordering without reducing complexity.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub(super) fn embed_and_commit_wave(
    wave: Vec<Vec<PreparedFile>>,
    ctx: &mut IndexContext<'_>,
    stats: &Arc<Mutex<IndexStats>>,
    scheduler: &Arc<EmbeddingScheduler>,
    abort: &AtomicBool,
    timings: &mut TimingCollector,
    progress_base: Option<ProgressBase>,
    total: usize,
    embed_elapsed_ms: &mut f64,
) -> EngineResult<()> {
    throw_if_index_cancelled(ctx)?;
    let started = Instant::now();
    // Operation's canonical root set for authorization: derived from the
    // indexed workspace, never the process working directory. Cloned per
    // worker below; an empty set fails closed in authorizing backends.
    let workspace_roots: Vec<String> = ctx
        .workspace_index
        .root_paths
        .iter()
        .map(|root| root.absolute_path.clone())
        .collect();
    let outcomes: Vec<UnitOutcome> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(wave.len());
        for unit in &wave {
            let scheduler = Arc::clone(scheduler);
            let model = Arc::clone(&ctx.embedding_model);
            let abort_ref = abort;
            let on_progress = ctx.on_progress.clone();
            let cancel = ctx.cancel.clone();
            let stats = Arc::clone(stats);
            let workspace_roots = workspace_roots.clone();
            handles.push(scope.spawn(move || {
                embed_unit(
                    unit,
                    &*model,
                    &workspace_roots,
                    &scheduler,
                    abort_ref,
                    cancel.as_ref(),
                    on_progress.as_ref(),
                    &stats,
                    progress_base,
                    total,
                )
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                if let Ok(outcome) = handle.join() {
                    outcome
                } else {
                    abort.store(true, Ordering::Relaxed);
                    UnitOutcome::Failed(EngineError::new(
                        EngineErrorCode::IndexingEmbeddingThreadFailed,
                        "embedding worker thread failed",
                    ))
                }
            })
            .collect()
    });
    *embed_elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
    if let Some(error) = outcomes.iter().find_map(|outcome| match outcome {
        UnitOutcome::Failed(error)
            if should_fail_fast_embedding_error(error, &*ctx.embedding_model) =>
        {
            Some(error.clone())
        }
        UnitOutcome::Failed(_) | UnitOutcome::Embedded(_) => None,
    }) {
        return Err(error);
    }
    // Phase 1: order-preserving plan with no storage, stats, or progress.
    // Phase 2 commits every success with one `replace_files_batch`, then
    // phase 3 replays marks, stats, and progress in input order.
    let mut plan: Vec<WaveEntry> = Vec::new();
    for (unit, outcome) in wave.into_iter().zip(outcomes) {
        throw_if_index_cancelled(ctx)?;
        match outcome {
            UnitOutcome::Embedded(results) => {
                for (prepared, result) in unit.into_iter().zip(results) {
                    match result {
                        FileOutcome::Embedded(embed) => {
                            if embed.vectors.len() == prepared.fragments.len() {
                                plan.push(WaveEntry::Commit {
                                    truncated_fragment_count: embed.truncated_fragment_count,
                                    vectors: embed.vectors,
                                    prepared,
                                });
                            } else {
                                let error = EngineError::new(
                                    EngineErrorCode::StorageEntityVectorCountMismatch,
                                    "embedding returned mismatched entity/vector counts",
                                )
                                .with_context(format!(
                                    "fileId={} fragmentCount={} vectorCount={}",
                                    prepared.file.id.as_str(),
                                    prepared.fragments.len(),
                                    embed.vectors.len()
                                ));
                                plan.push(WaveEntry::CommitFailed { prepared, error });
                            }
                        }
                        FileOutcome::Failed(error) => {
                            plan.push(WaveEntry::EmbedFailed { prepared, error });
                        }
                    }
                }
            }
            UnitOutcome::Failed(error) => {
                for prepared in unit {
                    plan.push(WaveEntry::UnitFailed {
                        prepared,
                        error: error.clone(),
                    });
                }
            }
        }
    }
    // Phase 2: move fragments and vectors into one batch payload (no clones:
    // each `IndexedFragment` allocation is sized to its file's fragment count).
    let mut batch: Vec<(FileInfo, Vec<IndexedFragment>, usize)> = Vec::new();
    let mut commit_count = 0usize;
    for entry in &mut plan {
        if let WaveEntry::Commit {
            prepared,
            vectors,
            truncated_fragment_count,
        } = entry
        {
            let fragments = std::mem::take(&mut prepared.fragments);
            let vectors_taken = std::mem::take(vectors);
            let truncated = *truncated_fragment_count;
            let file_vectors: Vec<IndexedFragment> = fragments
                .into_iter()
                .zip(vectors_taken)
                .map(|(fragment, vector)| IndexedFragment {
                    fragment: fragment.fragment,
                    vector,
                })
                .collect();
            batch.push((prepared.file.clone(), file_vectors, truncated));
            commit_count += 1;
        }
    }
    let committed: Vec<bool> =
        timings.time("index_commit", || commit_files_batch(ctx, batch, stats))?;
    debug_assert_eq!(committed.len(), commit_count);
    // Results align with the `Commit` entries in plan order, so the replay
    // below consumes them positionally: no index table, no panicking access.
    let mut results = committed.into_iter();
    // Phase 3: failure marks (staged, no per-file flush), stats, and in-order
    // progress. Batch successes already updated stats inside the batch commit.
    let mut staged_failure = false;
    for entry in plan {
        throw_if_index_cancelled(ctx)?;
        match entry {
            WaveEntry::Commit { prepared, .. } => {
                // `commit_files_batch` returns one flag per `Commit` entry in
                // order; `false` here only fires if the batch shape ever
                // drifts, and fail-closed reporting is the safe direction.
                let succeeded = results.next().unwrap_or(false);
                report_indexing(
                    ctx,
                    &lock_stats(stats),
                    Some(finished_file_detail(
                        succeeded,
                        &prepared.file.relative_path,
                    )),
                    progress_base,
                    total,
                    Some(scheduler.snapshot()),
                );
            }
            WaveEntry::CommitFailed { prepared, error } => {
                let reason = mark_file_failed(ctx.storage, &prepared.file, &error, "commit");
                record_file_failed(&mut lock_stats_mut(stats), &prepared.file, Some(reason));
                staged_failure = true;
                report_indexing(
                    ctx,
                    &lock_stats(stats),
                    Some(finished_file_detail(false, &prepared.file.relative_path)),
                    progress_base,
                    total,
                    Some(scheduler.snapshot()),
                );
            }
            WaveEntry::UnitFailed { prepared, error }
            | WaveEntry::EmbedFailed { prepared, error } => {
                let reason = mark_file_failed(ctx.storage, &prepared.file, &error, "embed");
                record_file_failed(&mut lock_stats_mut(stats), &prepared.file, Some(reason));
                staged_failure = true;
                report_indexing(
                    ctx,
                    &lock_stats(stats),
                    Some(finished_file_detail(false, &prepared.file.relative_path)),
                    progress_base,
                    total,
                    Some(scheduler.snapshot()),
                );
            }
        }
    }
    debug_assert!(results.next().is_none());
    if staged_failure {
        ctx.storage.flush()?;
    }
    Ok(())
}
