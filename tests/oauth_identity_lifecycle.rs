//! OAuth account-identity + token-lifecycle parity (P74-001 / P126-001 / P127-001).
//!
//! 9router `createProviderConnection` (`connectionsRepo.js:144-164`) merges a
//! new OAuth grant onto an existing row only when it is genuinely the same
//! identity. Refresh tokens are rotated single-use, so collapsing a second
//! grant onto a bare-email row destroys the first account's token pair and makes
//! it look invalid the moment a second account is added. These tests drive the
//! real `POST /api/oauth/:provider/exchange` route with a mocked token endpoint
//! so the stored rows are exactly what a browser login would produce.

#![allow(clippy::await_holding_lock)]
use std::sync::Mutex;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_MGMT_KEY: &str = "oauth-identity-test-key";

/// `std::env::set_var` is process-wide; every test here repoints the codex token
/// endpoint, so they must not interleave. A failing test must not poison the
/// lock for the rest — each failure should surface its own assertion.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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
    let db = Db::load_from(temp.path()).await.expect("db");
    db.update(|state| {
        // OAuth routes sit in the admin tier, which requires a management key.
        state.api_keys.push(openproxy::types::ApiKey {
            id: "mgmt-1".into(),
            name: "Management".into(),
            key: TEST_MGMT_KEY.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            monthly_budget_usd: None,
            extra: Default::default(),
        });
    })
    .await
    .expect("seed db");
    AppState::new(std::sync::Arc::new(db))
}

fn post_request(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(
            "authorization",
            concat!("Bearer ", "oauth-identity-test-key"),
        )
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// A Codex id_token carrying the nested `https://api.openai.com/auth` claims
/// that `extract_codex_account_info` reads (9router `extractCodexAccountInfo`).
fn make_codex_id_token(email: &str, account_id: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_plan_type": "plus"
            }
        })
        .to_string(),
    );
    format!("{header}.{payload}.sig")
}

/// A ChatGPT-website style access token: top-level `account_id` / `plan_type`
/// instead of the nested auth claims (9router `route.js:419-425`).
fn make_chatgpt_access_token(email: &str, account_id: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        json!({ "email": email, "account_id": account_id, "plan_type": "pro" }).to_string(),
    );
    format!("{header}.{payload}.sig")
}

/// Mount a codex token endpoint that answers with the given grant.
async fn mock_codex_token(server: &MockServer, access_token: &str, refresh_token: &str) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "expires_in": 7200,
            "id_token": make_codex_id_token("shared@example.com", "acct_123")
        })))
        .mount(server)
        .await;
}

async fn codex_exchange(state: &AppState, code: &str) -> (StatusCode, serde_json::Value) {
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/codex/exchange",
            json!({
                "code": code,
                "redirectUri": "http://localhost:1455/auth/callback",
                "codeVerifier": "pkce-verifier"
            }),
        ))
        .await
        .unwrap();
    response_json(response).await
}

/// P126-001 / P74-001: two Codex workspaces under one email must stay two rows.
/// Under the old bare-email dedupe the second login overwrote the first
/// account's rotated refresh token, silently destroying it.
#[tokio::test]
async fn second_codex_workspace_under_same_email_gets_its_own_row() {
    let _lock = env_lock();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CODEX_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );

    // Workspace 1.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("code=first-grant"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "workspace-1-access",
            "refresh_token": "workspace-1-refresh",
            "expires_in": 7200,
            "id_token": make_codex_id_token("shared@example.com", "acct_workspace_1")
        })))
        .mount(&server)
        .await;
    // Workspace 2 — same email, different ChatGPT account id.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("code=second-grant"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "workspace-2-access",
            "refresh_token": "workspace-2-refresh",
            "expires_in": 7200,
            "id_token": make_codex_id_token("shared@example.com", "acct_workspace_2")
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    for code in ["first-grant", "second-grant"] {
        let (status, json) = codex_exchange(&state, code).await;
        assert_eq!(status, StatusCode::OK, "exchange {code} failed: {json}");
        assert_eq!(json["success"], true);
    }

    let snapshot = state.db.snapshot();
    let codex: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|c| c.provider == "codex")
        .collect();
    assert_eq!(
        codex.len(),
        2,
        "a second Codex workspace under the same email must not collapse onto the first"
    );

    // Neither account's token pair was clobbered.
    let refreshes: Vec<&str> = codex
        .iter()
        .filter_map(|c| c.refresh_token.as_deref())
        .collect();
    assert!(refreshes.contains(&"workspace-1-refresh"));
    assert!(refreshes.contains(&"workspace-2-refresh"));

    let account_ids: Vec<&str> = codex
        .iter()
        .filter_map(|c| {
            c.provider_specific_data
                .get("chatgptAccountId")
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    assert_eq!(account_ids.len(), 2, "both workspace ids must be retained");
}

/// The other half of the same predicate: an identical ChatGPT account id IS the
/// same identity, so a re-login must re-auth the existing row rather than
/// stacking a duplicate.
#[tokio::test]
async fn re_login_of_same_workspace_reuses_the_existing_row() {
    let _lock = env_lock();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CODEX_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );
    mock_codex_token(&server, "rotated-access", "rotated-refresh").await;

    let state = app_state().await;
    for _ in 0..2 {
        let (status, json) = codex_exchange(&state, "auth-code").await;
        assert_eq!(status, StatusCode::OK, "{json}");
    }

    let snapshot = state.db.snapshot();
    let codex: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|c| c.provider == "codex")
        .collect();
    assert_eq!(codex.len(), 1, "same workspace id is the same identity");
    assert_eq!(
        codex[0].refresh_token.as_deref(),
        Some("rotated-refresh"),
        "the re-login must land the rotated refresh token on the existing row"
    );
}

/// P74-001: a re-auth must not wipe the user-set proxy bindings, which live in
/// the same `provider_specific_data` map as the ChatGPT account id.
#[tokio::test]
async fn re_auth_preserves_user_set_proxy_bindings() {
    let _lock = env_lock();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CODEX_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );
    mock_codex_token(&server, "rotated-access", "rotated-refresh").await;

    let state = app_state().await;
    let (status, _) = codex_exchange(&state, "auth-code").await;
    assert_eq!(status, StatusCode::OK);

    // The dashboard binds this connection to a proxy pool.
    state
        .db
        .update(|db| {
            let conn = db
                .provider_connections
                .iter_mut()
                .find(|c| c.provider == "codex")
                .expect("seeded codex connection");
            conn.provider_specific_data
                .insert("proxyPoolId".to_string(), json!("pool-eu-west"));
        })
        .await
        .expect("bind proxy pool");

    // Re-auth the same identity.
    let (status, _) = codex_exchange(&state, "auth-code").await;
    assert_eq!(status, StatusCode::OK);

    let snapshot = state.db.snapshot();
    let conn = snapshot
        .provider_connections
        .iter()
        .find(|c| c.provider == "codex")
        .expect("codex connection");
    assert_eq!(
        conn.provider_specific_data.get("proxyPoolId"),
        Some(&json!("pool-eu-west")),
        "re-auth must merge provider_specific_data, not replace the map"
    );
    assert_eq!(
        conn.provider_specific_data.get("chatgptAccountId"),
        Some(&json!("acct_123")),
        "the incoming grant still refreshes the identity keys"
    );
}

/// P127-001: a pasted raw ChatGPT/Copilot JWT is a valid `code`. 9router
/// (`route.js:414-455`) short-circuits the token endpoint entirely and stores an
/// immediately-active `access_token` connection; OpenProxy used to forward the
/// JWT to the token endpoint and answer 500.
#[tokio::test]
async fn pasted_raw_jwt_creates_an_active_access_token_connection() {
    let _lock = env_lock();
    // No token endpoint override and no mock: a JWT paste must never reach one.
    let _token_url = EnvVarGuard::set("OPENPROXY_CODEX_TOKEN_URL", "http://127.0.0.1:1/nope");

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/codex/exchange",
            // 9router's gate: `code.startsWith("eyJ") && code.includes(".")`.
            json!({ "code": make_chatgpt_access_token("pasted@example.com", "acct_pasted") }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "raw-JWT paste rejected: {json}");
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "codex");
    assert_eq!(json["connection"]["email"], "pasted@example.com");

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let conn = &snapshot.provider_connections[0];
    assert_eq!(conn.auth_type, "access_token");
    assert_eq!(conn.test_status.as_deref(), Some("active"));
    assert_eq!(conn.email.as_deref(), Some("pasted@example.com"));
    assert_eq!(
        conn.provider_specific_data.get("authMethod"),
        Some(&json!("access_token"))
    );
    assert_eq!(
        conn.provider_specific_data.get("chatgptAccountId"),
        Some(&json!("acct_pasted"))
    );
    assert_eq!(
        conn.provider_specific_data.get("chatgptPlanType"),
        Some(&json!("pro"))
    );
    // A pasted access token has nothing to refresh with.
    assert!(conn.refresh_token.is_none());
}

/// `access_token` rows are never deduped (9router `connectionsRepo.js:169`) —
/// users manage pasted-token duplicates themselves.
#[tokio::test]
async fn pasted_jwts_stack_instead_of_overwriting() {
    let _lock = env_lock();
    let _token_url = EnvVarGuard::set("OPENPROXY_CODEX_TOKEN_URL", "http://127.0.0.1:1/nope");

    let state = app_state().await;
    for (email, account) in [("dup@example.com", "acct_a"), ("dup@example.com", "acct_b")] {
        let app = openproxy::build_app(state.clone());
        let response = app
            .oneshot(post_request(
                "/api/oauth/codex/exchange",
                json!({ "code": make_chatgpt_access_token(email, account) }),
            ))
            .await
            .unwrap();
        let (status, json) = response_json(response).await;
        assert_eq!(status, StatusCode::OK, "{json}");
    }

    let snapshot = state.db.snapshot();
    assert_eq!(
        snapshot.provider_connections.len(),
        2,
        "pasted access tokens sharing an email must not collapse"
    );
}
