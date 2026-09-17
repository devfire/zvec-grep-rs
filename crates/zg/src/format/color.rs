//! ANSI paint and color gating.
//!
//! [`use_color`] mirrors the TS color selection: explicit `--color` wins,
//! `NO_COLOR` (or `--no-color`) always disables, otherwise `auto` follows
//! the stdout terminal.

use crate::cli::ColorMode;

/// ANSI reset.
pub(crate) const RESET: &str = "\x1b[0m";
/// ANSI dim.
pub(crate) const DIM: &str = "\x1b[2m";
/// ANSI green.
pub(crate) const GREEN: &str = "\x1b[32m";
/// ANSI red.
pub(crate) const RED: &str = "\x1b[31m";
/// ANSI cyan.
pub(crate) const CYAN: &str = "\x1b[36m";

/// Resolved color verdict for printers: `use_color` settles `auto` once so
/// call sites cannot transpose a `(human, color)` bool pair (defensive
/// #10) — printers take this, never a bare `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Emit ANSI escapes.
    Always,
    /// Plain output.
    Never,
}

impl Color {
    /// True for [`Color::Always`].
    #[must_use]
    pub fn enabled(self) -> bool {
        matches!(self, Self::Always)
    }
}

/// Settles the color verdict for stdout printers: explicit `--color` wins,
/// `NO_COLOR` (or `--no-color`) always disables, otherwise `auto` follows
/// the stdout terminal.
#[must_use]
pub fn use_color(mode: Option<ColorMode>, no_color: bool) -> Color {
    resolve_color(
        mode,
        no_color,
        std::io::IsTerminal::is_terminal(&std::io::stdout()),
    )
}

/// Settles the color verdict for stderr painters (progress reporter):
/// same precedence as [`use_color`], but `auto` follows the stderr
/// terminal — the stream the paint lands on.
#[must_use]
pub fn use_color_stderr(mode: Option<ColorMode>, no_color: bool) -> Color {
    resolve_color(
        mode,
        no_color,
        std::io::IsTerminal::is_terminal(&std::io::stderr()),
    )
}

/// Shared precedence: explicit flags win, otherwise `auto` follows `tty`.
fn resolve_color(mode: Option<ColorMode>, no_color: bool, tty: bool) -> Color {
    if no_color || std::env::var("NO_COLOR").is_ok() {
        return Color::Never;
    }
    match mode {
        Some(ColorMode::Always) => Color::Always,
        Some(ColorMode::Never) => Color::Never,
        Some(ColorMode::Auto) | None => {
            if tty {
                Color::Always
            } else {
                Color::Never
            }
        }
    }
}
