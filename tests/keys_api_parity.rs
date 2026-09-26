use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

/// The management key — every request below authenticates with it.
const MANAGEMENT_KEY: &str = "keys-parity-management-key";
/// Plaintext of `key-2`, the row the by-id tests read and mutate.
const TEST_KEY: &str = "keys-parity-test-key";

fn api_key(id: &str, name: &str, key: &str) -> ApiKey {
    ApiKey {
        id: id.into(),
        name: name.into(),
        key: key.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
        monthly_budget_usd: None,
    }
}

/// `key-1` is the management key and `key-2` the row under test, so a test that
/// deactivates its target cannot take its own Bearer credential with it.
async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![
            api_key("key-1", "Management", MANAGEMENT_KEY),
            api_key("key-2", "Target", TEST_KEY),
        ];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn key_request(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {MANAGEMENT_KEY}"))
        .header("content-type", "application/json");
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    builder.body(body).expect("request")
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test]
async fn get_key_by_id_returns_the_key() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(key_request("GET", "/api/keys/key-2", None))
        .await
        .unwrap();

    // Before the route was registered axum's MethodRouter answered 405 here.
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["key"]["id"], "key-2");
    assert_eq!(body["key"]["key"], TEST_KEY);
}

#[tokio::test]
async fn get_key_by_id_404s_for_an_unknown_id() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(key_request("GET", "/api/keys/does-not-exist", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json_body(response).await;
    assert_eq!(body["error"], "Key not found");
}

/// 9router's `updateApiKey` returns the stored row, so the plaintext key
/// round-trips; the mask used to stand in for it and corrupted any client that
/// re-stored the response.
#[tokio::test]
async fn put_key_returns_the_real_key_in_the_response() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(key_request(
            "PUT",
            "/api/keys/key-2",
            Some(json!({ "isActive": false })),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["key"]["key"], TEST_KEY);
}

/// Writing the mask into the row would satisfy the test above, so pin the
/// stored value separately.
#[tokio::test]
async fn put_key_does_not_mutate_the_stored_key() {
    let app = openproxy::build_app(app_state().await);
    app.clone()
        .oneshot(key_request(
            "PUT",
            "/api/keys/key-2",
            Some(json!({ "isActive": false })),
        ))
        .await
        .unwrap();

    let response = app
        .oneshot(key_request("GET", "/api/keys", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let stored = body["keys"]
        .as_array()
        .expect("keys array")
        .iter()
        .find(|k| k["id"] == "key-2")
        .expect("key-2 listed");
    assert_eq!(stored["key"], TEST_KEY);
    assert_eq!(stored["isActive"], false);
}

#[tokio::test]
async fn key_not_found_error_string_matches_across_put_and_delete() {
    for (method, body) in [("PUT", Some(json!({}))), ("DELETE", None)] {
        let app = openproxy::build_app(app_state().await);
        let response = app
            .oneshot(key_request(method, "/api/keys/nope", body))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} miss");
        let body = json_body(response).await;
        assert_eq!(body["error"], "Key not found", "{method} miss");
    }
}

#[tokio::test]
async fn delete_key_returns_the_javascript_message() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(key_request("DELETE", "/api/keys/key-2", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["message"], "Key deleted successfully");
    assert!(body.get("success").is_none());
}
