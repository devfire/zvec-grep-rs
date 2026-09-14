//! CLI error reporting, mirroring `printError`.
//!
//! Red `error:` prefix on color terminals, the message, and the wire code
//! with `--debug` plus the full cause chain.

use crate::error::CliError;

use super::color::{Color, DIM, RED, RESET};

/// Prints a CLI error like `printError`: red `error:` prefix on color
/// terminals, the message, and the wire code with `--debug`.
pub fn print_error(error: &CliError, color: Color, debug: bool) {
    if color.enabled() {
        eprintln!("{RED}error:{RESET} {error}");
    } else {
        eprintln!("error: {error}");
    }
    if debug {
        for line in debug_lines(error) {
            eprintln!("{DIM}{line}{RESET}");
        }
    }
}

/// `--debug` detail lines: the wire code, then one entry per cause.
/// Transparent variants (`Engine`, `Daemon`) display identically to their
/// inner error, so the walk starts *inside* the inner error — otherwise the
/// first `caused by` would duplicate the `error:` line verbatim. The
/// owned-variant arm spells every variant (no wildcard) so a new one
/// forces a deliberate routing decision here.
pub(crate) fn debug_lines(error: &CliError) -> Vec<String> {
    let mut lines = vec![format!("code: {}", error.code())];
    let mut source = match error {
        CliError::Engine(inner) => std::error::Error::source(inner),
        CliError::Daemon(inner) => std::error::Error::source(inner),
        CliError::Usage { .. }
        | CliError::ConfigInvalid { .. }
        | CliError::AuthorizationDeclined { .. }
        | CliError::AuthorizationRequired { .. }
        | CliError::InstallRefused { .. }
        | CliError::ServerIncompatible { .. }
        | CliError::RgIncompatible { .. }
        | CliError::DaemonUnavailable { .. }
        | CliError::NotReady { .. }
        | CliError::Io { .. } => std::error::Error::source(error),
    };
    while let Some(next) = source {
        lines.push(format!("caused by: {next}"));
        source = std::error::Error::source(next);
    }
    lines
}
