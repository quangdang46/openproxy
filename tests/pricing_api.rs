use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.pricing = BTreeMap::from([(
            "openai".to_string(),
            BTreeMap::from([(
                "gpt-4o".to_string(),
                json!({
                    "input": 9.0,
                    "output": 12.0
                }),
            )]),
        )]);
    })
    .await
    .expect("seed pricing");
    AppState::new(db)
}

fn request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn pricing_get_merges_defaults_with_user_pricing() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(request(Method::GET, "/api/pricing", Body::empty()))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["openai"]["gpt-4o"]["input"], 9.0);
    assert_eq!(json["openai"]["gpt-4o"]["output"], 12.0);
    assert_eq!(json["gh"]["gpt-5.3-codex"]["input"], 1.75);
    assert_eq!(json["gh"]["gpt-5.3-codex"]["cache_creation"], 1.75);
}

#[tokio::test]
async fn pricing_patch_validates_and_returns_user_pricing_only() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(request(
            Method::PATCH,
            "/api/pricing",
            Body::from(
                json!({
                    "gh": {
                        "gpt-5.3-codex": {
                            "input": 2.0,
                            "output": 16.0
                        }
                    },
                    "custom": {
                        "model-x": {
                            "reasoning": 4.5
                        }
                    }
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["gh"]["gpt-5.3-codex"]["input"], 2.0);
    assert_eq!(json["gh"]["gpt-5.3-codex"]["output"], 16.0);
    assert_eq!(json["custom"]["model-x"]["reasoning"], 4.5);
    assert!(json.get("openai").is_some());

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.pricing["gh"]["gpt-5.3-codex"]["input"], 2.0);
    assert_eq!(snapshot.pricing["custom"]["model-x"]["reasoning"], 4.5);
}

#[tokio::test]
async fn pricing_patch_rejects_invalid_field() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(request(
            Method::PATCH,
            "/api/pricing",
            Body::from(
                json!({
                    "openai": {
                        "gpt-4o": {
                            "bogus": 1.0
                        }
                    }
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        json["error"],
        "Invalid pricing field: bogus for openai/gpt-4o"
    );
}

#[tokio::test]
async fn pricing_delete_resets_model_and_returns_merged_pricing() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.pricing.entry("gh".into()).or_default().insert(
                "gpt-5.3-codex".into(),
                json!({
                    "input": 3.0
                }),
            );
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(request(
            Method::DELETE,
            "/api/pricing?provider=gh&model=gpt-5.3-codex",
            Body::empty(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["gh"]["gpt-5.3-codex"]["input"], 1.75);
    assert_eq!(json["gh"]["gpt-5.3-codex"]["output"], 14.0);

    let snapshot = state.db.snapshot();
    assert!(snapshot
        .pricing
        .get("gh")
        .and_then(|provider| provider.get("gpt-5.3-codex"))
        .is_none());
}
