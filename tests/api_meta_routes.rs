use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "api-meta-test-key";

async fn build_test_app() -> axum::Router {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "test-key-id".to_string(),
            name: "test".to_string(),
            key: TEST_KEY.to_string(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: Default::default(),
        }];
        state.settings.require_login = false;
    })
    .await
    .expect("seed auth");
    openproxy::build_app(AppState::new(db))
}

#[tokio::test]
async fn api_health_returns_sidecar_compatible_payload() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json, serde_json::json!({ "ok": true }));
}

#[tokio::test]
async fn cloud_auth_route_is_served_by_rust() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/cloud/auth")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["connections"], serde_json::json!([]));
    assert!(json["modelAliases"].is_object());
}

#[tokio::test]
async fn settings_proxy_test_route_rejects_missing_proxy_url() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/proxy-test")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "ok": false, "error": "proxyUrl is required" })
    );
}

#[tokio::test]
async fn usage_logs_route_returns_array_payload() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/usage/logs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json, serde_json::json!([]));
}
