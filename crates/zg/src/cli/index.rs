//! `zg index` flags: create, update, rebuild, or drop the workspace index.

use std::path::PathBuf;

use clap::Args;

use super::values::{ClientModeArg, DeviceArg};

/// `zg index` flags.
#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Workspace root to index (default: current directory).
    pub roots: Vec<PathBuf>,
    /// Delete the index instead of building it.
    #[arg(long = "drop", action = clap::ArgAction::SetTrue)]
    pub drop: bool,
    /// Confirm destructive operations without prompting.
    #[arg(long = "yes", action = clap::ArgAction::SetTrue)]
    pub yes: bool,
    /// Rebuild from scratch.
    #[arg(long = "rebuild", action = clap::ArgAction::SetTrue)]
    pub rebuild: bool,
    /// Replace the index root-path configuration.
    #[arg(long = "reset-paths", action = clap::ArgAction::SetTrue)]
    pub reset_paths: bool,
    /// Case-sensitive glob rules (repeatable).
    #[arg(long = "glob", short = 'g')]
    pub globs: Vec<String>,
    /// Case-insensitive glob rules (repeatable).
    #[arg(long = "iglob")]
    pub iglobs: Vec<String>,
    /// Ripgrep file-type names to include (repeatable).
    #[arg(long = "type", short = 't')]
    pub file_types: Vec<String>,
    /// Ripgrep file-type names to exclude (repeatable).
    #[arg(long = "type-not", short = 'T')]
    pub excluded_file_types: Vec<String>,
    /// Index hidden files.
    #[arg(long = "hidden", action = clap::ArgAction::SetTrue)]
    pub hidden: bool,
    /// Ignore `.gitignore`/`.ignore` rules.
    #[arg(long = "no-ignore", action = clap::ArgAction::SetTrue)]
    pub no_ignore: bool,
    /// Extra ignore files (repeatable).
    #[arg(long = "ignore-file")]
    pub ignore_files: Vec<String>,
    /// Maximum directory depth.
    #[arg(long = "max-depth")]
    pub max_depth: Option<u32>,
    /// Skip files larger than this (bytes or `10MB`).
    #[arg(long = "max-filesize")]
    pub max_filesize: Option<String>,
    /// Follow symlinks.
    #[arg(long = "follow", short = 'L', action = clap::ArgAction::SetTrue)]
    pub follow: bool,
    /// Embedding requests processed concurrently.
    #[arg(long = "embedding-concurrency")]
    pub embedding_concurrency: Option<usize>,
    /// Transport selection: direct, server, or auto.
    #[arg(long = "mode")]
    pub mode: Option<ClientModeArg>,
    /// Force direct mode (requires `--mode direct`).
    #[arg(long = "force-direct", action = clap::ArgAction::SetTrue)]
    pub force_direct: bool,
    /// Daemon/global-config home override.
    #[arg(long = "home")]
    pub home: Option<PathBuf>,
    /// Explicit embedding model reference for a new index.
    #[arg(long = "embedding")]
    pub embedding: Option<String>,
    /// Local model cache directory override.
    #[arg(long = "model-cache")]
    pub model_cache: Option<PathBuf>,
    /// Device placement for local models.
    #[arg(long = "device")]
    pub device: Option<DeviceArg>,
    /// API key for remote embedding providers.
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
    /// Remote embedding endpoint override.
    #[arg(long = "endpoint")]
    pub endpoint: Option<String>,
    /// Print engine debug diagnostics to stderr.
    #[arg(long = "debug", action = clap::ArgAction::SetTrue)]
    pub debug: bool,
    /// Allow one remote-embedding operation without a stored grant.
    #[arg(
        long = "allow-remote",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub allow_remote: Option<String>,
}
