//! `zg server on|off|status|run` and `--stdio`: loopback daemon control.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::values::{DeviceArg, McpToolsetArg};

/// `zg server on|off|status|run` and `--stdio`.
#[derive(Debug, Args)]
pub struct ServerArgs {
    #[command(subcommand)]
    pub action: Option<ServerAction>,
    /// Serve MCP over stdio instead of HTTP.
    #[arg(long = "stdio", global = true, action = clap::ArgAction::SetTrue)]
    pub stdio: bool,
    /// Listen address for `on`/`run` (loopback only).
    #[arg(long = "listen", global = true)]
    pub listen: Option<String>,
    /// Bearer-token file for the daemon.
    #[arg(long = "token-file", global = true)]
    pub token_file: Option<PathBuf>,
    /// MCP toolset: agent (default) or full.
    #[arg(long = "mcp-toolset", global = true)]
    pub mcp_toolset: Option<McpToolsetArg>,
    /// Daemon/global-config home override.
    #[arg(long = "home", global = true)]
    pub home: Option<PathBuf>,
    /// Explicit embedding model reference (run/stdio).
    #[arg(long = "embedding", global = true)]
    pub embedding: Option<String>,
    /// Local model cache directory override (run/stdio).
    #[arg(long = "model-cache", global = true)]
    pub model_cache: Option<PathBuf>,
    /// Device placement for local models (run/stdio).
    #[arg(long = "device", global = true)]
    pub device: Option<DeviceArg>,
    /// API key for remote embedding providers (run/stdio).
    #[arg(long = "api-key", global = true)]
    pub api_key: Option<String>,
    /// Remote embedding endpoint override (run/stdio).
    #[arg(long = "endpoint", global = true)]
    pub endpoint: Option<String>,
}

/// Server actions.
#[derive(Debug, Subcommand)]
pub enum ServerAction {
    /// Spawn the daemon in the background.
    On,
    /// Stop the running daemon.
    Off,
    /// Show daemon liveness.
    Status(ServerStatusArgs),
    /// Run the daemon in the foreground.
    Run,
}

/// `zg server status` flags.
#[derive(Debug, Args)]
pub struct ServerStatusArgs {
    /// Fail when the server is not ready.
    #[arg(long = "check-ready", action = clap::ArgAction::SetTrue)]
    pub check_ready: bool,
}
