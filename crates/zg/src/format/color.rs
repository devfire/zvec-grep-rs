//! ANSI paint and color gating.
//!
//! [`use_color`] mirrors the TS color selection: explicit `--color` wins,
//! a present-and-non-empty `NO_COLOR` (or `--no-color`) always disables,
//! otherwise `auto` follows the stdout terminal. An empty `NO_COLOR`
//! leaves colors on, per no-color.org.
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
/// a present-and-non-empty `NO_COLOR` (or `--no-color`) always disables,
/// otherwise `auto` follows the stdout terminal.
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

/// Settles the color verdict for an explicit `tty`: the single precedence
/// behind [`use_color`] and [`use_color_stderr`] — no call site duplicates
/// it. Probes `NO_COLOR` once and delegates to [`resolve_color_with_env`].
#[must_use]
pub fn resolve_color(mode: Option<ColorMode>, no_color: bool, tty: bool) -> Color {
    resolve_color_with_env(
        mode,
        ColorFlags {
            no_color,
            no_color_env: matches!(std::env::var("NO_COLOR"), Ok(value) if !value.is_empty()),
            tty,
        },
    )
}

/// Flag bundle for [`resolve_color_with_env`]: a struct (not three bare
/// `bool`s) so precedence inputs cannot be mis-ordered at call sites.
#[derive(Debug, Clone, Copy)]
struct ColorFlags {
    no_color: bool,
    no_color_env: bool,
    tty: bool,
}

/// Shared precedence: explicit flags win, otherwise `auto` follows `tty`.
///
/// `no_color_env` is the `NO_COLOR` verdict — present *and* non-empty, per
/// no-color.org. It arrives as a parameter (instead of being read here) so
/// the matrix stays unit-testable without touching the process environment
/// (env mutation is `unsafe` under edition 2024).
fn resolve_color_with_env(mode: Option<ColorMode>, flags: ColorFlags) -> Color {
    if flags.no_color || flags.no_color_env {
        return Color::Never;
    }
    match mode {
        Some(ColorMode::Always) => Color::Always,
        Some(ColorMode::Never) => Color::Never,
        Some(ColorMode::Auto) | None => {
            if flags.tty {
                Color::Always
            } else {
                Color::Never
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_empty_leaves_colors_on_while_set_disables() {
        // Empty `NO_COLOR` (env absent or "") must not disable: explicit
        // `--color always` and auto-on-TTY survive it.
        let on_tty = ColorFlags {
            no_color: false,
            no_color_env: false,
            tty: true,
        };
        let off_tty = ColorFlags {
            no_color: false,
            no_color_env: false,
            tty: false,
        };
        assert_eq!(
            resolve_color_with_env(Some(ColorMode::Always), off_tty),
            Color::Always
        );
        assert_eq!(resolve_color_with_env(None, on_tty), Color::Always);
        // Present-and-non-empty `NO_COLOR` disables even `--color always`.
        assert_eq!(
            resolve_color_with_env(
                Some(ColorMode::Always),
                ColorFlags {
                    no_color: false,
                    no_color_env: true,
                    tty: true,
                },
            ),
            Color::Never
        );
        // Explicit `--color never` mutes on a color terminal, exactly like
        // the error-report path must.
        assert_eq!(
            resolve_color_with_env(Some(ColorMode::Never), on_tty),
            Color::Never
        );
        // `--no-color` mutes even with `--color always` absent and a TTY.
        assert_eq!(
            resolve_color_with_env(
                None,
                ColorFlags {
                    no_color: true,
                    no_color_env: false,
                    tty: true,
                },
            ),
            Color::Never
        );
        // Auto still follows the terminal when nothing overrides it.
        assert_eq!(resolve_color_with_env(None, off_tty), Color::Never);
    }
}
