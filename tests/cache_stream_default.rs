//! Regression tests for openproxy-w3nh: the response cache must not store or
//! serve SSE bodies as `application/json`.
//!
//! The cache guard used to decide "is this streaming?" with its own default
//! (`body.stream.unwrap_or(false)`), while the dispatcher resolved streaming
//! with a different default (`body.stream != Some(false)`). For a body that
//! omits `stream` the two disagree, so the SSE body was stored under a JSON
//! cache key and later served as `application/json`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProviderNode, Settings};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn active_key(key: &str) -> ApiKey {
    ApiKey {
        id: format!("{key}-id"),
        name: "Local".into(),
        key: key.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
        monthly_budget_usd: None,
    }
}

fn provider_node(id: &str, prefix: &str, base_url: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: "Compatible".into(),
        prefix: Some(prefix.into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url.into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn connection(id: &str, provider: &str, priority: u32, api_key: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        priority: Some(priority),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: Some("gpt-4o-mini".into()),
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: Some(api_key.into()),
        test_status: None,
        last_tested: None,
        last_error: None,
        last_error_at: None,
        rate_limited_until: None,
        expires_in: None,
        error_code: None,
        consecutive_use_count: None,
        backoff_level: None,
        consecutive_errors: None,
        proxy_url: None,
        proxy_label: None,
        use_connection_proxy: None,
        runtime_transport: None,
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

async fn seeded_state(
    nodes: Vec<ProviderNode>,
    connections: Vec<ProviderConnection>,
) -> (AppState, Arc<openproxy::core::cache::ResponseCache>) {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.provider_nodes = nodes;
        state.provider_connections = connections;
        let mut settings = Settings::default();
        settings.require_login = false;
        state.settings = settings;
    })
    .await
    .expect("seed db");
    let app_state = AppState::new(db);
    let cache = app_state.response_cache.clone();
    (app_state, cache)
}

/// A body with NO `stream` field — the ordinary shape clients send.
fn no_stream_body() -> serde_json::Value {
    json!({
        "model": "custom/gpt-4o-mini",
        "messages": [{"role": "user", "content": "hi"}],
    })
}

fn chat_request(body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// The core invariant: a body that omits `stream` resolves to streaming, so
/// the response is a real SSE stream and must NEVER be served as a JSON cache
/// hit. Two identical requests must both get correctly-labelled SSE.
#[tokio::test]
async fn omitted_stream_field_is_never_served_from_cache_as_json() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .mount(&upstream)
        .await;

    let (state, _cache) = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
    )
    .await;

    let app = openproxy::build_app(state);
    let body = no_stream_body();

    // First request: resolved stream default is true => genuine SSE response.
    let resp1 = app.clone().oneshot(chat_request(&body)).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    assert_eq!(
        resp1.headers().get("content-type").unwrap(),
        "text/event-stream",
        "omitted stream must resolve to an SSE response (not JSON)",
    );
    let b1 = axum::body::to_bytes(resp1.into_body(), usize::MAX)
        .await
        .unwrap();
    let t1 = String::from_utf8(b1.to_vec()).unwrap();
    assert!(t1.starts_with("data:"), "first response should be SSE");

    // Second, identical request. Before the fix this was a cache HIT that
    // replayed the SSE bytes under `application/json`. After the fix the
    // streaming request is not cached, so we get a fresh, correctly-labelled
    // SSE response — and never SSE bytes under a JSON content-type.
    let resp2 = app.oneshot(chat_request(&body)).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let ct2 = resp2
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let b2 = axum::body::to_bytes(resp2.into_body(), usize::MAX)
        .await
        .unwrap();
    let t2 = String::from_utf8(b2.to_vec()).unwrap();

    if ct2.starts_with("application/json") {
        // If it IS labelled JSON, the body must be a well-formed JSON
        // completion — never raw SSE bytes.
        serde_json::from_str::<serde_json::Value>(&t2).unwrap_or_else(|e| {
            panic!("SSE bytes served under application/json (parse error: {e}): {t2:?}")
        });
    } else {
        // Correct behaviour: a fresh SSE response, properly labelled.
        assert_eq!(ct2, "text/event-stream");
        assert!(t2.starts_with("data:"), "second response should be SSE");
    }
}

/// The cache must not retain an entry whose stored body is raw SSE. This
/// directly checks the poisoned-entry root cause (the cache key excludes
/// `stream`, so a stored SSE body would be served to any later variant).
#[tokio::test]
async fn cache_never_stores_an_sse_body() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .mount(&upstream)
        .await;

    let (state, cache) = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
    )
    .await;

    let app = openproxy::build_app(state);
    let body = no_stream_body();

    let resp = app.oneshot(chat_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();

    // If anything was cached for this key, it must not be an SSE body.
    if let Some((cached, _ttl)) = cache.get_with_ttl(&body) {
        let text = String::from_utf8_lossy(&cached);
        assert!(
            !text.trim_start().starts_with("data:"),
            "cache stored an SSE body for a streaming request: {text:?}"
        );
    }
}

/// Guard against over-correcting: a request that explicitly asks for a
/// non-streaming JSON response must STILL be cached (2nd request is a HIT and
/// the upstream is hit only once). The fix must target the default mismatch,
/// not disable the cache for genuinely non-streaming traffic.
#[tokio::test]
async fn explicit_non_streaming_request_is_still_cached() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-json",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "hello" },
                "finish_reason": "stop"
            }]
        })))
        .mount(&upstream)
        .await;

    let (state, _cache) = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
    )
    .await;

    let app = openproxy::build_app(state);
    let body = json!({
        "model": "custom/gpt-4o-mini",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": false,
    });

    let resp1 = app.clone().oneshot(chat_request(&body)).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    let b1 = axum::body::to_bytes(resp1.into_body(), usize::MAX)
        .await
        .unwrap();
    let t1: serde_json::Value = serde_json::from_slice(&b1).expect("first response is JSON");
    assert_eq!(t1["choices"][0]["message"]["content"], "hello");

    // Second identical request: must be a cache HIT served from memory.
    let resp2 = app.oneshot(chat_request(&body)).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    assert_eq!(
        resp2.headers().get("x-cache").map(|v| v.to_str().unwrap()),
        Some("HIT"),
        "explicit non-streaming request should still be served from cache",
    );
    let b2 = axum::body::to_bytes(resp2.into_body(), usize::MAX)
        .await
        .unwrap();
    let t2: serde_json::Value = serde_json::from_slice(&b2).expect("cached response is JSON");
    assert_eq!(t2["choices"][0]["message"]["content"], "hello");

    // The upstream must have been hit exactly once (second served from cache).
    let requests = upstream
        .received_requests()
        .await
        .expect("received requests");
    assert_eq!(
        requests.len(),
        1,
        "non-streaming request should be cached, not re-dispatched"
    );
}
