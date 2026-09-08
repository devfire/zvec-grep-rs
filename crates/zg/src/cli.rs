//! `zg` command surface: clap-derive tree mirroring `cli/args.ts`.
//!
//! The TypeScript CLI hand-rolls `parseArgs` over `process.argv`; the port
//! uses clap derive (see `docs/ts-divergence.md`). Frozen: subcommand
//! names, flag spellings (`--model-cache`, `--type-not`, `--mcp-toolset`,
//! …), and every user-facing validation message, which stays byte-identical
//! to `validateCliShape` / `parseCommand`. Clap owns `--help` rendering
//! and generic unknown-flag errors instead.
//!
//! Layout: [`Cli`] and [`Command`] live here. Per-command args live in
//! sibling modules (`query`, `index`, `status`, `integrate`, `config`,
//! `auth`, `server`, `help`); shared value enums in [`values`]; cross-flag
//! checks in [`mod@validate`]; value parsers in [`parse`]. The facade re-exports
//! every name the binary uses, so existing `crate::cli::X` paths work.

mod auth;
mod config;
mod help;
mod index;
mod integrate;
mod parse;
mod query;
mod server;
mod status;
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
mod validate;
mod values;

pub use auth::{AuthAction, AuthArgs, AuthGrantArgs, AuthRootArgs};
pub use config::{
    ConfigArgs, ConfigModelOp, ConfigModelSetArgs, ConfigProviderOp, ConfigProviderSetArgs,
    ConfigTarget,
};
pub use help::{CompletionsArgs, HelpArgs};
pub use index::IndexArgs;
pub use integrate::{InstallArgs, UninstallArgs};
pub use parse::{parse_byte_size, parse_environment_variable, parse_modified_time, split_targets};
pub use query::QueryArgs;
pub use server::{ServerAction, ServerArgs, ServerStatusArgs};
pub use status::StatusArgs;
pub use validate::validate;
pub use values::{
    ClientModeArg, ColorMode, DeviceArg, McpToolsetArg, McpTransportArg, RefreshMode, SymbolType,
};

use clap::{Parser, Subcommand};

/// Top-level parser: every TS `CliCommand` plus `completions`.
#[derive(Debug, Parser)]
#[command(
    name = "zg",
    version,
    about = "Hybrid workspace search for humans and agents",
    long_about = "Hybrid workspace search: semantic + lexical retrieval over an indexed workspace, with a loopback daemon and MCP endpoint.",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Subcommand; absent prints the main help like bare `zg` (exit 0).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// All subcommands, mirroring `CliCommand` plus `completions`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search the workspace index (or exhaustively with `--rg`).
    Query(Box<QueryArgs>),
    /// Create, update, rebuild, or drop the workspace index.
    Index(Box<IndexArgs>),
    /// Show workspace index state.
    Status(StatusArgs),
    /// Install IDE MCP integrations.
    Install(InstallArgs),
    /// Remove IDE MCP integrations.
    Uninstall(UninstallArgs),
    /// Manage provider credentials and model defaults.
    Config(ConfigArgs),
    /// Manage remote-embedding workspace grants.
    Auth(AuthArgs),
    /// Control the loopback daemon.
    Server(ServerArgs),
    /// Show help for a command or topic.
    Help(HelpArgs),
    /// Print the version.
    Version,
    /// Print shell completions.
    Completions(CompletionsArgs),
    /// Removed alias: errors with the migration message.
    #[command(name = "serve", hide = true)]
    Serve,
}
