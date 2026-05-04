use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::api::providers;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "providers-api-test-key";

// Helper to create a test AppState with provider connections
fn connection_with_id(provider: &str, id: &str) -> ProviderConnection {
    let mut provider_specific_data = BTreeMap::new();
    provider_specific_data.insert(
        "baseUrl".into(),
        serde_json::Value::String("https://api.test.com".into()),
    );

    ProviderConnection {
        id: id.to_string(),
        provider: provider.to_string(),
        auth_type: "api_key".to_string(),
        name: Some(format!("{} Provider", provider)),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: Some("gpt-4".to_string()),
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: None, // No API key
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

async fn test_state(connections: Vec<ProviderConnection>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.provider_connections = connections;
        state.api_keys = vec![ApiKey {
            id: "test-key-id".into(),
            name: "test".into(),
            key: TEST_KEY.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: BTreeMap::new(),
        }];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

// ============================================================
// Tests for GET /api/providers/kilo/free-models
// ============================================================

#[tokio::test]
async fn test_kilo_free_models_returns_models() {
    let state = test_state(vec![]).await;
    let app = providers::routes().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/providers/kilo/free-models")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 2048)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(json["models"].is_array());
    assert!(!json["models"].as_array().unwrap().is_empty());

    // Verify structure of first model
    let first_model = &json["models"][0];
    assert!(first_model["id"].is_string());
    assert!(first_model["name"].is_string());
}

#[tokio::test]
async fn test_kilo_free_models_contains_expected_models() {
    let state = test_state(vec![]).await;
    let app = providers::routes().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/providers/kilo/free-models")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(response.into_body(), 2048)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let model_ids: Vec<&str> = json["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();

    // Check for known free models
    assert!(model_ids.contains(&"kilo/gpt-4.1-mini"));
    assert!(model_ids.contains(&"kilo/qwen3-8b"));
    assert!(model_ids.contains(&"kilo/phi-4"));
}

#[tokio::test]
async fn test_kilo_free_models_has_pricing_info() {
    let state = test_state(vec![]).await;
    let app = providers::routes().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/providers/kilo/free-models")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(response.into_body(), 2048)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Check that some models have pricing
    let models_with_pricing = json["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["pricing"].is_object())
        .count();

    assert!(
        models_with_pricing > 0,
        "At least some models should have pricing info"
    );
}

// ============================================================
// Tests for POST /api/providers/test-batch
// ============================================================

#[tokio::test]
async fn test_test_batch_empty_list() {
    let state = test_state(vec![]).await;
    let app = providers::routes().with_state(state);

    let request_body = json!({
        "providerIds": []
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/providers/test-batch")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .header("Content-Type", "application/json")
                .body(Body::from(request_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 2048)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(json["results"].is_array());
    assert_eq!(json["results"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_test_batch_not_found() {
    let state = test_state(vec![]).await;
    let app = providers::routes().with_state(state);

    let request_body = json!({
        "providerIds": ["non-existent-id"]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/providers/test-batch")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .header("Content-Type", "application/json")
                .body(Body::from(request_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 2048)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["results"].as_array().unwrap().len(), 1);
    assert_eq!(json["results"][0]["providerId"], "non-existent-id");
    assert_eq!(json["results"][0]["valid"], false);
    assert!(json["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("not found"));
}

#[tokio::test]
async fn test_test_batch_multiple_providers() {
    let connections = vec![
        connection_with_id("openai", "provider-1"),
        connection_with_id("openai", "provider-2"),
    ];

    let state = test_state(connections).await;
    let app = providers::routes().with_state(state);

    let request_body = json!({
        "providerIds": ["provider-1", "provider-2", "non-existent"]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/providers/test-batch")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .header("Content-Type", "application/json")
                .body(Body::from(request_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 2048)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["results"].as_array().unwrap().len(), 3);
}

// ============================================================
// Tests for GET /api/providers/client
// ============================================================

#[tokio::test]
async fn test_client_info_returns_info() {
    let state = test_state(vec![]).await;
    let app = providers::routes().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/providers/client")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 2048)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Check that all expected fields are present
    assert!(json["clientId"].is_string());
    assert!(json["clientName"].is_string());
    assert!(json["version"].is_string());
    assert!(json["provider"].is_string());

    // Version should match the crate version
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn test_client_info_provider_from_settings() {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.settings.tunnel_provider = "cloudflare".to_string();
        state.api_keys = vec![ApiKey {
            id: "test-key-id".into(),
            name: "test".into(),
            key: TEST_KEY.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: BTreeMap::new(),
        }];
    })
    .await
    .expect("seed db");
    let state = AppState::new(db);

    let app = providers::routes().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/providers/client")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(response.into_body(), 2048)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["provider"], "cloudflare");
}
