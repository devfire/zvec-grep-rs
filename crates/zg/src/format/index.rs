//! Index result rendering (`format/status.ts` `printIndexResult`).
//!
//! The pure [`format_index_result`] builder is covered by goldens; the
//! `print_*` wrappers own stdout/stderr.

use zg_core::types::{IndexResult, ZERO_FILES_INDEXED_WARNING};

/// Renders an index result, mirroring `printIndexResult` counters.
#[must_use]
pub fn format_index_result(label: &str, result: &IndexResult) -> String {
    let mut lines = vec![format!(
        "{label}: {} scanned, {} added, {} modified, {} unchanged, {} failed, {} entities in {}ms",
        result.files_scanned,
        result.files_added,
        result.files_modified,
        result.files_unchanged,
        result.files_failed,
        result.entities_created,
        result.duration_ms
    )];
    if result.files_deleted > 0 {
        lines.push(format!("deleted: {}", result.files_deleted));
    }
    if result.files_pending > 0 {
        lines.push(format!("pending: {}", result.files_pending));
    }
    lines.join("\n")
}

/// Prints an index result.
pub fn print_index_result(label: &str, result: &IndexResult) {
    println!("{}", format_index_result(label, result));
}

/// Warning for a completed build that indexed zero files, or `None`
/// when at least one file was scanned (a full build scans every file it
/// indexes, so zero scanned means zero indexed). Without this, an empty
/// build looks identical to a successful one and the empty result only
/// surfaces later as fruitless lookups.
#[must_use]
pub fn zero_files_indexed_warning(result: &IndexResult) -> Option<&'static str> {
    if result.files_scanned == 0 {
        Some(ZERO_FILES_INDEXED_WARNING)
    } else {
        None
    }
}

/// Prints the zero-files-indexed warning, when the result warrants one.
pub fn print_zero_files_indexed_warning(result: &IndexResult) {
    if let Some(warning) = zero_files_indexed_warning(result) {
        eprintln!("warning: {warning}");
    }
}
