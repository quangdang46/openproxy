use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_json, header, method, path};
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

fn request(body: Body) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/api/oauth/iflow/cookie")
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
async fn iflow_cookie_route_matches_9router_success_flow() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _env = EnvVarGuard::set("OPENPROXY_IFLOW_API_BASE_URL", &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/openapi/apikey"))
        .and(header("cookie", "BXAuth=abc123; session=1;"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "data": {
                "name": "Primary Key"
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/openapi/apikey"))
        .and(header("cookie", "BXAuth=abc123; session=1;"))
        .and(body_json(json!({ "name": "Primary Key" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "data": {
                "name": "Renamed Key",
                "apiKey": "iflow-secret-123456",
                "expireTime": "2099-01-01T00:00:00Z"
            }
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(request(Body::from(
            json!({ "cookie": "  BXAuth=abc123; session=1" }).to_string(),
        )))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "iflow");
    assert_eq!(json["connection"]["email"], "Renamed Key");
    assert_eq!(json["connection"]["apiKey"], "iflow-secr...");
    assert_eq!(json["connection"]["expireTime"], "2099-01-01T00:00:00Z");

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "iflow");
    assert_eq!(connection.auth_type, "cookie");
    assert_eq!(connection.name.as_deref(), Some("Renamed Key"));
    assert_eq!(connection.email.as_deref(), Some("Renamed Key"));
    assert_eq!(connection.api_key.as_deref(), Some("iflow-secret-123456"));
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert_eq!(
        connection.provider_specific_data.get("cookie"),
        Some(&json!("BXAuth=abc123;"))
    );
    assert_eq!(
        connection.provider_specific_data.get("expireTime"),
        Some(&json!("2099-01-01T00:00:00Z"))
    );
}

#[tokio::test]
async fn iflow_cookie_route_validates_cookie_input_like_9router() {
    let app = openproxy::build_app(app_state().await);

    let missing_cookie = app
        .clone()
        .oneshot(request(Body::from(r#"{}"#)))
        .await
        .unwrap();
    let (status, json) = response_json(missing_cookie).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Cookie is required" }));

    let missing_bxauth = app
        .oneshot(request(Body::from(
            json!({ "cookie": "session=1;" }).to_string(),
        )))
        .await
        .unwrap();
    let (status, json) = response_json(missing_bxauth).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Cookie must contain BXAuth field" }));
}

#[tokio::test]
async fn iflow_cookie_route_propagates_get_failure_status_and_message() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _env = EnvVarGuard::set("OPENPROXY_IFLOW_API_BASE_URL", &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/openapi/apikey"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid cookie"))
        .mount(&server)
        .await;

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(request(Body::from(
            json!({ "cookie": "BXAuth=abc123;" }).to_string(),
        )))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        json,
        json!({ "error": "Failed to fetch API key info: invalid cookie" })
    );
}

#[tokio::test]
async fn iflow_cookie_route_propagates_refresh_failure_message() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _env = EnvVarGuard::set("OPENPROXY_IFLOW_API_BASE_URL", &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/openapi/apikey"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "data": {
                "name": "Primary Key"
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/openapi/apikey"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": false,
            "message": "quota exceeded"
        })))
        .mount(&server)
        .await;

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(request(Body::from(
            json!({ "cookie": "BXAuth=abc123;" }).to_string(),
        )))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json,
        json!({ "error": "API key refresh failed: quota exceeded" })
    );
}
