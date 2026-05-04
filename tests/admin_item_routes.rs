use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, Combo, ProviderConnection, ProxyPool};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "admin-item-routes-test-key";

fn active_key() -> ApiKey {
    ApiKey {
        id: "key-1".into(),
        name: "Local".into(),
        key: TEST_KEY.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
    }
}

fn provider_connection() -> ProviderConnection {
    let mut provider_specific_data = BTreeMap::new();
    provider_specific_data.insert("proxyPoolId".into(), Value::String("pool-1".into()));
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
        access_token: Some("secret-access".into()),
        refresh_token: Some("secret-refresh".into()),
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: Some("secret-id".into()),
        project_id: None,
        api_key: Some("secret-api-key".into()),
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
        provider_specific_data,
        extra: BTreeMap::new(),
    }
}

fn proxy_pool() -> ProxyPool {
    ProxyPool {
        id: "pool-1".into(),
        name: "Primary".into(),
        proxy_url: "http://proxy.test:8080".into(),
        no_proxy: String::new(),
        r#type: "http".into(),
        is_active: Some(true),
        strict_proxy: Some(false),
        test_status: None,
        last_tested_at: None,
        last_error: None,
        success_rate: None,
        rtt_ms: None,
        total_requests: None,
        failed_requests: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn combo() -> Combo {
    Combo {
        id: "combo-1".into(),
        name: "writer".into(),
        models: vec!["openai/gpt-4o-mini".into()],
        kind: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.provider_connections = vec![provider_connection()];
        state.proxy_pools = vec![proxy_pool()];
        state.combos = vec![combo()];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn get_provider_item_redacts_secrets() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/providers/provider-1")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(json["connection"]["apiKey"].is_null());
    assert!(json["connection"]["accessToken"].is_null());
    assert!(json["connection"]["refreshToken"].is_null());
}

#[tokio::test]
async fn update_key_item_toggles_active_flag() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/keys/key-1")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "isActive": false }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["key"]["isActive"], false);
}

#[tokio::test]
async fn delete_proxy_pool_rejects_bound_connections() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/proxy-pools/pool-1")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["error"], "Proxy pool is currently in use");
    assert_eq!(json["boundConnectionCount"], 1);
}

#[tokio::test]
async fn create_provider_round_trips_api_key_updates() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/providers")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "provider": "openai",
                        "name": "Created Provider",
                        "apiKey": "sk-old",
                        "providerSpecificData": {
                            "region": "us-east-1"
                        },
                        "connectionProxyEnabled": true,
                        "connectionProxyUrl": "http://proxy.created:8080",
                        "connectionNoProxy": "localhost",
                        "proxyPoolId": "pool-1"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(created.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(created.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = json["connection"]["id"]
        .as_str()
        .expect("provider id")
        .to_string();

    let updated = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/providers/{id}"))
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "apiKey": "sk-new" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(updated.status(), StatusCode::OK);

    let snapshot = state.db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .expect("provider exists");
    assert_eq!(connection.auth_type, "apikey");
    assert_eq!(connection.api_key.as_deref(), Some("sk-new"));
    assert_eq!(
        connection
            .provider_specific_data
            .get("proxyPoolId")
            .and_then(Value::as_str),
        Some("pool-1")
    );
    assert_eq!(
        connection
            .provider_specific_data
            .get("connectionProxyEnabled")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        connection
            .provider_specific_data
            .get("connectionProxyUrl")
            .and_then(Value::as_str),
        Some("http://proxy.created:8080")
    );
    assert_eq!(
        connection
            .provider_specific_data
            .get("connectionNoProxy")
            .and_then(Value::as_str),
        Some("localhost")
    );
    assert_eq!(
        connection
            .provider_specific_data
            .get("region")
            .and_then(Value::as_str),
        Some("us-east-1")
    );
}
