use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_json, body_string_contains, method, path};
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

fn is_base64url_no_pad(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn make_codex_id_token(email: &str, account_id: &str, plan_type: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_plan_type": plan_type
            }
        })
        .to_string(),
    );
    format!("{header}.{payload}.sig")
}

#[tokio::test]
async fn claude_authorize_matches_9router_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/claude/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code_pkce");
    assert_eq!(json["redirectUri"], "http://localhost:4624/callback");
    assert_eq!(json["callbackPath"], "/callback");
    assert!(json.get("fixedPort").is_none());

    let state = json["state"].as_str().expect("state");
    let code_verifier = json["codeVerifier"].as_str().expect("code verifier");
    let code_challenge = json["codeChallenge"].as_str().expect("code challenge");
    assert_eq!(state.len(), 43);
    assert_eq!(code_verifier.len(), 43);
    assert_eq!(code_challenge.len(), 43);
    assert!(is_base64url_no_pad(state));
    assert!(is_base64url_no_pad(code_verifier));
    assert!(is_base64url_no_pad(code_challenge));

    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    assert_eq!(code_challenge, expected_challenge);
    assert_eq!(
        json["authUrl"],
        format!(
            "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback&scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference&code_challenge={code_challenge}&code_challenge_method=S256&state={state}"
        )
    );
}

#[tokio::test]
async fn codex_authorize_matches_9router_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/codex/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code_pkce");
    assert_eq!(json["redirectUri"], "http://localhost:1455/auth/callback");
    assert_eq!(json["callbackPath"], "/auth/callback");
    assert_eq!(json["fixedPort"], 1455);

    let state = json["state"].as_str().expect("state");
    let code_verifier = json["codeVerifier"].as_str().expect("code verifier");
    let code_challenge = json["codeChallenge"].as_str().expect("code challenge");
    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    assert_eq!(code_challenge, expected_challenge);
    assert_eq!(
        json["authUrl"],
        format!(
            "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_EMoamEEZ73f0CkXaXp7hrann&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&scope=openid%20profile%20email%20offline_access&code_challenge={code_challenge}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=codex_cli_rs&state={state}"
        )
    );
}

#[tokio::test]
async fn exchange_compat_rejects_missing_required_fields() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(post_request("/api/oauth/claude/exchange", json!({})))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Missing required fields" }));
}

#[tokio::test]
async fn claude_exchange_matches_9router_and_saves_connection() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CLAUDE_TOKEN_URL",
        &format!("{}/v1/oauth/token", server.uri()),
    );

    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_json(json!({
            "code": "auth-code",
            "state": "fragment-state",
            "grant_type": "authorization_code",
            "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            "redirect_uri": "http://localhost:4624/callback",
            "code_verifier": "pkce-verifier"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "claude-access",
            "refresh_token": "claude-refresh",
            "expires_in": 3600,
            "scope": "org:create_api_key user:profile user:inference"
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/claude/exchange",
            json!({
                "code": "auth-code#fragment-state",
                "redirectUri": "http://localhost:4624/callback",
                "codeVerifier": "pkce-verifier",
                "state": "body-state"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "claude");
    assert!(json["connection"].get("email").is_none());

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "claude");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.name.as_deref(), Some("Account 1"));
    assert_eq!(connection.access_token.as_deref(), Some("claude-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("claude-refresh"));
    assert_eq!(
        connection.scope.as_deref(),
        Some("org:create_api_key user:profile user:inference")
    );
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert!(connection.expires_at.is_some());
}

#[tokio::test]
async fn codex_exchange_matches_9router_and_maps_id_token() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CODEX_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );
    let id_token = make_codex_id_token("me@example.com", "acct_123", "plus");

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains(
            "client_id=app_EMoamEEZ73f0CkXaXp7hrann",
        ))
        .and(body_string_contains("code=auth-code"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .and(body_string_contains("code_verifier=pkce-verifier"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "codex-access",
            "refresh_token": "codex-refresh",
            "expires_in": 7200,
            "id_token": id_token
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/codex/exchange",
            json!({
                "code": "auth-code",
                "redirectUri": "http://localhost:1455/auth/callback",
                "codeVerifier": "pkce-verifier"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "codex");
    assert_eq!(json["connection"]["email"], "me@example.com");
    assert!(json["connection"].get("displayName").is_none());

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "codex");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.name.as_deref(), Some("me@example.com"));
    assert_eq!(connection.email.as_deref(), Some("me@example.com"));
    assert_eq!(connection.access_token.as_deref(), Some("codex-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("codex-refresh"));
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert_eq!(
        connection.provider_specific_data.get("chatgptAccountId"),
        Some(&json!("acct_123"))
    );
    assert_eq!(
        connection.provider_specific_data.get("chatgptPlanType"),
        Some(&json!("plus"))
    );
}
