//! Indexing progress: green bar plus stderr reporter (`format/progress.ts`).
//!
//! [`format_green_progress_bar`] is a pure builder covered by goldens;
//! [`ProgressReporter`] owns the TTY/throttled stderr policy.

use super::color::{GREEN, RESET};

/// Renders a green progress bar, mirroring `formatGreenProgressBar`.
#[must_use]
pub fn format_green_progress_bar(
    completed: usize,
    total: usize,
    width: usize,
    color: bool,
) -> String {
    if total == 0 || width == 0 {
        return String::new();
    }
    let ratio = (completed.min(total) as f64) / (total as f64);
    let filled = (ratio * width as f64).round() as usize;
    let bar: String = std::iter::repeat_n('█', filled.min(width)).collect::<String>()
        + &std::iter::repeat_n('░', width.saturating_sub(filled)).collect::<String>();
    let percent = (ratio * 100.0).round() as usize;
    if color {
        format!("{GREEN}{bar}{RESET} {percent}% ({completed}/{total})")
    } else {
        format!("{bar} {percent}% ({completed}/{total})")
    }
}

/// Minimal stderr progress reporter: TTY bar plus throttled plain lines.
pub struct ProgressReporter {
    color: bool,
    last_line: std::time::Instant,
}

impl ProgressReporter {
    /// Builds a reporter; color follows the CLI color selection.
    pub fn new(color: bool) -> Self {
        Self {
            color,
            last_line: std::time::Instant::now() - std::time::Duration::from_secs(60),
        }
    }

    /// Reports one progress event.
    pub fn report(&mut self, progress: &zg_core::types::IndexProgress) {
        let (done, total) = (
            progress.files_indexed.unwrap_or(0),
            progress.files_total.unwrap_or(0),
        );
        if total == 0 {
            return;
        }
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            eprint!(
                "\r\x1b[2K{}",
                format_green_progress_bar(done, total, 20, self.color)
            );
        } else if self.last_line.elapsed() >= std::time::Duration::from_secs(15) {
            eprintln!(
                "indexing: {done}/{total} files{}",
                progress
                    .detail
                    .as_deref()
                    .map_or_else(String::new, |detail| format!(" ({detail})"))
            );
            self.last_line = std::time::Instant::now();
        }
    }

    /// Clears the TTY line (no-op off-TTY).
    pub fn finish(&self) {
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            eprint!("\r\x1b[2K");
        }
    }
}
