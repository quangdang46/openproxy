//! A2A endpoint auth + credential redaction tests.
//!
//! The A2A task endpoints previously serialized full `ProviderConnection`
//! snapshots (tokens included) with no authentication. They must now require
//! admin auth, and the provider-management skill response must be redacted.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.settings.require_login = true;
        state.api_keys = vec![openproxy::types::ApiKey {
            id: "admin-1".into(),
            name: "Local".into(),
            key: "admin-key".into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: Default::default(),
        }];
    })
    .await
    .expect("db update");
    AppState::new(db)
}

#[tokio::test]
async fn a2a_task_endpoints_require_auth() {
    let app = openproxy::build_app(app_state().await);

    // POST /api/a2a/tasks/send without any auth → 401.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/a2a/tasks/send")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "message": "hi" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // GET /api/a2a/tasks/{id} without auth → 401.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/a2a/tasks/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // POST /api/a2a/tasks/{id}/cancel without auth → 401.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/a2a/tasks/nonexistent/cancel")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a2a_task_send_with_valid_key_passes_auth() {
    let app = openproxy::build_app(app_state().await);

    // Valid management API key → auth passes; the request reaches the handler
    // (a malformed body yields 400 from the handler, not 401 from auth).
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/a2a/tasks/send")
                .header("content-type", "application/json")
                .header("authorization", "Bearer admin-key")
                .body(Body::from(json!({ "message": "hi" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a2a_discovery_endpoints_stay_public() {
    let app = openproxy::build_app(app_state().await);

    // Agent card is discovery — no auth.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/agent.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/a2a/agent-card")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
