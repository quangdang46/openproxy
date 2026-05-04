// Integration tests for /v1/web/fetch endpoint.
// Covers: CORS preflight, request validation, auth gating, provider error handling.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection};
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
    }
}



async fn seeded_state(
    keys: Vec<ApiKey>,
    connections: Vec<ProviderConnection>,
    require_login: bool,
) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(move |state| {
        state.api_keys = keys;
        state.provider_connections = connections;
        state.settings.require_login = require_login;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn web_fetch_options_exposes_cors_and_post_method() {
    let app = openproxy::build_app(seeded_state(vec![], vec![], false).await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/web/fetch")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("access-control-allow-origin").unwrap().to_str().unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers().get("access-control-allow-methods").unwrap().to_str().unwrap(),
        "POST, OPTIONS"
    );
}

#[tokio::test]
async fn web_fetch_requires_auth_when_require_login_is_true() {
    let app = openproxy::build_app(
        seeded_state(vec![active_key("valid-key")], vec![], true).await,
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/web/fetch")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"firecrawl","url":"https://example.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn web_fetch_allows_no_auth_when_require_login_is_false() {
    let app = openproxy::build_app(seeded_state(vec![], vec![], false).await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/web/fetch")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"firecrawl","url":"https://example.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // Should NOT be 401 — but 400 because no credentials
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("No credentials"));
}

#[tokio::test]
async fn web_fetch_rejects_missing_url() {
    let app = openproxy::build_app(
        seeded_state(vec![active_key("key")], vec![], true).await,
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/web/fetch")
                .header("authorization", "Bearer key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"firecrawl"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn web_fetch_rejects_missing_provider() {
    let app = openproxy::build_app(
        seeded_state(vec![active_key("key")], vec![], true).await,
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/web/fetch")
                .header("authorization", "Bearer key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"https://example.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn web_fetch_rejects_invalid_url() {
    let app = openproxy::build_app(
        seeded_state(vec![active_key("key")], vec![], true).await,
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/web/fetch")
                .header("authorization", "Bearer key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"firecrawl","url":"not-valid"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("Invalid URL"));
}

#[tokio::test]
async fn web_fetch_rejects_empty_provider() {
    let app = openproxy::build_app(
        seeded_state(vec![active_key("key")], vec![], true).await,
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/web/fetch")
                .header("authorization", "Bearer key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"","url":"https://example.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn web_fetch_model_alias_works_as_provider() {
    // UI sends "model" instead of "provider" — must work
    let app = openproxy::build_app(
        seeded_state(vec![active_key("key")], vec![], true).await,
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/web/fetch")
                .header("authorization", "Bearer key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"firecrawl","url":"https://example.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // Should reach provider lookup (401 if auth ok but no creds) not BAD_REQUEST for missing field
    // 400 because no credentials is the correct "empty state" response
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("No credentials"));
}
