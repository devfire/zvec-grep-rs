//! Transport client: mode routing, search policies, and the daemon
//! JSON-RPC client (`client/` in TypeScript).
//!
//! The TypeScript `DaemonClient` drives `@modelcontextprotocol/client`
//! over StreamableHTTP with elicitation and progress heartbeats. The port
//! speaks raw JSON-RPC 2.0 over reqwest with the same framing the
//! `zg-server` endpoint tests pin (initialize → `notifications/initialized`
//! → `tools/call`, `mcp-session-id` header, plain-JSON or SSE bodies) —
//! no rmcp client feature needed, and the TLS stack stays the
//! workspace-standard rustls one (see `docs/ts-divergence.md`).
//! Elicitation is not wired: the server fails closed with a
//! `zg auth grant` directive, which the CLI surfaces verbatim.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use zg_core::config::ClientMode;

use crate::cli::{ClientModeArg, RefreshMode};
use crate::error::CliError;

/// Default daemon URL, mirroring `configuredServerUrl`.
///
/// `process.env.ZVEC_GREP_SERVER_URL` wins, else the global-config
/// server section, else loopback:7999.
pub const SERVER_URL_ENV: &str = "ZVEC_GREP_SERVER_URL";

/// Environment variable selecting the client mode.
pub const MODE_ENV: &str = "ZVEC_GREP_MODE";

/// MCP protocol version sent in `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Upper bound for one daemon tool call: heartbeats make the TS wait
/// open-ended; the CLI keeps a practical bound instead.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Byte-identical `server-search.ts` incompatibility text.
pub const INCOMPATIBLE_SERVER_SEARCH_MESSAGE: &str = "The running zvec-grep server is incompatible with grouped CLI query output. Restart the currently configured daemon after upgrading zvec-grep, then retry.";

/// Resolves explicit flag → environment → global config → `auto`,
/// mirroring `resolveClientMode`.
pub fn resolve_client_mode(explicit: Option<ClientModeArg>) -> Result<ClientMode, CliError> {
    if let Some(mode) = explicit {
        return Ok(match mode {
            ClientModeArg::Direct => ClientMode::Direct,
            ClientModeArg::Server => ClientMode::Server,
            ClientModeArg::Auto => ClientMode::Auto,
        });
    }
    if let Some(value) = std::env::var(MODE_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    {
        return parse_mode_value(&value);
    }
    let path = zg_core::config::global_config_path();
    let configured = zg_core::config::read_global_config(&path)
        .ok()
        .and_then(|config| config.client)
        .and_then(|client| client.mode);
    Ok(configured.unwrap_or(ClientMode::Auto))
}

/// Parses a `ZVEC_GREP_MODE` value with the TS-verbatim rejection.
pub fn parse_mode_value(value: &str) -> Result<ClientMode, CliError> {
    match value {
        "direct" => Ok(ClientMode::Direct),
        "server" => Ok(ClientMode::Server),
        "auto" => Ok(ClientMode::Auto),
        _ => Err(CliError::usage(
            "ZVEC_GREP_MODE must be direct, server, or auto",
        )),
    }
}

/// Resolves the daemon base URL: environment → global config → default,
/// mirroring `resolveServerUrl`.
pub fn resolve_server_url() -> String {
    if let Some(url) = std::env::var(SERVER_URL_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        return url;
    }
    let path = zg_core::config::global_config_path();
    if let Ok(config) = zg_core::config::read_global_config(&path) {
        if let Some(url) = config
            .client
            .as_ref()
            .and_then(|client| client.server_url.clone())
        {
            return url;
        }
        if let Some(server) = config.server {
            let host = server.host.as_deref().unwrap_or("127.0.0.1");
            let port = server.port.unwrap_or(7999);
            return format!("http://{host}:{port}");
        }
    }
    "http://127.0.0.1:7999".to_owned()
}

/// Routes one operation by mode, mirroring `routeByMode`: direct and
/// server run as given; auto probes the daemon first.
pub async fn route_by_mode<T>(
    mode: ClientMode,
    direct: impl Future<Output = Result<T, CliError>>,
    server: impl Future<Output = Result<T, CliError>>,
    server_available: impl Future<Output = bool>,
) -> Result<T, CliError> {
    match mode {
        ClientMode::Direct => direct.await,
        ClientMode::Server => server.await,
        ClientMode::Auto => {
            if server_available.await {
                server.await
            } else {
                direct.await
            }
        }
    }
}

/// Server-side freshness for one search, mirroring `ServerSearchPolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerSearchPolicy {
    /// `eventual` or `wait_for_fresh` on the wire.
    pub freshness: SearchFreshness,
    /// Whether an eventual search may schedule a background refresh.
    pub auto_update: bool,
}

/// Direct-mode freshness, mirroring `DirectSearchPolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectSearchPolicy {
    /// `eventual` or `wait_for_fresh` on the wire.
    pub freshness: SearchFreshness,
    /// Direct mode only refreshes while waiting.
    pub auto_update: bool,
}

/// Wire freshness values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFreshness {
    /// Search immediately.
    Eventual,
    /// Wait for a fresh index first.
    WaitForFresh,
}

impl SearchFreshness {
    /// Wire string for the `freshness` tool argument.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Eventual => "eventual",
            Self::WaitForFresh => "wait_for_fresh",
        }
    }
}

/// Resolves the server search policy: `wait` waits for fresh, `off`
/// disables the background refresh, otherwise eventual + auto-update.
pub fn resolve_server_search_policy(refresh: Option<RefreshMode>) -> ServerSearchPolicy {
    match refresh.unwrap_or(RefreshMode::Background) {
        RefreshMode::Wait => ServerSearchPolicy {
            freshness: SearchFreshness::WaitForFresh,
            auto_update: true,
        },
        RefreshMode::Off => ServerSearchPolicy {
            freshness: SearchFreshness::Eventual,
            auto_update: false,
        },
        RefreshMode::Background => ServerSearchPolicy {
            freshness: SearchFreshness::Eventual,
            auto_update: true,
        },
    }
}

/// Resolves the direct search policy: only `wait` refreshes.
pub fn resolve_direct_search_policy(refresh: Option<RefreshMode>) -> DirectSearchPolicy {
    let wait = refresh == Some(RefreshMode::Wait);
    DirectSearchPolicy {
        freshness: if wait {
            SearchFreshness::WaitForFresh
        } else {
            SearchFreshness::Eventual
        },
        auto_update: wait,
    }
}

/// One tool result: concatenated text plus structured content, if any.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Concatenated `content[].text`.
    pub text: String,
    /// `structuredContent`, when the tool returned any.
    pub structured: Value,
}

/// Raw JSON-RPC daemon client over the `/mcp` endpoint.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl DaemonClient {
    /// Builds a client for `server_url` (trailing slashes trimmed).
    pub fn new(server_url: &str, token: Option<String>) -> Self {
        Self {
            base_url: server_url.trim_end_matches('/').to_owned(),
            token,
            http: reqwest::Client::new(),
        }
    }

    /// Builds a client from the environment: resolved server URL plus
    /// the daemon client token (env, explicit file, or default file).
    pub fn from_env(token_file: Option<PathBuf>, home: Option<&Path>) -> Result<Self, CliError> {
        let token = zg_server::config::resolve_client_token(token_file, home)?;
        Ok(Self::new(&resolve_server_url(), token))
    }

    /// True when `GET /healthz` succeeds.
    pub async fn server_available(&self) -> bool {
        self.http
            .get(format!("{}/healthz", self.base_url))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    /// Calls one MCP tool: initialize → notify → `tools/call` → close.
    ///
    /// The session is always closed before returning — on success, tool
    /// failure, and transport failure — so repeated CLI calls never
    /// accumulate live daemon sessions toward the 256-session cap. A
    /// close failure is logged to stderr and never masks the primary
    /// result.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult, CliError> {
        let session = self.initialize().await?;
        let outcome = self.call_tool_with_session(name, arguments, &session).await;
        self.close_session(&session).await;
        outcome
    }

    /// Runs `tools/call` on an open session; the caller owns closing it.
    async fn call_tool_with_session(
        &self,
        name: &str,
        arguments: Value,
        session: &str,
    ) -> Result<ToolResult, CliError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        });
        let message = self.post(&body, Some(session)).await?;
        if let Some(error) = message.get("error") {
            return Err(tool_error(name, error));
        }
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        Ok(ToolResult {
            text: tool_text(&result),
            structured: result
                .get("structuredContent")
                .cloned()
                .unwrap_or(Value::Null),
        })
    }

    /// Best-effort session close: `DELETE /mcp` with the session id.
    ///
    /// Never fails: transport errors and unexpected statuses are logged
    /// to stderr so a close failure cannot mask the primary call result.
    /// A 404 means the session is already gone and counts as closed.
    async fn close_session(&self, session: &str) {
        let mut request = self
            .http
            .delete(format!("{}/mcp", self.base_url))
            .header("origin", self.base_url.clone())
            .timeout(Duration::from_secs(5));
        if let Some(token) = &self.token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        request = request.header("mcp-session-id", session);
        match request.send().await {
            Err(error) => {
                eprintln!("warning: failed to close the MCP session: {error}");
            }
            Ok(response) => {
                let status = response.status();
                if !status.is_success() && status.as_u16() != 404 {
                    eprintln!("warning: failed to close the MCP session (HTTP {status})");
                }
            }
        }
    }

    /// Opens a session: `initialize` returns the session id, then the
    /// client sends `notifications/initialized`.
    async fn initialize(&self) -> Result<String, CliError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "zvec-grep-cli", "version": env!("CARGO_PKG_VERSION")},
            },
        });
        let response = self.post_raw(&body, None).await?;
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| {
                CliError::daemon_unavailable(format!(
                    "zvec-grep server at {} did not open an MCP session",
                    self.base_url
                ))
            })?;
        let notified = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let ack = match self.post_raw(&notified, Some(&session)).await {
            Ok(ack) => ack,
            Err(error) => {
                // The server created the session before the handshake send
                // failed; close it so the partial open never leaks.
                self.close_session(&session).await;
                return Err(error);
            }
        };
        if !ack.status().is_success() {
            let status = ack.status();
            self.close_session(&session).await;
            return Err(CliError::daemon_unavailable(format!(
                "zvec-grep server at {} rejected the MCP handshake (HTTP {status})",
                self.base_url,
            )));
        }
        Ok(session)
    }

    async fn post(&self, body: &Value, session: Option<&str>) -> Result<Value, CliError> {
        let response = self.post_raw(body, session).await?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            CliError::daemon_unavailable(format!("failed to read the daemon response: {error}"))
        })?;
        if !status.is_success() {
            return Err(http_error(&self.base_url, status.as_u16(), &text));
        }
        Ok(parse_message(&text))
    }

    async fn post_raw(
        &self,
        body: &Value,
        session: Option<&str>,
    ) -> Result<reqwest::Response, CliError> {
        let mut request = self
            .http
            .post(format!("{}/mcp", self.base_url))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("origin", self.base_url.clone())
            .timeout(CALL_TIMEOUT)
            .json(body);
        if let Some(token) = &self.token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        request.send().await.map_err(|error| {
            CliError::daemon_unavailable(format!(
                "zvec-grep server at {} is not reachable: {error}",
                self.base_url
            ))
        })
    }
}

/// Maps a JSON-RPC tool error to a CLI error, preserving the server
/// message (including the `zg auth grant` directive on refusals).
fn tool_error(tool: &str, error: &Value) -> CliError {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the daemon rejected the request");
    CliError::usage(format!("{tool} failed: {message}"))
}

/// Maps a non-2xx MCP HTTP status to a CLI error.
fn http_error(base_url: &str, status: u16, body: &str) -> CliError {
    let message = parse_message(body)
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| body.trim().to_owned());
    match status {
        401 => CliError::daemon_unavailable(format!(
            "zvec-grep server at {base_url} rejected the bearer token: {message}"
        )),
        403 => CliError::daemon_unavailable(format!(
            "zvec-grep server at {base_url} refused the request: {message}"
        )),
        404 => CliError::daemon_unavailable(format!(
            "zvec-grep server at {base_url} has no such session; restart the daemon and retry: {message}"
        )),
        _ => CliError::daemon_unavailable(format!(
            "zvec-grep server at {base_url} answered HTTP {status}: {message}"
        )),
    }
}

/// Concatenates `content[]` text blocks.
fn tool_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Parses one JSON-RPC message: plain JSON, or the last SSE `data:`
/// frame when the transport streams.
fn parse_message(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).unwrap_or(Value::Null);
    }
    let mut last = Value::Null;
    for line in trimmed.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if !data.is_empty() && data != "[DONE]" {
                last = serde_json::from_str(data).unwrap_or(Value::Null);
            }
        }
    }
    last
}

/// Interprets a `zvec_grep_search` response for CLI printing.
///
/// Structured groups win when a server returns them; text-only responses
/// (what the current daemon returns) print as-is; anything else is the
/// frozen incompatibility error.
pub fn parse_server_search_response(result: &ToolResult) -> Result<ServerSearchBody, CliError> {
    if let Some(groups) = result
        .structured
        .get("result")
        .and_then(|result| result.get("groupResults"))
        .and_then(Value::as_array)
        && !groups.is_empty()
    {
        return Ok(ServerSearchBody::Groups(result.structured.clone()));
    }
    if !result.text.trim().is_empty() {
        return Ok(ServerSearchBody::Text(result.text.clone()));
    }
    Err(CliError::server_incompatible(
        INCOMPATIBLE_SERVER_SEARCH_MESSAGE,
    ))
}

/// Either structured search groups or pre-rendered text.
#[derive(Debug, Clone)]
pub enum ServerSearchBody {
    /// Structured `structuredContent` with non-empty `groupResults`.
    Groups(Value),
    /// Pre-rendered response text.
    Text(String),
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mode_resolution_order() {
        assert!(matches!(
            resolve_client_mode(Some(ClientModeArg::Server)).unwrap(),
            ClientMode::Server
        ));
    }

    #[test]
    fn mode_env_rejection_uses_ts_text() {
        let error = parse_mode_value("sometimes").expect_err("bad mode must fail");
        assert_eq!(
            error.to_string(),
            "ZVEC_GREP_MODE must be direct, server, or auto"
        );
        assert!(matches!(
            parse_mode_value("direct").unwrap(),
            ClientMode::Direct
        ));
    }

    #[test]
    fn search_policies_match_ts() {
        let policy = resolve_server_search_policy(None);
        assert_eq!(policy.freshness, SearchFreshness::Eventual);
        assert!(policy.auto_update);
        let policy = resolve_server_search_policy(Some(RefreshMode::Wait));
        assert_eq!(policy.freshness, SearchFreshness::WaitForFresh);
        assert!(policy.auto_update);
        let policy = resolve_server_search_policy(Some(RefreshMode::Off));
        assert!(!policy.auto_update);
        let direct = resolve_direct_search_policy(Some(RefreshMode::Background));
        assert!(!direct.auto_update);
        let direct = resolve_direct_search_policy(Some(RefreshMode::Wait));
        assert!(direct.auto_update);
    }

    #[tokio::test]
    async fn routes_by_mode() {
        async fn direct() -> Result<&'static str, CliError> {
            Ok("direct")
        }
        async fn server() -> Result<&'static str, CliError> {
            Ok("server")
        }
        assert_eq!(
            route_by_mode(ClientMode::Direct, direct(), server(), async { false })
                .await
                .unwrap(),
            "direct"
        );
        assert_eq!(
            route_by_mode(ClientMode::Server, direct(), server(), async { false })
                .await
                .unwrap(),
            "server"
        );
        assert_eq!(
            route_by_mode(ClientMode::Auto, direct(), server(), async { true })
                .await
                .unwrap(),
            "server"
        );
        assert_eq!(
            route_by_mode(ClientMode::Auto, direct(), server(), async { false })
                .await
                .unwrap(),
            "direct"
        );
    }

    #[test]
    fn server_search_bodies() {
        let grouped = ToolResult {
            text: String::new(),
            structured: json!({"result": {"groupResults": [{"id": "g"}]}}),
        };
        assert!(matches!(
            parse_server_search_response(&grouped).unwrap(),
            ServerSearchBody::Groups(_)
        ));
        let text = ToolResult {
            text: "freshness: fresh\n\n# hits".to_owned(),
            structured: Value::Null,
        };
        let body = parse_server_search_response(&text).unwrap();
        assert!(matches!(body, ServerSearchBody::Text(_)));
        let empty = ToolResult {
            text: String::new(),
            structured: Value::Null,
        };
        let error = parse_server_search_response(&empty).expect_err("empty must fail");
        assert_eq!(error.to_string(), INCOMPATIBLE_SERVER_SEARCH_MESSAGE);
    }

    #[test]
    fn contract_violation_mentions_the_tool() {
        let error = tool_error("zvec_grep_search", &json!({"message": "boom"}));
        assert_eq!(error.to_string(), "zvec_grep_search failed: boom");
    }
}
