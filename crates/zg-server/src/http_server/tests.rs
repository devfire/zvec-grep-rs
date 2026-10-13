use super::guard::{valid_host, valid_origin, valid_token};
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
    assert!(valid_host(Some(&loopback), 7999));
    // Loopback host on any other port than the bound one is rejected.
    assert!(!valid_host(Some(&loopback), 8000));
    assert!(!valid_host(
        Some(&HeaderValue::from_static("127.0.0.1")),
        7999
    ));
    assert!(!valid_host(Some(&remote), 7999));
    assert!(!valid_host(None, 7999));
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

#[tokio::test]
async fn shutdown_rejects_foreign_origin_without_cancelling() {
    // Anonymous mode (no token): the Origin guard is the only thing standing
    // between a web page and a daemon shutdown.
    let server = server(None);
    let address = server.start().await.unwrap();
    let base = format!("http://{address}");
    let denied = client()
        .post(format!("{base}/control/shutdown"))
        .header("origin", "http://example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(!server.shutdown_token().is_cancelled());
    // A request with no Origin (curl/CLI) still reaches the accepted path.
    let accepted = client()
        .post(format!("{base}/control/shutdown"))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert!(server.shutdown_token().is_cancelled());
    server.close().await;
}

#[tokio::test]
async fn shutdown_accepts_loopback_origin() {
    let server = server(None);
    let address = server.start().await.unwrap();
    let base = format!("http://{address}");
    let accepted = client()
        .post(format!("{base}/control/shutdown"))
        .header("origin", format!("http://127.0.0.1:{}", address.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert!(server.shutdown_token().is_cancelled());
    server.close().await;
}

#[tokio::test]
async fn shutdown_rejects_host_with_wrong_port_without_cancelling() {
    let server = server(None);
    let address = server.start().await.unwrap();
    let base = format!("http://{address}");
    let wrong_port = if address.port() == u16::MAX {
        address.port() - 1
    } else {
        address.port() + 1
    };
    let denied = client()
        .post(format!("{base}/control/shutdown"))
        .header("host", format!("127.0.0.1:{wrong_port}"))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(!server.shutdown_token().is_cancelled());
    server.close().await;
}
