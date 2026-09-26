//! Bead openproxy-mv0w.8 — audit P101-001, the Ollama Local no-API-key
//! exemption on `POST /api/providers`.
//!
//! 9router's key requirement is conditional (route.js:119-121):
//! `if (!apiKey && provider !== "ollama-local")` 400s, and the row is built with
//! `apiKey: apiKey || ""` (:179). OpenProxy required a non-empty key from every
//! provider, so the dashboard's Ollama Local form — which has no key field at
//! all and posts `""` — could never create a connection.
//!
//! The exemption is a literal string, not the registry `noAuth` flag the
//! validate route checks, which is why it is one provider and not a table.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const BEARER: &str = "ollama-local-bearer";

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "key-id".into(),
            name: "Local".into(),
            key: BEARER.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            monthly_budget_usd: None,
            extra: BTreeMap::new(),
        }];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn create(state: &AppState, body: Value) -> (StatusCode, Value) {
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/providers")
                .header("authorization", format!("Bearer {BEARER}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// The connection read back out of the store rather than echoed from the
/// response — the response body is redacted.
fn created_connection(state: &AppState, provider: &str) -> Value {
    let snapshot = state.db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.provider == provider)
        .unwrap_or_else(|| panic!("a {provider} connection was created"));
    serde_json::to_value(connection).expect("serialize connection")
}

/// The dashboard's Ollama Local form posts exactly this: no key field is
/// rendered, so `apiKey` arrives as the empty string.
#[tokio::test]
async fn create_provider_allows_ollama_local_without_an_api_key() {
    let state = app_state().await;
    let (status, _) = create(
        &state,
        json!({
            "provider": "ollama-local",
            "name": "Ollama Local",
            "apiKey": "",
            "providerSpecificData": { "baseUrl": "http://127.0.0.1:11434" },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);

    let connection = created_connection(&state, "ollama-local");
    assert_eq!(connection["apiKey"], "");
    assert_eq!(connection["provider"], "ollama-local");
    // Ollama Local is a plain category-apikey registry provider
    // (open-sse/providers/registry/ollama-local.js:14). Holding auth_type keeps
    // list filtering and executor dispatch on the same arm as 9router — the
    // exemption is about the missing key only.
    assert_eq!(connection["authType"], "apikey");
    assert_eq!(
        connection["providerSpecificData"]["baseUrl"], "http://127.0.0.1:11434",
        "the host URL the form collects is the endpoint the executor dials"
    );
}

/// The guard against over-loosening. Reusing the validate route's wider `noAuth`
/// set here would create keyless connections 9router still rejects, and a test
/// that only asserted the exemption would pass if the check were simply deleted.
#[tokio::test]
async fn create_provider_still_rejects_a_missing_key_for_openai() {
    let state = app_state().await;
    let (status, json) = create(
        &state,
        json!({ "provider": "openai", "name": "OpenAI", "apiKey": "" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "API key is required");
    assert!(
        state.db.snapshot().provider_connections.is_empty(),
        "the rejected row must not reach the store"
    );
}

/// The rest of the validate route's `noAuth` set — media and device providers
/// that 9router also 400s on a create with no key, because the exemption in
/// route.js:119-121 names Ollama Local alone.
#[tokio::test]
async fn create_provider_still_rejects_a_missing_key_for_the_other_no_auth_providers() {
    for provider in [
        "edge-tts",
        "local-device",
        "sdwebui",
        "comfyui",
        "opencode-zen",
    ] {
        let state = app_state().await;
        let (status, json) = create(
            &state,
            json!({ "provider": provider, "name": "No Auth", "apiKey": "" }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{provider} is still keyed in 9router's create route"
        );
        assert_eq!(json["error"], "API key is required");
    }
}
