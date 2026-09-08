//! IDE MCP integration flags: `zg install` and `zg uninstall`.

use clap::Args;

use super::values::{McpToolsetArg, McpTransportArg};

/// `zg install` flags.
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Integration targets: claude, codex, opencode, cursor, qwen, qoder
    /// (repeatable, comma-separated).
    #[arg(long = "target")]
    pub target: Vec<String>,
    /// MCP tool request timeout in seconds.
    #[arg(long = "mcp-tool-timeout")]
    pub mcp_tool_timeout: Option<u32>,
    /// Environment variable carrying the daemon bearer token (http only).
    #[arg(long = "mcp-token-env")]
    pub mcp_token_env: Option<String>,
    /// MCP transport: stdio (default) or http.
    #[arg(long = "mcp-transport")]
    pub mcp_transport: Option<McpTransportArg>,
    /// MCP toolset: agent (default) or full.
    #[arg(long = "mcp-toolset")]
    pub mcp_toolset: Option<McpToolsetArg>,
    /// Skip confirmation prompts and allow overwrites.
    #[arg(long = "yes", action = clap::ArgAction::SetTrue)]
    pub yes: bool,
    /// Replace unmanaged entries without prompting.
    #[arg(long = "force", action = clap::ArgAction::SetTrue)]
    pub force: bool,
}

/// `zg uninstall` flags.
#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Integration targets (repeatable, comma-separated).
    #[arg(long = "target")]
    pub target: Vec<String>,
    /// Skip confirmation prompts.
    #[arg(long = "yes", action = clap::ArgAction::SetTrue)]
    pub yes: bool,
}
