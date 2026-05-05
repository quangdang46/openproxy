use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ModelAliasTarget, ProviderModelRef};
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
async fn models_alias_get_returns_wrapped_string_aliases() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.model_aliases.insert(
                "draft".into(),
                ModelAliasTarget::Path("openai/gpt-4.1".into()),
            );
            db.model_aliases.insert(
                "realtime".into(),
                ModelAliasTarget::Mapping(ProviderModelRef {
                    provider: "openai".into(),
                    model: "gpt-4o-realtime-preview".into(),
                    extra: BTreeMap::new(),
                }),
            );
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/models/alias",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "aliases": {
                "draft": "openai/gpt-4.1",
                "realtime": "openai/gpt-4o-realtime-preview"
            }
        })
    );
}

#[tokio::test]
async fn models_alias_put_requires_model_and_alias() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::PUT,
            "/api/models/alias",
            Body::from(r#"{"model":"","alias":"draft"}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Model and alias required" }));
}

#[tokio::test]
async fn models_alias_put_persists_path_alias() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(authorized_request(
            Method::PUT,
            "/api/models/alias",
            Body::from(r#"{"model":"openai/gpt-4.1","alias":"draft"}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "model": "openai/gpt-4.1",
            "alias": "draft"
        })
    );

    let snapshot = state.db.snapshot();
    assert_eq!(
        snapshot.model_aliases.get("draft"),
        Some(&ModelAliasTarget::Path("openai/gpt-4.1".into()))
    );
}

#[tokio::test]
async fn models_alias_delete_requires_alias_query() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/models/alias",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Alias required" }));
}

#[tokio::test]
async fn models_alias_delete_removes_only_requested_alias() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.model_aliases.insert(
                "draft".into(),
                ModelAliasTarget::Path("openai/gpt-4.1".into()),
            );
            db.model_aliases.insert(
                "mini".into(),
                ModelAliasTarget::Path("openai/gpt-4.1-mini".into()),
            );
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/models/alias?alias=draft",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true }));

    let snapshot = state.db.snapshot();
    assert!(!snapshot.model_aliases.contains_key("draft"));
    assert_eq!(
        snapshot.model_aliases.get("mini"),
        Some(&ModelAliasTarget::Path("openai/gpt-4.1-mini".into()))
    );
}
