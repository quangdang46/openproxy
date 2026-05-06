use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_json, method, path};
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
        .uri("/api/oauth/kiro/import")
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

fn make_jwt(email: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        json!({
            "email": email,
            "preferred_username": email,
            "sub": "user-123"
        })
        .to_string(),
    );
    format!("{header}.{payload}.kiro-signature-padding")
}

#[tokio::test]
async fn kiro_import_route_matches_9router_success_flow() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _env = EnvVarGuard::set("OPENPROXY_KIRO_AUTH_SERVICE_BASE_URL", &server.uri());
    let access_token = make_jwt("me@example.com");

    Mock::given(method("POST"))
        .and(path("/refreshToken"))
        .and(body_json(
            json!({ "refreshToken": "aorAAAAAG-original-token" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accessToken": access_token,
            "profileArn": "arn:aws:iam::123:role/KiroProfile",
            "expiresIn": 7200
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(request(Body::from(
            json!({ "refreshToken": "  aorAAAAAG-original-token  " }).to_string(),
        )))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "kiro");
    assert_eq!(json["connection"]["email"], "me@example.com");

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "kiro");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.name.as_deref(), Some("me@example.com"));
    assert_eq!(connection.email.as_deref(), Some("me@example.com"));
    assert_eq!(
        connection.access_token.as_deref(),
        Some(access_token.as_str())
    );
    assert_eq!(
        connection.refresh_token.as_deref(),
        Some("aorAAAAAG-original-token")
    );
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert_eq!(
        connection.provider_specific_data.get("profileArn"),
        Some(&json!("arn:aws:iam::123:role/KiroProfile"))
    );
    assert_eq!(
        connection.provider_specific_data.get("authMethod"),
        Some(&json!("imported"))
    );
    assert_eq!(
        connection.provider_specific_data.get("provider"),
        Some(&json!("Imported"))
    );
}

#[tokio::test]
async fn kiro_import_route_validates_missing_and_invalid_tokens_like_9router() {
    let app = openproxy::build_app(app_state().await);

    let missing = app
        .clone()
        .oneshot(request(Body::from(r#"{}"#)))
        .await
        .unwrap();
    let (status, json) = response_json(missing).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Refresh token is required" }));

    let invalid = app
        .oneshot(request(Body::from(
            json!({ "refreshToken": "bad-token" }).to_string(),
        )))
        .await
        .unwrap();
    let (status, json) = response_json(invalid).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        json,
        json!({ "error": "Invalid token format. Token should start with aorAAAAAG..." })
    );
}

#[tokio::test]
async fn kiro_import_route_wraps_refresh_failure_like_9router() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _env = EnvVarGuard::set("OPENPROXY_KIRO_AUTH_SERVICE_BASE_URL", &server.uri());

    Mock::given(method("POST"))
        .and(path("/refreshToken"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad token"))
        .mount(&server)
        .await;

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(request(Body::from(
            json!({ "refreshToken": "aorAAAAAG-bad-token" }).to_string(),
        )))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        json,
        json!({ "error": "Token validation failed: Token refresh failed: bad token" })
    );
}
