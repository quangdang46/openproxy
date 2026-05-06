use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::core::auth::parse_api_key;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "keys-api-test-key";

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

async fn app_state(keys: Vec<ApiKey>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = keys;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn create_key_returns_machine_bound_key_shape() {
    let state = app_state(vec![]).await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": "Laptop" }).to_string()))
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
    assert!(json.get("isActive").is_none());
    assert!(json.get("createdAt").is_none());
    assert_eq!(json["name"], "Laptop");
    assert!(json["id"].is_string());
    assert!(json["machineId"].is_string());

    let key = json["key"].as_str().expect("key");
    let machine_id = json["machineId"].as_str().expect("machineId");
    let parsed = parse_api_key(key).expect("generated key should parse");
    assert_eq!(parsed.machine_id.as_deref(), Some(machine_id));

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.api_keys.len(), 1);
    assert_eq!(snapshot.api_keys[0].name, "Laptop");
    assert_eq!(snapshot.api_keys[0].machine_id.as_deref(), Some(machine_id));
    assert_eq!(snapshot.api_keys[0].key, key);
}

#[tokio::test]
async fn create_key_rejects_missing_name() {
    let app = openproxy::build_app(app_state(vec![]).await);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys")
                .header("content-type", "application/json")
                .body(Body::from(json!({}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "Name is required");
}

#[tokio::test]
async fn create_key_with_existing_keys_requires_auth_and_keeps_response_shape() {
    let app = openproxy::build_app(app_state(vec![active_key()]).await);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": "Desktop" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["name"], "Desktop");
    assert!(json["machineId"].is_string());
    assert!(parse_api_key(json["key"].as_str().expect("key")).is_some());
}
