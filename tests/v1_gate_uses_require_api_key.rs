#![allow(clippy::await_holding_lock)]
//! openproxy-d5lf: `/v1/*` must gate on `requireApiKey`, not on `requireLogin`.
//!
//! The two are independent in 9router. Conflating them means locking the
//! dashboard also locks every API client, and — much worse for a headless
//! proxy — leaving the dashboard open silently removes API auth from `/v1`.
//!
//! These set the two knobs to opposite values, so a handler that reads the
//! wrong one gives the wrong answer unambiguously.

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

async fn post_embeddings(app: axum::Router, with_key: bool) -> StatusCode {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/v1/embeddings")
        .header("content-type", "application/json");
    if with_key {
        builder = builder.header("authorization", "Bearer valid-bearer");
    }
    app.oneshot(
        builder
            .body(Body::from(
                json!({"model": "openai/x", "input": "hi"}).to_string(),
            ))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

/// requireApiKey=false must let the request through even though requireLogin
/// is true. If the handler reads requireLogin, this 401s.
#[tokio::test]
async fn v1_is_open_when_only_the_dashboard_is_locked() {
    let app = openproxy::build_app(app_state(true, false).await);
    let status = post_embeddings(app, false).await;
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "requireLogin=true must not gate /v1 when requireApiKey=false"
    );
}

/// requireApiKey=true must 401 a request with no key, even though the
/// dashboard is wide open.
#[tokio::test]
async fn v1_is_locked_when_only_the_api_key_setting_is_on() {
    let app = openproxy::build_app(app_state(false, true).await);
    let status = post_embeddings(app, false).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "requireApiKey=true must gate /v1 even when requireLogin=false"
    );
    // And a valid key is still accepted, so this is the auth gate and not a
    // blanket rejection.
    let app = openproxy::build_app(app_state(false, true).await);
    let status = post_embeddings(app, true).await;
    assert_ne!(status, StatusCode::UNAUTHORIZED);
}
