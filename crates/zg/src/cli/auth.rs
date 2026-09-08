//! `zg auth grant|status|revoke`: remote-embedding workspace grants.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::values::{CapabilityArg, DeviceArg, ScopeArg};

/// `zg auth grant|status|revoke`.
#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub action: Option<AuthAction>,
    /// Daemon/global-config home override.
    #[arg(long = "home", global = true)]
    pub home: Option<PathBuf>,
    /// Explicit embedding model reference.
    #[arg(long = "embedding", global = true)]
    pub embedding: Option<String>,
    /// Local model cache directory override.
    #[arg(long = "model-cache", global = true)]
    pub model_cache: Option<PathBuf>,
    /// Device placement for local models.
    #[arg(long = "device", global = true)]
    pub device: Option<DeviceArg>,
    /// API key for remote embedding providers.
    #[arg(long = "api-key", global = true)]
    pub api_key: Option<String>,
    /// Remote embedding endpoint override.
    #[arg(long = "endpoint", global = true)]
    pub endpoint: Option<String>,
}

/// Auth actions, mirroring `grant|status|revoke`.
#[derive(Debug, Subcommand)]
pub enum AuthAction {
    /// Grant workspace remote-embedding authorization.
    Grant(AuthGrantArgs),
    /// Show workspace authorization status.
    Status(AuthRootArgs),
    /// Revoke workspace grants.
    Revoke(AuthRootArgs),
}

/// `zg auth grant` flags.
#[derive(Debug, Args)]
pub struct AuthGrantArgs {
    /// Workspace root (default: nearest indexed ancestor or cwd).
    pub roots: Vec<PathBuf>,
    /// Capability to grant (only `embedding`).
    #[arg(long = "capability")]
    pub capability: Option<CapabilityArg>,
    /// Grant scope (only `workspace`).
    #[arg(long = "scope")]
    pub scope: Option<ScopeArg>,
}

/// `zg auth status|revoke` flags.
#[derive(Debug, Args)]
pub struct AuthRootArgs {
    /// Workspace root (default: nearest indexed ancestor or cwd).
    pub roots: Vec<PathBuf>,
    /// Rejected unless granting, with the TS message.
    #[arg(long = "capability", hide = true)]
    pub capability_rejected: Option<String>,
    /// Rejected unless granting, with the TS message.
    #[arg(long = "scope", hide = true)]
    pub scope_rejected: Option<String>,
}
