use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, CustomModel};
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
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn authorized_request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

#[tokio::test]
async fn models_custom_get_returns_wrapped_models() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.custom_models.push(CustomModel {
                provider_alias: "oa".into(),
                id: "gpt-custom".into(),
                r#type: "llm".into(),
                name: Some("Custom".into()),
                extra: BTreeMap::new(),
            });
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/models/custom",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "models": [{
                "providerAlias": "oa",
                "id": "gpt-custom",
                "type": "llm",
                "name": "Custom"
            }]
        })
    );
}

#[tokio::test]
async fn models_custom_post_requires_provider_alias_and_id() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::POST,
            "/api/models/custom",
            Body::from(r#"{"providerAlias":"","id":"gpt-custom"}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "providerAlias and id required" }));
}

#[tokio::test]
async fn models_custom_post_returns_added_true_then_false_for_duplicate() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());

    let first = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/models/custom",
            Body::from(r#"{"providerAlias":"oa","id":"gpt-custom","type":"llm","name":"Custom"}"#),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(first).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "added": true }));

    let second = app
        .oneshot(authorized_request(
            Method::POST,
            "/api/models/custom",
            Body::from(r#"{"providerAlias":"oa","id":"gpt-custom","type":"llm","name":"Other"}"#),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(second).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "added": false }));

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.custom_models.len(), 1);
    assert_eq!(snapshot.custom_models[0].name.as_deref(), Some("Custom"));
}

#[tokio::test]
async fn models_custom_delete_requires_provider_alias_and_id() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/models/custom?providerAlias=oa",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "providerAlias and id required" }));
}

#[tokio::test]
async fn models_custom_delete_query_removes_matching_model_only() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.custom_models.push(CustomModel {
                provider_alias: "oa".into(),
                id: "gpt-custom".into(),
                r#type: "llm".into(),
                name: Some("Custom".into()),
                extra: BTreeMap::new(),
            });
            db.custom_models.push(CustomModel {
                provider_alias: "oa".into(),
                id: "gpt-custom".into(),
                r#type: "embedding".into(),
                name: Some("Embedding".into()),
                extra: BTreeMap::new(),
            });
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/models/custom?providerAlias=oa&id=gpt-custom&type=llm",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true }));

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.custom_models.len(), 1);
    assert_eq!(snapshot.custom_models[0].r#type, "embedding");
}
