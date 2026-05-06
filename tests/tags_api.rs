use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    AppState::new(db)
}

async fn response_json(
    response: axum::response::Response,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, headers, json)
}

#[tokio::test]
async fn tags_get_matches_9router_payload_and_cors_headers() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/tags")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let (status, headers, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some("*")
    );
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .and_then(|value| value.to_str().ok()),
        Some("GET, OPTIONS")
    );
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .and_then(|value| value.to_str().ok()),
        Some("*")
    );
    assert_eq!(
        json,
        json!({
            "models": [
                {
                    "name": "llama3.2",
                    "modified_at": "2025-12-26T00:00:00Z",
                    "size": 2000000000_u64,
                    "digest": "abc123def456",
                    "details": {
                        "format": "gguf",
                        "family": "llama",
                        "parameter_size": "3B",
                        "quantization_level": "Q4_K_M"
                    }
                },
                {
                    "name": "qwen2.5",
                    "modified_at": "2025-12-26T00:00:00Z",
                    "size": 4000000000_u64,
                    "digest": "def456abc123",
                    "details": {
                        "format": "gguf",
                        "family": "qwen",
                        "parameter_size": "7B",
                        "quantization_level": "Q4_K_M"
                    }
                }
            ]
        })
    );
}

#[tokio::test]
async fn tags_options_returns_cors_headers() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/tags")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some("*")
    );
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .and_then(|value| value.to_str().ok()),
        Some("GET, OPTIONS")
    );
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .and_then(|value| value.to_str().ok()),
        Some("*")
    );
}

#[tokio::test]
async fn tags_legacy_subroutes_are_not_exposed() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/tags/legacy-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
