//! `zg config model set` / `zg config provider set`: provider credentials
//! and model defaults.

use clap::{Args, Subcommand};

use super::values::DeviceArg;

/// `zg config model set` / `zg config provider set`.
#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub target: Option<ConfigTarget>,
}

/// Config targets, mirroring `config model set` / `config provider set`.
#[derive(Debug, Subcommand)]
pub enum ConfigTarget {
    /// Per-model overrides and defaults.
    Model(ConfigModelCmd),
    /// Per-provider credentials.
    Provider(ConfigProviderCmd),
}

/// `zg config model <op>`.
#[derive(Debug, Args)]
pub struct ConfigModelCmd {
    #[command(subcommand)]
    pub op: Option<ConfigModelOp>,
}

/// `zg config model set` operations.
#[derive(Debug, Subcommand)]
pub enum ConfigModelOp {
    /// Set endpoint/device/default for one catalog reference.
    Set(ConfigModelSetArgs),
}

/// `zg config model set` flags.
#[derive(Debug, Args)]
pub struct ConfigModelSetArgs {
    /// Catalog reference (`provider/model`).
    pub reference: Vec<String>,
    /// Remote endpoint override (remote models only).
    #[arg(long = "endpoint")]
    pub endpoint: Option<String>,
    /// Device placement (local models only).
    #[arg(long = "device")]
    pub device: Option<DeviceArg>,
    /// Make this reference the global default embedding.
    #[arg(long = "default", action = clap::ArgAction::SetTrue)]
    pub default: bool,
}

/// `zg config provider <op>`.
#[derive(Debug, Args)]
pub struct ConfigProviderCmd {
    #[command(subcommand)]
    pub op: Option<ConfigProviderOp>,
}

/// `zg config provider set` operations.
#[derive(Debug, Subcommand)]
pub enum ConfigProviderOp {
    /// Store the API key for one provider.
    Set(ConfigProviderSetArgs),
}

/// `zg config provider set` flags.
#[derive(Debug, Args)]
pub struct ConfigProviderSetArgs {
    /// Provider name (not `local`).
    pub reference: Vec<String>,
    /// API key to store.
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
}
