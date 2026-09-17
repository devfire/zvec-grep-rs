//! Daemon HTTP server: health, shutdown, and MCP routes with loopback/auth guards.
//!
//! Mirrors `../zvec-grep/src/daemon/http-server.ts` (`DaemonHttpServer`:
//! `GET /healthz`, `POST /control/shutdown`, `POST /mcp`, `POST
//! /mcp/admin`, loopback-host guard, bearer token, 1 MB body cap). The
//! axum port keeps every guard's status code and error body:
//! healthz is unauthenticated; shutdown demands loopback Host plus token
//! (401 otherwise); MCP routes additionally demand a loopback Origin
//! (403) and answer 401 with `WWW-Authenticate: Bearer`.
//!
//! `/mcp` and `/mcp/admin` serve the rmcp StreamableHTTP endpoint (plus
//! the TS legacy-session guards) behind the same loopback/auth guards.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::backend::DaemonBackend;
use crate::config::is_loopback_host;
use crate::errors::DaemonError;
use crate::mcp::http_transport::{McpHttpEndpoint, McpHttpEndpointOptions};
use crate::mcp::toolset::McpToolset;
use crate::sync::MutexExt;

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
    /// MCP toolset; defaults to the `agent` set.
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
    shutdown: CancellationToken,
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
            options.mcp_endpoint,
        )
        .map_err(|error| DaemonError::IndexFailed {
            message: format!("invalid MCP endpoint options: {error}"),
        })?;
        Ok(Self {
            state: Arc::new(AppState {
                token: options.token,
                backend: options.backend,
                mcp,
                shutdown: CancellationToken::new(),
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
            post(mcp_post)
                .get(mcp_session_request)
                .delete(mcp_session_request),
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
    if !valid_host(headers.get("host"))
        || !valid_token(headers.get("authorization"), state.token.as_deref())
    {
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
    if !valid_host(headers.get("host")) || !valid_origin(headers.get("origin")) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({ "error": "forbidden_origin" })),
        )
            .into_response();
    }
    if !valid_token(headers.get("authorization"), state.token.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
            axum::Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
    }
    state.mcp.handle_session_request(&method, &headers).await
}

async fn mcp_post(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    if !valid_host(headers.get("host")) || !valid_origin(headers.get("origin")) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({ "error": "forbidden_origin" })),
        )
            .into_response();
    }
    if !valid_token(headers.get("authorization"), state.token.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
            axum::Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
    }
    let bytes = match to_bytes(body, MAX_REQUEST_BYTES + 1).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return rpc_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                -32700,
                "Request body too large.",
            );
        }
    };
    if bytes.len() > MAX_REQUEST_BYTES {
        return rpc_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            -32700,
            "Request body too large.",
        );
    }
    if serde_json::from_slice::<Value>(&bytes).is_err() {
        return rpc_error(StatusCode::BAD_REQUEST, -32700, "Invalid JSON.");
    }
    state.mcp.handle_post(&headers, &bytes).await
}

fn rpc_error(status: StatusCode, code: i32, message: &str) -> axum::response::Response {
    (status, axum::Json(json!({ "jsonrpc": "2.0", "error": { "code": code, "message": message }, "id": Value::Null }))).into_response()
}

/// Missing Host fails closed; otherwise the hostname must be loopback.
fn valid_host(header: Option<&axum::http::HeaderValue>) -> bool {
    let Some(value) = header.and_then(|header| header.to_str().ok()) else {
        return false;
    };
    url::Url::parse(&format!("http://{value}"))
        .ok()
        .map(|url| {
            url.host_str()
                .unwrap_or("")
                .trim_matches(&['[', ']'] as &[_])
                .to_owned()
        })
        .is_some_and(|host| is_loopback_host(&host))
}

/// Missing Origin is allowed (non-browser clients); a present one must be
/// `http:` with a loopback host.
fn valid_origin(header: Option<&axum::http::HeaderValue>) -> bool {
    let Some(value) = header.and_then(|header| header.to_str().ok()) else {
        return true;
    };
    url::Url::parse(value).ok().is_some_and(|url| {
        url.scheme() == "http"
            && is_loopback_host(
                url.host_str()
                    .unwrap_or("")
                    .trim_matches(&['[', ']'] as &[_]),
            )
    })
}

/// Anonymous mode (`None` expected) allows everything; otherwise
/// constant-time `Bearer` comparison (mirrors TS `timingSafeEqual`).
fn valid_token(header: Option<&axum::http::HeaderValue>, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let Some(actual) = header.and_then(|header| header.to_str().ok()) else {
        return false;
    };
    let Some(presented) = actual.strip_prefix("Bearer ") else {
        return false;
    };
    // Length is not secret: early exit, then constant-time content
    // comparison via `subtle` (same policy as the grant-signature check in
    // `zg_core::authorization`).
    if presented.len() != expected.len() {
        return false;
    }
    presented.as_bytes().ct_eq(expected.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{DaemonBackend, DaemonBackendOptions};

    fn server(token: Option<&str>) -> DaemonHttpServer {
        DaemonHttpServer::new(DaemonHttpServerOptions {
            host: "127.0.0.1".to_owned(),
            port: 0,
            token: token.map(str::to_owned),
            backend: DaemonBackend::new(DaemonBackendOptions::default()),
            ..DaemonHttpServerOptions::default()
        })
        .unwrap()
    }

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[test]
    fn rejects_non_loopback_listen() {
        assert!(matches!(
            DaemonHttpServer::new(DaemonHttpServerOptions {
                host: "0.0.0.0".to_owned(),
                port: 0,
                token: None,
                backend: DaemonBackend::new(DaemonBackendOptions::default()),
                ..DaemonHttpServerOptions::default()
            }),
            Err(DaemonError::LoopbackRequired { .. })
        ));
    }

    #[test]
    fn host_and_origin_guards() {
        use axum::http::HeaderValue;
        let loopback = HeaderValue::from_static("127.0.0.1:7999");
        let remote = HeaderValue::from_static("example.com");
        assert!(valid_host(Some(&loopback)));
        assert!(!valid_host(Some(&remote)));
        assert!(!valid_host(None));
        assert!(valid_origin(None));
        assert!(valid_origin(Some(&HeaderValue::from_static(
            "http://127.0.0.1:3000"
        ))));
        assert!(!valid_origin(Some(&HeaderValue::from_static(
            "http://example.com"
        ))));
        assert!(!valid_origin(Some(&HeaderValue::from_static(
            "https://127.0.0.1:3000"
        ))));
        assert!(valid_token(None, None));
        assert!(!valid_token(None, Some("secret")));
        assert!(valid_token(
            Some(&HeaderValue::from_static("Bearer secret")),
            Some("secret")
        ));
        assert!(!valid_token(
            Some(&HeaderValue::from_static("Bearer wrong")),
            Some("secret")
        ));
        assert!(!valid_token(
            Some(&HeaderValue::from_static("Bearer secert")),
            Some("secret")
        ));
        assert!(!valid_token(
            Some(&HeaderValue::from_static("Bearer sec")),
            Some("secret")
        ));
    }

    #[tokio::test]
    async fn healthz_is_public_and_shutdown_needs_token() {
        let server = server(Some(&"t".repeat(32)));
        let address = server.start().await.unwrap();
        let base = format!("http://{address}");
        let health: Value = client()
            .get(format!("{base}/healthz"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(health, json!({ "status": "ok" }));
        // No token: 401.
        let denied = client()
            .post(format!("{base}/control/shutdown"))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        // Wrong token: 401.
        let denied = client()
            .post(format!("{base}/control/shutdown"))
            .header("authorization", "Bearer wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        server.close().await;
    }

    #[tokio::test]
    async fn oversized_mcp_bodies_are_rejected() {
        let server = server(None);
        let address = server.start().await.unwrap();
        let base = format!("http://{address}");
        let big = vec![b'x'; MAX_REQUEST_BYTES + 1];
        let response = client()
            .post(format!("{base}/mcp"))
            .header("content-type", "application/json")
            .body(big)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        // Small invalid JSON is a 400, proving the route is otherwise live.
        let response = client()
            .post(format!("{base}/mcp"))
            .header("content-type", "application/json")
            .body("not json")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        server.close().await;
    }

    #[tokio::test]
    async fn shutdown_stops_the_server() {
        let server = server(None);
        let address = server.start().await.unwrap();
        let base = format!("http://{address}");
        let accepted = client()
            .post(format!("{base}/control/shutdown"))
            .send()
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        server.close().await;
        assert!(server.bound_address().is_none());
    }
}
