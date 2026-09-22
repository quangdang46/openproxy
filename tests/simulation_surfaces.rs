//! Simulation surfaces: CLI roundtrip + API mode/status (bead sim-19).
//!
//! Covers plan §3.3/§3.5: `provider mode` show/set, `provider status`,
//! `GET /api/mock/status`, `PUT /api/providers/:id {mode}`, and the additive
//! `simulation` payload on provider GET. Uses the admin_item_routes harness
//! pattern (tempdir Db + oneshot app).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "sim-surfaces-test-key";

fn active_key() -> ApiKey {
    ApiKey {
        id: "key-1".into(),
        name: "Local".into(),
        key: TEST_KEY.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
        monthly_budget_usd: None,
    }
}

fn openai_connection() -> ProviderConnection {
    ProviderConnection {
        id: "provider-1".into(),
        provider: "openai".into(),
        auth_type: "api_key".into(),
        name: Some("OpenAI".into()),
        priority: Some(1),
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
        api_key: Some("sk-test".into()),
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

async fn app_state() -> (AppState, tempfile::TempDir) {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.provider_connections = vec![openai_connection()];
    })
    .await
    .expect("seed db");
    (AppState::new(db), temp)
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    (status, json)
}

async fn put_json(app: axum::Router, uri: &str, payload: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    (status, json)
}

#[tokio::test]
async fn mock_status_lists_providers_with_modes() {
    let (state, _temp) = app_state().await;
    let app = openproxy::build_app(state);
    let (status, json) = get_json(app, "/api/mock/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["forcedAll"], false);
    let providers = json["providers"].as_object().unwrap();
    assert!(providers.contains_key("openai"), "openai listed");
    assert!(providers.contains_key("anthropic"), "anthropic listed");
    assert!(providers.contains_key("gemini"), "gemini listed");
    assert_eq!(json["providers"]["openai"]["configured"], "real");
    assert_eq!(json["providers"]["openai"]["effective"], "real");
    assert_eq!(json["providers"]["openai"]["reason"], "default");
    assert_eq!(json["providers"]["openai"]["simulationSupported"], true);
}

#[tokio::test]
async fn provider_mode_roundtrip_via_put() {
    let (state, _temp) = app_state().await;
    // Set mock.
    let app = openproxy::build_app(state.clone());
    let (status, json) = put_json(app, "/api/providers/provider-1", json!({"mode": "mock"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["simulation"]["configuredMode"], "mock");
    assert_eq!(json["simulation"]["effectiveMode"], "mock");
    assert_eq!(json["simulation"]["effectiveReason"], "provider-config");
    // Status endpoint reflects it.
    let app = openproxy::build_app(state.clone());
    let (_, json) = get_json(app, "/api/mock/status").await;
    assert_eq!(json["providers"]["openai"]["configured"], "mock");
    assert_eq!(json["providers"]["openai"]["effective"], "mock");
    // GET provider shows it too.
    let app = openproxy::build_app(state.clone());
    let (status, json) = get_json(app, "/api/providers/provider-1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["simulation"]["configuredMode"], "mock");
    // Back to real.
    let app = openproxy::build_app(state.clone());
    let (status, json) = put_json(app, "/api/providers/provider-1", json!({"mode": "real"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["simulation"]["effectiveMode"], "real");
    // Invalid mode rejected.
    let app = openproxy::build_app(state);
    let (status, _) = put_json(app, "/api/providers/provider-1", json!({"mode": "bogus"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mock_status_forced_all_via_settings() {
    let (state, temp) = app_state().await;
    state
        .db
        .update_settings(|s| s.dev_mock_all = true)
        .await
        .expect("settings");
    let _ = temp;
    let app = openproxy::build_app(state);
    let (status, json) = get_json(app, "/api/mock/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["forcedAll"], true);
    assert_eq!(json["providers"]["openai"]["effective"], "mock");
    assert_eq!(json["providers"]["openai"]["reason"], "settings-force");
}

#[tokio::test]
async fn provider_mode_survives_restart() {
    // sim-19 review gap: mode must survive binary rebuilds/restarts (Core
    // Product Surfaces: configuration is user data in SQLite). Reload the Db
    // from the same data dir and assert the mode persists.
    let (state, temp) = app_state().await;
    let app = openproxy::build_app(state);
    let (status, _) = put_json(app, "/api/providers/provider-1", json!({"mode": "mock"})).await;
    assert_eq!(status, StatusCode::OK);
    // "Restart": fresh Db handle on the same dir (temp kept alive; SQLite
    // allows concurrent handles, WAL mode).
    let db2 = Arc::new(Db::load_from(temp.path()).await.expect("db reload"));
    let modes = db2
        .sqlite
        .with_conn(|conn| {
            Ok::<_, rusqlite::Error>(openproxy::core::simulation::status_for(
                conn, "openai", false,
            ))
        })
        .unwrap();
    assert_eq!(modes.configured.to_string(), "mock");
    assert_eq!(modes.effective.to_string(), "mock");
    // `temp` kept alive by binding: dir survives until end of test.
    let _ = temp.path();
}
