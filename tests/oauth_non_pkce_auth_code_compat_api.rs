use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::{engine::general_purpose::STANDARD, Engine};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
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

fn expected_google_platform_enum() -> i64 {
    let is_arm64 = matches!(std::env::consts::ARCH, "aarch64" | "arm64");
    match std::env::consts::OS {
        "macos" => {
            if is_arm64 {
                2
            } else {
                1
            }
        }
        "linux" => {
            if is_arm64 {
                4
            } else {
                3
            }
        }
        "windows" => 5,
        _ => 0,
    }
}

fn cline_code(payload: serde_json::Value) -> String {
    STANDARD.encode(payload.to_string())
}

#[tokio::test]
async fn gemini_authorize_matches_9router_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/gemini-cli/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code");
    assert_eq!(json["redirectUri"], "http://localhost:4624/callback");
    assert_eq!(json["callbackPath"], "/callback");

    let state = json["state"].as_str().expect("state");
    let code_verifier = json["codeVerifier"].as_str().expect("code verifier");
    let code_challenge = json["codeChallenge"].as_str().expect("code challenge");
    let expected_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(code_verifier.as_bytes()));
    assert_eq!(code_challenge, expected_challenge);
    assert_eq!(
        json["authUrl"],
        format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.email+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.profile&state={state}&access_type=offline&prompt=consent"
        )
    );
}

#[tokio::test]
async fn antigravity_authorize_matches_9router_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/antigravity/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code");
    assert_eq!(json["redirectUri"], "http://localhost:4624/callback");
    assert_eq!(json["callbackPath"], "/callback");

    let state = json["state"].as_str().expect("state");
    assert_eq!(
        json["authUrl"],
        format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.email+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.profile+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcclog+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fexperimentsandconfigs&state={state}&access_type=offline&prompt=consent"
        )
    );
}

#[tokio::test]
async fn iflow_authorize_matches_9router_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/iflow/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code");
    assert_eq!(json["redirectUri"], "http://localhost:4624/callback");
    assert_eq!(json["callbackPath"], "/callback");

    let state = json["state"].as_str().expect("state");
    assert_eq!(
        json["authUrl"],
        format!(
            "https://iflow.cn/oauth?loginMethod=phone&type=phone&redirect=http%3A%2F%2Flocalhost%3A4624%2Fcallback&state={state}&client_id=10009311001"
        )
    );
}

#[tokio::test]
async fn cline_authorize_matches_9router_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/cline/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code");
    assert_eq!(json["redirectUri"], "http://localhost:4624/callback");
    assert_eq!(json["callbackPath"], "/callback");
    assert_eq!(
        json["authUrl"],
        "https://api.cline.bot/api/v1/auth/authorize?client_type=extension&callback_url=http%3A%2F%2Flocalhost%3A4624%2Fcallback&redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback"
    );
}

#[tokio::test]
async fn gemini_exchange_matches_9router_and_saves_connection() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_GEMINI_TOKEN_URL",
        &format!("{}/token", server.uri()),
    );
    let _user_info_url = EnvVarGuard::set(
        "OPENPROXY_GEMINI_USER_INFO_URL",
        &format!("{}/userinfo", server.uri()),
    );
    let _load_url = EnvVarGuard::set(
        "OPENPROXY_GEMINI_LOAD_CODE_ASSIST_ENDPOINT",
        &format!("{}/v1internal:loadCodeAssist", server.uri()),
    );

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains(
            "client_id=681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com",
        ))
        .and(body_string_contains("code=auth-code"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gemini-access",
            "refresh_token": "gemini-refresh",
            "expires_in": 7200,
            "scope": "scope-a scope-b"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(query_param("alt", "json"))
        .and(header("authorization", "Bearer gemini-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "gemini@example.com"
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1internal:loadCodeAssist"))
        .and(header("authorization", "Bearer gemini-access"))
        .and(body_json(json!({
            "metadata": {
                "ideType": 9,
                "platform": expected_google_platform_enum(),
                "pluginType": 2
            },
            "mode": 1
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "cloudaicompanionProject": { "id": "gcp-project-123" }
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/gemini-cli/exchange",
            json!({
                "code": "auth-code",
                "redirectUri": "http://localhost:4624/callback",
                "codeVerifier": "pkce-verifier"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "gemini-cli");
    assert_eq!(json["connection"]["email"], "gemini@example.com");

    let snapshot = state.db.snapshot();
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "gemini-cli");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.email.as_deref(), Some("gemini@example.com"));
    assert_eq!(connection.access_token.as_deref(), Some("gemini-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("gemini-refresh"));
    assert_eq!(connection.scope.as_deref(), Some("scope-a scope-b"));
    assert_eq!(connection.project_id.as_deref(), Some("gcp-project-123"));
    assert_eq!(connection.test_status.as_deref(), Some("active"));
}

#[tokio::test]
async fn antigravity_exchange_matches_9router_and_saves_connection() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_TOKEN_URL",
        &format!("{}/token", server.uri()),
    );
    let _user_info_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_USER_INFO_URL",
        &format!("{}/userinfo", server.uri()),
    );
    let _load_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_LOAD_CODE_ASSIST_ENDPOINT",
        &format!("{}/v1internal:loadCodeAssist", server.uri()),
    );
    let _onboard_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_ONBOARD_USER_ENDPOINT",
        &format!("{}/v1internal:onboardUser", server.uri()),
    );

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains(
            "client_id=1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com",
        ))
        .and(body_string_contains("code=auth-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "antigravity-access",
            "refresh_token": "antigravity-refresh",
            "expires_in": 5400,
            "scope": "scope-antigravity"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(query_param("alt", "json"))
        .and(header("authorization", "Bearer antigravity-access"))
        .and(header("x-request-source", "local"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "antigravity@example.com"
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1internal:loadCodeAssist"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "cloudaicompanionProject": { "id": "ag-project" },
            "allowedTiers": [
                { "id": "legacy-tier", "isDefault": false },
                { "id": "preferred-tier", "isDefault": true }
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1internal:onboardUser"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "done": true
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/antigravity/exchange",
            json!({
                "code": "auth-code",
                "redirectUri": "http://localhost:4624/callback",
                "codeVerifier": "pkce-verifier"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "antigravity");
    assert_eq!(json["connection"]["email"], "antigravity@example.com");

    let requests = server.received_requests().await.expect("requests");
    let load_request = requests
        .iter()
        .find(|request| request.url.path() == "/v1internal:loadCodeAssist")
        .expect("load request");
    assert_eq!(
        load_request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer antigravity-access")
    );
    assert_eq!(
        load_request
            .headers
            .get("x-request-source")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    let load_body: serde_json::Value =
        serde_json::from_slice(&load_request.body).expect("load request body");
    assert_eq!(
        load_body,
        json!({
            "metadata": {
                "ideType": "IDE_UNSPECIFIED",
                "platform": "PLATFORM_UNSPECIFIED",
                "pluginType": "GEMINI"
            }
        })
    );

    let snapshot = state.db.snapshot();
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "antigravity");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.email.as_deref(), Some("antigravity@example.com"));
    assert_eq!(
        connection.access_token.as_deref(),
        Some("antigravity-access")
    );
    assert_eq!(
        connection.refresh_token.as_deref(),
        Some("antigravity-refresh")
    );
    assert_eq!(connection.scope.as_deref(), Some("scope-antigravity"));
    assert_eq!(connection.project_id.as_deref(), Some("ag-project"));
    assert_eq!(connection.test_status.as_deref(), Some("active"));
}

#[tokio::test]
async fn iflow_exchange_matches_9router_and_saves_connection() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_IFLOW_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );
    let _user_info_url = EnvVarGuard::set(
        "OPENPROXY_IFLOW_USER_INFO_URL",
        &format!("{}/api/oauth/getUserInfo", server.uri()),
    );

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header(
            "authorization",
            "Basic MTAwMDkzMTEwMDE6NFozWWpYeWNWc1F2eUdGMWV0aU5sSUJCNFJzcVNEdFc=",
        ))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=iflow-code"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "iflow-access",
            "refresh_token": "iflow-refresh",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/oauth/getUserInfo"))
        .and(query_param("accessToken", "iflow-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "data": {
                "apiKey": "iflow-api-key",
                "email": "iflow@example.com",
                "nickname": "iFlow Name"
            }
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/iflow/exchange",
            json!({
                "code": "iflow-code",
                "redirectUri": "http://localhost:4624/callback",
                "codeVerifier": "pkce-verifier"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "iflow");
    assert_eq!(json["connection"]["email"], "iflow@example.com");
    assert_eq!(json["connection"]["displayName"], "iFlow Name");

    let snapshot = state.db.snapshot();
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "iflow");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.display_name.as_deref(), Some("iFlow Name"));
    assert_eq!(connection.email.as_deref(), Some("iflow@example.com"));
    assert_eq!(connection.access_token.as_deref(), Some("iflow-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("iflow-refresh"));
    assert_eq!(connection.api_key.as_deref(), Some("iflow-api-key"));
    assert_eq!(connection.test_status.as_deref(), Some("active"));
}

#[tokio::test]
async fn cline_exchange_accepts_base64_code_without_pkce() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/cline/exchange",
            json!({
                "code": cline_code(json!({
                    "accessToken": "cline-access",
                    "refreshToken": "cline-refresh",
                    "email": "cline@example.com",
                    "firstName": "Cli",
                    "lastName": "Ne",
                    "expiresAt": "2035-01-02T03:04:05Z"
                })),
                "redirectUri": "http://localhost:4624/callback"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "cline");
    assert_eq!(json["connection"]["email"], "cline@example.com");

    let snapshot = state.db.snapshot();
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "cline");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.email.as_deref(), Some("cline@example.com"));
    assert_eq!(connection.access_token.as_deref(), Some("cline-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("cline-refresh"));
    assert_eq!(
        connection.provider_specific_data.get("firstName"),
        Some(&json!("Cli"))
    );
    assert_eq!(
        connection.provider_specific_data.get("lastName"),
        Some(&json!("Ne"))
    );
    assert!(connection.expires_at.is_some());
    assert_eq!(connection.test_status.as_deref(), Some("active"));
}
