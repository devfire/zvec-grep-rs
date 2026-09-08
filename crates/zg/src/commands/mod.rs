//! Command handlers: direct engine calls or daemon tool calls per mode
//! (`cli/commands.ts` + `cli/auth.ts` behavior).
//!
//! Direct mode drives `ZvecGrepService` in-process with the same permit
//! guard as the daemon (authorization lives in `zg-core`, phase F);
//! server mode goes through [`crate::client::DaemonClient`]. Server-mode index sends
//! only `{root, rebuild, wait, debug}`: per-request credentials and
//! index scoping are daemon configuration in this port and fail fast
//! here instead of tripping the server rejection (see
//! `docs/ts-divergence.md`).
//!
//! One submodule per command; [`support`], [`catalog`], and [`authz`]
//! hold the wiring they share.

mod auth;
mod authz;
mod catalog;
mod config;
mod index;
mod install;
mod query;
mod rg;
mod server;
mod status;
mod support;

use std::path::Path;

use clap::CommandFactory;
use zg_core::models::catalog::list_embedding_models;

use crate::cli::{Command, HelpArgs};
use crate::error::CliError;

use self::catalog::catalog_identity;

/// Runs the parsed CLI tree.
pub async fn run(cli: crate::cli::Cli) -> Result<(), CliError> {
    match cli.command {
        None => print_main_help(),
        Some(Command::Query(args)) => query::run_query(*args).await,
        Some(Command::Index(args)) => index::run_index(*args).await,
        Some(Command::Status(args)) => status::run_status(args).await,
        Some(Command::Install(args)) => install::run_install(args),
        Some(Command::Uninstall(args)) => install::run_uninstall(args),
        Some(Command::Config(args)) => config::run_config(args),
        Some(Command::Auth(args)) => auth::run_auth(args),
        Some(Command::Server(args)) => server::run_server(args).await,
        Some(Command::Help(args)) => run_help(args),
        Some(Command::Version) => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Command::Completions(args)) => {
            let mut command = crate::cli::Cli::command();
            clap_complete::generate(args.shell, &mut command, "zg", &mut std::io::stdout());
            Ok(())
        }
        Some(Command::Serve) => Err(CliError::usage(
            "zg serve has been removed; use zg server on and Streamable HTTP MCP",
        )),
    }
}

fn print_main_help() -> Result<(), CliError> {
    crate::cli::Cli::command()
        .print_long_help()
        .map_err(|error| CliError::io(Path::new("<stdout>"), error))?;
    println!();
    Ok(())
}

fn run_help(args: HelpArgs) -> Result<(), CliError> {
    let Some(topic) = args.topic else {
        return print_main_help();
    };
    if topic == "models" {
        print_models_help();
        return Ok(());
    }
    let mut command = crate::cli::Cli::command();
    let known = [
        "query",
        "index",
        "status",
        "install",
        "uninstall",
        "config",
        "auth",
        "server",
        "help",
        "version",
        "completions",
    ];
    // Topics name top-level subcommands only.
    if known.contains(&topic.as_str()) {
        for sub in command.get_subcommands_mut() {
            if sub.get_name() == topic {
                sub.print_long_help()
                    .map_err(|error| CliError::io(Path::new("<stdout>"), error))?;
                println!();
                return Ok(());
            }
        }
    }
    print_main_help()
}

fn print_models_help() {
    println!("Embedding models:");
    for entry in list_embedding_models() {
        let (reference, provider, model, dimension) = catalog_identity(entry);
        println!("  {reference}  ({provider}/{model}, dim {dimension})");
    }
}
