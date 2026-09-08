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

/// True when color output is enabled: `always`, or `auto` on a terminal.
pub fn use_color(mode: Option<ColorMode>, no_color: bool) -> bool {
    if no_color || std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    match mode {
        Some(ColorMode::Always) => true,
        Some(ColorMode::Never) => false,
        Some(ColorMode::Auto) | None => std::io::IsTerminal::is_terminal(&std::io::stdout()),
    }
}
