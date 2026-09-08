//! Workspace indexing: scan, diff, prepare, embed, commit.
//!
//! Port of `engine/pipeline/indexing/index.ts`. The TypeScript implementation
//! is async (`AbortSignal`, promise sets); this port is synchronous with
//! scoped-thread embedding parallelism:
//! - file preparation stays sequential in the calling thread;
//! - embedding units run on scoped threads bounded by the scheduler policy,
//!   each gated by the adaptive [`EmbeddingScheduler`] semaphore;
//! - storage commits stay serial in the calling thread (`&mut` storage is
//!   never shared across threads), in deterministic file order;
//! - cancellation flows through [`CancelFlag`] instead of `AbortSignal`.
//!
//! Retry, backoff, adaptive concurrency, and failure accounting mirror the TS
//! originals, including the batch → per-file → one-by-one fallback chain.

pub mod input_budget;
pub mod root_paths;
pub mod scanner;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::error::{
    DetailEntry, DetailValue, EngineError, EngineErrorCode, EngineResult, error_details,
    workspace_index_detail,
};
use crate::extraction::vector_content::vector_content_for_fragment;
use crate::extraction::{Source, extract_for_indexing};
use crate::models::embeddings::EmbeddingResult;
use crate::models::{
    EmbeddingInput, EmbeddingInputKind, EmbeddingModel, EmbeddingModelProgress, EmbeddingPurpose,
    EmbeddingStageKind, ModelLoadSink,
};
use crate::storage::{FileIndexDiagnostics, IndexedFragment, WorkspaceIndexStorage};
use crate::types::{
    Content, EmbeddingStage, EntityFragment, FileInfo, FileScanDiagnostics, ImageFormat,
    IndexEmbeddingProgress, IndexProgress, IndexProgressPhase, IndexResult, WorkspaceIndexInfo,
    WorkspaceIndexStatus,
};
use crate::utils::hash::sha256_bytes;
use crate::utils::timing::TimingCollector;

use self::input_budget::index_chunk_options;
use self::scanner::{
    CancelFlag, ScanOptions, create_scan_diagnostics, scan_directory_path, scan_file_path,
    scan_root_paths,
};

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
struct DiffResult {
    added: Vec<FileInfo>,
    modified: Vec<FileInfo>,
    pending: Vec<FileInfo>,
    deleted: Vec<FileInfo>,
    unchanged: Vec<FileInfo>,
}

#[derive(Debug, Clone)]
struct PreparedFragment {
    fragment: EntityFragment,
    embedding_content: Content,
}

#[derive(Debug, Clone)]
struct PreparedFile {
    file: FileInfo,
    fragments: Vec<PreparedFragment>,
}

#[derive(Debug, Clone, Default)]
struct IndexStats {
    files_indexed: usize,
    files_failed: usize,
    failed_files: Vec<String>,
    failed_file_reasons: Vec<String>,
    entities_created: usize,
}

#[derive(Debug, Clone)]
struct IndexPassResult {
    files_scanned: usize,
    scan_diagnostics: FileScanDiagnostics,
    diff: DiffResult,
    stats: IndexStats,
}

#[derive(Debug, Clone, Copy)]
struct ProgressBase {
    files_succeeded: usize,
    files_total: usize,
}

const EMBEDDING_TRANSIENT_MAX_RETRIES: u32 = 3;
const EMBEDDING_RATE_LIMIT_MAX_RETRIES: u32 = 6;
const EMBEDDING_TRANSIENT_RETRY_BASE_DELAY_MS: u64 = 500;
const EMBEDDING_RATE_LIMIT_RETRY_BASE_DELAY_MS: u64 = 2000;
const EMBEDDING_TRANSIENT_RETRY_MAX_DELAY_MS: u64 = 8000;
const EMBEDDING_RATE_LIMIT_RETRY_MAX_DELAY_MS: u64 = 30000;
const EMBEDDING_RETRY_JITTER_MS: u64 = 500;
const EMBEDDING_SUCCESS_STREAK_MIN: usize = 4;
const MAX_SKIPPED_FILE_SAMPLES: usize = 20;

const PERMANENT_REMOTE_MODEL_PROVIDER_CODES: &[&str] = &[
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

/// Index the whole workspace (mirrors `indexWorkspace`).
pub fn index_workspace(ctx: &mut IndexContext<'_>) -> EngineResult<IndexResult> {
    index_workspace_inner(ctx).map_err(|error| {
        let context = workspace_index_context(&ctx.workspace_index);
        EngineError::new(
            EngineErrorCode::from_static("INDEXING.WORKSPACE_FAILED"),
            "indexing workspace failed",
        )
        .with_context(format!("{context}\ncause={}", error_to_message(&error)))
    })
}

/// Index explicit changed paths (mirrors `indexWorkspacePaths`).
pub fn index_workspace_paths(
    ctx: &mut IndexContext<'_>,
    changed_paths: &[String],
) -> EngineResult<IndexResult> {
    index_workspace_paths_inner(ctx, changed_paths).map_err(|error| {
        let context = workspace_index_context(&ctx.workspace_index);
        EngineError::new(
            EngineErrorCode::from_static("INDEXING.WORKSPACE_FAILED"),
            "indexing changed paths failed",
        )
        .with_context(format!("{context}\ncause={}", error_to_message(&error)))
    })
}

/// Inspect status without writing (mirrors `getWorkspaceIndexStatus`).
pub fn get_workspace_index_status(
    workspace_index: &WorkspaceIndexInfo,
    stored_files: &[FileInfo],
    cancel: Option<&CancelFlag>,
) -> EngineResult<WorkspaceIndexStatus> {
    (|| {
        let scan = scan_root_paths(
            &workspace_index.id,
            &workspace_index.root_paths,
            &ScanOptions {
                known_files: stored_files.to_vec(),
                cancel: cancel.cloned(),
            },
        )?;
        let diff = compute_diff_from_files(&scan.files, stored_files)?;
        let pending_files: Vec<FileInfo> = stored_files
            .iter()
            .filter(|file| {
                file.index_status
                    .as_ref()
                    .and_then(|status| status.indexed_time)
                    .is_none()
            })
            .cloned()
            .collect();
        let failed_files: Vec<FileInfo> = pending_files
            .iter()
            .filter(|file| {
                file.index_status
                    .as_ref()
                    .and_then(|status| status.error.clone())
                    .is_some()
            })
            .cloned()
            .collect();
        let indexed_files: Vec<&FileInfo> = stored_files
            .iter()
            .filter(|file| {
                file.index_status
                    .as_ref()
                    .and_then(|status| status.indexed_time)
                    .is_some()
            })
            .collect();
        let entities_indexed: usize = indexed_files
            .iter()
            .map(|file| {
                file.index_status
                    .as_ref()
                    .map(|status| status.entity_count)
                    .unwrap_or(0)
            })
            .sum();
        let fragments_truncated: usize = indexed_files
            .iter()
            .map(|file| {
                file.index_status
                    .as_ref()
                    .and_then(|status| status.truncated_fragment_count)
                    .unwrap_or(0)
            })
            .sum();
        Ok(WorkspaceIndexStatus {
            files_scanned: scan.files.len(),
            files_added: diff.added.len(),
            files_modified: diff.modified.len(),
            files_pending: pending_files.len(),
            files_deleted: diff.deleted.len(),
            files_unchanged: diff.unchanged.len(),
            files_failed: failed_files.len(),
            files_stored: stored_files.len(),
            pending_files,
            failed_files,
            added_files: diff.added,
            modified_files: diff.modified,
            deleted_files: diff.deleted,
            entities_indexed,
            fragments_truncated,
        })
    })()
    .map_err(|error: EngineError| {
        EngineError::new(
            EngineErrorCode::from_static("INDEXING.STATUS_FAILED"),
            "inspecting workspace index status failed",
        )
        .with_context(format!(
            "{}\ncause={}",
            workspace_index_context(workspace_index),
            error_to_message(&error)
        ))
    })
}

fn index_workspace_inner(ctx: &mut IndexContext<'_>) -> EngineResult<IndexResult> {
    let start = Instant::now();
    let mut timings = TimingCollector::new();
    throw_if_index_cancelled(ctx)?;
    let first_pass = run_index_pass(ctx, "Scanning files...", &mut timings, None)?;
    let mut passes = vec![first_pass];
    let mut progress_base = None;
    if passes[0].stats.files_failed > 0 {
        let failed = passes[0].stats.files_failed;
        progress_base = Some(retry_progress_base(&passes[0]));
        if let Some(base) = progress_base {
            report(
                ctx,
                IndexProgress {
                    phase: Some(IndexProgressPhase::Scanning),
                    files_total: Some(base.files_total),
                    files_indexed: Some(base.files_succeeded),
                    files_failed: None,
                    detail: Some(format!(
                        "Retrying {failed} failed {}...",
                        if failed == 1 { "file" } else { "files" }
                    )),
                    embedding: None,
                },
            );
        }
        passes.push(run_index_pass(
            ctx,
            "Scanning retry candidates...",
            &mut timings,
            progress_base,
        )?);
    }
    let [.., final_pass] = passes.as_slice() else {
        unreachable!("index passes always contain the initial pass");
    };
    let final_pass = final_pass.clone();
    throw_if_index_cancelled(ctx)?;
    report_index_finalizing(ctx, &final_pass, progress_base);
    timings.time("index_optimize", || optimize_storage(ctx))?;
    let result = build_index_result(
        ctx,
        &passes,
        start.elapsed().as_millis() as u64,
        &mut timings,
    );
    if result.files_failed > 0 {
        report(
            ctx,
            IndexProgress {
                phase: Some(IndexProgressPhase::Done),
                detail: Some("Indexing completed with failed files".to_owned()),
                ..IndexProgress::default()
            },
        );
        return Err(files_failed_error(ctx, &result, &final_pass, passes.len()));
    }
    report(
        ctx,
        IndexProgress {
            phase: Some(IndexProgressPhase::Done),
            detail: Some("Indexing complete".to_owned()),
            ..IndexProgress::default()
        },
    );
    Ok(result)
}

fn index_workspace_paths_inner(
    ctx: &mut IndexContext<'_>,
    changed_paths: &[String],
) -> EngineResult<IndexResult> {
    let start = Instant::now();
    let mut timings = TimingCollector::new();
    let mut seen = HashSet::new();
    let normalized_paths: Vec<String> = changed_paths
        .iter()
        .map(|path| normalize_for_diff(path))
        .filter(|path| seen.insert(path.clone()))
        .collect();
    throw_if_index_cancelled(ctx)?;
    let first_pass = run_path_index_pass(ctx, &normalized_paths, &mut timings, None)?;
    let mut passes = vec![first_pass];
    let mut progress_base = None;
    if passes[0].stats.files_failed > 0 {
        progress_base = Some(retry_progress_base(&passes[0]));
        passes.push(run_path_index_pass(
            ctx,
            &normalized_paths,
            &mut timings,
            progress_base,
        )?);
    }
    let [.., final_pass] = passes.as_slice() else {
        unreachable!("path index passes always contain the initial pass");
    };
    let final_pass = final_pass.clone();
    throw_if_index_cancelled(ctx)?;
    report_index_finalizing(ctx, &final_pass, progress_base);
    timings.time("index_optimize", || optimize_storage(ctx))?;
    let result = build_index_result(
        ctx,
        &passes,
        start.elapsed().as_millis() as u64,
        &mut timings,
    );
    if result.files_failed > 0 {
        return Err(files_failed_error(ctx, &result, &final_pass, passes.len()));
    }
    report(
        ctx,
        IndexProgress {
            phase: Some(IndexProgressPhase::Done),
            detail: Some("Indexing complete".to_owned()),
            ..IndexProgress::default()
        },
    );
    Ok(result)
}

fn normalize_for_diff(path: &str) -> String {
    crate::paths::to_display_path(&crate::paths::normalize_path(std::path::Path::new(path)))
}

fn files_failed_error(
    ctx: &IndexContext<'_>,
    result: &IndexResult,
    final_pass: &IndexPassResult,
    pass_count: usize,
) -> EngineError {
    let failed_files = summarize_failed_files(&final_pass.stats.failed_files);
    let failed_reasons = summarize_failed_files(&final_pass.stats.failed_file_reasons);
    let hint = if pass_count > 1 {
        "Retried failed files once automatically; if failures persist, fix the failed files or embedding configuration."
    } else {
        "If the failure was transient, rerun the same indexing command; if it persists, fix the failed files or embedding configuration."
    };
    let detail = error_details(vec![
        DetailEntry::Line(&workspace_index_detail(&ctx.workspace_index.name)),
        DetailEntry::Pair("filesFailed", DetailValue::Uint(result.files_failed as u64)),
        DetailEntry::Pair(
            "filesScanned",
            DetailValue::Uint(result.files_scanned as u64),
        ),
        DetailEntry::Pair("failedFiles", DetailValue::Str(&failed_files)),
        DetailEntry::Pair("failedReasons", DetailValue::Str(&failed_reasons)),
        DetailEntry::Pair("hint", DetailValue::Str(hint)),
    ])
    .unwrap_or_default();
    EngineError::new(
        EngineErrorCode::from_static("INDEXING.FILES_FAILED"),
        format!(
            "Indexing completed with {} failed {}",
            result.files_failed,
            if result.files_failed == 1 {
                "file"
            } else {
                "files"
            }
        ),
    )
    .with_context(detail)
}

fn run_path_index_pass(
    ctx: &mut IndexContext<'_>,
    changed_paths: &[String],
    timings: &mut TimingCollector,
    progress_base: Option<ProgressBase>,
) -> EngineResult<IndexPassResult> {
    report_scanning(ctx, "Scanning changed paths...", progress_base);
    let scanned = timings.time("index_scan_paths", || {
        let mut files = Vec::new();
        let mut diagnostics = create_scan_diagnostics();
        for path in changed_paths {
            throw_if_index_cancelled(ctx)?;
            let options = ScanOptions {
                known_files: Vec::new(),
                cancel: ctx.cancel.clone(),
            };
            let is_dir = std::fs::metadata(path)
                .map(|meta| meta.is_dir())
                .unwrap_or(false);
            let scan = if is_dir {
                scan_directory_path(
                    &ctx.workspace_index.id,
                    &ctx.workspace_index.root_paths,
                    path,
                    &options,
                )?
            } else {
                scan_file_path(
                    &ctx.workspace_index.id,
                    &ctx.workspace_index.root_paths,
                    path,
                    &options,
                )?
            };
            files.extend(scan.files);
            merge_scan_diagnostics(&mut diagnostics, scan.diagnostics);
        }
        Ok::<_, EngineError>((dedupe_by_id(files), diagnostics))
    })?;
    throw_if_index_cancelled(ctx)?;
    let mut exact_existing = Vec::new();
    let mut prefix_paths = Vec::new();
    for path in changed_paths {
        match ctx.storage.get_file_by_path(path) {
            Some(file) => exact_existing.push(file),
            None => prefix_paths.push(path.clone()),
        }
    }
    let mut existing = exact_existing;
    existing.extend(ctx.storage.list_files_by_path_prefixes(&prefix_paths));
    let existing = dedupe_by_id(existing);
    run_diff_pass(ctx, scanned.0, &existing, timings, progress_base, scanned.1)
}

fn run_index_pass(
    ctx: &mut IndexContext<'_>,
    scanning_detail: &str,
    timings: &mut TimingCollector,
    progress_base: Option<ProgressBase>,
) -> EngineResult<IndexPassResult> {
    report_scanning(ctx, scanning_detail, progress_base);
    let existing = ctx.storage.list_files();
    let scan = timings.time("index_scan", || {
        scan_root_paths(
            &ctx.workspace_index.id,
            &ctx.workspace_index.root_paths,
            &ScanOptions {
                known_files: existing.clone(),
                cancel: ctx.cancel.clone(),
            },
        )
    })?;
    throw_if_index_cancelled(ctx)?;
    run_diff_pass(
        ctx,
        scan.files,
        &existing,
        timings,
        progress_base,
        scan.diagnostics,
    )
}

fn run_diff_pass(
    ctx: &mut IndexContext<'_>,
    scanned_files: Vec<FileInfo>,
    existing_files: &[FileInfo],
    timings: &mut TimingCollector,
    progress_base: Option<ProgressBase>,
    scan_diagnostics: FileScanDiagnostics,
) -> EngineResult<IndexPassResult> {
    let diff = timings.time("index_diff", || {
        compute_diff_from_files(&scanned_files, existing_files)
    })?;
    throw_if_index_cancelled(ctx)?;
    let mut pending =
        Vec::with_capacity(diff.added.len() + diff.modified.len() + diff.pending.len());
    pending.extend(diff.added.iter().cloned());
    pending.extend(diff.modified.iter().cloned());
    pending.extend(diff.pending.iter().cloned());

    let scanned_len = scanned_files.len();
    report(
        ctx,
        IndexProgress {
            phase: Some(IndexProgressPhase::Scanning),
            files_total: Some(
                progress_base
                    .map(|base| base.files_total)
                    .unwrap_or(scanned_len),
            ),
            files_indexed: Some(progress_base.map(|base| base.files_succeeded).unwrap_or(0)),
            files_failed: None,
            detail: Some(format!(
                "{} added, {} modified, {} pending, {} deleted, {} unchanged",
                diff.added.len(),
                diff.modified.len(),
                diff.pending.len(),
                diff.deleted.len(),
                diff.unchanged.len()
            )),
            embedding: None,
        },
    );

    timings.time("index_delete_stale", || {
        for file in &diff.deleted {
            throw_if_index_cancelled(ctx)?;
            ctx.storage.delete_file(&file.id).map_err(|error| {
                EngineError::new(
                    EngineErrorCode::from_static("INDEXING.DELETE_FILE_FAILED"),
                    "indexing failed to delete stale file records",
                )
                .with_context(format!(
                    "{}\ncause={}",
                    file_context(file),
                    error_to_message(&error)
                ))
            })?;
        }
        Ok::<_, EngineError>(())
    })?;

    report_indexing(
        ctx,
        &IndexStats::default(),
        None,
        progress_base,
        pending.len(),
        None,
    );
    let stats = index_files(pending, ctx, timings, progress_base)?;
    throw_if_index_cancelled(ctx)?;
    Ok(IndexPassResult {
        files_scanned: scanned_len,
        scan_diagnostics,
        diff,
        stats,
    })
}

fn report_index_finalizing(
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

fn retry_progress_base(pass: &IndexPassResult) -> ProgressBase {
    ProgressBase {
        files_succeeded: pass.stats.files_indexed,
        files_total: pass.diff.added.len() + pass.diff.modified.len() + pass.diff.pending.len(),
    }
}

fn report_scanning(ctx: &IndexContext<'_>, detail: &str, progress_base: Option<ProgressBase>) {
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

fn report_indexing(
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

fn report(ctx: &IndexContext<'_>, progress: IndexProgress) {
    if let Some(callback) = &ctx.on_progress {
        callback(progress);
    }
}

fn build_index_result(
    ctx: &IndexContext<'_>,
    passes: &[IndexPassResult],
    duration_ms: u64,
    timings: &mut TimingCollector,
) -> IndexResult {
    let _ = ctx;
    let first = &passes[0];
    let retries = &passes[1..];
    let final_pass = &passes[passes.len() - 1];
    IndexResult {
        files_scanned: final_pass.files_scanned,
        files_added: first.diff.added.len()
            + retries
                .iter()
                .map(|pass| pass.diff.added.len())
                .sum::<usize>(),
        files_modified: first.diff.modified.len()
            + retries
                .iter()
                .map(|pass| pass.diff.modified.len())
                .sum::<usize>(),
        files_pending: first.diff.pending.len()
            + retries
                .iter()
                .map(|pass| pass.diff.pending.len())
                .sum::<usize>(),
        files_deleted: first.diff.deleted.len()
            + retries
                .iter()
                .map(|pass| pass.diff.deleted.len())
                .sum::<usize>(),
        files_unchanged: first.diff.unchanged.len(),
        files_failed: final_pass.stats.files_failed,
        entities_created: passes.iter().map(|pass| pass.stats.entities_created).sum(),
        duration_ms,
        timings: Some(timings.entries()),
        scan_diagnostics: if final_pass.scan_diagnostics.skipped_files > 0 {
            Some(final_pass.scan_diagnostics.clone())
        } else {
            None
        },
    }
}

fn merge_scan_diagnostics(target: &mut FileScanDiagnostics, source: FileScanDiagnostics) {
    target.skipped_files += source.skipped_files;
    for (reason, count) in source.skipped_by_reason {
        *target.skipped_by_reason.entry(reason).or_insert(0) += count;
    }
    let remaining = MAX_SKIPPED_FILE_SAMPLES.saturating_sub(target.skipped_samples.len());
    target
        .skipped_samples
        .extend(source.skipped_samples.into_iter().take(remaining));
}

fn dedupe_by_id(files: Vec<FileInfo>) -> Vec<FileInfo> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        if seen.insert(file.id.clone()) {
            out.push(file);
        }
    }
    out
}

fn optimize_storage(ctx: &mut IndexContext<'_>) -> EngineResult<()> {
    ctx.storage.finalize_writes().map_err(|error| {
        EngineError::new(
            EngineErrorCode::from_static("INDEXING.OPTIMIZE_FAILED"),
            "indexing failed to finalize storage",
        )
        .with_context(format!(
            "{}\ncause={}",
            workspace_index_context(&ctx.workspace_index),
            error_to_message(&error)
        ))
    })
}

fn compute_diff_from_files(
    scanned_files: &[FileInfo],
    existing_files: &[FileInfo],
) -> EngineResult<DiffResult> {
    let existing_by_id: HashMap<&crate::ids::FileId, &FileInfo> =
        existing_files.iter().map(|file| (&file.id, file)).collect();
    let mut seen = HashSet::new();
    let mut diff = DiffResult::default();
    for file in scanned_files {
        seen.insert(file.id.clone());
        match existing_by_id.get(&file.id) {
            None => diff.added.push(with_content_hash(file)?),
            Some(existing) => {
                if existing
                    .index_status
                    .as_ref()
                    .and_then(|status| status.indexed_time)
                    .is_none()
                {
                    diff.pending.push(with_content_hash(file)?);
                    continue;
                }
                if existing.size_bytes == file.size_bytes
                    && existing.last_modified_time == file.last_modified_time
                    && existing.content_hash.is_some()
                {
                    diff.unchanged.push((*existing).clone());
                    continue;
                }
                let hashed = with_content_hash(file)?;
                if existing.size_bytes == hashed.size_bytes
                    && existing.content_hash == hashed.content_hash
                {
                    diff.unchanged.push((*existing).clone());
                } else {
                    diff.modified.push(hashed);
                }
            }
        }
    }
    diff.deleted = existing_by_id
        .values()
        .filter(|file| !seen.contains(&file.id))
        .map(|file| (*file).clone())
        .collect();
    Ok(diff)
}

fn with_content_hash(file: &FileInfo) -> EngineResult<FileInfo> {
    match std::fs::read(&file.absolute_path) {
        Ok(bytes) => {
            let mut hashed = file.clone();
            hashed.content_hash = Some(sha256_bytes(&bytes));
            Ok(hashed)
        }
        Err(err) => Err(EngineError::new(
            EngineErrorCode::from_static("INDEXING.CONTENT_HASH_FAILED"),
            "indexing failed to compute file content hash",
        )
        .with_context(format!("{}\ndetail={err}", file_context(file)))),
    }
}

// ---------------------------------------------------------------------------
// File preparation and serial commit.
// ---------------------------------------------------------------------------

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

fn index_files(
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
            _ => None,
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

fn lock_stats(stats: &Arc<Mutex<IndexStats>>) -> IndexStats {
    stats.lock().map(|guard| guard.clone()).unwrap_or_default()
}

fn lock_stats_mut(stats: &Arc<Mutex<IndexStats>>) -> std::sync::MutexGuard<'_, IndexStats> {
    match stats.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn report_download_progress(
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

fn merge_progress(
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
    if unit.len() == 1 {
        let prepared = &unit[0];
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

fn thread_report(
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

fn thread_progress_sink(
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
        let vectors = result.vectors[offset..end].to_vec();
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

fn prepare_file(
    file: &FileInfo,
    ctx: &mut IndexContext<'_>,
) -> EngineResult<Result<PreparedFile, String>> {
    match prepare_file_inner(file, ctx) {
        Ok(prepared) => Ok(Ok(prepared)),
        Err(error) => {
            if is_cancelled_error(&error) {
                return Err(error);
            }
            Ok(Err(mark_file_failed(ctx.storage, file, &error, "prepare")))
        }
    }
}

fn prepare_file_inner(file: &FileInfo, ctx: &IndexContext<'_>) -> EngineResult<PreparedFile> {
    throw_if_index_cancelled(ctx)?;
    let bytes = std::fs::read(&file.absolute_path).map_err(|err| {
        EngineError::new(
            EngineErrorCode::from_static("INDEXING.READ_SOURCE_FAILED"),
            "indexing failed to read source file",
        )
        .with_context(format!("{}\ndetail={err}", file_context(file)))
    })?;
    let text = String::from_utf8_lossy(&bytes);
    let source = if file.kind == crate::types::FileKind::Image {
        Source::Image {
            file,
            data: &bytes,
            format: image_format_of(file)?,
        }
    } else {
        Source::Text {
            file,
            text: text.as_ref(),
        }
    };
    let owned_text: Option<String> = match &source {
        Source::Text { text, .. } => Some((*text).to_owned()),
        Source::Image { .. } => None,
    };
    let chunk_options = index_chunk_options(
        ctx.embedding_model.info().max_input_tokens,
        owned_text.as_deref(),
    );
    let extracted = extract_for_indexing(&source, &chunk_options)?;
    throw_if_index_cancelled(ctx)?;
    let max_chars = chunk_options.max_chunk_chars();
    let fragments: Vec<PreparedFragment> = extracted
        .into_iter()
        .filter(|item| {
            model_accepts_content(ctx.embedding_model.as_ref(), &item.fragment.entity.content)
        })
        .map(|item| {
            let embedding_content = vector_content_for_fragment(
                &item.fragment,
                item.embedding_source.as_ref(),
                Some(max_chars),
            );
            PreparedFragment {
                fragment: item.fragment,
                embedding_content,
            }
        })
        .collect();
    Ok(PreparedFile {
        file: file.clone(),
        fragments,
    })
}

fn image_format_of(file: &FileInfo) -> EngineResult<ImageFormat> {
    match file.format.as_str() {
        "png" => Ok(ImageFormat::Png),
        "jpg" | "jpeg" => Ok(ImageFormat::Jpeg),
        "webp" => Ok(ImageFormat::Webp),
        "gif" => Ok(ImageFormat::Gif),
        other => Err(EngineError::new(
            EngineErrorCode::from_static("INDEXING.READ_SOURCE_FAILED"),
            "indexing found an unsupported image format",
        )
        .with_context(format!("{}\nformat={other}", file_context(file)))),
    }
}

fn model_accepts_content(model: &dyn EmbeddingModel, content: &Content) -> bool {
    let kinds = &model.info().input_kinds;
    match content {
        Content::Text { .. } => kinds.contains(&EmbeddingInputKind::Text),
        Content::Image { .. } => kinds.contains(&EmbeddingInputKind::Image),
    }
}

fn commit_file(
    ctx: &mut IndexContext<'_>,
    prepared: &PreparedFile,
    vectors: &[Vec<f32>],
    truncated_fragment_count: usize,
    stats: &Arc<Mutex<IndexStats>>,
) -> EngineResult<bool> {
    throw_if_index_cancelled(ctx)?;
    if !vectors.is_empty() && prepared.fragments.len() != vectors.len() {
        let error = EngineError::new(
            EngineErrorCode::from_static("STORAGE.ENTITY_VECTOR_COUNT_MISMATCH"),
            "embedding returned mismatched entity/vector counts",
        )
        .with_context(format!(
            "fileId={} fragmentCount={} vectorCount={}",
            prepared.file.id.as_str(),
            prepared.fragments.len(),
            vectors.len()
        ));
        let reason = mark_file_failed(ctx.storage, &prepared.file, &error, "commit");
        record_file_failed(&mut lock_stats_mut(stats), &prepared.file, Some(reason));
        return Ok(false);
    }
    let file_vectors: Vec<IndexedFragment> = prepared
        .fragments
        .iter()
        .zip(vectors.iter())
        .map(|(fragment, vector)| IndexedFragment {
            fragment: fragment.fragment.clone(),
            vector: vector.clone(),
        })
        .collect();
    commit_vectors(
        ctx,
        prepared,
        &file_vectors,
        truncated_fragment_count,
        stats,
    )
}

fn commit_vectors(
    ctx: &mut IndexContext<'_>,
    prepared: &PreparedFile,
    file_vectors: &[IndexedFragment],
    truncated_fragment_count: usize,
    stats: &Arc<Mutex<IndexStats>>,
) -> EngineResult<bool> {
    throw_if_index_cancelled(ctx)?;
    match ctx.storage.replace_file(
        &prepared.file,
        file_vectors,
        Some(&FileIndexDiagnostics {
            truncated_fragment_count: Some(truncated_fragment_count),
        }),
    ) {
        Ok(()) => {
            {
                let mut guard = lock_stats_mut(stats);
                guard.files_indexed += 1;
                guard.entities_created += count_public_entities(
                    &prepared
                        .fragments
                        .iter()
                        .map(|fragment| fragment.fragment.clone())
                        .collect::<Vec<_>>(),
                );
            }
            Ok(true)
        }
        Err(error) => {
            if is_cancelled_error(&error) {
                return Err(error);
            }
            let reason = mark_file_failed(ctx.storage, &prepared.file, &error, "commit");
            record_file_failed(&mut lock_stats_mut(stats), &prepared.file, Some(reason));
            Ok(false)
        }
    }
}

fn record_file_failed(stats: &mut IndexStats, file: &FileInfo, reason: Option<String>) {
    stats.files_failed += 1;
    stats.failed_files.push(file.relative_path.clone());
    if let Some(reason) = reason {
        stats
            .failed_file_reasons
            .push(format!("{}: {reason}", file.relative_path));
    }
}

fn throw_if_index_cancelled(ctx: &IndexContext<'_>) -> EngineResult<()> {
    if ctx.cancel.as_ref().is_some_and(CancelFlag::is_cancelled) {
        return Err(EngineError::new(
            EngineErrorCode::from_static("INDEXING.CANCELLED"),
            "indexing was cancelled",
        )
        .with_context(workspace_index_context(&ctx.workspace_index)));
    }
    Ok(())
}

fn is_cancelled_error(error: &EngineError) -> bool {
    error.code().suffix() == "INDEXING.CANCELLED"
}

fn mark_file_failed(
    storage: &mut dyn WorkspaceIndexStorage,
    file: &FileInfo,
    error: &EngineError,
    stage: &str,
) -> String {
    let reason = file_failure_reason(stage, error);
    if storage.mark_file_failed(file, &reason).is_err() {
        return file_failure_reason(stage, error);
    }
    reason
}

fn file_failure_reason(stage: &str, error: &EngineError) -> String {
    one_line(&format!("{stage}: {}", error_to_message(error)))
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn error_to_message(error: &EngineError) -> String {
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

fn file_context(file: &FileInfo) -> String {
    format!("fileId={} path={}", file.id.as_str(), file.relative_path)
}

fn workspace_index_context(index: &WorkspaceIndexInfo) -> String {
    error_details(vec![
        DetailEntry::Line(&workspace_index_detail(&index.name)),
        DetailEntry::Pair("workspace_index_id", DetailValue::Str(&index.id)),
    ])
    .unwrap_or_default()
}

fn summarize_failed_files(files: &[String]) -> String {
    const SHOWN: usize = 5;
    if files.len() > SHOWN {
        format!(
            "{} and {} more",
            files[..SHOWN].join(", "),
            files.len() - SHOWN
        )
    } else {
        files.join(", ")
    }
}

fn describe_prepared_files(files: &[PreparedFile]) -> String {
    if files.is_empty() {
        return "0 files".to_owned();
    }
    if files.len() == 1 {
        return files[0].file.relative_path.clone();
    }
    format!(
        "{} files, starting with {}",
        files.len(),
        files[0].file.relative_path
    )
}

fn finished_file_detail(succeeded: bool, relative_path: &str) -> String {
    if succeeded {
        format!("indexed {relative_path}")
    } else {
        format!("failed {relative_path}")
    }
}

fn count_public_entities(fragments: &[EntityFragment]) -> usize {
    fragments
        .iter()
        .filter(|fragment| {
            fragment
                .group
                .as_deref()
                .is_none_or(|group| group == fragment.entity.id.as_str())
        })
        .count()
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
        match embed_fragment_batch(
            &fragments[start..end],
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
                    vectors[start + offset] = vector;
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

fn embed_contents_with_retry(
    contents: &[Content],
    model: &dyn EmbeddingModel,
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<ModelLoadSink>,
    on_terminal_failure: Option<&AtomicBool>,
) -> EngineResult<EmbeddingResult> {
    let _ = on_model_progress;
    let mut attempt: u32 = 0;
    loop {
        throw_if_aborted(abort, cancel)?;
        let outcome = scheduler.run(abort, cancel, |abort, cancel| {
            throw_if_aborted(abort, cancel)?;
            let inputs: Vec<EmbeddingInput<'_>> = contents.iter().map(content_to_input).collect();
            model.embed(EmbeddingPurpose::Document, &inputs)
        });
        match outcome {
            Ok(result) => {
                scheduler.record_success();
                return Ok(result);
            }
            Err(error) => {
                if is_cancelled_or_aborted(&error) {
                    return Err(error);
                }
                let retry = classify_embedding_retry(&error, model);
                let delay_ms = retry_delay_ms(attempt, &retry);
                if retry.retryable {
                    scheduler.record_retryable_failure(EmbeddingRetryDecision {
                        rate_limited: retry.rate_limited,
                        delay_ms,
                    });
                }
                if retry.fail_fast && (!retry.retryable || attempt >= max_retry_attempts(&retry)) {
                    if let Some(flag) = on_terminal_failure {
                        flag.store(true, Ordering::Relaxed);
                    }
                }
                if attempt >= max_retry_attempts(&retry) || !retry.retryable {
                    return Err(error);
                }
                abortable_sleep(delay_ms, abort, cancel)?;
                attempt += 1;
            }
        }
    }
}

fn content_to_input(content: &Content) -> EmbeddingInput<'_> {
    match content {
        Content::Text { text } => EmbeddingInput::Text { text },
        Content::Image { data, format } => EmbeddingInput::Image {
            data,
            format: *format,
        },
    }
}

fn throw_if_aborted(abort: &AtomicBool, cancel: Option<&CancelFlag>) -> EngineResult<()> {
    if abort.load(Ordering::Relaxed) || cancel.is_some_and(CancelFlag::is_cancelled) {
        return Err(EngineError::new(
            EngineErrorCode::from_static("INDEXING.CANCELLED"),
            "embedding was cancelled",
        ));
    }
    Ok(())
}

fn is_cancelled_or_aborted(error: &EngineError) -> bool {
    error.code().suffix() == "INDEXING.CANCELLED"
}

fn abortable_sleep(ms: u64, abort: &AtomicBool, cancel: Option<&CancelFlag>) -> EngineResult<()> {
    if ms == 0 {
        return throw_if_aborted(abort, cancel);
    }
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        throw_if_aborted(abort, cancel)?;
        if Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Adaptive embedding scheduler (sync semaphore + cooldown).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct ConcurrencyPolicy {
    pub initial: usize,
    pub min: usize,
    pub max: usize,
    pub adaptive: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct EmbeddingRetryDecision {
    pub rate_limited: bool,
    pub delay_ms: u64,
}

#[derive(Debug, Clone)]
struct RetryClassification {
    retryable: bool,
    rate_limited: bool,
    fail_fast: bool,
    retry_after_ms: Option<u64>,
}

struct SchedulerState {
    active: usize,
    current: usize,
    cooldown_until: Option<Instant>,
    retryable_failures: usize,
    success_streak: usize,
}

/// Bounded adaptive semaphore around embedding calls (mirrors
/// `AdaptiveEmbeddingScheduler`).
pub struct EmbeddingScheduler {
    policy: ConcurrencyPolicy,
    state: Mutex<SchedulerState>,
    cvar: Condvar,
}

impl EmbeddingScheduler {
    pub fn new(policy: ConcurrencyPolicy) -> Self {
        let initial = policy.initial;
        Self {
            policy,
            state: Mutex::new(SchedulerState {
                active: 0,
                current: initial,
                cooldown_until: None,
                retryable_failures: 0,
                success_streak: 0,
            }),
            cvar: Condvar::new(),
        }
    }

    pub fn policy(&self) -> ConcurrencyPolicy {
        self.policy
    }

    pub fn task_concurrency(&self) -> usize {
        self.policy.max
    }

    pub fn run<T>(
        &self,
        abort: &AtomicBool,
        cancel: Option<&CancelFlag>,
        task: impl FnOnce(&AtomicBool, Option<&CancelFlag>) -> EngineResult<T>,
    ) -> EngineResult<T> {
        throw_if_aborted(abort, cancel)?;
        self.wait_for_cooldown(abort, cancel)?;
        self.acquire(abort, cancel)?;
        self.wait_for_cooldown(abort, cancel)?;
        let result = task(abort, cancel);
        self.release();
        result
    }

    pub fn record_success(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !self.policy.adaptive || state.current >= self.policy.max {
            return;
        }
        state.success_streak += 1;
        if state.success_streak < EMBEDDING_SUCCESS_STREAK_MIN.max(state.current * 2) {
            return;
        }
        state.current += 1;
        state.success_streak = 0;
        self.cvar.notify_all();
    }

    pub fn record_retryable_failure(&self, retry: EmbeddingRetryDecision) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.retryable_failures += 1;
        if retry.rate_limited && retry.delay_ms > 0 {
            let until = Instant::now() + Duration::from_millis(retry.delay_ms);
            state.cooldown_until = Some(
                state
                    .cooldown_until
                    .map_or(until, |current| current.max(until)),
            );
        }
        if !self.policy.adaptive {
            return;
        }
        state.current = self.policy.min.max(state.current / 2);
        state.success_streak = 0;
    }

    pub fn snapshot(&self) -> IndexEmbeddingProgress {
        let (current, max, retryable_failures) = match self.state.lock() {
            Ok(state) => (state.current, self.policy.max, state.retryable_failures),
            Err(_) => (self.policy.initial, self.policy.max, 0),
        };
        IndexEmbeddingProgress {
            concurrency: Some(current),
            max_concurrency: Some(max),
            retryable_failures: Some(retryable_failures),
            ..IndexEmbeddingProgress::default()
        }
    }

    fn wait_for_cooldown(
        &self,
        abort: &AtomicBool,
        cancel: Option<&CancelFlag>,
    ) -> EngineResult<()> {
        loop {
            throw_if_aborted(abort, cancel)?;
            let remaining = match self.state.lock() {
                Ok(state) => state
                    .cooldown_until
                    .map(|until| until.saturating_duration_since(Instant::now())),
                Err(_) => None,
            };
            match remaining {
                Some(duration) if !duration.is_zero() => {
                    std::thread::sleep(duration.min(Duration::from_millis(50)));
                }
                _ => return Ok(()),
            }
        }
    }

    fn acquire(&self, abort: &AtomicBool, cancel: Option<&CancelFlag>) -> EngineResult<()> {
        let mut state = self.state.lock().map_err(|_| {
            EngineError::new(
                EngineErrorCode::from_static("INDEXING.SCHEDULER_FAILED"),
                "embedding scheduler lock failed",
            )
        })?;
        loop {
            throw_if_aborted(abort, cancel)?;
            if state.active < state.current {
                state.active += 1;
                return Ok(());
            }
            state = self
                .cvar
                .wait_timeout(state, Duration::from_millis(50))
                .map_err(|_| {
                    EngineError::new(
                        EngineErrorCode::from_static("INDEXING.SCHEDULER_FAILED"),
                        "embedding scheduler lock failed",
                    )
                })?
                .0;
        }
    }

    fn release(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = state.active.saturating_sub(1);
        }
        self.cvar.notify_all();
    }
}

fn resolve_embedding_concurrency_policy(
    requested: Option<usize>,
    model: &dyn EmbeddingModel,
) -> ConcurrencyPolicy {
    if let Some(requested) = requested.filter(|value| *value > 0) {
        return ConcurrencyPolicy {
            initial: requested,
            min: 1,
            max: requested,
            adaptive: requested > 1,
        };
    }
    let info = model.info();
    let remote = info.provider != "local";
    let multimodal = info.supports_images;
    let local_default = info
        .default_concurrency
        .filter(|value| *value > 0)
        .unwrap_or(1);
    let initial = if remote {
        if multimodal { 4 } else { 8 }
    } else {
        local_default
    };
    let max = if remote {
        if multimodal { 8 } else { 12 }
    } else {
        local_default
    };
    let min = initial.min(4);
    ConcurrencyPolicy {
        initial,
        min,
        max: initial.max(max),
        adaptive: max > 1,
    }
}

fn should_fail_fast_embedding_error(error: &EngineError, model: &dyn EmbeddingModel) -> bool {
    classify_embedding_retry(error, model).fail_fast
}

fn classify_embedding_retry(
    error: &EngineError,
    model: &dyn EmbeddingModel,
) -> RetryClassification {
    let code = error.code().qualified();
    let text = format!(
        "{} {} {}",
        code,
        error.message(),
        error.context().unwrap_or("")
    );
    let status = http_status_from_text(&text);
    let remote = model.info().provider != "local";
    let rate_limited = remote
        && (status == Some(429)
            || contains_insensitive(&text, "rate limit")
            || contains_insensitive(&text, "quota exceeded")
            || contains_insensitive(&text, "too many requests")
            || contains_insensitive(&text, "request rate increased too quickly"));
    let server_error = remote && status.is_some_and(|status| (500..600).contains(&status));
    let request_timeout = remote && status == Some(408);
    let request_failure = code.ends_with("_REQUEST_FAILED");
    let transient_network = remote && request_failure && is_transient_network_failure(&text);
    let shared_local_failure = matches!(
        code.as_str(),
        "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_DOWNLOAD_FAILED"
            | "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_LOAD_FAILED"
            | "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_DISPOSED"
    );
    let retryable = !shared_local_failure
        && (rate_limited || server_error || request_timeout || transient_network);
    let remote_configuration_failure = remote
        && (code.ends_with("_MISSING_API_KEY")
            || code.ends_with("_MISSING_ENDPOINT")
            || status == Some(401)
            || status == Some(403)
            || status == Some(404)
            || contains_insensitive(&text, "api key"));
    let permanent_remote = remote
        && (code == "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_DIMENSION_MISMATCH"
            || (status == Some(400) && is_permanent_remote_model_bad_request(&text)));
    RetryClassification {
        retryable,
        rate_limited,
        fail_fast: retryable
            || shared_local_failure
            || remote_configuration_failure
            || permanent_remote
            || (remote && request_failure),
        retry_after_ms: retry_after_ms_from_text(&text),
    }
}

fn is_permanent_remote_model_bad_request(text: &str) -> bool {
    if let Some(code) = find_key_value(text, "providerCode=") {
        let normalized: String = code
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let normalized = normalized.trim_matches('_');
        if PERMANENT_REMOTE_MODEL_PROVIDER_CODES.contains(&normalized) {
            return true;
        }
    }
    match find_provider_message(text) {
        Some(message) => {
            let lower = message.to_lowercase();
            (contains_word(&lower, "invalid")
                || contains_word(&lower, "unsupported")
                || contains_word(&lower, "unknown"))
                && contains_word(&lower, "model")
        }
        None => false,
    }
}

fn find_key_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let position = text.to_lowercase().find(&key.to_lowercase())?;
    text[position + key.len()..].split_whitespace().next()
}

fn find_provider_message(text: &str) -> Option<String> {
    let key = "providermessage=";
    let position = text.to_lowercase().find(key)?;
    let start = position + key.len();
    let end = text.to_lowercase()[start..]
        .find("zvec_grep.")
        .map(|offset| start + offset)
        .unwrap_or(text.len());
    Some(text[start..end].to_owned())
}

fn contains_word(haystack: &str, word: &str) -> bool {
    haystack
        .split(|ch: char| !ch.is_alphanumeric())
        .any(|part| part == word)
}

fn contains_insensitive(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn is_transient_network_failure(text: &str) -> bool {
    const MARKERS: &[&str] = &[
        "EAI_AGAIN",
        "ECONNREFUSED",
        "ECONNRESET",
        "EHOSTUNREACH",
        "ENETDOWN",
        "ENETUNREACH",
        "ENOTFOUND",
        "ETIMEDOUT",
        "UND_ERR_CONNECT_TIMEOUT",
        "UND_ERR_HEADERS_TIMEOUT",
        "UND_ERR_SOCKET",
        "TimeoutError",
        "socket hang up",
        "temporary failure",
    ];
    MARKERS
        .iter()
        .any(|marker| contains_insensitive(text, marker))
        || contains_insensitive(text, "connection reset")
        || contains_insensitive(text, "connection timed out")
        || contains_insensitive(text, "network connection failed")
        || contains_insensitive(text, "network connection was lost")
}

fn max_retry_attempts(retry: &RetryClassification) -> u32 {
    if retry.rate_limited {
        EMBEDDING_RATE_LIMIT_MAX_RETRIES
    } else {
        EMBEDDING_TRANSIENT_MAX_RETRIES
    }
}

fn retry_delay_ms(attempt: u32, retry: &RetryClassification) -> u64 {
    if let Some(retry_after) = retry.retry_after_ms {
        return retry_after;
    }
    let (base, max) = if retry.rate_limited {
        (
            EMBEDDING_RATE_LIMIT_RETRY_BASE_DELAY_MS,
            EMBEDDING_RATE_LIMIT_RETRY_MAX_DELAY_MS,
        )
    } else {
        (
            EMBEDDING_TRANSIENT_RETRY_BASE_DELAY_MS,
            EMBEDDING_TRANSIENT_RETRY_MAX_DELAY_MS,
        )
    };
    let exponential = base.saturating_mul(1u64 << attempt.min(20));
    exponential.saturating_add(jitter_ms()).min(max)
}

fn jitter_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| u64::from(duration.subsec_nanos()) % (EMBEDDING_RETRY_JITTER_MS + 1))
        .unwrap_or(0)
}

fn http_status_from_text(text: &str) -> Option<u16> {
    find_key_value(text, "status=").and_then(|value| value.parse().ok())
}

fn retry_after_ms_from_text(text: &str) -> Option<u64> {
    if let Some(ms) = find_key_value(text, "retryAfterMs=") {
        if let Ok(value) = ms.parse() {
            return Some(value);
        }
    }
    find_key_value(text, "retryAfter=")
        .and_then(|value| value.parse::<f64>().ok())
        .map(|seconds| (seconds * 1000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diff_of_equal_sets() {
        let diff = compute_diff_from_files(&[], &[]).expect("diff");
        assert!(diff.added.is_empty() && diff.deleted.is_empty());
    }

    #[test]
    fn scheduler_serializes_permits() {
        let scheduler = EmbeddingScheduler::new(ConcurrencyPolicy {
            initial: 1,
            min: 1,
            max: 1,
            adaptive: false,
        });
        let abort = AtomicBool::new(false);
        let abort_ref = &abort;
        let scheduler_ref = &scheduler;
        let counter = Arc::new(Mutex::new(0usize));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let counter = Arc::clone(&counter);
                scope.spawn(move || {
                    scheduler_ref
                        .run(abort_ref, None, |_, _| {
                            *counter
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
                            Ok::<_, EngineError>(())
                        })
                        .expect("run");
                });
            }
        });
        assert_eq!(
            *counter
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            4
        );
    }

    #[test]
    fn transient_retryable_then_gives_up() {
        struct Failing;
        impl EmbeddingModel for Failing {
            fn info(&self) -> &crate::models::EmbeddingModelInfo {
                use std::sync::OnceLock;
                static INFO: OnceLock<crate::models::EmbeddingModelInfo> = OnceLock::new();
                INFO.get_or_init(|| crate::models::EmbeddingModelInfo {
                    reference: "test/failing".to_owned(),
                    provider: "qwen".to_owned(),
                    model: "test-model".to_owned(),
                    dimension: 4,
                    metric: crate::types::SearchMetric::Cosine,
                    supports_images: false,
                    max_input_tokens: None,
                    input_kinds: vec![EmbeddingInputKind::Text],
                    endpoint: None,
                    default_concurrency: None,
                })
            }
            fn max_batch_size(&self) -> usize {
                8
            }
            fn embed(
                &self,
                _purpose: EmbeddingPurpose,
                _inputs: &[EmbeddingInput<'_>],
            ) -> EngineResult<EmbeddingResult> {
                Err(EngineError::new(
                    EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_REQUEST_FAILED"),
                    "request failed",
                )
                .with_context("status=503"))
            }
        }
        let model: Arc<dyn EmbeddingModel> = Arc::new(Failing);
        let scheduler =
            EmbeddingScheduler::new(resolve_embedding_concurrency_policy(None, &*model));
        let abort = AtomicBool::new(false);
        let contents = vec![Content::Text {
            text: "hello".to_owned(),
        }];
        let result =
            embed_contents_with_retry(&contents, &*model, &scheduler, &abort, None, None, None);
        assert!(result.is_err());
        // 1 initial + 3 retries for transient failures.
        assert!(scheduler.snapshot().retryable_failures.unwrap_or(0) >= 3);
    }
}
