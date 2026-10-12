//! Streaming orchestration: bounded prepare windows, batching, wave drain.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::models::{EmbeddingModelProgress, ModelLoadSink};
use crate::storage::IndexedFragment;
use crate::types::FileInfo;
use crate::utils::timing::TimingCollector;

use super::super::context::{
    IndexContext, IndexStats, PreparedFile, ProgressBase, is_cancelled_error,
    throw_if_index_cancelled,
};
use super::super::prepare::{
    PurePrepareOutcome, commit_files_batch, finished_file_detail, mark_file_failed,
    read_and_prepare_pure, record_file_failed,
};
use super::super::progress::{
    lock_stats, lock_stats_mut, report_download_progress, report_indexing,
};
use super::super::retry::{EmbeddingScheduler, resolve_embedding_concurrency_policy};
use super::wave::embed_and_commit_wave;

// `index_files` is a linear streaming orchestrator (prepare window -> serial
// apply -> wave drain); splitting it further would scatter the ordering
// invariants without reducing complexity, so keep the phases inline.
#[allow(clippy::too_many_lines)]
pub(crate) fn index_files(
    files: &[FileInfo],
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
        .map_or(1, std::num::NonZero::get)
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
        // Fragment-free files and prepare failures apply serially in input
        // order; metadata persists once per window below (#56), not per file.
        let mut window_empty: Vec<PreparedFile> = Vec::new();
        let mut window_failed = false;
        for (_, outcome) in file_window.iter().zip(window_outcomes) {
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
                    window_failed = true;
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
                        window_empty.push(prepared);
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
        // One batch commit for every fragment-free file in the window: two
        // metadata snapshots per window instead of two per file (#56).
        if !window_empty.is_empty() {
            let batch_payload: Vec<(FileInfo, Vec<IndexedFragment>, usize)> = window_empty
                .iter()
                .map(|prepared| (prepared.file.clone(), Vec::new(), 0))
                .collect();
            let committed_vec: Vec<bool> = timings.time("index_commit", || {
                commit_files_batch(ctx, batch_payload, &stats)
            })?;
            debug_assert_eq!(committed_vec.len(), window_empty.len());
            for (prepared, committed) in window_empty.into_iter().zip(committed_vec) {
                throw_if_index_cancelled(ctx)?;
                report_indexing(
                    ctx,
                    &lock_stats(&stats),
                    Some(finished_file_detail(
                        committed,
                        &prepared.file.relative_path,
                    )),
                    progress_base,
                    total,
                    Some(scheduler.snapshot()),
                );
            }
        }
        if window_failed {
            ctx.storage.flush()?;
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
