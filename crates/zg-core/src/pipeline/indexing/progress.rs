//! Index progress reporting: phase reports, download merging, shared stats locks.

use std::sync::{Arc, Mutex};

use crate::models::{EmbeddingModelProgress, EmbeddingStageKind, ModelLoadSink};
use crate::types::{EmbeddingStage, IndexEmbeddingProgress, IndexProgress, IndexProgressPhase};

use super::context::{IndexContext, IndexPassResult, IndexProgressSink, IndexStats, ProgressBase};
use super::retry::EmbeddingScheduler;

pub(crate) fn report_index_finalizing(
    ctx: &IndexContext<'_>,
    pass: &IndexPassResult,
    progress_base: Option<ProgressBase>,
) {
    report(
        ctx,
        IndexProgress {
            phase: Some(IndexProgressPhase::Indexing),
            files_total: Some(
                progress_base
                    .map(|base| base.files_total)
                    .unwrap_or_else(|| {
                        pass.diff.added.len() + pass.diff.modified.len() + pass.diff.pending.len()
                    }),
            ),
            files_indexed: Some(
                progress_base.map(|base| base.files_succeeded).unwrap_or(0)
                    + pass.stats.files_indexed
                    + pass.stats.files_failed,
            ),
            files_failed: Some(pass.stats.files_failed),
            detail: Some("finalizing index".to_owned()),
            embedding: None,
        },
    );
}

pub(crate) fn retry_progress_base(pass: &IndexPassResult) -> ProgressBase {
    ProgressBase {
        files_succeeded: pass.stats.files_indexed,
        files_total: pass.diff.added.len() + pass.diff.modified.len() + pass.diff.pending.len(),
    }
}

pub(crate) fn report_scanning(
    ctx: &IndexContext<'_>,
    detail: &str,
    progress_base: Option<ProgressBase>,
) {
    report(
        ctx,
        IndexProgress {
            phase: Some(IndexProgressPhase::Scanning),
            files_total: progress_base.map(|base| base.files_total),
            files_indexed: progress_base.map(|base| base.files_succeeded),
            files_failed: None,
            detail: Some(detail.to_owned()),
            embedding: None,
        },
    );
}

pub(crate) fn report_indexing(
    ctx: &IndexContext<'_>,
    stats: &IndexStats,
    detail: Option<String>,
    progress_base: Option<ProgressBase>,
    pending_len: usize,
    embedding: Option<IndexEmbeddingProgress>,
) {
    report(
        ctx,
        IndexProgress {
            phase: Some(IndexProgressPhase::Indexing),
            files_total: Some(
                progress_base
                    .map(|base| base.files_total)
                    .unwrap_or(pending_len),
            ),
            files_indexed: Some(
                progress_base.map(|base| base.files_succeeded).unwrap_or(0)
                    + stats.files_indexed
                    + stats.files_failed,
            ),
            files_failed: Some(stats.files_failed),
            detail,
            embedding,
        },
    );
}

pub(crate) fn report(ctx: &IndexContext<'_>, progress: IndexProgress) {
    if let Some(callback) = &ctx.on_progress {
        callback(progress);
    }
}

pub(crate) fn lock_stats(stats: &Arc<Mutex<IndexStats>>) -> IndexStats {
    // Same poison policy as `lock_stats_mut`: read through instead of
    // silently zeroing progress.
    stats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub(crate) fn lock_stats_mut(
    stats: &Arc<Mutex<IndexStats>>,
) -> std::sync::MutexGuard<'_, IndexStats> {
    match stats.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(crate) fn report_download_progress(
    callback: &IndexProgressSink,
    scheduler: &EmbeddingScheduler,
    stats: &IndexStats,
    progress: &EmbeddingModelProgress,
) {
    callback(IndexProgress {
        phase: Some(IndexProgressPhase::Indexing),
        files_total: None,
        files_indexed: Some(stats.files_indexed + stats.files_failed),
        files_failed: Some(stats.files_failed),
        detail: Some(format!(
            "downloading {}",
            progress.message.as_deref().unwrap_or("model")
        )),
        embedding: Some(merge_progress(scheduler.snapshot(), progress)),
    });
}

pub(crate) fn merge_progress(
    snapshot: IndexEmbeddingProgress,
    progress: &EmbeddingModelProgress,
) -> IndexEmbeddingProgress {
    IndexEmbeddingProgress {
        concurrency: snapshot.concurrency,
        max_concurrency: snapshot.max_concurrency,
        retryable_failures: snapshot.retryable_failures,
        stage: progress.stage.map(|stage| match stage {
            EmbeddingStageKind::Preparing => EmbeddingStage::Preparing,
            EmbeddingStageKind::Downloading => EmbeddingStage::Downloading,
            EmbeddingStageKind::Ready => EmbeddingStage::Ready,
            EmbeddingStageKind::Warning => EmbeddingStage::Warning,
        }),
        model: progress.message.clone(),
        downloaded_bytes: progress.downloaded_bytes,
        total_bytes: progress.total_bytes,
        message: progress.message.clone(),
    }
}

pub(crate) fn thread_report(
    on_progress: Option<&IndexProgressSink>,
    scheduler: &EmbeddingScheduler,
    detail: &str,
) {
    if let Some(callback) = on_progress {
        callback(IndexProgress {
            phase: Some(IndexProgressPhase::Indexing),
            files_total: None,
            files_indexed: None,
            files_failed: None,
            detail: Some(detail.to_owned()),
            embedding: Some(scheduler.snapshot()),
        });
    }
}

pub(crate) fn thread_progress_sink(
    on_progress: Option<&IndexProgressSink>,
    scheduler: &Arc<EmbeddingScheduler>,
    stats: &Arc<Mutex<IndexStats>>,
    progress_base: Option<ProgressBase>,
    total: usize,
) -> Option<ModelLoadSink> {
    let callback = on_progress?.clone();
    let scheduler = Arc::clone(scheduler);
    let stats = Arc::clone(stats);
    Some(Arc::new(move |progress: EmbeddingModelProgress| {
        let snapshot = lock_stats(&stats);
        callback(IndexProgress {
            phase: Some(IndexProgressPhase::Indexing),
            files_total: Some(progress_base.map(|base| base.files_total).unwrap_or(total)),
            files_indexed: Some(
                progress_base.map(|base| base.files_succeeded).unwrap_or(0)
                    + snapshot.files_indexed
                    + snapshot.files_failed,
            ),
            files_failed: Some(snapshot.files_failed),
            detail: Some(format!(
                "downloading {}",
                progress.message.as_deref().unwrap_or("model")
            )),
            embedding: Some(merge_progress(scheduler.snapshot(), &progress)),
        });
    }) as ModelLoadSink)
}
