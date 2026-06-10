//! OpenAI OAuth 2.0 flow (PKCE Authorization Code).
//!
//! - PKCE Auth Code (same as codex but originator=openai_native)
//! - Uses codex-compatible endpoints with originator=openai_native
//! - Form-urlencoded token exchange
//! - connect_openai(), refresh_openai()

use crate::oauth::pkce::{generate_code_challenge, generate_code_verifier};
use crate::oauth::TokenResponse;
use base64::Engine;
use rand::RngCore;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_SCOPE: &str = "openid profile email offline_access";
const OPENAI_LOOPBACK_PORT: u16 = 1455;
const OPENAI_CALLBACK_PATH: &str = "/auth/callback";

pub const REFRESH_LEAD_MS: u64 = 5 * 60 * 1000;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Auth URL builder
// ---------------------------------------------------------------------------

fn build_openai_auth_url(redirect_uri: &str, state: &str, code_challenge: &str) -> String {
    let pairs: Vec<(&str, String)> = vec![
        ("response_type", "code".to_string()),
        ("client_id", OPENAI_CLIENT_ID.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("scope", OPENAI_SCOPE.to_string()),
        ("code_challenge", code_challenge.to_string()),
        ("code_challenge_method", "S256".to_string()),
        ("id_token_add_organizations", "true".to_string()),
        ("codex_cli_simplified_flow", "true".to_string()),
        ("originator", "openai_native".to_string()),
        ("state", state.to_string()),
    ];

    let query = pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                k,
                url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("&");

    format!("{OPENAI_AUTHORIZE_URL}?{query}")
}

// ---------------------------------------------------------------------------
// Token exchange (form-urlencoded)
// ---------------------------------------------------------------------------

async fn exchange_code_for_token(
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", OPENAI_CLIENT_ID),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];

    let response = client
        .post(OPENAI_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Token exchange request failed: {e}"))?;

    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed: {body}"));
    }

    response
        .json()
        .await
        .map_err(|e| format!("Token exchange parse failed: {e}"))
}

// ---------------------------------------------------------------------------
// Extra data extraction from id_token
// ---------------------------------------------------------------------------

fn decode_jwt_claims(access_token: &str) -> Option<Value> {
    let mut parts = access_token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let mut padded = payload.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }

    let decoded = base64::engine::general_purpose::URL_SAFE
        .decode(padded)
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn extract_account_info(id_token: Option<&str>) -> (Option<String>, BTreeMap<String, Value>) {
    let mut extra = BTreeMap::new();
    let Some(id_token) = id_token else {
        return (None, extra);
    };

    let claims = decode_jwt_claims(id_token);
    let email = claims
        .as_ref()
        .and_then(|v| v.get("email"))
        .and_then(Value::as_str)
        .map(str::to_string);

    if let Some(auth) = claims
        .as_ref()
        .and_then(|v| v.get("https://api.openai.com/auth"))
        .and_then(Value::as_object)
    {
        if let Some(account_id) = auth
            .get("chatgpt_account_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            extra.insert(
                "chatgptAccountId".to_string(),
                Value::String(account_id.to_string()),
            );
        }
        if let Some(plan_type) = auth
            .get("chatgpt_plan_type")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            extra.insert(
                "chatgptPlanType".to_string(),
                Value::String(plan_type.to_string()),
            );
        }
    }

    (email, extra)
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

pub async fn refresh_openai(refresh_token: &str) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", OPENAI_CLIENT_ID),
    ];

    let response = client
        .post(OPENAI_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Refresh request failed: {e}"))?;

    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Refresh failed: {body}"));
    }

    response
        .json()
        .await
        .map_err(|e| format!("Refresh parse failed: {e}"))
}

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

pub async fn connect_openai() -> Result<(TokenResponse, BTreeMap<String, Value>), String> {
    // 1. Generate PKCE pair
    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);
    let state = generate_state();

    let redirect_uri = format!(
        "http://127.0.0.1:{}{}",
        OPENAI_LOOPBACK_PORT, OPENAI_CALLBACK_PATH
    );

    // 2. Build auth URL
    let auth_url = build_openai_auth_url(&redirect_uri, &state, &code_challenge);

    // 3. Start local callback server
    let listener = TcpListener::bind(("127.0.0.1", OPENAI_LOOPBACK_PORT))
        .await
        .map_err(|e| format!("Failed to bind loopback: {e}"))?;

    eprintln!(
        "Open this URL in your browser to authorize OpenAI:\n  {}\n\n\
         Waiting for callback on http://127.0.0.1:{}...",
        auth_url, OPENAI_LOOPBACK_PORT
    );

    // 4. Accept callback
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| format!("Accept failed: {e}"))?;

    let mut buf = vec![0u8; 8192];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| format!("Read failed: {e}"))?;
    let request = String::from_utf8_lossy(&buf[..n]);

    let request_line = request.lines().next().unwrap_or("");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");

    let parsed_url = url::Url::parse(&format!("http://127.0.0.1:{port}", port = OPENAI_LOOPBACK_PORT))
        .and_then(|base| base.join(path))
        .map_err(|e| format!("Failed to parse callback URL: {e}"))?;

    let returned_state = parsed_url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();

    let code = parsed_url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| {
            let error = parsed_url
                .query_pairs()
                .find(|(k, _)| k == "error")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_else(|| "missing_code".to_string());
            let error_desc = parsed_url
                .query_pairs()
                .find(|(k, _)| k == "error_description")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            format!("Authorization error: {error} {error_desc}")
        })?;

    if returned_state != state {
        return Err("State mismatch: possible CSRF attack".to_string());
    }

    // Send success response
    let response_body =
        "<html><body><h1>Authentication successful!</h1><p>You can close this window.</p></body></html>";
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
    );
    let _ = stream.write_all(http_response.as_bytes()).await;
    let _ = stream.shutdown().await;

    // 5. Exchange code for tokens
    let token_response = exchange_code_for_token(&code, &redirect_uri, &code_verifier).await?;

    // 6. Extract account info from id_token
    let (email, mut extra) = extract_account_info(token_response.id_token.as_deref());
    if let Some(email) = email {
        extra.insert("email".to_string(), Value::String(email));
    }
    extra.insert("originator".to_string(), Value::String("openai_native".to_string()));

    Ok((token_response, extra))
}

pub fn openai_needs_refresh(expires_at: &Option<String>) -> bool {
    crate::oauth::token_refresh::needs_refresh_with_lead(expires_at, REFRESH_LEAD_MS)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_openai_auth_url_includes_params() {
        let url = build_openai_auth_url(
            "http://127.0.0.1:1455/auth/callback",
            "test_state",
            "test_challenge",
        );
        assert!(url.contains("client_id="));
        assert!(url.contains("code_challenge=test_challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("originator=openai_native"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("state=test_state"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback"));
    }

    #[test]
    fn test_extract_account_info_with_id_token() {
        // Build a minimal id_token JWT-like structure
        let claims = serde_json::json!({
            "email": "user@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_123",
                "chatgpt_plan_type": "plus"
            }
        });
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::json!({"alg":"HS256"}).to_string(),
        );
        let payload_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        let id_token = format!("{header}.{payload_b64}.fakesig");

        let (email, extra) = extract_account_info(Some(&id_token));
        assert_eq!(email, Some("user@example.com".to_string()));
        assert_eq!(
            extra.get("chatgptAccountId").and_then(Value::as_str),
            Some("acct_123")
        );
        assert_eq!(
            extra.get("chatgptPlanType").and_then(Value::as_str),
            Some("plus")
        );
    }

    #[test]
    fn test_extract_account_info_no_id_token() {
        let (email, extra) = extract_account_info(None);
        assert!(email.is_none());
        assert!(extra.is_empty());
    }

    #[test]
    fn test_generate_state_is_unique() {
        let s1 = generate_state();
        let s2 = generate_state();
        assert_ne!(s1, s2);
    }
}
