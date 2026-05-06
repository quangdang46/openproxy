use std::sync::Arc;

use axum::body::Body;
use axum::http::{header::SET_COOKIE, Request, StatusCode};
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

#[tokio::test]
async fn locale_post_sets_cookie_and_matches_js_payload() {
    let app = openproxy::build_app(app_state().await);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/locale")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "locale": "zh-CN"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("set-cookie header");
    assert!(cookie.contains("locale=zh-CN"));
    assert!(cookie.contains("Path=/"));
    assert!(cookie.contains("Max-Age=31536000"));

    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        json,
        json!({
            "success": true,
            "locale": "zh-CN"
        })
    );
}

#[tokio::test]
async fn locale_post_rejects_unsupported_locale_like_js() {
    let app = openproxy::build_app(app_state().await);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/locale")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "locale": "zh"
                    })
                    .to_string(),
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

    assert_eq!(
        json,
        json!({
            "error": "Invalid locale"
        })
    );
}
