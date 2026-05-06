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

fn get_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn post_request(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
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

fn is_base64url_no_pad(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

#[tokio::test]
async fn kiro_device_code_defaults_match_9router_builder_id_flow() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _env = EnvVarGuard::set("OPENPROXY_KIRO_OIDC_BASE_URL", &server.uri());

    Mock::given(method("POST"))
        .and(path("/client/register"))
        .and(body_json(json!({
            "clientName": "kiro-oauth-client",
            "clientType": "public",
            "scopes": [
                "codewhisperer:completions",
                "codewhisperer:analysis",
                "codewhisperer:conversations"
            ],
            "grantTypes": [
                "urn:ietf:params:oauth:grant-type:device_code",
                "refresh_token"
            ],
            "issuerUrl": "https://identitycenter.amazonaws.com/ssoins-722374e8c3c8e6c6"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "clientId": "client-123",
            "clientSecret": "secret-123"
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/device_authorization"))
        .and(body_json(json!({
            "clientId": "client-123",
            "clientSecret": "secret-123",
            "startUrl": "https://view.awsapps.com/start"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "deviceCode": "device-123",
            "userCode": "ABCD-EFGH",
            "verificationUri": "https://device.example.com",
            "verificationUriComplete": "https://device.example.com/?user_code=ABCD-EFGH",
            "expiresIn": 600,
            "interval": 5
        })))
        .mount(&server)
        .await;

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request("/api/oauth/kiro/device-code"))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["device_code"], "device-123");
    assert_eq!(json["user_code"], "ABCD-EFGH");
    assert_eq!(json["verification_uri"], "https://device.example.com");
    assert_eq!(
        json["verification_uri_complete"],
        "https://device.example.com/?user_code=ABCD-EFGH"
    );
    assert_eq!(json["expires_in"], 600);
    assert_eq!(json["interval"], 5);
    assert_eq!(json["_clientId"], "client-123");
    assert_eq!(json["_clientSecret"], "secret-123");
    assert_eq!(json["_region"], "us-east-1");
    assert_eq!(json["_authMethod"], "builder-id");
    assert_eq!(json["_startUrl"], "https://view.awsapps.com/start");

    let code_verifier = json["codeVerifier"].as_str().expect("code verifier");
    assert_eq!(code_verifier.len(), 43);
    assert!(is_base64url_no_pad(code_verifier));
}

#[tokio::test]
async fn kiro_device_code_supports_idc_query_params_like_9router() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _env = EnvVarGuard::set("OPENPROXY_KIRO_OIDC_BASE_URL", &server.uri());

    Mock::given(method("POST"))
        .and(path("/client/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "clientId": "client-idc",
            "clientSecret": "secret-idc"
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/device_authorization"))
        .and(body_json(json!({
            "clientId": "client-idc",
            "clientSecret": "secret-idc",
            "startUrl": "https://company.awsapps.com/start"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "deviceCode": "device-idc",
            "userCode": "WXYZ-1234",
            "verificationUri": "https://idc.example.com",
            "expiresIn": 900,
            "interval": 7
        })))
        .mount(&server)
        .await;

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/kiro/device-code?start_url=https%3A%2F%2Fcompany.awsapps.com%2Fstart&region=eu-west-1&auth_method=idc",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["device_code"], "device-idc");
    assert_eq!(json["_clientId"], "client-idc");
    assert_eq!(json["_clientSecret"], "secret-idc");
    assert_eq!(json["_region"], "eu-west-1");
    assert_eq!(json["_authMethod"], "idc");
    assert_eq!(json["_startUrl"], "https://company.awsapps.com/start");
    assert_eq!(json["interval"], 7);
    assert_eq!(json["expires_in"], 900);
}

#[tokio::test]
async fn kiro_poll_returns_missing_device_code_without_api_key() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(post_request("/api/oauth/kiro/poll", json!({})))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Missing device code" }));
}

#[tokio::test]
async fn kiro_poll_returns_pending_shape_like_9router() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _env = EnvVarGuard::set("OPENPROXY_KIRO_OIDC_BASE_URL", &server.uri());

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_json(json!({
            "clientId": "client-123",
            "clientSecret": "secret-123",
            "deviceCode": "device-123",
            "grantType": "urn:ietf:params:oauth:grant-type:device_code"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": "authorization_pending",
            "error_description": "Waiting for approval"
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/kiro/poll",
            json!({
                "deviceCode": "device-123",
                "codeVerifier": "ignored",
                "extraData": {
                    "_clientId": "client-123",
                    "_clientSecret": "secret-123",
                    "_region": "us-east-1",
                    "_authMethod": "builder-id",
                    "_startUrl": "https://view.awsapps.com/start"
                }
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": false,
            "error": "authorization_pending",
            "errorDescription": "Waiting for approval",
            "pending": true
        })
    );
    assert!(state.db.snapshot().provider_connections.is_empty());
}

#[tokio::test]
async fn kiro_poll_success_saves_connection_like_9router() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _env = EnvVarGuard::set("OPENPROXY_KIRO_OIDC_BASE_URL", &server.uri());
    let access_token = make_jwt("me@example.com");

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_json(json!({
            "clientId": "client-123",
            "clientSecret": "secret-123",
            "deviceCode": "device-123",
            "grantType": "urn:ietf:params:oauth:grant-type:device_code"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accessToken": access_token,
            "refreshToken": "refresh-123",
            "expiresIn": 7200,
            "profileArn": "arn:aws:iam::123:role/KiroDevice"
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/kiro/poll",
            json!({
                "deviceCode": "device-123",
                "codeVerifier": "ignored",
                "extraData": {
                    "_clientId": "client-123",
                    "_clientSecret": "secret-123",
                    "_region": "us-east-1",
                    "_authMethod": "builder-id",
                    "_startUrl": "https://view.awsapps.com/start"
                }
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "kiro");

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
    assert_eq!(connection.refresh_token.as_deref(), Some("refresh-123"));
    assert!(connection.expires_at.is_some());
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert_eq!(
        connection.provider_specific_data.get("profileArn"),
        Some(&json!("arn:aws:iam::123:role/KiroDevice"))
    );
    assert_eq!(
        connection.provider_specific_data.get("clientId"),
        Some(&json!("client-123"))
    );
    assert_eq!(
        connection.provider_specific_data.get("clientSecret"),
        Some(&json!("secret-123"))
    );
    assert_eq!(
        connection.provider_specific_data.get("region"),
        Some(&json!("us-east-1"))
    );
    assert_eq!(
        connection.provider_specific_data.get("authMethod"),
        Some(&json!("builder-id"))
    );
    assert_eq!(
        connection.provider_specific_data.get("startUrl"),
        Some(&json!("https://view.awsapps.com/start"))
    );
}
