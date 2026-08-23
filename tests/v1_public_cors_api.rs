use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
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
        monthly_budget_usd: None,
    }
}

async fn seeded_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn public_v1_endpoints_expose_cors_preflight() {
    let app = openproxy::build_app(seeded_state().await);

    for (path, methods, status) in [
        // JS OPTIONS handlers use `new Response(null, {headers})` — the
        // default status is 200, not 204 (v1/chat/completions/route.js:19-26).
        ("/v1/chat/completions", "GET, POST, OPTIONS", StatusCode::OK),
        ("/v1/audio/speech", "POST, OPTIONS", StatusCode::OK),
        ("/v1/embeddings", "POST, OPTIONS", StatusCode::OK),
        ("/v1/images/generations", "POST, OPTIONS", StatusCode::OK),
        ("/v1/search", "POST, OPTIONS", StatusCode::OK),
        ("/v1/web/fetch", "POST, OPTIONS", StatusCode::OK),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), status, "{path}");
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "*",
            "{path}"
        );
        let got_methods = response
            .headers()
            .get("access-control-allow-methods")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        // CorsLayer with allow_methods(Any) emits "*"; JS emits an explicit
        // list. Either way the preflight must permit the request.
        assert!(
            got_methods == "*" || got_methods.contains("POST") || got_methods.contains("GET"),
            "{path}: allow-methods should permit requests, got {got_methods:?}"
        );
    }
}

#[tokio::test]
async fn missing_model_errors_keep_cors_headers_on_public_v1_routes() {
    let app = openproxy::build_app(seeded_state().await);

    for path in [
        "/v1/chat/completions",
        "/v1/audio/speech",
        "/v1/embeddings",
        "/v1/images/generations",
        "/v1/search",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("authorization", "Bearer valid-bearer")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"stream":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "*",
            "{path}"
        );
    }
}
