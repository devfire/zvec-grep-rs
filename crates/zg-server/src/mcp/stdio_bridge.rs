//! MCP stdio server: serves the tool router over stdin/stdout.
//!
//! Mirrors the serving half of `../zvec-grep/src/mcp/stdio-bridge.ts`.
//! The TS bridge spawns the daemon as a child process and forwards
//! elicitation between downstream and upstream clients; the port serves
//! the router in-process (the unit the daemon links directly), so there
//! is no subprocess to supervise and no elicitation to forward —
//! `shouldStopStdioBridge` has no subject here (see
//! `docs/ts-divergence.md`).

use crate::backend::DaemonBackend;
use crate::mcp::error::McpError;
use crate::mcp::tools::ZvecGrepMcpServer;
use crate::mcp::toolset::McpToolset;

/// Serves the MCP tool router over stdio until the client disconnects.
///
/// # Errors
///
/// Returns [`McpError::Transport`] when serving starts or the server task fails.
pub async fn run_stdio_server(
    backend: DaemonBackend,
    version: String,
    toolset: McpToolset,
) -> Result<(), McpError> {
    let router = ZvecGrepMcpServer::new(backend, version, toolset).router();
    let running = rmcp::serve_server(router, rmcp::transport::io::stdio())
        .await
        .map_err(|error| McpError::Transport {
            message: format!("stdio serve failed: {error}"),
        })?;
    running
        .waiting()
        .await
        .map_err(|error| McpError::Transport {
            message: format!("stdio server failed: {error}"),
        })?;
    Ok(())
}
