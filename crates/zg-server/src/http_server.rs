//! Daemon HTTP server: health, shutdown, and MCP routes with loopback/auth guards.
//!
//! Mirrors `../zvec-grep/src/daemon/http-server.ts` (`DaemonHttpServer`:
//! `GET /healthz`, `POST /control/shutdown`, `POST /mcp`, `POST
//! /mcp/admin`, loopback-host guard, bearer token, 1 MB body cap). The
//! axum port keeps every guard's status code and error body:
//! healthz is unauthenticated; shutdown and MCP routes demand a loopback
//! Host carrying the actual bound port plus, when present, a loopback
//! Origin (403 otherwise; a missing Origin is allowed for non-browser
//! clients), then the bearer token (401 otherwise, with
//! `WWW-Authenticate: Bearer` on the MCP routes).
//!
//! `/mcp` serves `options.mcp_toolset` (default `agent`: search only); `/mcp/admin`
//! always serves the `full` toolset (search plus index lifecycle, rg, status)
//! behind the same loopback/auth guards, mirroring the TS admin endpoint.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::backend::DaemonBackend;
use crate::config::is_loopback_host;
use crate::errors::DaemonError;
use crate::mcp::http_transport::{McpHttpEndpoint, McpHttpEndpointOptions};
use crate::mcp::toolset::McpToolset;
use crate::sync::MutexExt;

mod guard;
#[cfg(test)]
mod tests;

use guard::{loopback_guard, mcp_guard, valid_token};

/// Maximum MCP request body: 1 MiB, mirroring TS `MAX_REQUEST_BYTES`.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// Options for [`DaemonHttpServer`].
#[derive(Clone)]
pub struct DaemonHttpServerOptions {
    /// Listen host (must be loopback).
    pub host: String,
    /// Listen port (0 assigns an ephemeral port).
    pub port: u16,
    /// Bearer token; `None` disables auth (anonymous mode).
    pub token: Option<String>,
    /// Backend serving index/search/status.
    pub backend: DaemonBackend,
    /// MCP toolset for `/mcp`; defaults to the `agent` set. `/mcp/admin` always
    /// serves the `full` toolset so CLI lifecycle commands work against a
    /// default daemon without exposing lifecycle tools to agents on `/mcp`.
    pub mcp_toolset: McpToolset,
    /// MCP HTTP endpoint tuning.
    pub mcp_endpoint: McpHttpEndpointOptions,
    /// Server version reported over MCP.
    pub version: String,
}

impl Default for DaemonHttpServerOptions {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 0,
            token: None,
            backend: DaemonBackend::new(crate::backend::DaemonBackendOptions::default()),
            mcp_toolset: McpToolset::Agent,
            mcp_endpoint: McpHttpEndpointOptions::default(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

struct AppState {
    token: Option<String>,
    backend: DaemonBackend,
    mcp: McpHttpEndpoint,
    mcp_admin: McpHttpEndpoint,
    shutdown: CancellationToken,
    /// Port the listener is actually bound to (0 until `start` binds).
    /// Guards bind the Host header to it, so it is set before serving.
    bound_port: AtomicU16,
}

impl AppState {
    fn bound_port(&self) -> u16 {
        self.bound_port.load(Ordering::SeqCst)
    }
}

/// Axum loopback daemon server.
///
/// `start` binds and serves in the background; `close` stops the listener
/// and awaits it. Dropping without `close` leaks the task — prefer `close`.
pub struct DaemonHttpServer {
    state: Arc<AppState>,
    host: String,
    port: u16,
    bound: Mutex<Option<SocketAddr>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl DaemonHttpServer {
    /// Builds the server, rejecting non-loopback hosts exactly like the TS
    /// constructor (`Daemon HTTP server requires a loopback host.`).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::LoopbackRequired`] when the host is not loopback, or
    /// [`DaemonError::IndexFailed`] when the MCP endpoint options are invalid.
    pub fn new(options: DaemonHttpServerOptions) -> Result<Self, DaemonError> {
        if !is_loopback_host(&options.host) {
            return Err(DaemonError::LoopbackRequired { host: options.host });
        }
        let mcp = McpHttpEndpoint::new(
            options.backend.clone(),
            options.version.clone(),
            options.mcp_toolset,
            &options.mcp_endpoint,
        )
        .map_err(|error| DaemonError::IndexFailed {
            message: format!("invalid MCP endpoint options: {error}"),
        })?;
        let mcp_admin = McpHttpEndpoint::new(
            options.backend.clone(),
            options.version.clone(),
            McpToolset::Full,
            &options.mcp_endpoint,
        )
        .map_err(|error| DaemonError::IndexFailed {
            message: format!("invalid MCP endpoint options: {error}"),
        })?;
        Ok(Self {
            state: Arc::new(AppState {
                token: options.token,
                backend: options.backend,
                mcp,
                mcp_admin,
                shutdown: CancellationToken::new(),
                bound_port: AtomicU16::new(0),
            }),
            host: options.host,
            port: options.port,
            bound: Mutex::new(None),
            task: Mutex::new(None),
        })
    }

    /// Binds and starts serving; returns the bound address. Idempotent:
    /// a second call returns the existing address.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::AddressInUse`] when the port is taken, or
    /// [`DaemonError::IndexFailed`] when binding or reading the bound address fails.
    pub async fn start(&self) -> Result<SocketAddr, DaemonError> {
        if let Some(address) = self.bound.lock_ignore_poison().as_ref().copied() {
            return Ok(address);
        }
        let listener = tokio::net::TcpListener::bind((self.host.as_str(), self.port))
            .await
            .map_err(|error| {
                use std::io::ErrorKind;
                let address = format!("{}:{}", self.host, self.port);
                if error.kind() == ErrorKind::AddrInUse {
                    DaemonError::AddressInUse { address }
                } else {
                    DaemonError::IndexFailed {
                        message: format!("failed to bind {address}: {error}"),
                    }
                }
            })?;
        let address = listener
            .local_addr()
            .map_err(|error| DaemonError::IndexFailed {
                message: format!("failed to read bound address: {error}"),
            })?;
        self.state
            .bound_port
            .store(address.port(), Ordering::SeqCst);
        let router = router(Arc::clone(&self.state));
        let shutdown = self.state.shutdown.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router.into_make_service())
                .with_graceful_shutdown(async move { shutdown.cancelled().await })
                .await
                .ok();
        });
        *self.bound.lock_ignore_poison() = Some(address);
        *self.task.lock_ignore_poison() = Some(task);
        Ok(address)
    }

    /// Bound address, if started.
    pub fn bound_address(&self) -> Option<SocketAddr> {
        *self.bound.lock_ignore_poison()
    }

    /// Process-level shutdown signal, cancelled by `POST /control/shutdown`.
    /// The daemon main task selects on this token (or Ctrl-C) and then runs
    /// the single cleanup sequence. Cloning is cheap; the token is never
    /// replaced.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.state.shutdown.clone()
    }

    /// Stops the listener, closes the backend (actors, in-flight work,
    /// models), and awaits the serve task. Idempotent.
    pub async fn close(&self) {
        self.state.shutdown.cancel();
        self.state.backend.close().await;
        let task = self.task.lock_ignore_poison().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        *self.bound.lock_ignore_poison() = None;
    }
}

fn router(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route("/healthz", get(healthz).post(method_not_allowed))
        .route("/control/shutdown", post(shutdown))
        .route(
            "/mcp",
            post(mcp_post)
                .get(mcp_session_request)
                .delete(mcp_session_request),
        )
        .route(
            "/mcp/admin",
            post(mcp_admin_post)
                .get(mcp_admin_session_request)
                .delete(mcp_admin_session_request),
        )
        .fallback(not_found)
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, axum::Json(json!({ "status": "ok" })))
}

async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        axum::Json(json!({ "error": "method_not_allowed" })),
    )
}

async fn not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        axum::Json(json!({ "error": "not_found" })),
    )
}

async fn shutdown(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    // Origin/Host guard first (403): a cross-site page must not reach the
    // token check, let alone the shutdown side effect below.
    if let Some(denied) = loopback_guard(&headers, state.bound_port()) {
        return denied;
    }
    if !valid_token(headers.get("authorization"), state.token.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
    }
    // Signal the shared process-level token and return: the daemon main task
    // observes it and runs the single cleanup sequence (stop listener, close
    // backend, release instance lock). No partial cleanup runs here, so the
    // Ctrl-C and HTTP paths cannot diverge.
    state.shutdown.cancel();
    (
        StatusCode::ACCEPTED,
        axum::Json(json!({ "status": "stopping" })),
    )
        .into_response()
}

async fn mcp_session_request(
    State(state): State<Arc<AppState>>,
    method: axum::http::Method,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(denied) = mcp_guard(&headers, state.token.as_deref(), state.bound_port()) {
        return denied;
    }
    state.mcp.handle_session_request(&method, &headers).await
}

async fn mcp_admin_session_request(
    State(state): State<Arc<AppState>>,
    method: axum::http::Method,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(denied) = mcp_guard(&headers, state.token.as_deref(), state.bound_port()) {
        return denied;
    }
    state
        .mcp_admin
        .handle_session_request(&method, &headers)
        .await
}

async fn mcp_post(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    if let Some(denied) = mcp_guard(&headers, state.token.as_deref(), state.bound_port()) {
        return denied;
    }
    let body = match read_mcp_body(body).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    state.mcp.handle_post(&headers, &body).await
}

async fn mcp_admin_post(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    if let Some(denied) = mcp_guard(&headers, state.token.as_deref(), state.bound_port()) {
        return denied;
    }
    let body = match read_mcp_body(body).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    state.mcp_admin.handle_post(&headers, &body).await
}

/// Reads and validates one MCP body, preserving the pre-split status
/// mapping: oversized reads are 413, malformed JSON is 400.
async fn read_mcp_body(body: Body) -> Result<bytes::Bytes, axum::response::Response> {
    let Ok(bytes) = to_bytes(body, MAX_REQUEST_BYTES + 1).await else {
        return Err(rpc_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            -32700,
            "Request body too large.",
        ));
    };
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(rpc_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            -32700,
            "Request body too large.",
        ));
    }
    if serde_json::from_slice::<Value>(&bytes).is_err() {
        return Err(rpc_error(StatusCode::BAD_REQUEST, -32700, "Invalid JSON."));
    }
    Ok(bytes)
}

fn rpc_error(status: StatusCode, code: i32, message: &str) -> axum::response::Response {
    (status, axum::Json(json!({ "jsonrpc": "2.0", "error": { "code": code, "message": message }, "id": Value::Null }))).into_response()
}
