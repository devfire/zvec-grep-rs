//! Index result rendering (`format/status.ts` `printIndexResult`).
//!
//! The pure [`format_index_result`] builder is covered by goldens; the
//! `print_*` wrappers own stdout/stderr.

use zg_core::types::IndexResult;

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

/// Prints the no-indexable-files tip.
pub fn print_no_indexable_files_tip() {
    eprintln!(
        "No indexable files found. Adjust --glob/--type filters or check --hidden/--no-ignore."
    );
}
