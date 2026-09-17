//! Embedding pipeline: batching, scoped-thread waves, serial commit, fallback chain.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::models::embeddings::EmbeddingResult;
use crate::models::{EmbeddingInput, EmbeddingModel, EmbeddingModelProgress, ModelLoadSink};
use crate::storage::IndexedFragment;
use crate::types::FileInfo;
use crate::utils::timing::TimingCollector;

use super::context::{
    IndexContext, IndexProgressSink, IndexStats, PreparedFile, PreparedFragment, ProgressBase,
    error_to_message, file_failure_reason, is_cancelled_error, throw_if_index_cancelled,
};
use super::prepare::{
    PurePrepareOutcome, commit_file, commit_vectors, describe_prepared_files, finished_file_detail,
    mark_file_failed, read_and_prepare_pure, record_file_failed,
};
use super::progress::{
    lock_stats, lock_stats_mut, report_download_progress, report_indexing, thread_progress_sink,
    thread_report,
};
use super::retry::{
    EmbeddingScheduler, content_to_input, embed_inputs_with_retry,
    resolve_embedding_concurrency_policy, should_fail_fast_embedding_error,
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

    // Bounded streaming preparation: parallel pure phase (`fs::read` plus the
    // Step 3 hash-carrying extraction in-thread, sharing only `Send + Sync`
    // inputs) over a bounded lookahead window, then a serial apply phase
    // preserving input-order failures and fragment-count unit membership.
    // Workers run only the pure `read_and_prepare_pure` phase, so they never
    // borrow `ctx`; failure marking, unit batching, and stats stay on the
    // main thread. Complete units embed and commit before more files prepare,
    // earlier storage writes are visible while later files still prepare and
    // live owned text/vectors stay bounded independent of the file count.
    let prepare_width = std::thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(1)
        .max(1);
    let prepare_model = Arc::clone(&ctx.embedding_model);
    let prepare_cancel = ctx.cancel.clone();
    let wave_size = scheduler.policy().max.max(1);
    // Bounded lookahead feeding both stages: a constant multiple of the
    // parallel widths, never of the file count. Live owned payload is at most
    // one prepare window plus queued complete units (each holding at most
    // `max_batch` fragments) plus one embed wave.
    let prepare_window = prepare_width.max(wave_size).saturating_mul(2).max(1);
    let total = files.len();
    let abort = Arc::new(AtomicBool::new(false));
    let mut prepare_elapsed_ms = 0.0;
    let mut embed_elapsed_ms = 0.0;
    // Wave-granularity progress replaces the per-file "reading ..." detail so
    // workers never contend on the shared stats lock; counters are unchanged
    // (failures land in the serial apply below).
    let mut prepared_done = 0usize;
    let mut batch: Vec<PreparedFile> = Vec::new();
    let mut batch_fragments = 0usize;
    let mut ready: Vec<Vec<PreparedFile>> = Vec::new();
    for file_window in files.chunks(prepare_window) {
        throw_if_index_cancelled(ctx)?;
        report_indexing(
            ctx,
            &lock_stats(&stats),
            Some(format!(
                "preparing {} of {} files",
                prepared_done.saturating_add(file_window.len()),
                total
            )),
            progress_base,
            total,
            Some(scheduler.snapshot()),
        );
        let window_started = Instant::now();
        let mut window_outcomes: Vec<PurePrepareOutcome> = Vec::with_capacity(file_window.len());
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(file_window.len());
            for file in file_window {
                let model = Arc::clone(&prepare_model);
                let cancel = prepare_cancel.clone();
                handles.push(
                    scope.spawn(move || read_and_prepare_pure(file, &*model, cancel.as_ref())),
                );
            }
            for (file, handle) in file_window.iter().zip(handles) {
                match handle.join() {
                    Ok(outcome) => window_outcomes.push(outcome),
                    Err(_) => window_outcomes.push(Err(Box::new((
                        file.clone(),
                        EngineError::new(
                            EngineErrorCode::IndexingEmbeddingThreadFailed,
                            "prepare worker thread failed",
                        ),
                    )))),
                }
            }
        });
        prepare_elapsed_ms += window_started.elapsed().as_secs_f64() * 1000.0;
        prepared_done = prepared_done.saturating_add(file_window.len());
        for (file, outcome) in file_window.iter().zip(window_outcomes) {
            throw_if_index_cancelled(ctx)?;
            match outcome {
                Err(failure) => {
                    let (failed_file, error) = *failure;
                    if is_cancelled_error(&error) {
                        throw_if_index_cancelled(ctx)?;
                        return Err(error);
                    }
                    let reason = mark_file_failed(ctx.storage, &failed_file, &error, "prepare");
                    record_file_failed(&mut lock_stats_mut(&stats), &failed_file, Some(reason));
                    ctx.storage.flush()?;
                    report_indexing(
                        ctx,
                        &lock_stats(&stats),
                        Some(format!("failed {}", failed_file.relative_path)),
                        progress_base,
                        total,
                        Some(scheduler.snapshot()),
                    );
                }
                Ok(prepared) => {
                    if prepared.fragments.is_empty() {
                        let committed = timings.time("index_commit", || {
                            commit_file(ctx, prepared, Vec::new(), 0, &stats)
                        })?;
                        report_indexing(
                            ctx,
                            &lock_stats(&stats),
                            Some(finished_file_detail(committed, &file.relative_path)),
                            progress_base,
                            total,
                            Some(scheduler.snapshot()),
                        );
                        continue;
                    }
                    push_prepared(
                        prepared,
                        &mut batch,
                        &mut batch_fragments,
                        &mut ready,
                        max_batch,
                    );
                }
            }
        }
        // `window_outcomes` drops here: consumed files release their owned
        // text before the next window prepares.
        //
        // Embed and commit complete units before preparing more, so completed
        // writes are observable while later windows still prepare. The
        // trailing partial batch stays queued: unit membership is decided by
        // fragment counts alone, identical to whole-repo batching.
        while ready.len() >= wave_size {
            if abort.load(Ordering::Relaxed) {
                break;
            }
            let wave: Vec<Vec<PreparedFile>> = ready.drain(..wave_size.min(ready.len())).collect();
            embed_and_commit_wave(
                wave,
                ctx,
                &stats,
                &scheduler,
                &abort,
                timings,
                progress_base,
                total,
                &mut embed_elapsed_ms,
            )?;
        }
        if abort.load(Ordering::Relaxed) {
            break;
        }
    }
    flush_batch(&mut batch, &mut batch_fragments, &mut ready);
    while !ready.is_empty() {
        if abort.load(Ordering::Relaxed) {
            break;
        }
        throw_if_index_cancelled(ctx)?;
        let wave: Vec<Vec<PreparedFile>> = ready.drain(..wave_size.min(ready.len())).collect();
        embed_and_commit_wave(
            wave,
            ctx,
            &stats,
            &scheduler,
            &abort,
            timings,
            progress_base,
            total,
            &mut embed_elapsed_ms,
        )?;
    }
    if !files.is_empty() {
        timings.add("index_prepare", prepare_elapsed_ms, files.len() as u64);
    }
    // One `index_embedding` entry (count 1, hence count-free in
    // `TimingCollector::entries`): per-wave wall-clock accumulates locally.
    timings.add("index_embedding", embed_elapsed_ms, 1);
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

/// Batches one prepared file into fragment-count units for the streaming loop:
/// oversized singles flush through, otherwise the file joins the open batch,
/// flushing on overflow or exactly-full. Only the open batch carries across
/// producer-window boundaries, so unit membership and input order match
/// whole-repo batching exactly.
fn push_prepared(
    prepared: PreparedFile,
    batch: &mut Vec<PreparedFile>,
    batch_fragments: &mut usize,
    ready: &mut Vec<Vec<PreparedFile>>,
    max_batch: usize,
) {
    if prepared.fragments.len() > max_batch {
        flush_batch(batch, batch_fragments, ready);
        ready.push(vec![prepared]);
        return;
    }
    if *batch_fragments > 0 && *batch_fragments + prepared.fragments.len() > max_batch {
        flush_batch(batch, batch_fragments, ready);
    }
    *batch_fragments += prepared.fragments.len();
    batch.push(prepared);
    if *batch_fragments == max_batch {
        flush_batch(batch, batch_fragments, ready);
    }
}

/// Embeds one bounded wave of complete units in parallel, then commits the
/// owned results serially in input order. Units are borrowed by workers and
/// moved into the commit phase after joining, so no `unit.clone()` ever
/// materializes; vectors and fragments move into storage payloads without
/// re-cloning. Embedding wall-clock accumulates into `embed_elapsed_ms`; the
/// caller records `index_embedding` once (count 1).
#[allow(clippy::too_many_arguments)]
fn embed_and_commit_wave(
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
    let outcomes: Vec<UnitOutcome> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(wave.len());
        for unit in &wave {
            let scheduler = Arc::clone(scheduler);
            let model = Arc::clone(&ctx.embedding_model);
            let abort_ref = abort;
            let on_progress = ctx.on_progress.clone();
            let cancel = ctx.cancel.clone();
            let stats = Arc::clone(stats);
            handles.push(scope.spawn(move || {
                embed_unit(
                    unit,
                    &*model,
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
            .map(|handle| match handle.join() {
                Ok(outcome) => outcome,
                Err(_) => {
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
    for (unit, outcome) in wave.into_iter().zip(outcomes) {
        throw_if_index_cancelled(ctx)?;
        match outcome {
            UnitOutcome::Embedded(results) => {
                for (mut prepared, result) in unit.into_iter().zip(results) {
                    match result {
                        FileOutcome::Embedded(embed) => {
                            if embed.vectors.len() != prepared.fragments.len() {
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
                                let reason =
                                    mark_file_failed(ctx.storage, &prepared.file, &error, "commit");
                                record_file_failed(
                                    &mut lock_stats_mut(stats),
                                    &prepared.file,
                                    Some(reason),
                                );
                                ctx.storage.flush()?;
                                report_indexing(
                                    ctx,
                                    &lock_stats(stats),
                                    Some(finished_file_detail(false, &prepared.file.relative_path)),
                                    progress_base,
                                    total,
                                    Some(scheduler.snapshot()),
                                );
                                continue;
                            }
                            let truncated_fragment_count = embed.truncated_fragment_count;
                            let fragments = std::mem::take(&mut prepared.fragments);
                            let file_vectors: Vec<IndexedFragment> = fragments
                                .into_iter()
                                .zip(embed.vectors)
                                .map(|(fragment, vector)| IndexedFragment {
                                    fragment: fragment.fragment,
                                    vector,
                                })
                                .collect();
                            let committed = timings.time("index_commit", || {
                                commit_vectors(
                                    ctx,
                                    &prepared.file,
                                    &file_vectors,
                                    truncated_fragment_count,
                                    stats,
                                )
                            })?;
                            report_indexing(
                                ctx,
                                &lock_stats(stats),
                                Some(finished_file_detail(
                                    committed,
                                    &prepared.file.relative_path,
                                )),
                                progress_base,
                                total,
                                Some(scheduler.snapshot()),
                            );
                        }
                        FileOutcome::Failed(reason) => {
                            record_file_failed(
                                &mut lock_stats_mut(stats),
                                &prepared.file,
                                Some(reason),
                            );
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
            }
            UnitOutcome::Failed(error) => {
                for prepared in unit {
                    let reason = mark_file_failed(ctx.storage, &prepared.file, &error, "embed");
                    record_file_failed(&mut lock_stats_mut(stats), &prepared.file, Some(reason));
                    report_indexing(
                        ctx,
                        &lock_stats(stats),
                        Some(finished_file_detail(false, &prepared.file.relative_path)),
                        progress_base,
                        total,
                        Some(scheduler.snapshot()),
                    );
                }
                ctx.storage.flush()?;
            }
        }
    }
    Ok(())
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
    // Borrowed views over the already-prepared fragments: no per-item
    // `Content` clone, only reference-sized `EmbeddingInput` descriptors.
    let inputs: Vec<EmbeddingInput<'_>> = fragments
        .iter()
        .map(|fragment| content_to_input(&fragment.embedding_content))
        .collect();
    match embed_inputs_with_retry(
        &inputs,
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
        let input = content_to_input(&fragment.embedding_content);
        match embed_inputs_with_retry(
            std::slice::from_ref(&input),
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
