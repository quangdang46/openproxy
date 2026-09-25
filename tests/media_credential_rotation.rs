#![allow(clippy::await_holding_lock)]
//! openproxy-hj0p: /v1/embeddings must rotate to the next credential on an
//! auth or quota failure.
//!
//! The handler made exactly one attempt against one priority-ordered
//! connection, so a single quota-limited or revoked account failed a request
//! that a second account would have served. 9router rotates across accounts on
//! these surfaces (embeddings.js:97-164).
//!
//! Rotation is scoped to 401/403/429. A 400 or 5xx is the request's own
//! problem, not the credential's, and retrying it per account would multiply
//! latency for a guaranteed-identical answer.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{header, method};
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

/// One credential for the provider. `priority` fixes the order rotation
/// follows; the api_key is what distinguishes it upstream.
fn seed_connection(id: &str, priority: u32, key: &str, base_url: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.to_string(),
        provider: "node-embed".to_string(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        priority: Some(priority),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: None,
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: Some(key.into()),
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
        provider_specific_data: BTreeMap::from([(
            "baseUrl".to_string(),
            Value::String(base_url.to_string()),
        )]),
        extra: BTreeMap::new(),
    }
}

fn seed_node(upstream: &str) -> ProviderNode {
    ProviderNode {
        id: "node-embed".into(),
        r#type: "openai-compatible".into(),
        name: "Node".into(),
        prefix: Some("emb".into()),
        api_type: Some("embeddings".into()),
        base_url: Some(upstream.to_string()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

async fn app_state(upstream: &str) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.provider_nodes = vec![seed_node(upstream)];
        state.provider_connections = vec![
            seed_connection("first", 1, "sk-first", upstream),
            seed_connection("second", 2, "sk-second", upstream),
        ];
        // Credential auth is not the subject here, and db.update does not
        // rebuild api_key_map, so the login guard would 401 every request and
        // mask the rotation being tested. Same reason as chat_completions.rs.
        state.settings.require_login = false;
        state.settings.require_api_key = Some(false);
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn embeddings_request() -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/embeddings")
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"model": "emb/text-embedding-3-small", "input": "hello"}).to_string(),
        ))
        .unwrap()
}

async fn post_embeddings(app: axum::Router) -> (StatusCode, Value) {
    let response = app.oneshot(embeddings_request()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// The second credential serves the request, so the caller sees 200 — not the
/// 401 the first credential earned.
#[tokio::test]
async fn embeddings_rotates_to_the_second_credential_after_401() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(header("authorization", "Bearer sk-first"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "revoked"})))
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(header("authorization", "Bearer sk-second"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 4, "total_tokens": 4}
        })))
        .mount(&upstream)
        .await;

    let app = openproxy::build_app(app_state(&upstream.uri()).await);
    let (status, body) = post_embeddings(app).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "the second credential should have served the request, got {status}: {body}"
    );
    assert_eq!(body["data"][0]["embedding"][0], 0.1);
}

/// When every credential fails on auth, the real 401 comes back — not a generic
/// error that hides why it failed.
#[tokio::test]
async fn embeddings_returns_the_401_when_every_credential_fails() {
    let upstream = MockServer::start().await;

    for key in ["sk-first", "sk-second"] {
        Mock::given(method("POST"))
            .and(header("authorization", format!("Bearer {key}")))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "revoked"})))
            .mount(&upstream)
            .await;
    }

    let app = openproxy::build_app(app_state(&upstream.uri()).await);
    let (status, _body) = post_embeddings(app).await;

    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the upstream 401 must survive rather than being flattened into a 500"
    );
}

// ── Usage metering (openproxy-uxj9) ─────────────────────────────────────────
//
// The target here is the tracker, not a rotation sequence. The metering block
// used to sit inline in the generic upstream forwarder, AFTER the media-adapter
// attempt, so every provider served by an adapter returned before reaching it
// and its embedding spend was never recorded at all. This asserts the ledger
// actually gained a row.

/// A single successful adapter-backed embedding request must be metered.
#[tokio::test]
async fn successful_adapter_backed_embedding_is_recorded_in_the_ledger() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(header("authorization", "Bearer sk-first"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 42, "total_tokens": 42}
        })))
        .mount(&upstream)
        .await;

    let app = openproxy::build_app(app_state(&upstream.uri()).await);
    let (status, _body) = post_embeddings(app.clone()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the embedding should have succeeded"
    );

    // Give the tracker a moment — track_request is fire-and-forget from the
    // response path.
    let mut recorded = Vec::new();
    for _ in 0..40 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/usage/request-details?page=1&pageSize=50")
                    .header("authorization", "Bearer valid-bearer")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        recorded = value
            .get("details")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !recorded.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    assert!(
        !recorded.is_empty(),
        "a successful embedding request recorded nothing in the usage ledger"
    );
    let entry = &recorded[0];
    assert_eq!(
        entry
            .get("tokens")
            .and_then(|t| t.get("prompt_tokens"))
            .and_then(Value::as_i64),
        Some(42),
        "the ledger recorded the wrong prompt token count: {entry}"
    );
}

/// The same assertion, but on a provider that actually HAS an embeddings
/// adapter (`openai` is one of the arms in `embeddings::dispatch`).
///
/// The test above uses a node-registered provider, which has no adapter and so
/// falls through to the generic forwarder — that path already metered, which is
/// exactly why it passed even with the adapter-path call removed. This one
/// exercises the branch the bead was about.
#[tokio::test]
async fn node_adapter_backed_embedding_is_recorded_in_the_ledger() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": [0.1]}],
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 42, "total_tokens": 42}
        })))
        .mount(&upstream)
        .await;

    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    let base = upstream.uri();
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.settings.require_login = false;
        state.settings.require_api_key = Some(false);
        state.provider_connections = vec![ProviderConnection {
            provider: "custom-embedding-test".into(),
            // get_provider_base_url reads this, NOT the node's base_url.
            provider_specific_data: BTreeMap::from([(
                "baseUrl".to_string(),
                Value::String(base.clone()),
            )]),
            ..seed_connection("first", 1, "sk-first", &base)
        }];
    })
    .await
    .expect("seed db");

    let app = openproxy::build_app(AppState::new(db));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model": "custom-embedding-test/text-embedding-3-small", "input": "hi"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the adapter-backed embedding should have succeeded"
    );

    let mut recorded = Vec::new();
    for _ in 0..40 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/usage/request-details?page=1&pageSize=50")
                    .header("authorization", "Bearer valid-bearer")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        recorded = value
            .get("details")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !recorded.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    assert!(
        !recorded.is_empty(),
        "an adapter-backed embedding recorded nothing in the usage ledger"
    );
    assert_eq!(
        recorded[0]
            .get("tokens")
            .and_then(|t| t.get("prompt_tokens"))
            .and_then(Value::as_i64),
        Some(42),
        "the ledger recorded the wrong prompt token count: {}",
        recorded[0]
    );
}
