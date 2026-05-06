use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::core::combo::{
    get_rotated_models, reset_combo_rotation, rotation_index, ComboStrategy,
};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, Combo};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "combos-api-test-key";

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

fn combo(id: &str, name: &str) -> Combo {
    Combo {
        id: id.into(),
        name: name.into(),
        models: vec!["openai/gpt-4o-mini".into()],
        kind: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

async fn app_state(combos: Vec<Combo>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.combos = combos;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn create_combo_returns_direct_combo_body_with_null_kind() {
    let app = openproxy::build_app(app_state(vec![]).await);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/combos")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "writer.combo",
                        "models": ["openai/gpt-4o-mini", "claude/sonnet"]
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

    assert!(json.get("combo").is_none());
    assert!(json["id"].is_string());
    assert_eq!(json["name"], "writer.combo");
    assert_eq!(
        json["models"],
        json!(["openai/gpt-4o-mini", "claude/sonnet"])
    );
    assert!(json["kind"].is_null());
    assert!(json["createdAt"].is_string());
    assert!(json["updatedAt"].is_string());
}

#[tokio::test]
async fn create_combo_rejects_missing_name() {
    let app = openproxy::build_app(app_state(vec![]).await);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/combos")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "models": ["openai/gpt-4o-mini"] }).to_string(),
                ))
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
async fn create_combo_rejects_duplicate_name() {
    let app = openproxy::build_app(app_state(vec![combo("combo-1", "writer")]).await);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/combos")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": "writer" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "Combo name already exists");
}

#[tokio::test]
async fn update_combo_resets_rotation_state() {
    let original_name = "writer-update-reset";
    let renamed_name = "writer-update-reset-renamed";
    reset_combo_rotation(Some(original_name));
    reset_combo_rotation(Some(renamed_name));

    let models = vec![
        "openai/gpt-4o-mini".to_string(),
        "claude/sonnet".to_string(),
    ];
    let _ = get_rotated_models(&models, Some(original_name), ComboStrategy::RoundRobin);
    assert_eq!(rotation_index(original_name), Some(1));

    let app = openproxy::build_app(app_state(vec![combo("combo-1", original_name)]).await);
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/combos/combo-1")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": renamed_name }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], renamed_name);
    assert_eq!(rotation_index(original_name), None);
    assert_eq!(rotation_index(renamed_name), None);
}

#[tokio::test]
async fn delete_combo_resets_rotation_state() {
    let combo_name = "writer-delete-reset";
    reset_combo_rotation(Some(combo_name));

    let models = vec![
        "openai/gpt-4o-mini".to_string(),
        "claude/sonnet".to_string(),
    ];
    let _ = get_rotated_models(&models, Some(combo_name), ComboStrategy::RoundRobin);
    assert_eq!(rotation_index(combo_name), Some(1));

    let app = openproxy::build_app(app_state(vec![combo("combo-1", combo_name)]).await);
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/combos/combo-1")
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
    assert_eq!(json["success"], true);
    assert_eq!(rotation_index(combo_name), None);
}
