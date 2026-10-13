//! Shared loopback/auth guards for the daemon HTTP routes.
//!
//! Both `/mcp*` and `/control/*` use the same Origin/Host validation so the
//! two cannot diverge: a present Origin must be `http:` with a loopback host
//! (a missing Origin is allowed for non-browser clients such as curl and the
//! CLI), and the Host header must be loopback and carry the port the server
//! is actually bound to. Failures are 403 before any side effect or token
//! handling; only a request that passes this guard reaches the token check.

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use serde_json::json;
use subtle::ConstantTimeEq;

use crate::config::is_loopback_host;

/// The 403 denial shared by every guarded route.
fn forbidden_origin() -> axum::response::Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(json!({ "error": "forbidden_origin" })),
    )
        .into_response()
}

/// Shared loopback guard. `Some` is the denial response; `None` means the
/// request may proceed to the token check / endpoint.
pub(crate) fn loopback_guard(
    headers: &HeaderMap,
    bound_port: u16,
) -> Option<axum::response::Response> {
    if !valid_host(headers.get("host"), bound_port) || !valid_origin(headers.get("origin")) {
        return Some(forbidden_origin());
    }
    None
}

/// Shared loopback/auth guard for both MCP routes. `Some` is the denial
/// response; `None` means the request may proceed to the endpoint.
pub(crate) fn mcp_guard(
    headers: &HeaderMap,
    token: Option<&str>,
    bound_port: u16,
) -> Option<axum::response::Response> {
    if let Some(denied) = loopback_guard(headers, bound_port) {
        return Some(denied);
    }
    if !valid_token(headers.get("authorization"), token) {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
                axum::Json(json!({ "error": "unauthorized" })),
            )
                .into_response(),
        );
    }
    None
}

/// Missing Host fails closed; otherwise the hostname must be loopback and
/// the port must be the port the server is actually bound to (so a Host
/// pointing at some other local service is rejected, not just non-loopback
/// hosts). `bound_port` is never 0 for a serving listener, so an unbound
/// server fails closed too.
pub(crate) fn valid_host(header: Option<&HeaderValue>, bound_port: u16) -> bool {
    let Some(value) = header.and_then(|header| header.to_str().ok()) else {
        return false;
    };
    url::Url::parse(&format!("http://{value}"))
        .ok()
        .is_some_and(|url| {
            // `port_or_known_default` covers a Host that omits the port when
            // (and only when) the bound port is http's default 80.
            url.port_or_known_default() == Some(bound_port)
                && is_loopback_host(
                    url.host_str()
                        .unwrap_or("")
                        .trim_matches(&['[', ']'] as &[_]),
                )
        })
}

/// Missing Origin is allowed (non-browser clients); a present one must be
/// `http:` with a loopback host.
pub(crate) fn valid_origin(header: Option<&HeaderValue>) -> bool {
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
pub(crate) fn valid_token(header: Option<&HeaderValue>, expected: Option<&str>) -> bool {
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
