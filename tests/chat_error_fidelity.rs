//! Bead openproxy-mv0w.3 — the chat surface's client-facing error fidelity.
//!
//! 9router hands the status it was given straight to the response
//! (`errorResponse`, open-sse/utils/error.js:27-35) and never inspects the
//! message, so an unconfigured provider — which `getExecutor`
//! (executors/index.js:67-71) always builds an executor for — surfaces at the
//! fetch as the 502 of chatCore.js:398-402, not as a server fault. OpenProxy
//! answered 500 and put a Rust variant name in the body.
//!
//! The companion arm matters because the two conditions are not the same
//! thing: a provider with no stored credential is a 404 (chat.js:237-243),
//! the same provider once it has one is a 502. The status re-derivation that
//! used to run in both `attempt_error_response` and `json_error_response` is
//! what blurred them, so those two are pinned here too.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProviderNode, Settings};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::MockServer;

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

async fn seeded_state(nodes: Vec<ProviderNode>, connections: Vec<ProviderConnection>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.provider_nodes = nodes;
        state.provider_connections = connections;
        state.combos = Vec::new();
        // Auth is not under test here (see api_auth_and_models) — disable the
        // login guard so requests without keys reach the chat pipeline.
        state.settings = Settings {
            require_login: false,
            ..Settings::default()
        };
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn post_chat(state: AppState, model: &str) -> axum::response::Response {
    openproxy::build_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn error_message(json: &Value) -> String {
    json["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 9router executors/index.js:67-71 never fails to return an executor, so an
/// unconfigured provider surfaces at the fetch — a 502, not a 500. The Rust
/// variant name must not reach the client either way.
#[tokio::test]
async fn unconfigured_provider_answers_502_not_500() {
    let state = seeded_state(
        Vec::new(),
        vec![connection(
            "conn-1",
            "totally-unknown-provider",
            1,
            "upstream-key",
        )],
    )
    .await;

    let response = post_chat(state, "totally-unknown-provider/gpt-4o-mini").await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_GATEWAY,
        "an unconfigured provider is a gateway failure, not a server fault"
    );
    let json = body_json(response).await;
    let message = error_message(&json);
    assert!(
        message.contains("totally-unknown-provider"),
        "the message must name the provider: {message}"
    );
    assert!(
        !message.contains("UnsupportedProvider"),
        "a Rust variant leaked to the client: {message}"
    );
    assert!(
        !message.contains("Default executor creation failed"),
        "the internal frame survived: {message}"
    );
}

/// The 404 arm is a different condition and must stay reachable: the same
/// unknown provider with no connection at all is "no usable credential", not
/// "no upstream configured".
#[tokio::test]
async fn unknown_provider_without_connections_still_answers_404() {
    let state = seeded_state(Vec::new(), Vec::new()).await;

    let response = post_chat(state, "totally-unknown-provider/gpt-4o-mini").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let json = body_json(response).await;
    assert_eq!(
        json["error"]["code"], "model_not_found",
        "404 must carry the model_not_found code: {json}"
    );
    assert_eq!(
        json["error"]["message"],
        "No active credentials for provider: totally-unknown-provider"
    );
}

/// 9router chat.js:237-243 — nothing was ever attempted, so the provider has no
/// usable credential and the client gets a 404, not a 400 it will read as a bad
/// request it should fix.
#[tokio::test]
async fn no_usable_credentials_answers_404_with_the_active_credentials_message() {
    let upstream = MockServer::start().await;

    let mut unusable = connection("conn-1", "node-openai", 1, "unused-key");
    unusable.api_key = None;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![unusable],
    )
    .await;

    let response = post_chat(state, "custom/gpt-4o-mini").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let json = body_json(response).await;
    assert_eq!(
        json["error"]["message"],
        "No active credentials for provider: node-openai"
    );
    assert!(upstream.received_requests().await.unwrap().is_empty());
}
