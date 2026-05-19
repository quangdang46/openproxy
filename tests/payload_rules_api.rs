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

const TEST_KEY: &str = "payload-rules-api-test-key";

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

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state
            .settings
            .extra
            .insert("password".into(), Value::String("hashed-secret".into()));
        state.settings.require_login = true;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn get_payload_rules_requires_auth() {
    let app = openproxy::build_app(app_state().await);

    let unauthenticated = app
        .oneshot(
            Request::builder()
                .uri("/api/settings/payload-rules")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn put_payload_rules_persists_and_normalizes() {
    let app = openproxy::build_app(app_state().await);

    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings/payload-rules")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "default": [
                            {
                                "models": [{ "name": "gpt-4*" }],
                                "params": { "temperature": 0.2 }
                            }
                        ],
                        "override": [
                            {
                                "models": [{ "name": "o1*", "protocol": "openai" }],
                                "params": {
                                    "reasoning_effort": "medium",
                                    "max_tokens": 4096
                                }
                            }
                        ],
                        "filter": [
                            {
                                "models": [{ "name": "claude-*" }],
                                "params": ["metadata.user_id"]
                            }
                        ],
                        "defaultRaw": [
                            // Empty params block — should be dropped by normalize().
                            { "models": [{ "name": "*" }], "params": {} }
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(put.status(), StatusCode::OK);
    let body = axum::body::to_bytes(put.into_body(), 8192).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["summary"]["default"], 1);
    assert_eq!(json["summary"]["override"], 1);
    assert_eq!(json["summary"]["filter"], 1);
    assert_eq!(json["summary"]["defaultRaw"], 0); // dropped — empty params
    assert_eq!(
        json["config"]["override"][0]["params"]["reasoning_effort"],
        "medium"
    );

    let get = app
        .oneshot(
            Request::builder()
                .uri("/api/settings/payload-rules")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let body = axum::body::to_bytes(get.into_body(), 8192).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["summary"]["default"], 1);
    assert_eq!(json["summary"]["override"], 1);
}

#[tokio::test]
async fn system_prompt_round_trip() {
    let app = openproxy::build_app(app_state().await);

    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings/system-prompt")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "mode": "override",
                        "content": "You are an internal helper."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(put.status(), StatusCode::OK);
    let body = axum::body::to_bytes(put.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["mode"], "override");
    assert_eq!(json["active"], true);

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/settings/system-prompt")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(get.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["mode"], "override");
    assert_eq!(json["content"], "You are an internal helper.");

    // Switching to Off makes it inactive even with content present.
    let put = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings/system-prompt")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "mode": "off" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(put.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["mode"], "off");
    assert_eq!(json["active"], false);
}
