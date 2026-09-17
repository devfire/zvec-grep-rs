//! Indexing progress: green bar plus stderr reporter (`format/progress.ts`).
//!
//! [`format_green_progress_bar`] is a pure builder covered by goldens;
//! [`format_progress_line`] renders every `IndexProgress` phase (scan,
//! download, indexing) so long model downloads never sit silent, and
//! [`ProgressReporter`] owns the TTY/throttled stderr policy.

use std::io::Write as _;
use std::time::{Duration, Instant};

use super::color::{GREEN, RESET};

/// Width of the TTY file bar.
const BAR_WIDTH: usize = 20;
/// Minimum gap between TTY repaints; progress callbacks fire per file.
const TTY_THROTTLE: Duration = Duration::from_millis(120);
/// Minimum gap between non-TTY log lines; stderr may be a file/pipe.
const LINE_THROTTLE: Duration = Duration::from_secs(2);
/// Spinner frames for unknown-total phases (scan, model download).
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// Longest `detail` (usually a file path) kept on one TTY line.
const MAX_DETAIL_CHARS: usize = 60;

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

/// Short phase label for progress lines.
fn phase_label(progress: &zg_core::types::IndexProgress) -> &'static str {
    match progress.phase {
        Some(zg_core::types::IndexProgressPhase::Scanning) => "scanning",
        Some(zg_core::types::IndexProgressPhase::Indexing) => "indexing",
        Some(zg_core::types::IndexProgressPhase::Done) => "done",
        None => "indexing",
    }
}

/// Truncates `detail` to [`MAX_DETAIL_CHARS`] on a char boundary.
fn short_detail(detail: Option<&str>) -> Option<String> {
    let detail = detail?;
    if detail.chars().count() <= MAX_DETAIL_CHARS {
        return Some(detail.to_owned());
    }
    let truncated: String = detail
        .chars()
        .take(MAX_DETAIL_CHARS.saturating_sub(1))
        .collect();
    Some(format!("{truncated}…"))
}

/// `12.3 MB` for embedding download counters.
fn format_megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Pure line builder for every progress shape; covered by goldens.
///
/// `tick` advances the spinner for unknown-total phases.
#[must_use]
pub fn format_progress_line(
    progress: &zg_core::types::IndexProgress,
    color: bool,
    tick: usize,
) -> String {
    let phase = phase_label(progress);
    let detail = short_detail(progress.detail.as_deref());
    let total = progress.files_total.unwrap_or(0);
    let done = progress.files_indexed.unwrap_or(0);
    if total > 0 {
        let mut line = format!(
            "{} {phase}",
            format_green_progress_bar(done, total, BAR_WIDTH, color)
        );
        if let Some(embedding) = progress.embedding.as_ref()
            && let (Some(downloaded), Some(total_bytes)) =
                (embedding.downloaded_bytes, embedding.total_bytes)
            && total_bytes > 0
        {
            let percent =
                (downloaded.min(total_bytes) as f64 / total_bytes as f64 * 100.0).round() as usize;
            line.push_str(&format!(
                " downloading {} / {} ({percent}%)",
                format_megabytes(downloaded),
                format_megabytes(total_bytes)
            ));
        }
        if let Some(detail) = detail {
            line.push(' ');
            line.push_str(&detail);
        }
        return line;
    }
    let spinner = SPINNER
        .get(tick % SPINNER.len())
        .copied()
        .unwrap_or('\u{00B7}');
    let spinner = if color {
        format!("{GREEN}{spinner}{RESET}")
    } else {
        spinner.to_string()
    };
    let mut line = format!("{spinner} {phase}");
    if done > 0 {
        line.push_str(&format!(" {done} files"));
    }
    if let Some(embedding) = progress.embedding.as_ref()
        && let (Some(downloaded), Some(total_bytes)) =
            (embedding.downloaded_bytes, embedding.total_bytes)
        && total_bytes > 0
    {
        let percent =
            (downloaded.min(total_bytes) as f64 / total_bytes as f64 * 100.0).round() as usize;
        line.push_str(&format!(
            " downloading {} / {} ({percent}%)",
            format_megabytes(downloaded),
            format_megabytes(total_bytes)
        ));
    }
    if let Some(detail) = detail {
        line.push(' ');
        line.push_str(&detail);
    }
    line
}

/// Minimal stderr progress reporter: TTY bar plus throttled plain lines.
pub struct ProgressReporter {
    color: bool,
    enabled: bool,
    tty: bool,
    tick: usize,
    last_tty: Instant,
    last_line: Instant,
}

impl ProgressReporter {
    /// Builds a reporter; color follows the CLI color selection and
    /// `enabled` follows `--no-progress`/`--quiet`.
    #[must_use]
    pub fn new(color: bool, enabled: bool) -> Self {
        Self {
            color,
            enabled,
            tty: std::io::IsTerminal::is_terminal(&std::io::stderr()),
            tick: 0,
            last_tty: Instant::now() - Duration::from_secs(60),
            last_line: Instant::now() - Duration::from_secs(60),
        }
    }

    /// Reports one progress event.
    pub fn report(&mut self, progress: &zg_core::types::IndexProgress) {
        if !self.enabled {
            return;
        }
        let done_phase = matches!(
            progress.phase,
            Some(zg_core::types::IndexProgressPhase::Done)
        );
        if self.tty {
            if !done_phase && self.last_tty.elapsed() < TTY_THROTTLE {
                self.tick = self.tick.wrapping_add(1);
                return;
            }
            let line = format_progress_line(progress, self.color, self.tick);
            self.tick = self.tick.wrapping_add(1);
            eprint!("\r\x1b[2K{line}");
            let _ = std::io::stderr().flush();
            self.last_tty = Instant::now();
        } else {
            self.tick = self.tick.wrapping_add(1);
            if !done_phase && self.last_line.elapsed() < LINE_THROTTLE {
                return;
            }
            eprintln!("{}", format_progress_line(progress, false, self.tick));
            self.last_line = Instant::now();
        }
    }

    /// Clears the TTY line (no-op off-TTY or when disabled).
    pub fn finish(&self) {
        if self.enabled && self.tty {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        }
    }
}
