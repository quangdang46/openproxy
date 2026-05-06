use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProxyPool};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "proxy-pools-api-test-key";

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

fn provider_connection(id: &str, proxy_pool_id: &str) -> ProviderConnection {
    let mut provider_specific_data = BTreeMap::new();
    provider_specific_data.insert("proxyPoolId".into(), Value::String(proxy_pool_id.into()));

    ProviderConnection {
        id: id.into(),
        provider: "openai".into(),
        auth_type: "api_key".into(),
        name: Some(format!("Conn {id}")),
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
        api_key: None,
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

fn proxy_pool(id: &str, name: &str, is_active: bool, updated_at: &str) -> ProxyPool {
    ProxyPool {
        id: id.into(),
        name: name.into(),
        proxy_url: format!("http://{id}.proxy.test:8080"),
        no_proxy: String::new(),
        r#type: "http".into(),
        is_active: Some(is_active),
        strict_proxy: Some(false),
        test_status: Some("unknown".into()),
        last_tested_at: None,
        last_error: None,
        success_rate: None,
        rtt_ms: None,
        total_requests: None,
        failed_requests: None,
        created_at: Some(updated_at.into()),
        updated_at: Some(updated_at.into()),
        extra: BTreeMap::new(),
    }
}

async fn app_state(proxy_pools: Vec<ProxyPool>, connections: Vec<ProviderConnection>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.proxy_pools = proxy_pools;
        state.provider_connections = connections;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn list_proxy_pools_filters_sorts_and_counts_usage() {
    let state = app_state(
        vec![
            proxy_pool("pool-1", "Primary", true, "2026-05-05T10:00:00Z"),
            proxy_pool("pool-2", "Backup", true, "2026-05-05T11:00:00Z"),
            proxy_pool("pool-3", "Disabled", false, "2026-05-05T12:00:00Z"),
        ],
        vec![
            provider_connection("conn-1", "pool-1"),
            provider_connection("conn-2", "pool-3"),
        ],
    )
    .await;
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/proxy-pools?isActive=true&includeUsage=true")
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
    let proxy_pools = json["proxyPools"].as_array().expect("proxyPools array");

    assert_eq!(proxy_pools.len(), 2);
    assert_eq!(proxy_pools[0]["id"], "pool-2");
    assert_eq!(proxy_pools[0]["boundConnectionCount"], 0);
    assert_eq!(proxy_pools[1]["id"], "pool-1");
    assert_eq!(proxy_pools[1]["boundConnectionCount"], 1);
}

#[tokio::test]
async fn create_proxy_pool_matches_js_defaults_and_shape() {
    let state = app_state(vec![], vec![]).await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/proxy-pools")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": " Relay ",
                        "proxyUrl": " http://relay.proxy.test:8080 ",
                        "noProxy": " localhost,127.0.0.1 ",
                        "strictProxy": true,
                        "type": "invalid"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(json.get("success").is_none());
    let proxy_pool = &json["proxyPool"];
    assert_eq!(proxy_pool["name"], "Relay");
    assert_eq!(proxy_pool["proxyUrl"], "http://relay.proxy.test:8080");
    assert_eq!(proxy_pool["noProxy"], "localhost,127.0.0.1");
    assert_eq!(proxy_pool["type"], "http");
    assert_eq!(proxy_pool["isActive"], true);
    assert_eq!(proxy_pool["strictProxy"], true);
    assert_eq!(proxy_pool["testStatus"], "unknown");
    assert!(proxy_pool["createdAt"].is_string());
    assert!(proxy_pool["updatedAt"].is_string());

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.proxy_pools.len(), 1);
    assert_eq!(snapshot.proxy_pools[0].name, "Relay");
    assert_eq!(snapshot.proxy_pools[0].r#type, "http");
    assert_eq!(snapshot.proxy_pools[0].strict_proxy, Some(true));
}

#[tokio::test]
async fn update_proxy_pool_matches_js_normalization() {
    let state = app_state(
        vec![proxy_pool(
            "pool-1",
            "Primary",
            true,
            "2026-05-05T10:00:00Z",
        )],
        vec![],
    )
    .await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/proxy-pools/pool-1")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": " Renamed Pool ",
                        "proxyUrl": " http://renamed.proxy.test:8080 ",
                        "noProxy": " localhost ",
                        "isActive": false,
                        "strictProxy": false,
                        "type": "not-real"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["proxyPool"]["name"], "Renamed Pool");
    assert_eq!(
        json["proxyPool"]["proxyUrl"],
        "http://renamed.proxy.test:8080"
    );
    assert_eq!(json["proxyPool"]["noProxy"], "localhost");
    assert_eq!(json["proxyPool"]["isActive"], false);
    assert_eq!(json["proxyPool"]["strictProxy"], false);
    assert_eq!(json["proxyPool"]["type"], "http");

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.proxy_pools[0].name, "Renamed Pool");
    assert_eq!(
        snapshot.proxy_pools[0].proxy_url,
        "http://renamed.proxy.test:8080"
    );
    assert_eq!(snapshot.proxy_pools[0].r#type, "http");
}
