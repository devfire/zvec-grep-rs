//! Embedding pipeline: batching, scoped-thread waves, serial commit, fallback chain.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::models::embeddings::EmbeddingResult;
use crate::models::{EmbeddingModel, EmbeddingModelProgress, ModelLoadSink};
use crate::storage::IndexedFragment;
use crate::types::{Content, FileInfo};
use crate::utils::timing::TimingCollector;

use super::context::{
    IndexContext, IndexProgressSink, IndexStats, PreparedFile, PreparedFragment, ProgressBase,
    error_to_message, file_failure_reason, throw_if_index_cancelled,
};
use super::prepare::{
    commit_file, commit_vectors, describe_prepared_files, finished_file_detail, mark_file_failed,
    prepare_file, record_file_failed,
};
use super::progress::{
    lock_stats, lock_stats_mut, report_download_progress, report_indexing, thread_progress_sink,
    thread_report,
};
use super::retry::{
    EmbeddingScheduler, embed_contents_with_retry, resolve_embedding_concurrency_policy,
    should_fail_fast_embedding_error,
};
use super::scanner::CancelFlag;

/// Embedded vectors for one file, or the stage-tagged failure reason.
struct FileEmbed {
    vectors: Vec<Vec<f32>>,
    truncated_fragment_count: usize,
}

enum FileOutcome {
    Embedded(FileEmbed),
    Failed(String),
}

/// Outcome of embedding one unit (batch or oversized single file).
enum UnitOutcome {
    Embedded(Vec<FileOutcome>),
    Failed(EngineError),
}

pub(crate) fn index_files(
    files: Vec<FileInfo>,
    ctx: &mut IndexContext<'_>,
    timings: &mut TimingCollector,
    progress_base: Option<ProgressBase>,
) -> EngineResult<IndexStats> {
    let stats = Arc::new(Mutex::new(IndexStats::default()));
    let scheduler = Arc::new(EmbeddingScheduler::new(
        resolve_embedding_concurrency_policy(
            ctx.embedding_concurrency,
            ctx.embedding_model.as_ref(),
        ),
    ));
    let max_batch = ctx.embedding_model.max_batch_size().max(1);

    if ctx.embedding_model.info().provider == "local" {
        throw_if_index_cancelled(ctx)?;
        let sink = ctx.on_progress.clone().map(|callback| {
            let scheduler = Arc::clone(&scheduler);
            let stats = Arc::clone(&stats);
            Arc::new(move |progress: EmbeddingModelProgress| {
                let snapshot = lock_stats(&stats);
                report_download_progress(&callback, &scheduler, &snapshot, &progress);
            }) as ModelLoadSink
        });
        let started = Instant::now();
        ctx.embedding_model.prepare(sink)?;
        timings.add(
            "index_embedding_prepare",
            started.elapsed().as_secs_f64() * 1000.0,
            1,
        );
        throw_if_index_cancelled(ctx)?;
    }

    let mut units: Vec<Vec<PreparedFile>> = Vec::new();
    let mut batch: Vec<PreparedFile> = Vec::new();
    let mut batch_fragments = 0usize;
    for file in &files {
        throw_if_index_cancelled(ctx)?;
        report_indexing(
            ctx,
            &lock_stats(&stats),
            Some(format!("reading {}", file.relative_path)),
            progress_base,
            files.len(),
            Some(scheduler.snapshot()),
        );
        let prepared = timings.time("index_prepare", || prepare_file(file, ctx))?;
        match prepared {
            Err(reason) => {
                record_file_failed(&mut lock_stats_mut(&stats), file, Some(reason));
                report_indexing(
                    ctx,
                    &lock_stats(&stats),
                    Some(format!("failed {}", file.relative_path)),
                    progress_base,
                    files.len(),
                    Some(scheduler.snapshot()),
                );
            }
            Ok(prepared) => {
                if prepared.fragments.is_empty() {
                    let committed = timings.time("index_commit", || {
                        commit_file(ctx, &prepared, &[], 0, &stats)
                    })?;
                    report_indexing(
                        ctx,
                        &lock_stats(&stats),
                        Some(finished_file_detail(committed, &file.relative_path)),
                        progress_base,
                        files.len(),
                        Some(scheduler.snapshot()),
                    );
                    continue;
                }
                if prepared.fragments.len() > max_batch {
                    flush_batch(&mut batch, &mut batch_fragments, &mut units);
                    units.push(vec![prepared]);
                    continue;
                }
                if batch_fragments > 0 && batch_fragments + prepared.fragments.len() > max_batch {
                    flush_batch(&mut batch, &mut batch_fragments, &mut units);
                }
                batch_fragments += prepared.fragments.len();
                batch.push(prepared);
                if batch_fragments == max_batch {
                    flush_batch(&mut batch, &mut batch_fragments, &mut units);
                }
            }
        }
    }
    flush_batch(&mut batch, &mut batch_fragments, &mut units);

    // Parallel embedding in bounded waves; serial commit in file order.
    let abort = Arc::new(AtomicBool::new(false));
    let embed_started = Instant::now();
    let wave_size = scheduler.policy().max.max(1);
    let mut outcomes: Vec<(Vec<PreparedFile>, UnitOutcome)> = Vec::with_capacity(units.len());
    for wave in units.chunks(wave_size) {
        if abort.load(Ordering::Relaxed) {
            break;
        }
        throw_if_index_cancelled(ctx)?;
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(wave.len());
            for unit in wave {
                let scheduler = Arc::clone(&scheduler);
                let model = Arc::clone(&ctx.embedding_model);
                let abort = Arc::clone(&abort);
                let on_progress = ctx.on_progress.clone();
                let cancel = ctx.cancel.clone();
                let stats = Arc::clone(&stats);
                let total = files.len();
                handles.push(scope.spawn(move || {
                    embed_unit(
                        unit,
                        &*model,
                        &scheduler,
                        &abort,
                        cancel.as_ref(),
                        on_progress.as_ref(),
                        &stats,
                        progress_base,
                        total,
                    )
                }));
            }
            for (unit, handle) in wave.iter().zip(handles) {
                match handle.join() {
                    Ok(outcome) => outcomes.push((unit.clone(), outcome)),
                    Err(_) => {
                        abort.store(true, Ordering::Relaxed);
                        outcomes.push((
                            unit.clone(),
                            UnitOutcome::Failed(EngineError::new(
                                EngineErrorCode::from_static("INDEXING.EMBEDDING_THREAD_FAILED"),
                                "embedding worker thread failed",
                            )),
                        ));
                    }
                }
            }
        });
        if let Some(error) = outcomes.iter().find_map(|(_, outcome)| match outcome {
            UnitOutcome::Failed(error)
                if should_fail_fast_embedding_error(error, &*ctx.embedding_model) =>
            {
                Some(error.clone())
            }
            UnitOutcome::Failed(_) | UnitOutcome::Embedded(_) => None,
        }) {
            return Err(error);
        }
    }
    timings.add(
        "index_embedding",
        embed_started.elapsed().as_secs_f64() * 1000.0,
        1,
    );
    throw_if_index_cancelled(ctx)?;

    for (unit, outcome) in &outcomes {
        throw_if_index_cancelled(ctx)?;
        match outcome {
            UnitOutcome::Embedded(results) => {
                for (prepared, result) in unit.iter().zip(results.iter()) {
                    match result {
                        FileOutcome::Embedded(embed) => {
                            let file_vectors: Vec<IndexedFragment> = prepared
                                .fragments
                                .iter()
                                .zip(embed.vectors.iter())
                                .map(|(fragment, vector)| IndexedFragment {
                                    fragment: fragment.fragment.clone(),
                                    vector: vector.clone(),
                                })
                                .collect();
                            let committed = timings.time("index_commit", || {
                                commit_vectors(
                                    ctx,
                                    prepared,
                                    &file_vectors,
                                    embed.truncated_fragment_count,
                                    &stats,
                                )
                            })?;
                            report_indexing(
                                ctx,
                                &lock_stats(&stats),
                                Some(finished_file_detail(
                                    committed,
                                    &prepared.file.relative_path,
                                )),
                                progress_base,
                                files.len(),
                                Some(scheduler.snapshot()),
                            );
                        }
                        FileOutcome::Failed(reason) => {
                            record_file_failed(
                                &mut lock_stats_mut(&stats),
                                &prepared.file,
                                Some(reason.clone()),
                            );
                            report_indexing(
                                ctx,
                                &lock_stats(&stats),
                                Some(finished_file_detail(false, &prepared.file.relative_path)),
                                progress_base,
                                files.len(),
                                Some(scheduler.snapshot()),
                            );
                        }
                    }
                }
            }
            UnitOutcome::Failed(error) => {
                for prepared in unit {
                    let reason = mark_file_failed(ctx.storage, &prepared.file, error, "embed");
                    record_file_failed(&mut lock_stats_mut(&stats), &prepared.file, Some(reason));
                    report_indexing(
                        ctx,
                        &lock_stats(&stats),
                        Some(finished_file_detail(false, &prepared.file.relative_path)),
                        progress_base,
                        files.len(),
                        Some(scheduler.snapshot()),
                    );
                }
            }
        }
    }
    throw_if_index_cancelled(ctx)?;
    Ok(lock_stats(&stats))
}

fn flush_batch(
    batch: &mut Vec<PreparedFile>,
    count: &mut usize,
    units: &mut Vec<Vec<PreparedFile>>,
) {
    if batch.is_empty() {
        return;
    }
    units.push(std::mem::take(batch));
    *count = 0;
}

/// Embeds one unit (batch or oversized single file) with the TS fallback
/// chain: batch → per-file → one-by-one (inside [`embed_fragments`]).
#[allow(clippy::too_many_arguments)]
fn embed_unit(
    unit: &[PreparedFile],
    model: &dyn EmbeddingModel,
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
            scheduler,
            abort,
            cancel,
            thread_progress_sink(on_progress, scheduler, stats, progress_base, total),
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
                    UnitOutcome::Embedded(vec![FileOutcome::Failed(file_failure_reason(
                        "embed", &error,
                    ))])
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
                    scheduler,
                    abort,
                    cancel,
                    thread_progress_sink(on_progress, scheduler, stats, progress_base, total),
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
                        results.push(FileOutcome::Failed(file_failure_reason("embed", &error)));
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
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<ModelLoadSink>,
) -> EngineResult<Vec<FileOutcome>> {
    let contents: Vec<Content> = unit
        .iter()
        .flat_map(|file| {
            file.fragments
                .iter()
                .map(|fragment| fragment.embedding_content.clone())
        })
        .collect();
    let result = embed_contents_with_retry(
        &contents,
        model,
        scheduler,
        abort,
        cancel,
        on_model_progress,
        Some(abort),
    )?;
    let truncated: HashSet<usize> = result.truncated.into_iter().collect();
    let mut offset = 0usize;
    let mut out = Vec::with_capacity(unit.len());
    for file in unit {
        let end = offset + file.fragments.len();
        let vectors = result
            .vectors
            .get(offset..end)
            .map(|window| window.to_vec())
            .unwrap_or_default();
        let truncated_fragment_count = (offset..end)
            .filter(|index| truncated.contains(index))
            .count();
        offset = end;
        out.push(FileOutcome::Embedded(FileEmbed {
            vectors,
            truncated_fragment_count,
        }));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Embedding execution: batching, retry, adaptive scheduling.
// ---------------------------------------------------------------------------

fn embed_fragments(
    fragments: &[PreparedFragment],
    model: &dyn EmbeddingModel,
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<ModelLoadSink>,
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
            start,
            scheduler,
            abort,
            cancel,
            on_model_progress.clone(),
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
    Ok(EmbeddingResult { vectors, truncated })
}

#[allow(clippy::too_many_arguments)]
fn embed_fragment_batch(
    fragments: &[PreparedFragment],
    model: &dyn EmbeddingModel,
    start_index: usize,
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<ModelLoadSink>,
    on_terminal_failure: Option<&AtomicBool>,
) -> EngineResult<EmbeddingResult> {
    let contents: Vec<Content> = fragments
        .iter()
        .map(|fragment| fragment.embedding_content.clone())
        .collect();
    match embed_contents_with_retry(
        &contents,
        model,
        scheduler,
        abort,
        cancel,
        on_model_progress.clone(),
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
    start_index: usize,
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<ModelLoadSink>,
    on_terminal_failure: Option<&AtomicBool>,
) -> EngineResult<EmbeddingResult> {
    let mut vectors = Vec::with_capacity(fragments.len());
    let mut truncated = Vec::new();
    for (index, fragment) in fragments.iter().enumerate() {
        match embed_contents_with_retry(
            std::slice::from_ref(&fragment.embedding_content),
            model,
            scheduler,
            abort,
            cancel,
            on_model_progress.clone(),
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
                    EngineErrorCode::from_static("INDEXING.EMBEDDING_FRAGMENT_FAILED"),
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
