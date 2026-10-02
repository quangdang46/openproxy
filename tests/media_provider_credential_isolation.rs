//! A media request must use the credentials of the account the caller named —
//! not whichever account happened to sort first.
//!
//! `generic_media_handler` resolved the provider purely from the model string
//! and then `select_media_connections` took the first active connection for
//! that provider. `body["provider"]` — which is what the CLI's
//! `media image generate --provider X` / `media embed --provider X` /
//! `media search --provider X` send — was read by nothing. So with two
//! connections under one provider, `media image generate --provider B` sent
//! A's API key to the upstream. That is a credential leak across accounts, not
//! just a routing mistake.
//!
//! The dashboard's media skills build `provider/model` model strings, so the
//! model-string path has to keep working; only a bare model name (no `/`) is
//! allowed to honour an explicit `provider`.
#![cfg(test)]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

fn key(name: &str) -> ApiKey {
    ApiKey {
        id: format!("{name}-id"),
        name: name.into(),
        key: name.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
        monthly_budget_usd: None,
    }
}

/// Two connections under the same provider, each with a distinct key, so a
/// wrong selection is observable in the error text.
fn two_accounts() -> Vec<ProviderConnection> {
    vec![
        connection("openai", "account-a", "sk-AAA"),
        connection("openai", "account-b", "sk-BBB"),
    ]
}

fn connection(provider: &str, name: &str, api_key: &str) -> ProviderConnection {
    ProviderConnection {
        id: format!("{name}-conn"),
        provider: provider.into(),
        auth_type: "apikey".into(),
        name: Some(name.into()),
        priority: Some(1),
        is_active: Some(true),
        api_key: Some(api_key.into()),
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
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
        runtime_transport: None,
    }
}

async fn app() -> axum::Router {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![key("valid-bearer")];
        state.provider_connections = two_accounts();
    })
    .await
    .expect("seed");
    openproxy::build_app(AppState::new(db))
}

fn post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap()
}

/// The upstream rejects any key that is not the one the caller named, so the
/// error text identifies which credential was used.
async fn which_key_was_sent(app: &axum::Router, body: Value) -> String {
    let response = app
        .clone()
        .oneshot(post("/v1/embeddings", body))
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    // 400 (no credentials for the provider) or 401 (the upstream rejected the
    // key) both prove a credential was selected and sent. What must never
    // happen is a 200, which would mean the request went out with no key at all.
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNAUTHORIZED,
        "unexpected status {status}: {text}"
    );
    text
}

#[tokio::test]
async fn an_explicit_provider_selects_that_accounts_credential() {
    let app = app().await;

    // `account-b` is the second connection, so a "first match wins" selection
    // would send sk-AAA.
    let text = which_key_was_sent(
        &app,
        json!({
            "provider": "account-b",
            "model": "text-embedding-3-small",
            "input": "hello",
        }),
    )
    .await;

    assert!(
        text.contains("sk-BBB"),
        "expected account-b's key (sk-BBB) to be sent, got: {text}"
    );
    assert!(
        !text.contains("sk-AAA"),
        "account-a's key leaked into account-b's request: {text}"
    );
}

#[tokio::test]
async fn a_model_string_without_a_provider_still_resolves() {
    let app = app().await;

    // The dashboard's shape: `provider/model`, no `provider` field.
    let text = which_key_was_sent(
        &app,
        json!({
            "model": "openai/text-embedding-3-small",
            "input": "hello",
        }),
    )
    .await;

    // Either account is acceptable here — the point is that it resolves at all
    // and does not 400 on a missing provider.
    assert!(
        text.contains("sk-AAA") || text.contains("sk-BBB"),
        "model-string routing broke: {text}"
    );
}
