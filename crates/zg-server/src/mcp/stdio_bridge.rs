//! MCP stdio server: serves the tool router over stdin/stdout.
//!
//! Mirrors the serving half of `../zvec-grep/src/mcp/stdio-bridge.ts`.
//! The TS bridge spawns the daemon as a child process and forwards
//! elicitation between downstream and upstream clients; the port serves
//! the router in-process (the unit the daemon links directly), so there
//! is no subprocess to supervise and no elicitation to forward —
//! `shouldStopStdioBridge` has no subject here (see
//! `docs/ts-divergence.md`).
//! `CappedStdin` budgets every stdio frame like the HTTP transport
//! (`crate::http_server::MAX_REQUEST_BYTES`), failing an over-budget frame
//! before rmcp deserializes it. Fails the session closed; memory stays bounded.
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

use crate::backend::DaemonBackend;
use crate::mcp::error::McpError;
use crate::mcp::tools::ZvecGrepMcpServer;
use crate::mcp::toolset::McpToolset;

/// Maximum bytes per stdio JSON-RPC frame: the HTTP per-request budget.
pub const MAX_STDIO_FRAME_BYTES: usize = 1024 * 1024;

/// `AsyncRead` adapter failing the stream once a newline-delimited frame
/// passes [`MAX_STDIO_FRAME_BYTES`]. Counts raw bytes before rmcp parses.
/// Legit inputs sit far below (≤32 groups of 4_000 chars); only hostile or
/// buggy peers trip it.
struct CappedStdin<R> {
    inner: R,
    line_len: usize,
}

impl<R> CappedStdin<R> {
    fn new(inner: R) -> Self {
        Self { inner, line_len: 0 }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CappedStdin<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {
                let mut over_budget = false;
                // `skip` over bytes counted on earlier polls: no slicing, so
                // the workspace `indexing_slicing` deny stays satisfied.
                for byte in buf.filled().iter().skip(before) {
                    if *byte == b'\n' {
                        self.line_len = 0;
                    } else {
                        self.line_len += 1;
                        if self.line_len > MAX_STDIO_FRAME_BYTES {
                            over_budget = true;
                            break;
                        }
                    }
                }
                if over_budget {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Request body too large.",
                    )));
                }
                Poll::Ready(Ok(()))
            }
        }
    }
}

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
    let (stdin, stdout) = rmcp::transport::io::stdio();
    let running = rmcp::serve_server(router, (CappedStdin::new(stdin), stdout))
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
