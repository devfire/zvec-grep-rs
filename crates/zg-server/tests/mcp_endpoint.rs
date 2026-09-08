//! MCP HTTP endpoint proofs: modern session lifecycle, legacy guards,
//! toolset gating, bound rejection, and tool round trips.
//!
//! Raw JSON-RPC over reqwest (no rmcp client): every status code and
//! session header is asserted directly, mirroring the TS
//! `mcp-modern-http` and `mcp-legacy-http` suites.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use serde_json::{Value, json};
use zg_core::models::stub::StubEmbeddingModel;
use zg_server::backend::{DaemonBackend, DaemonBackendOptions, IndexInput, ServiceConfig};
use zg_server::http_server::{DaemonHttpServer, DaemonHttpServerOptions};
use zg_server::mcp::http_transport::McpHttpEndpointOptions;
use zg_server::mcp::toolset::McpToolset;

const JSON: &str = "application/json";
const ACCEPT: &str = "application/json, text/event-stream";

fn stub_backend() -> DaemonBackend {
    DaemonBackend::new(DaemonBackendOptions {
        service: ServiceConfig {
            model_override: Some(Arc::new(StubEmbeddingModel::new(16))),
            ..ServiceConfig::default()
        },
        ..DaemonBackendOptions::default()
    })
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "fn beta() {}\n").unwrap();
    dir
}

async fn start(
    backend: DaemonBackend,
    toolset: McpToolset,
    endpoint: McpHttpEndpointOptions,
) -> (DaemonHttpServer, String) {
    let server = DaemonHttpServer::new(DaemonHttpServerOptions {
        host: "127.0.0.1".to_owned(),
        port: 0,
        token: None,
        backend,
        mcp_toolset: toolset,
        mcp_endpoint: endpoint,
        version: "0.0.0-test".to_owned(),
    })
    .unwrap();
    let address = server.start().await.unwrap();
    let base = format!("http://{address}");
    (server, base)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn post(base: &str, body: &Value, session: Option<&str>) -> reqwest::Response {
    let mut request = client()
        .post(format!("{base}/mcp"))
        .header("content-type", JSON)
        .header("accept", ACCEPT)
        .json(body);
    if let Some(id) = session {
        request = request.header("mcp-session-id", id);
    }
    request.send().await.unwrap()
}

/// Single JSON-RPC message from a POST body (plain JSON; SSE `data:`
/// framing unwrapped when the transport streams).
async fn message(response: reqwest::Response) -> (reqwest::StatusCode, Value) {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    (status, parse_message(&text))
}

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

fn initialize_body(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0.0.0"}
        }
    })
}

fn initialized_notification() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
}

fn call_body(id: u64, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    })
}

fn list_body(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}})
}

/// Initialize → notify → returns the session id.
async fn initialize(base: &str) -> String {
    let response = post(base, &initialize_body(1), None).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let session = response
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let notified = post(base, &initialized_notification(), Some(&session)).await;
    assert!(
        notified.status().is_success(),
        "initialized: {}",
        notified.status()
    );
    session
}

async fn index_fixture(backend: &DaemonBackend, root: &str) {
    let submitted = backend
        .index(
            root,
            IndexInput {
                rebuild: true,
                ..IndexInput::default()
            },
        )
        .await
        .unwrap();
    let terminal = backend
        .scheduler()
        .wait(&submitted.job.id, None)
        .await
        .unwrap();
    assert_eq!(
        terminal.state,
        zg_core::index_status::IndexJobState::Succeeded
    );
    // The scheduler marks the job terminal before the root actor applies
    // the finished payload; poll the actor-side status so a search issued
    // right after this helper cannot observe a missing index (the same
    // actor-apply race `wait_for_index` bridges in production). Without
    // this, slow runners fail the follow-up search with an error envelope
    // instead of tool content.
    for _ in 0..100 {
        let status = backend.index_status(root).await.unwrap();
        let settled = status
            .job
            .as_ref()
            .is_some_and(|live| live.id == submitted.job.id && live.is_terminal());
        if settled {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("index job {} never settled actor-side", submitted.job.id);
}

#[tokio::test]
async fn modern_lifecycle_lists_and_calls_tools() {
    let backend = stub_backend();
    let dir = fixture();
    let root = dir.path().to_string_lossy().into_owned();
    index_fixture(&backend, &root).await;
    let (server, base) = start(backend, McpToolset::Full, McpHttpEndpointOptions::default()).await;

    let session = initialize(&base).await;

    // tools/list exposes all six tools in the full set (order is the
    // router's own).
    let (status, listed) = message(post(&base, &list_body(2), Some(&session)).await).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let mut names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "zvec_grep_index",
            "zvec_grep_index_drop",
            "zvec_grep_index_status",
            "zvec_grep_rg",
            "zvec_grep_search",
            "zvec_grep_server_status",
        ]
    );

    // server_status round trip.
    let (status, called) = message(
        post(
            &base,
            &call_body(3, "zvec_grep_server_status", json!({})),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        called["result"]["structuredContent"]["version"],
        "0.0.0-test"
    );

    // search round trip over the indexed fixture.
    let (status, called) = message(
        post(
            &base,
            &call_body(
                4,
                "zvec_grep_search",
                json!({"root": root, "query": "alpha"}),
            ),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let text = called["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("search failed: {called}"));
    assert!(text.starts_with("freshness: fresh\n"), "{text}");

    // DELETE terminates the session: the next call is an unknown session.
    let deleted = client()
        .delete(format!("{base}/mcp"))
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert!(deleted.status().is_success(), "{}", deleted.status());
    let (status, gone) = message(post(&base, &list_body(5), Some(&session)).await).await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(gone["error"]["code"], -32000);
    server.close().await;
}

#[tokio::test]
async fn legacy_guards_reject_sessionless_and_unknown_requests() {
    let (server, base) = start(
        stub_backend(),
        McpToolset::Full,
        McpHttpEndpointOptions::default(),
    )
    .await;

    // Non-initialize POST without a session id.
    let (status, body) = message(post(&base, &list_body(1), None).await).await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32000);

    // Unknown session id on POST.
    let (status, body) = message(post(&base, &list_body(1), Some("nope")).await).await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], -32000);

    // Session-less GET is 405; unknown session is 404.
    let get = client().get(format!("{base}/mcp")).send().await.unwrap();
    assert_eq!(get.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
    let get = client()
        .get(format!("{base}/mcp"))
        .header("mcp-session-id", "nope")
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), reqwest::StatusCode::NOT_FOUND);
    let delete = client()
        .delete(format!("{base}/mcp"))
        .header("mcp-session-id", "nope")
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status(), reqwest::StatusCode::NOT_FOUND);
    server.close().await;
}

#[tokio::test]
async fn session_cap_returns_503() {
    let (server, base) = start(
        stub_backend(),
        McpToolset::Full,
        McpHttpEndpointOptions {
            max_legacy_sessions: 1,
            ..McpHttpEndpointOptions::default()
        },
    )
    .await;
    let first = initialize(&base).await;
    assert!(!first.is_empty());
    let (status, body) = message(post(&base, &initialize_body(2), None).await).await;
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], -32000);
    server.close().await;
}

#[tokio::test]
async fn agent_toolset_hides_lifecycle_tools() {
    let (server, base) = start(
        stub_backend(),
        McpToolset::Agent,
        McpHttpEndpointOptions::default(),
    )
    .await;
    let session = initialize(&base).await;
    let (status, listed) = message(post(&base, &list_body(2), Some(&session)).await).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["zvec_grep_search"]);
    // Hidden tools are unknown methods, not gated calls.
    let (status, unknown) = message(
        post(
            &base,
            &call_body(3, "zvec_grep_rg", json!({"root": "/x", "command": "rg a"})),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(unknown.get("error").is_some(), "{unknown}");
    server.close().await;
}

#[tokio::test]
async fn schema_bounds_reject_before_the_backend() {
    let (server, base) = start(
        stub_backend(),
        McpToolset::Full,
        McpHttpEndpointOptions::default(),
    )
    .await;
    let session = initialize(&base).await;
    let dir = fixture();
    let root = dir.path().to_string_lossy().into_owned();

    // Limit above the 50-item bound.
    let (status, rejected) = message(
        post(
            &base,
            &call_body(
                2,
                "zvec_grep_search",
                json!({"root": root, "query": "alpha", "limit": 51}),
            ),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(rejected["error"]["code"], -32602, "{rejected}");

    // 33 query groups above the 32-group bound.
    let groups: Vec<String> = (0..33).map(|index| format!("q{index}")).collect();
    let (status, rejected) = message(
        post(
            &base,
            &call_body(
                3,
                "zvec_grep_search",
                json!({"root": root, "queries": groups}),
            ),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
    server.close().await;
}

#[tokio::test]
async fn index_wait_and_rg_round_trips() {
    let backend = stub_backend();
    let (server, base) = start(backend, McpToolset::Full, McpHttpEndpointOptions::default()).await;
    let session = initialize(&base).await;
    let dir = fixture();
    // An empty file is skipped by the scanner, so `debug` diagnostics are
    // non-empty after the run.
    std::fs::write(dir.path().join("empty.txt"), "").unwrap();
    let root = dir.path().to_string_lossy().into_owned();

    // index with wait:true submits and settles inline.
    let (status, indexed) = message(
        post(
            &base,
            &call_body(
                2,
                "zvec_grep_index",
                json!({"root": root, "wait": true, "debug": true}),
            ),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let text = indexed["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("index failed: {indexed}"));
    assert!(text.contains("state: succeeded"), "{text}");
    assert!(text.contains("skipped_files:"), "{text}");

    // status reports the fresh index.
    let (status, reported) = message(
        post(
            &base,
            &call_body(3, "zvec_grep_index_status", json!({"root": root})),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let structured = &reported["result"]["structuredContent"];
    assert_eq!(structured["indexed"], true);
    assert_eq!(
        structured["runtime"]["dirty_revision"],
        structured["runtime"]["indexed_revision"]
    );

    // managed rg finds the fixture symbol.
    let (status, matched) = message(
        post(
            &base,
            &call_body(
                4,
                "zvec_grep_rg",
                json!({"root": root, "command": "rg 'fn alpha'"}),
            ),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let text = matched["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("a.rs"), "{text}");

    // drop removes the index.
    let (status, dropped) = message(
        post(
            &base,
            &call_body(5, "zvec_grep_index_drop", json!({"root": root})),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(dropped["result"]["structuredContent"]["removed"], true);
    server.close().await;
}

#[tokio::test]
async fn search_reports_missing_index_without_a_backend_call() {
    let (server, base) = start(
        stub_backend(),
        McpToolset::Full,
        McpHttpEndpointOptions::default(),
    )
    .await;
    let session = initialize(&base).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    let (status, missing) = message(
        post(
            &base,
            &call_body(
                2,
                "zvec_grep_search",
                json!({"root": root, "query": "alpha"}),
            ),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    // Backend INDEX_MISSING surfaces as an internal tool error.
    assert_eq!(missing["error"]["code"], -32603, "{missing}");
    server.close().await;
}
