#![allow(clippy::await_holding_lock)]
//! openproxy-mv0w.6: every `/v1` search surface must gate on `requireApiKey`.
//!
//! 9router makes the key check conditional (`src/sse/handlers/search.js:48-60`:
//! `if (settings.requireApiKey) { … }`), so turning the setting off genuinely
//! opens search to anonymous callers. A gate that runs unconditionally cannot
//! be opened by the setting at all, and reads as protected even when the
//! operator asked for an open LAN endpoint.
//!
//! The two knobs are set to opposite values in each configuration, so a
//! handler that reads `requireLogin` instead of `requireApiKey` gives the
//! wrong answer unambiguously rather than passing by accident.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

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

fn connection() -> ProviderConnection {
    ProviderConnection {
        id: "conn".into(),
        provider: "openai".into(),
        auth_type: "apikey".into(),
        name: Some("conn".into()),
        priority: Some(1),
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
        api_key: Some("sk-upstream".into()),
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

/// `require_api_key` set explicitly, so the two knobs disagree and the handler
/// has no way to pass by accident.
async fn app_state(require_login: bool, require_api_key: bool) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.provider_connections = vec![connection()];
        state.settings.require_login = require_login;
        state.settings.require_api_key = Some(require_api_key);
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

/// `bearer` is sent verbatim, so `None` means no `Authorization` header and
/// `Some("not-a-real-key")` is a present-but-invalid key.
async fn post(app: axum::Router, uri: &str, bearer: Option<&str>) -> StatusCode {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    app.oneshot(
        builder
            .body(Body::from(json!({"query": "rust"}).to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

/// Assert the full truth table on one search surface. With the key requirement
/// on, a missing key is a 401; with it off, neither a missing nor an invalid
/// key is — an invalid key is simply never checked, exactly as in 9router.
async fn assert_search_gate_follows_require_api_key(uri: &str) {
    // requireApiKey=true / requireLogin=false — the key setting alone gates.
    let app = openproxy::build_app(app_state(false, true).await);
    assert_eq!(
        post(app, uri, None).await,
        StatusCode::UNAUTHORIZED,
        "{uri}: requireApiKey=true must 401 a keyless request"
    );

    // A valid key is still admitted, so this is the key gate and not a
    // blanket rejection of the route.
    let app = openproxy::build_app(app_state(false, true).await);
    assert_ne!(
        post(app, uri, Some("valid-bearer")).await,
        StatusCode::UNAUTHORIZED,
        "{uri}: requireApiKey=true must admit a valid key"
    );

    // requireApiKey=false / requireLogin=true — the dashboard lock alone must
    // not keep the API surface closed.
    let app = openproxy::build_app(app_state(true, false).await);
    assert_ne!(
        post(app, uri, None).await,
        StatusCode::UNAUTHORIZED,
        "{uri}: requireLogin=true must not gate search when requireApiKey=false"
    );

    // An invalid key is not validated while the requirement is off, so it is
    // admitted exactly like no key at all.
    let app = openproxy::build_app(app_state(true, false).await);
    assert_ne!(
        post(app, uri, Some("not-a-real-key")).await,
        StatusCode::UNAUTHORIZED,
        "{uri}: requireApiKey=false must not validate an unused key"
    );
}

#[tokio::test]
async fn search_gate_follows_require_api_key_in_both_directions() {
    assert_search_gate_follows_require_api_key("/v1/search").await;
}

/// `/v1/chat/search` (the chat+search hybrid) is a separate handler from the
/// raw `/v1/search` surface, so it carries its own gate and needs its own
/// coverage.
#[tokio::test]
async fn chat_search_gate_follows_require_api_key_in_both_directions() {
    assert_search_gate_follows_require_api_key("/v1/chat/search").await;
}
