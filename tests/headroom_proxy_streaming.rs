//! Headroom reverse proxy (`/api/headroom/proxy/*`).
//!
//! 9router hands `response.body` straight to the `NextResponse`, so a long-lived
//! upstream such as `transformations/feed` reaches the browser chunk by chunk.
//! These tests pin that passthrough, the one path that still has to buffer (the
//! dashboard HTML rewrite), and the credential guard around both.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use tempfile::tempdir;
use tokio::net::{TcpListener, TcpStream};
use tower::util::ServiceExt;

/// How long the SSE upstream pauses between its two chunks.
const FEED_GAP: Duration = Duration::from_secs(3);
/// Comfortably below [`FEED_GAP`], so a proxy that buffers the body cannot pass.
const FIRST_CHUNK_BUDGET: Duration = Duration::from_secs(2);

/// Boot the app with a seeded API key and `headroom_url` set to `base_url`.
async fn app_for(base_url: &str) -> Router {
    let dir = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(dir.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "test-key-id".into(),
            name: "Test".into(),
            key: "test-key".into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            monthly_budget_usd: None,
            extra: BTreeMap::new(),
        }];
        state.settings.require_login = false;
        state.settings.headroom_url = base_url.to_string();
    })
    .await
    .expect("seed db");
    openproxy::build_app(AppState::new(db))
}

fn proxy_request(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("x-api-key", "test-key")
        .body(Body::empty())
        .expect("request")
}

/// Serve `router` on an ephemeral loopback port and return its address.
async fn spawn_upstream(router: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

/// Two `data:` chunks separated by [`FEED_GAP`], as `transformations/feed` does.
async fn feed() -> Response {
    let chunks = stream::unfold(0u8, |state| async move {
        match state {
            0 => Some((
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: first\n\n")),
                1,
            )),
            1 => {
                tokio::time::sleep(FEED_GAP).await;
                Some((Ok(Bytes::from_static(b"data: second\n\n")), 2))
            }
            _ => None,
        }
    });
    (
        [("content-type", "text/event-stream")],
        Body::from_stream(chunks),
    )
        .into_response()
}

#[tokio::test]
async fn headroom_proxy_streams_upstream_sse_chunks() {
    let upstream = spawn_upstream(Router::new().route("/transformations/feed", get(feed))).await;
    let app = app_for(&format!("http://{upstream}")).await;

    let mut body = tokio::time::timeout(FIRST_CHUNK_BUDGET, async {
        let response = app
            .oneshot(proxy_request("/api/headroom/proxy/transformations/feed"))
            .await
            .expect("proxy response");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("content-type");
        assert!(content_type.contains("text/event-stream"), "{content_type}");
        response.into_body().into_data_stream()
    })
    .await
    .expect("first SSE chunk must arrive before the upstream's pause elapses");

    let first = body.next().await.expect("a chunk").expect("a data chunk");
    assert_eq!(first.as_ref(), b"data: first\n\n".as_slice());

    let mut rest = Vec::new();
    while let Some(chunk) = body.next().await {
        rest.extend_from_slice(&chunk.expect("a data chunk"));
    }
    assert_eq!(rest.as_slice(), b"data: second\n\n".as_slice());
}

const DASHBOARD_HTML: &str = concat!(
    "<!doctype html><html><body><script>",
    "fetch('/stats');fetch('/health');fetch('/stats-history');",
    "fetch('/transformations/feed');",
    "</script></body></html>",
);

#[tokio::test]
async fn headroom_proxy_dashboard_html_is_still_buffered_and_rewritten() {
    let upstream = spawn_upstream(Router::new().route(
        "/dashboard",
        get(|| async {
            (
                [("content-type", "text/html; charset=utf-8")],
                DASHBOARD_HTML,
            )
        }),
    ))
    .await;
    let app = app_for(&format!("http://{upstream}")).await;

    let response = app
        .oneshot(proxy_request("/api/headroom/proxy/dashboard"))
        .await
        .expect("proxy response");
    assert_eq!(response.status(), StatusCode::OK);

    // The rewrite changes the length, so the handler re-declares it — proof the
    // dashboard path still buffers instead of streaming.
    let declared = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .expect("buffered response declares its length")
        .to_str()
        .expect("content-length")
        .parse::<usize>()
        .expect("numeric content-length");
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    assert_eq!(body.len(), declared);

    let html = std::str::from_utf8(&body).expect("utf8");
    for path in ["stats", "health", "stats-history", "transformations/feed"] {
        assert!(
            html.contains(&format!("fetch('/api/headroom/proxy/{path}'")),
            "{path} was not rewritten: {html}"
        );
    }
}

/// Echoes the two credentials the proxy may or may not forward.
async fn echo_credentials(headers: HeaderMap) -> String {
    let get = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    format!("{}|{}", get(header::AUTHORIZATION), get(header::COOKIE))
}

/// Hostnames the resolver sends to loopback but `is_loopback_url` does not
/// recognise, so the proxy must treat them as a remote host. The trailing-dot
/// form is a macOS/BSD answer; the `ip6-*` names are the Linux one.
const REMOTE_LOOPBACK_ALIASES: &[&str] = &["localhost.", "ip6-localhost", "ip6-loopback"];

/// The first alias that actually reaches the loopback upstream on `port`.
async fn remote_alias(port: u16) -> String {
    for &alias in REMOTE_LOOPBACK_ALIASES {
        if TcpStream::connect((alias, port)).await.is_ok() {
            return alias.to_string();
        }
    }
    panic!("no remote-looking loopback alias resolved on port {port}");
}

async fn echoed_credentials(app: Router) -> String {
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/headroom/proxy/stats")
                .header("x-api-key", "test-key")
                .header(header::AUTHORIZATION, "Token viewer-token")
                .header(header::COOKIE, "session=viewer-cookie")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("proxy response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .expect("body");
    String::from_utf8(body.to_vec()).expect("utf8")
}

#[tokio::test]
async fn headroom_proxy_strips_authorization_for_non_loopback_host() {
    let upstream = spawn_upstream(Router::new().route("/stats", get(echo_credentials))).await;
    let port = upstream.port();
    let alias = remote_alias(port).await;

    let loopback_app = app_for(&format!("http://{upstream}")).await;
    assert_eq!(
        echoed_credentials(loopback_app).await,
        "Token viewer-token|session=viewer-cookie"
    );

    let remote_app = app_for(&format!("http://{alias}:{port}")).await;
    assert_eq!(echoed_credentials(remote_app).await, "|");
}
