use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

struct EnvVarGuard {
    key: &'static str,
    old_value: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old_value = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old_value }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.old_value.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    AppState::new(db)
}

fn get_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
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

async fn stop_proxy(app: &axum::Router) {
    let _ = app
        .clone()
        .oneshot(get_request("/api/oauth/codex/stop-proxy"))
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn codex_start_proxy_registers_server_side_session_and_poll_status() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let app = openproxy::build_app(app_state().await);
    stop_proxy(&app).await;

    let response = app
        .clone()
        .oneshot(get_request(
            "/api/oauth/codex/start-proxy?app_port=4624&state=state-1&code_verifier=verifier-1&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "serverSide": true }));

    let response = app
        .clone()
        .oneshot(get_request("/api/oauth/codex/poll-status?state=state-1"))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "status": "pending" }));

    stop_proxy(&app).await;
}

#[tokio::test]
async fn codex_proxy_fallback_redirects_to_app_callback() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let app = openproxy::build_app(app_state().await);
    stop_proxy(&app).await;

    let response = app
        .clone()
        .oneshot(get_request("/api/oauth/codex/start-proxy?app_port=4624"))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "serverSide": false }));

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client
        .get("http://127.0.0.1:1455/auth/callback?code=legacy-code&state=legacy-state")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:4624/callback?code=legacy-code&state=legacy-state")
    );

    stop_proxy(&app).await;
}

#[tokio::test]
async fn codex_proxy_server_side_callback_exchanges_and_clears_session() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CODEX_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"))
        .and(body_string_contains("code=auth-code"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .and(body_string_contains("code_verifier=proxy-verifier"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "codex-access",
            "refresh_token": "codex-refresh",
            "expires_in": 3600,
            "id_token": "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6ImNvZGV4QGV4YW1wbGUuY29tIn0.sig"
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    stop_proxy(&app).await;

    let response = app
        .clone()
        .oneshot(get_request(
            "/api/oauth/codex/start-proxy?app_port=4624&state=proxy-state&code_verifier=proxy-verifier&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "serverSide": true }));

    let response =
        reqwest::get("http://127.0.0.1:1455/auth/callback?code=auth-code&state=proxy-state")
            .await
            .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("Authentication Successful"));

    let response = app
        .clone()
        .oneshot(get_request(
            "/api/oauth/codex/poll-status?state=proxy-state",
        ))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "done");
    assert_eq!(json["email"], "codex@example.com");
    assert!(json["connectionId"].as_str().is_some());

    let response = app
        .clone()
        .oneshot(get_request(
            "/api/oauth/codex/poll-status?state=proxy-state",
        ))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "status": "unknown" }));

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "codex");
    assert_eq!(connection.email.as_deref(), Some("codex@example.com"));
    assert_eq!(connection.access_token.as_deref(), Some("codex-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("codex-refresh"));

    stop_proxy(&app).await;
}
