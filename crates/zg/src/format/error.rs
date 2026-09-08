//! CLI error reporting, mirroring `printError`.
//!
//! Red `error:` prefix on color terminals, the message, and the wire code
//! with `--debug` plus the full cause chain.

use crate::error::CliError;

use super::color::{DIM, RED, RESET};

/// Prints a CLI error like `printError`: red `error:` prefix on color
/// terminals, the message, and the wire code with `--debug`.
pub fn print_error(error: &CliError, color: bool, debug: bool) {
    if color {
        eprintln!("{RED}error:{RESET} {error}");
    } else {
        eprintln!("error: {error}");
    }
    if debug {
        eprintln!("{DIM}code: {}{RESET}", error.code());
        let mut source = std::error::Error::source(error);
        while let Some(next) = source {
            eprintln!("{DIM}caused by: {next}{RESET}");
            source = std::error::Error::source(next);
        }
    }
}
