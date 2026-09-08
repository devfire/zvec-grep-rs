//! `zg` CLI entry point: parse, validate, dispatch, and report.
//!
//! Exit codes mirror the TypeScript CLI: `0` on success (including
//! `--help`/`--version`), `1` for every failure. Clap owns `--help`
//! rendering; usage errors keep the `args.ts` texts via [`validate`](crate::cli::validate).

// Test builds exercise fallible paths with `unwrap`/`expect` per the port
// plan (M8 permits `allow(unwrap_used)`/`allow(expect_used)` under
// `cfg(test)` only).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cli;
mod client;
mod commands;
mod error;
mod format;
mod install;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::error::CliError;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // Clap exits 0 for help/version and 2 for usage errors; the
            // frozen contract is 0 on success and 1 on every failure.
            use clap::error::ErrorKind;
            match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => error.exit(),
                _ => {
                    let _ = error.print();
                    std::process::exit(1);
                }
            }
        }
    };
    let debug = matches!(&cli.command, Some(Command::Query(args)) if args.debug)
        || matches!(&cli.command, Some(Command::Index(args)) if args.debug)
        || matches!(&cli.command, Some(Command::Status(args)) if args.debug);
    if let Err(error) = cli::validate(&cli) {
        report(error, debug);
    }
    if let Err(error) = commands::run(cli).await {
        report(error, debug);
    }
}

fn report(error: CliError, debug: bool) -> ! {
    format::print_error(&error, color_stderr(), debug);
    std::process::exit(1);
}

fn color_stderr() -> bool {
    std::env::var("NO_COLOR").is_err() && std::io::IsTerminal::is_terminal(&std::io::stderr())
}
