//! Shared clap value enums: transport, freshness, display, and auth vocabulary.
//!
//! These mirror the TypeScript option unions in `cli/args.ts`. Each enum is
//! referenced by several subcommands, so they live here instead of beside
//! any single args struct.

use clap::ValueEnum;

/// Transport selection, mirroring `ZvecGrepClientMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ClientModeArg {
    /// In-process engine.
    Direct,
    /// Loopback daemon over MCP.
    Server,
    /// Daemon when ready, else in-process.
    Auto,
}

/// Index freshness selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RefreshMode {
    /// Search now; refresh in the background (server only).
    Background,
    /// Wait for a fresh index before searching.
    Wait,
    /// Never refresh.
    Off,
}

/// Content preview length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PreviewMode {
    /// No content lines.
    None,
    /// Short source window.
    Short,
    /// Full content.
    Full,
}

/// Color selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorMode {
    /// Color when writing to a terminal.
    Auto,
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
}

/// Indexed symbol kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SymbolType {
    /// Modules.
    Module,
    /// Classes.
    Class,
    /// Interfaces.
    Interface,
    /// Functions.
    Function,
    /// Values.
    Value,
    /// Aliases.
    Alias,
}

/// Device placement for local models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DeviceArg {
    /// Automatic selection.
    Auto,
    /// CPU execution.
    Cpu,
    /// Apple Metal.
    Metal,
    /// Vulkan.
    Vulkan,
    /// CUDA.
    Cuda,
}

/// Authorization capability (only `embedding` exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CapabilityArg {
    /// Remote-embedding capability.
    Embedding,
}

/// Authorization scope (only `workspace` exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScopeArg {
    /// Workspace scope.
    Workspace,
}

/// MCP toolset selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpToolsetArg {
    /// Search only.
    Agent,
    /// Search plus index lifecycle, rg, and status.
    Full,
}

/// Install transport selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpTransportArg {
    /// Spawn `zg server --stdio`.
    Stdio,
    /// Connect to the daemon HTTP endpoint.
    Http,
}
