//! MCP HTTP endpoint: a stateful rmcp `StreamableHTTP` service with the
//! TS legacy-session guards in front.
//!
//! Mirrors `../zvec-grep/src/mcp/http-transport.ts` (`McpHttpEndpoint`:
//! legacy session LRU, 256-entry cap, 30-minute idle TTL, modern +
//! legacy POST routing, session GET/DELETE). rmcp's stateful session
//! manager owns the protocol sessions; this endpoint adds the TS
//! observable behavior rmcp does not provide: unknown session → 404,
//! session-less non-initialize POST → 400, session-less GET → 405,
//! initialize past the session cap → 503 (rmcp answers these with 401).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::IntoResponse;
use bytes::Bytes;
use http_body_util::Full;
use rmcp::transport::streamable_http_server::session::local::{LocalSessionManager, SessionConfig};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::sync::RwLock;

use crate::backend::DaemonBackend;
use crate::mcp::error::McpError;
use crate::mcp::tools::ZvecGrepMcpServer;
use crate::mcp::toolset::McpToolset;

/// Default legacy session cap (mirrors `MAX_LEGACY_SESSIONS`).
pub const DEFAULT_MAX_LEGACY_SESSIONS: usize = 256;
/// Default legacy session idle TTL (mirrors
/// `LEGACY_SESSION_IDLE_TTL_MS`).
pub const DEFAULT_LEGACY_SESSION_IDLE_TTL: Duration = Duration::from_secs(30 * 60);

/// Options for [`McpHttpEndpoint`], mirroring `McpHttpEndpointOptions`.
#[derive(Debug, Clone)]
pub struct McpHttpEndpointOptions {
    /// Maximum live legacy sessions.
    pub max_legacy_sessions: usize,
    /// Idle TTL for legacy sessions.
    pub legacy_session_idle_ttl: Duration,
}

impl Default for McpHttpEndpointOptions {
    fn default() -> Self {
        Self {
            max_legacy_sessions: DEFAULT_MAX_LEGACY_SESSIONS,
            legacy_session_idle_ttl: DEFAULT_LEGACY_SESSION_IDLE_TTL,
        }
    }
}

/// Daemon-wide legacy-session cap, shared by every endpoint (`/mcp` and
/// `/mcp/admin`) so the ceiling is not per-endpoint (#93). A slot is
/// reserved atomically before an initialize is forwarded and held until
/// forwarding returns; by then a created session is counted in its
/// manager's live map, so concurrent initializes cannot all observe a
/// stale count below the cap and overshoot it.
pub struct LegacySessionGate {
    max: usize,
    reserved: AtomicUsize,
    gate: tokio::sync::Mutex<()>,
    managers: std::sync::Mutex<Vec<Arc<LocalSessionManager>>>,
}

impl LegacySessionGate {
    pub(crate) fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            max,
            reserved: AtomicUsize::new(0),
            gate: tokio::sync::Mutex::new(()),
            managers: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn register(&self, sessions: &Arc<LocalSessionManager>) {
        if let Ok(mut managers) = self.managers.lock() {
            managers.push(Arc::clone(sessions));
        }
    }

    async fn live_count(&self) -> usize {
        let managers: Vec<Arc<LocalSessionManager>> = self
            .managers
            .lock()
            .map(|managers| managers.clone())
            .unwrap_or_default();
        let mut total = 0usize;
        for manager in &managers {
            total = total.saturating_add(manager.sessions.read().await.len());
        }
        total
    }

    /// Atomically reserves one slot when live + reserved is below the cap.
    async fn try_reserve(self: &Arc<Self>) -> Option<SessionReservation> {
        let _lock = self.gate.lock().await;
        if self.live_count().await + self.reserved.load(Ordering::SeqCst) >= self.max {
            return None;
        }
        self.reserved.fetch_add(1, Ordering::SeqCst);
        Some(SessionReservation {
            gate: Arc::clone(self),
        })
    }
}

/// One reserved session slot; releases on drop.
struct SessionReservation {
    gate: Arc<LegacySessionGate>,
}

impl Drop for SessionReservation {
    fn drop(&mut self) {
        self.gate.reserved.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Stateful MCP HTTP endpoint: one rmcp service plus the TS session
/// guards. Shared by reference behind the axum routes.
#[derive(Clone)]
pub struct McpHttpEndpoint {
    service: StreamableHttpService<
        rmcp::handler::server::router::Router<ZvecGrepMcpServer>,
        LocalSessionManager,
    >,
    sessions: Arc<LocalSessionManager>,
    session_gate: Arc<LegacySessionGate>,
}

impl McpHttpEndpoint {
    /// Builds the endpoint: session manager with the idle TTL, one
    /// stateful rmcp service whose factory mints a fresh tool router per
    /// session. Non-positive limits fail like the TS `RangeError`.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the legacy session limits are non-positive.
    pub fn new(
        backend: DaemonBackend,
        version: String,
        toolset: McpToolset,
        options: &McpHttpEndpointOptions,
    ) -> Result<Self, McpError> {
        Self::with_session_gate(
            backend,
            version,
            toolset,
            options,
            LegacySessionGate::new(options.max_legacy_sessions),
        )
    }

    /// Endpoint sharing `session_gate` with sibling endpoints, so the
    /// legacy-session cap is daemon-wide rather than per-endpoint (#93).
    pub(crate) fn with_session_gate(
        backend: DaemonBackend,
        version: String,
        toolset: McpToolset,
        options: &McpHttpEndpointOptions,
        session_gate: Arc<LegacySessionGate>,
    ) -> Result<Self, McpError> {
        if options.max_legacy_sessions == 0 || options.legacy_session_idle_ttl.is_zero() {
            return Err(McpError::invalid_params(
                "Legacy MCP session limits must be positive.",
            ));
        }
        let sessions = Arc::new(LocalSessionManager {
            sessions: RwLock::new(HashMap::new()),
            session_config: SessionConfig {
                channel_capacity: SessionConfig::DEFAULT_CHANNEL_CAPACITY,
                keep_alive: Some(options.legacy_session_idle_ttl),
            },
        });
        let service = StreamableHttpService::new(
            move || Ok(ZvecGrepMcpServer::new(backend.clone(), version.clone(), toolset).router()),
            Arc::clone(&sessions),
            StreamableHttpServerConfig {
                sse_keep_alive: Some(Duration::from_secs(15)),
                stateful_mode: true,
            },
        );
        session_gate.register(&sessions);
        Ok(Self {
            service,
            sessions,
            session_gate,
        })
    }

    /// Live session count (cap enforcement, tests).
    pub async fn session_count(&self) -> usize {
        self.sessions.sessions.read().await.len()
    }

    /// Handles `POST /mcp` (and `/mcp/admin`) after the daemon guards:
    /// legacy pre-checks first, then the rmcp service.
    pub async fn handle_post(&self, headers: &HeaderMap, body: &[u8]) -> axum::response::Response {
        if let Some(id) = session_id(headers) {
            if !self.has_session(&id).await {
                return mcp_error(StatusCode::NOT_FOUND, "Unknown or expired MCP session.");
            }
            return self.forward(Method::POST, headers, body).await;
        }
        if !is_initialize_request(body) {
            return mcp_error(
                StatusCode::BAD_REQUEST,
                "An MCP initialize request is required before other requests.",
            );
        }
        // Reserve before forwarding: the session is only created inside
        // rmcp during `forward`, so a bare count check is check-then-act
        // and concurrent initializes overshoot the cap (#93). The
        // reservation drops after forwarding, when a created session is
        // already visible in the live count.
        let Some(_reservation) = self.session_gate.try_reserve().await else {
            return mcp_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "MCP legacy session limit reached.",
            );
        };
        self.forward(Method::POST, headers, body).await
    }

    /// Handles `GET`/`DELETE /mcp` (and `/mcp/admin`): session lookup
    /// first, then the rmcp service.
    pub async fn handle_session_request(
        &self,
        method: &Method,
        headers: &HeaderMap,
    ) -> axum::response::Response {
        let Some(id) = session_id(headers) else {
            let status = if *method == Method::GET {
                StatusCode::METHOD_NOT_ALLOWED
            } else {
                StatusCode::BAD_REQUEST
            };
            return status.into_response();
        };
        if !self.has_session(&id).await {
            return mcp_error(StatusCode::NOT_FOUND, "Unknown or expired MCP session.");
        }
        self.forward(method.clone(), headers, &[]).await
    }

    async fn has_session(&self, id: &str) -> bool {
        let sessions = self.sessions.sessions.read().await;
        sessions
            .contains_key(&rmcp::transport::streamable_http_server::session::SessionId::from(id))
    }

    async fn forward(
        &self,
        method: Method,
        headers: &HeaderMap,
        body: &[u8],
    ) -> axum::response::Response {
        let mut builder = axum::http::Request::builder().method(method).uri("/mcp");
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let Ok(request) = builder.body(Full::new(Bytes::copy_from_slice(body))) else {
            return mcp_error(StatusCode::BAD_REQUEST, "Invalid MCP request.");
        };
        let response = self.service.handle(request).await;
        // Preserve the rmcp status and headers (notably the
        // `mcp-session-id` response header); SSE streams additionally
        // opt out of intermediary buffering.
        let (parts, body) = response.into_parts();
        let mut forwarded = axum::body::Body::new(body).into_response();
        *forwarded.status_mut() = parts.status;
        *forwarded.headers_mut() = parts.headers;
        forwarded.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        forwarded
    }
}

/// `mcp-session-id` request header, when present and non-empty.
fn session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// True for initialize requests: a single `{method: "initialize"}` or a
/// batch containing one.
fn is_initialize_request(body: &[u8]) -> bool {
    let value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return false,
    };
    match &value {
        serde_json::Value::Object(_) => is_initialize(&value),
        serde_json::Value::Array(items) => items.iter().any(is_initialize),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

fn is_initialize(value: &serde_json::Value) -> bool {
    value.get("method").and_then(|method| method.as_str()) == Some("initialize")
}

/// TS-shaped `writeMcpError`: JSON-RPC `-32000` body with no-store.
fn mcp_error(status: StatusCode, message: &str) -> axum::response::Response {
    (
        status,
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        axum::Json(serde_json::json!({
            "jsonrpc": "2.0",
            "error": { "code": -32000, "message": message },
            "id": null,
        })),
    )
        .into_response()
}
