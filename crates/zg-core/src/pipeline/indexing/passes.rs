//! Index passes: workspace/paths entry points, scan-diff-embed orchestration, result building.

use std::collections::HashSet;
use std::time::Instant;

use crate::error::{
    DetailEntry, DetailValue, EngineError, EngineErrorCode, EngineResult, error_details,
    workspace_index_detail,
};
use crate::types::{
    FileInfo, FileScanDiagnostics, IndexProgress, IndexProgressPhase, IndexResult,
    WorkspaceIndexInfo, WorkspaceIndexStatus,
};
use crate::utils::timing::TimingCollector;

use super::context::{
    IndexContext, IndexPassResult, IndexStats, MAX_SKIPPED_FILE_SAMPLES, ProgressBase,
    error_to_message, file_context, summarize_failed_files, throw_if_index_cancelled,
    workspace_index_context,
};
use super::diff::{compute_diff_from_files, normalize_for_diff};
use super::embed::index_files;
use super::progress::{
    report, report_index_finalizing, report_indexing, report_scanning, retry_progress_base,
};
use super::scanner::{
    CancelFlag, ScanOptions, create_scan_diagnostics, scan_directory_path, scan_file_path,
    scan_root_paths,
};

/// Index the whole workspace (mirrors `indexWorkspace`).
///
/// # Errors
///
/// Returns `INDEXING.WORKSPACE_FAILED` when the workspace scan, embedding, or result
/// build fails.
pub fn index_workspace(ctx: &mut IndexContext<'_>) -> EngineResult<IndexResult> {
    index_workspace_inner(ctx).map_err(|error| {
        let context = workspace_index_context(&ctx.workspace_index);
        EngineError::new(
            EngineErrorCode::IndexingWorkspaceFailed,
            "indexing workspace failed",
        )
        .with_context(format!("{context}\ncause={}", error_to_message(&error)))
    })
}

/// Index explicit changed paths (mirrors `indexWorkspacePaths`).
///
/// # Errors
///
/// Returns `INDEXING.WORKSPACE_FAILED` when scanning the changed paths, embedding, or
/// result build fails.
pub fn index_workspace_paths(
    ctx: &mut IndexContext<'_>,
    changed_paths: &[String],
) -> EngineResult<IndexResult> {
    index_workspace_paths_inner(ctx, changed_paths).map_err(|error| {
        let context = workspace_index_context(&ctx.workspace_index);
        EngineError::new(
            EngineErrorCode::IndexingWorkspaceFailed,
            "indexing changed paths failed",
        )
        .with_context(format!("{context}\ncause={}", error_to_message(&error)))
    })
}

/// Inspect status without writing (mirrors `getWorkspaceIndexStatus`).
///
/// # Errors
///
/// Returns an error when root-path scanning or stored-file diffing fails, or
/// `INDEXING.CANCELLED` when cancelled.
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
            EngineErrorCode::IndexingStatusFailed,
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
    let first_failed = passes.first().map_or(0, |pass| pass.stats.files_failed);
    if first_failed > 0 {
        let failed = first_failed;
        progress_base = passes.first().map(retry_progress_base);
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
    if passes
        .first()
        .is_some_and(|pass| pass.stats.files_failed > 0)
    {
        progress_base = passes.first().map(retry_progress_base);
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
        EngineErrorCode::IndexingFilesFailed,
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
                    EngineErrorCode::IndexingDeleteFileFailed,
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

fn build_index_result(
    ctx: &IndexContext<'_>,
    passes: &[IndexPassResult],
    duration_ms: u64,
    timings: &mut TimingCollector,
) -> IndexResult {
    let _ = ctx;
    let Some((first, retries)) = passes.split_first() else {
        return IndexResult::default();
    };
    let final_pass = retries.last().unwrap_or(first);
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
            EngineErrorCode::IndexingOptimizeFailed,
            "indexing failed to finalize storage",
        )
        .with_context(format!(
            "{}\ncause={}",
            workspace_index_context(&ctx.workspace_index),
            error_to_message(&error)
        ))
    })
}
