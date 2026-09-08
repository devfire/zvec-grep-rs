//! `zg status` flags: show workspace index state.

use std::path::PathBuf;

use clap::Args;

use super::values::{ClientModeArg, ColorMode, DeviceArg};

/// `zg status` flags.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Workspace root (default: current directory).
    pub roots: Vec<PathBuf>,
    /// Fail when the index is not ready.
    #[arg(long = "check-ready", action = clap::ArgAction::SetTrue)]
    pub check_ready: bool,
    /// Transport selection: direct, server, or auto.
    #[arg(long = "mode")]
    pub mode: Option<ClientModeArg>,
    /// Daemon/global-config home override.
    #[arg(long = "home")]
    pub home: Option<PathBuf>,
    /// Explicit embedding model reference.
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
    /// Color mode for status output.
    #[arg(long = "color")]
    pub color: Option<ColorMode>,
    /// Disable colored output.
    #[arg(long = "no-color", action = clap::ArgAction::SetTrue)]
    pub no_color: bool,
}
