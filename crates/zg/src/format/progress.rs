//! Indexing progress: green bar plus stderr reporter (`format/progress.ts`).
//!
//! [`format_green_progress_bar`] is a pure builder covered by goldens;
//! [`format_progress_line`] renders every `IndexProgress` phase (scan,
//! download, indexing) so long model downloads never sit silent, and
//! [`ProgressReporter`] owns the TTY/throttled stderr policy and clamps
//! TTY repaints to the live stderr width.

use std::io::Write as _;
use std::time::{Duration, Instant};

use super::color::{GREEN, RESET, use_color_stderr};
use crate::cli::ColorMode;
use unicode_width::UnicodeWidthChar;

/// Width of the TTY file bar.
const BAR_WIDTH: usize = 20;
/// Bar floor before the bar is dropped entirely on narrow terminals.
const MIN_BAR_WIDTH: usize = 4;
/// Fallback line width when the stderr terminal width is unknown.
const FALLBACK_TTY_WIDTH: usize = 80;
/// Narrowest honored terminal width; anything below falls back (guards
/// against bogus ioctl/`COLUMNS` values and degenerate truncation).
const MIN_TTY_WIDTH: usize = 20;
/// Minimum gap between TTY repaints; progress callbacks fire per file.
const TTY_THROTTLE: Duration = Duration::from_millis(120);
/// Minimum gap between non-TTY log lines while a model download is in
/// flight; stderr may be a file/pipe.
const LINE_THROTTLE_DOWNLOAD: Duration = Duration::from_secs(2);
/// Minimum gap between non-TTY log lines otherwise: a flat 2s emits ~300
/// lines over a 10-minute index vs ~40 before.
const LINE_THROTTLE: Duration = Duration::from_secs(10);
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
    if total == 0 {
        return String::new();
    }
    let ratio = (completed.min(total) as f64) / (total as f64);
    let percent = (ratio * 100.0).round() as usize;
    if width == 0 {
        // Narrow-terminal fallback: no glyphs, but keep the counts.
        return format!("{percent}% ({completed}/{total})");
    }
    let filled = (ratio * width as f64).round() as usize;
    let bar: String = std::iter::repeat_n('█', filled.min(width)).collect::<String>()
        + &std::iter::repeat_n('░', width.saturating_sub(filled)).collect::<String>();
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

/// Truncates `detail` to `max_chars` on a char boundary.
fn short_detail_to(detail: Option<&str>, max_chars: usize) -> Option<String> {
    let detail = detail?;
    if detail.chars().count() <= max_chars {
        return Some(detail.to_owned());
    }
    let truncated: String = detail.chars().take(max_chars.saturating_sub(1)).collect();
    Some(format!("{truncated}…"))
}

/// Live stderr width in columns: terminal size first, then `COLUMNS`,
/// then [`FALLBACK_TTY_WIDTH`]. Queried per paint so resizes apply to
/// the next frame.
#[must_use]
pub fn stderr_width() -> usize {
    if let Some((terminal_size::Width(columns), _)) =
        terminal_size::terminal_size_of(std::io::stderr())
        && usize::from(columns) >= MIN_TTY_WIDTH
    {
        return usize::from(columns);
    }
    if let Ok(columns) = std::env::var("COLUMNS")
        && let Ok(columns) = columns.parse::<usize>()
        && columns >= MIN_TTY_WIDTH
    {
        return columns;
    }
    FALLBACK_TTY_WIDTH
}

/// Visible width in terminal columns: SGR/CSI escapes contribute 0,
/// every other char contributes its Unicode width (CJK counts 2).
#[must_use]
pub fn visible_width(line: &str) -> usize {
    let mut width = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip the escape sequence through its alphabetic terminator;
            // covers the SGR colors this module emits.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        width += c.width().unwrap_or(0);
    }
    width
}

/// Hard-clamps `line` to `max_width` visible columns without splitting
/// chars, wide chars, or escape sequences; re-applies [`RESET`] when a
/// colored line is cut so no color leaks onto the terminal.
fn truncate_visible(line: &str, max_width: usize, color: bool) -> String {
    let mut clamped = String::with_capacity(line.len());
    let mut width = 0;
    let mut chars = line.chars();
    let mut truncated = false;
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            clamped.push(c);
            for c in chars.by_ref() {
                clamped.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        let char_width = c.width().unwrap_or(0);
        if width + char_width > max_width {
            truncated = true;
            break;
        }
        clamped.push(c);
        width += char_width;
    }
    if truncated && color {
        clamped.push_str(RESET);
    }
    clamped
}

/// Final non-TTY line for the `Done` phase: plain counts, no spinner.
/// Done events carry no totals themselves, so the caller passes the last
/// `files_indexed` it observed across earlier events.
#[must_use]
pub fn format_done_line(completed: usize) -> String {
    format!("done: {completed} files")
}

/// `12.3 MB` for embedding download counters.
fn format_megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Pure line builder for every progress shape; covered by goldens.
///
/// `tick` advances the spinner for unknown-total phases. Unclamped:
/// TTY callers that repaint with `\r` want [`format_progress_line_clamped`]
/// so long download counters cannot wrap and leak rows upward.
#[must_use]
pub fn format_progress_line(
    progress: &zg_core::types::IndexProgress,
    color: bool,
    tick: usize,
) -> String {
    format_progress_line_clamped(progress, color, tick, None)
}

/// Width-aware line builder: `Some(width)` guarantees at most `width`
/// visible columns by shrinking `detail` first, then the bar, then
/// hard-clamping; `None` keeps the legacy unclamped layout. Only TTY
/// repaints clamp — off-TTY log lines keep full paths because wrapped
/// `eprintln!` rows cannot corrupt a repaint.
#[must_use]
pub fn format_progress_line_clamped(
    progress: &zg_core::types::IndexProgress,
    color: bool,
    tick: usize,
    width: Option<usize>,
) -> String {
    let Some(term_width) = width else {
        return build_progress_line(progress, color, tick, BAR_WIDTH, MAX_DETAIL_CHARS);
    };
    let has_bar = progress.files_total.unwrap_or(0) > 0;
    let mut detail_budget = MAX_DETAIL_CHARS;
    let mut bar_width = BAR_WIDTH;
    let mut line = build_progress_line(progress, color, tick, bar_width, detail_budget);
    let mut overflow = visible_width(&line).saturating_sub(term_width);
    if overflow > 0
        && let Some(detail) = short_detail_to(progress.detail.as_deref(), detail_budget)
    {
        detail_budget = detail_budget.saturating_sub(detail.chars().count().min(overflow));
        line = build_progress_line(progress, color, tick, bar_width, detail_budget);
        overflow = visible_width(&line).saturating_sub(term_width);
    }
    if overflow > 0 && has_bar && bar_width > MIN_BAR_WIDTH {
        bar_width = bar_width.saturating_sub(overflow.min(bar_width.saturating_sub(MIN_BAR_WIDTH)));
        line = build_progress_line(progress, color, tick, bar_width, detail_budget);
        overflow = visible_width(&line).saturating_sub(term_width);
    }
    if overflow > 0 && has_bar && bar_width > 0 {
        // Narrow terminal: drop the bar, keep phase and counters.
        bar_width = 0;
        line = build_progress_line(progress, color, tick, bar_width, detail_budget);
        overflow = visible_width(&line).saturating_sub(term_width);
    }
    if overflow > 0 {
        line = truncate_visible(&line, term_width, color);
    }
    line
}

/// Shared line assembly behind [`format_progress_line`] and
/// [`format_progress_line_clamped`]: the prefix varies (file bar vs.
/// spinner plus scanned counts) while the embedding/download suffix and
/// the trailing `detail` are appended once below.
fn build_progress_line(
    progress: &zg_core::types::IndexProgress,
    color: bool,
    tick: usize,
    bar_width: usize,
    detail_budget: usize,
) -> String {
    let phase = phase_label(progress);
    let detail = short_detail_to(progress.detail.as_deref(), detail_budget);
    let total = progress.files_total.unwrap_or(0);
    let done = progress.files_indexed.unwrap_or(0);
    let mut line = if total > 0 {
        format!(
            "{} {phase}",
            format_green_progress_bar(done, total, bar_width, color)
        )
    } else {
        let spinner = SPINNER
            .get(tick % SPINNER.len())
            .copied()
            .unwrap_or('\u{00B7}');
        let spinner = if color {
            format!("{GREEN}{spinner}{RESET}")
        } else {
            spinner.to_string()
        };
        let mut prefix = format!("{spinner} {phase}");
        if done > 0 {
            prefix.push_str(&format!(" {done} files"));
        }
        prefix
    };
    append_embedding_suffix(&mut line, progress.embedding.as_ref());
    if let Some(detail) = detail {
        line.push(' ');
        line.push_str(&detail);
    }
    line
}

/// Appends ` downloading X / Y (Z%)` once; shared by the bar and spinner
/// prefixes so the counters cannot drift apart.
fn append_embedding_suffix(
    line: &mut String,
    embedding: Option<&zg_core::types::IndexEmbeddingProgress>,
) {
    if let Some(embedding) = embedding
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
}

/// True while an embedding download is still in flight (counters present
/// and incomplete): the only phase that justifies the chatty non-TTY
/// cadence.
pub(crate) fn is_downloading(progress: &zg_core::types::IndexProgress) -> bool {
    if let Some(embedding) = progress.embedding.as_ref()
        && let (Some(downloaded), Some(total)) = (embedding.downloaded_bytes, embedding.total_bytes)
    {
        return total > 0 && downloaded < total;
    }
    false
}

/// Minimal stderr progress reporter: TTY bar plus throttled plain lines.
pub struct ProgressReporter {
    color: bool,
    enabled: bool,
    tty: bool,
    tick: usize,
    completed: usize,
    last_tty: Instant,
    last_line: Instant,
}

impl ProgressReporter {
    /// Builds a reporter; `mode`/`no_color` settle `auto` against the
    /// stderr terminal (the stream the paint lands on) and `enabled`
    /// follows `--no-progress`/`--quiet`.
    #[must_use]
    pub fn new(mode: Option<ColorMode>, no_color: bool, enabled: bool) -> Self {
        Self {
            color: use_color_stderr(mode, no_color).enabled(),
            enabled,
            tty: std::io::IsTerminal::is_terminal(&std::io::stderr()),
            tick: 0,
            completed: 0,
            last_tty: Instant::now() - Duration::from_secs(60),
            last_line: Instant::now() - Duration::from_secs(60),
        }
    }

    /// Reports one progress event. `tick` advances once per painted frame
    /// (never on throttled drops) so the spinner rotates steadily even when
    /// callbacks fire per read-chunk.
    pub fn report(&mut self, progress: &zg_core::types::IndexProgress) {
        if !self.enabled {
            return;
        }
        let done_phase = matches!(
            progress.phase,
            Some(zg_core::types::IndexProgressPhase::Done)
        );
        if let Some(done) = progress.files_indexed {
            self.completed = self.completed.max(done);
        }
        if done_phase {
            // Terminal state: the Done event carries no totals, so report
            // the last observed count plainly (no spinner, no throttle).
            // On TTY there is nothing to paint: finish() clears the bar
            // and the summary carries the counts.
            if !self.tty {
                eprintln!("{}", format_done_line(self.completed));
                self.last_line = Instant::now();
            }
            return;
        }
        if self.tty {
            if !done_phase && self.last_tty.elapsed() < TTY_THROTTLE {
                return;
            }
            let width = stderr_width();
            let line = format_progress_line_clamped(progress, self.color, self.tick, Some(width));
            self.tick = self.tick.wrapping_add(1);
            eprint!("\r\x1b[2K{line}");
            let _ = std::io::stderr().flush();
            self.last_tty = Instant::now();
        } else {
            let throttle = if is_downloading(progress) {
                LINE_THROTTLE_DOWNLOAD
            } else {
                LINE_THROTTLE
            };
            if self.last_line.elapsed() < throttle {
                return;
            }
            eprintln!("{}", format_progress_line(progress, false, self.tick));
            self.tick = self.tick.wrapping_add(1);
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
