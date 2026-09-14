//! Shared index-run context: progress sink, run state, diff/prepare stats, and error helpers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{
    DetailEntry, DetailValue, EngineError, EngineErrorCode, EngineResult, error_details,
    workspace_index_detail,
};
use crate::models::EmbeddingModel;
use crate::storage::WorkspaceIndexStorage;
use crate::types::{
    Content, EntityFragment, FileInfo, FileScanDiagnostics, IndexProgress, WorkspaceIndexInfo,
};

use super::scanner::CancelFlag;

/// Index-progress sink shared across indexing threads.
///
/// Named per event type (M2): this is the single index-progress sink used by
/// the pipeline, the service options, and the workspace index handle —
/// distinct from the model-load sink ([`crate::models::ModelLoadSink`]).
pub type IndexProgressSink = Arc<dyn Fn(IndexProgress) + Send + Sync>;

/// Everything one index run needs (mirrors `IndexContext`).
pub struct IndexContext<'a> {
    pub workspace_index: WorkspaceIndexInfo,
    pub storage: &'a mut dyn WorkspaceIndexStorage,
    pub embedding_model: Arc<dyn EmbeddingModel>,
    pub embedding_concurrency: Option<usize>,
    pub on_progress: Option<IndexProgressSink>,
    pub cancel: Option<CancelFlag>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DiffResult {
    pub(crate) added: Vec<FileInfo>,
    pub(crate) modified: Vec<FileInfo>,
    pub(crate) pending: Vec<FileInfo>,
    pub(crate) deleted: Vec<FileInfo>,
    pub(crate) unchanged: Vec<FileInfo>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedFragment {
    pub(crate) fragment: EntityFragment,
    pub(crate) embedding_content: Content,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedFile {
    pub(crate) file: FileInfo,
    pub(crate) fragments: Vec<PreparedFragment>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct IndexStats {
    pub(crate) files_indexed: usize,
    pub(crate) files_failed: usize,
    pub(crate) failed_files: Vec<String>,
    pub(crate) failed_file_reasons: Vec<String>,
    pub(crate) entities_created: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct IndexPassResult {
    pub(crate) files_scanned: usize,
    pub(crate) scan_diagnostics: FileScanDiagnostics,
    pub(crate) diff: DiffResult,
    pub(crate) stats: IndexStats,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProgressBase {
    pub(crate) files_succeeded: usize,
    pub(crate) files_total: usize,
}

pub(crate) const EMBEDDING_TRANSIENT_MAX_RETRIES: u32 = 3;
pub(crate) const EMBEDDING_RATE_LIMIT_MAX_RETRIES: u32 = 6;
pub(crate) const EMBEDDING_TRANSIENT_RETRY_BASE_DELAY_MS: u64 = 500;
pub(crate) const EMBEDDING_RATE_LIMIT_RETRY_BASE_DELAY_MS: u64 = 2000;
pub(crate) const EMBEDDING_TRANSIENT_RETRY_MAX_DELAY_MS: u64 = 8000;
pub(crate) const EMBEDDING_RATE_LIMIT_RETRY_MAX_DELAY_MS: u64 = 30000;
pub(crate) const EMBEDDING_RETRY_JITTER_MS: u64 = 500;
pub(crate) const EMBEDDING_SUCCESS_STREAK_MIN: usize = 4;
pub(crate) const MAX_SKIPPED_FILE_SAMPLES: usize = 20;

pub(crate) const PERMANENT_REMOTE_MODEL_PROVIDER_CODES: &[&str] = &[
    "invalid_model",
    "model_not_found",
    "unsupported_model",
    "invalid_dimension",
    "invalid_dimensions",
    "unsupported_dimension",
    "unsupported_dimensions",
    "dimension_out_of_range",
    "invalid_embedding_dimension",
    "unsupported_embedding_dimension",
];

pub(crate) fn throw_if_index_cancelled(ctx: &IndexContext<'_>) -> EngineResult<()> {
    if ctx.cancel.as_ref().is_some_and(CancelFlag::is_cancelled) {
        return Err(EngineError::new(
            EngineErrorCode::from_static("INDEXING.CANCELLED"),
            "indexing was cancelled",
        )
        .with_context(workspace_index_context(&ctx.workspace_index)));
    }
    Ok(())
}

pub(crate) fn is_cancelled_error(error: &EngineError) -> bool {
    error.code().suffix() == "INDEXING.CANCELLED"
}

pub(crate) fn file_failure_reason(stage: &str, error: &EngineError) -> String {
    one_line(&format!("{stage}: {}", error_to_message(error)))
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn error_to_message(error: &EngineError) -> String {
    match error.context() {
        Some(context) => format!(
            "{}: {} ({})",
            error.code().qualified(),
            error.message(),
            one_line(context)
        ),
        None => format!("{}: {}", error.code().qualified(), error.message()),
    }
}

pub(crate) fn file_context(file: &FileInfo) -> String {
    format!("fileId={} path={}", file.id.as_str(), file.relative_path)
}

pub(crate) fn workspace_index_context(index: &WorkspaceIndexInfo) -> String {
    error_details(vec![
        DetailEntry::Line(&workspace_index_detail(&index.name)),
        DetailEntry::Pair("workspace_index_id", DetailValue::Str(&index.id)),
    ])
    .unwrap_or_default()
}

pub(crate) fn summarize_failed_files(files: &[String]) -> String {
    const SHOWN: usize = 5;
    if files.len() > SHOWN {
        format!(
            "{} and {} more",
            files
                .get(..SHOWN)
                .map(|window| window.join(", "))
                .unwrap_or_default(),
            files.len() - SHOWN
        )
    } else {
        files.join(", ")
    }
}

pub(crate) fn throw_if_aborted(
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
) -> EngineResult<()> {
    if abort.load(Ordering::Relaxed) || cancel.is_some_and(CancelFlag::is_cancelled) {
        return Err(EngineError::new(
            EngineErrorCode::from_static("INDEXING.CANCELLED"),
            "embedding was cancelled",
        ));
    }
    Ok(())
}

pub(crate) fn is_cancelled_or_aborted(error: &EngineError) -> bool {
    error.code().suffix() == "INDEXING.CANCELLED"
}
