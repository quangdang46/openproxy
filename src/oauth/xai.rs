//! xAI OAuth 2.0 flow (PKCE Authorization Code).
//!
//! - OpenID Connect Discovery with host validation (only x.ai / *.x.ai)
//! - 96-byte PKCE code verifier (NOT 32)
//! - Fixed loopback port 56121, callback path /callback
//! - Auth params: scope, nonce, plan=generic, referrer=cli-proxy-api
//! - Token exchange: form-urlencoded (NOT JSON)
//! - Refresh lead: 5 min
//! - connect_xai(), refresh_xai()

use crate::oauth::pkce::{generate_code_challenge, generate_code_verifier_with_len};
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

const XAI_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const XAI_CLIENT_ID: &str = "b1a00492-073a-073a-47ea-816f-4c329264a828";
const XAI_LOOPBACK_PORT: u16 = 56121;
const XAI_CALLBACK_PATH: &str = "/callback";
const PKCE_VERIFIER_BYTES: usize = 96;
pub const REFRESH_LEAD_MS: u64 = 5 * 60 * 1000;

// ---------------------------------------------------------------------------
// Discovery result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize)]
struct XaiOpenIdConfig {
    #[serde(default)]
    authorization_endpoint: String,
    #[serde(default)]
    token_endpoint: String,
}

/// Validate that the host in a URL is x.ai or *.x.ai.
fn validate_xai_host(url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Invalid URL: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "No host in URL".to_string())?;
    if host == "x.ai" || host.ends_with(".x.ai") {
        Ok(url.to_string())
    } else {
        Err(format!("Host {host} is not a valid x.ai domain"))
    }
}

/// Fetch and validate OpenID Connect discovery document from auth.x.ai.
pub async fn discover_xai_config() -> Result<(String, String), String> {
    let resp = reqwest::get(XAI_DISCOVERY_URL)
        .await
        .map_err(|e| format!("Discovery fetch failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Discovery returned {}", resp.status()));
    }
    let config: XaiOpenIdConfig =
        resp.json().await.map_err(|e| format!("Discovery parse failed: {e}"))?;

    let auth_url = validate_xai_host(&config.authorization_endpoint)?;
    let token_url = validate_xai_host(&config.token_endpoint)?;

    Ok((auth_url, token_url))
}

// ---------------------------------------------------------------------------
// Auth URL building
// ---------------------------------------------------------------------------

fn generate_nonce() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn build_xai_auth_url(
    auth_url: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
    nonce: &str,
) -> String {
    let mut pairs: Vec<(&str, String)> = vec![
        ("response_type", "code".to_string()),
        ("client_id", XAI_CLIENT_ID.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        (
            "scope",
            "openid profile email offline_access grok-cli:access api:access".to_string(),
        ),
        ("state", state.to_string()),
        ("code_challenge", code_challenge.to_string()),
        ("code_challenge_method", "S256".to_string()),
        ("nonce", nonce.to_string()),
        ("plan", "generic".to_string()),
        ("referrer", "cli-proxy-api".to_string()),
    ];

    // Force re-auth every time (no session reuse)
    pairs.push(("prompt", "login".to_string()));

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

    format!("{auth_url}?{query}")
}

// ---------------------------------------------------------------------------
// Token exchange (form-urlencoded)
// ---------------------------------------------------------------------------

async fn exchange_code_for_token(
    token_url: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", XAI_CLIENT_ID),
        ("code_verifier", code_verifier),
    ];

    let response = client
        .post(token_url)
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

async fn refresh_token_inner(
    token_url: &str,
    refresh_token: &str,
) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", XAI_CLIENT_ID),
    ];

    let response = client
        .post(token_url)
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
// Connect: run PKCE flow with local callback server
// ---------------------------------------------------------------------------

pub async fn connect_xai() -> Result<(TokenResponse, BTreeMap<String, Value>), String> {
    // 1. Discover endpoints
    let (auth_url, token_url) = discover_xai_config().await?;

    // 2. Generate PKCE pair (96-byte verifier)
    let code_verifier = generate_code_verifier_with_len(PKCE_VERIFIER_BYTES);
    let code_challenge = generate_code_challenge(&code_verifier);
    let state = generate_code_verifier_with_len(32);
    let nonce = generate_nonce();

    let redirect_uri = format!("http://127.0.0.1:{}{}", XAI_LOOPBACK_PORT, XAI_CALLBACK_PATH);

    // 3. Build auth URL and print instructions
    let auth_url = build_xai_auth_url(&auth_url, &redirect_uri, &state, &code_challenge, &nonce);

    // 4. Start local callback server
    let listener = TcpListener::bind(("127.0.0.1", XAI_LOOPBACK_PORT))
        .await
        .map_err(|e| format!("Failed to bind loopback: {e}"))?;

    eprintln!(
        "Open this URL in your browser to authorize xAI:\n  {}\n\n\
         Waiting for callback on http://127.0.0.1:{}...",
        auth_url, XAI_LOOPBACK_PORT
    );

    // 5. Accept one connection, read the HTTP request, extract code & state
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

    // Parse the HTTP request line
    let request_line = request.lines().next().unwrap_or("");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");

    let parsed_url = url::Url::parse(&format!("http://127.0.0.1:{port}", port = XAI_LOOPBACK_PORT))
        .and_then(|base| base.join(path))
        .map_err(|e| format!("Failed to parse callback URL: {e}"))?;

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
    let returned_state = parsed_url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();

    // Return a success HTML page
    let response_body = "<html><body><h1>Authentication successful!</h1><p>You can close this window.</p></body></html>";
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
    );
    let _ = stream.write_all(http_response.as_bytes()).await;
    let _ = stream.shutdown().await;

    // Verify state
    if returned_state != state {
        return Err("State mismatch: possible CSRF attack".to_string());
    }

    // 6. Exchange code for tokens
    let token_response = exchange_code_for_token(&token_url, &code, &code_verifier, &redirect_uri)
        .await?;

    // Build extra data (nonce for id_token verification if needed)
    let mut extra = BTreeMap::new();
    extra.insert("nonce".to_string(), Value::String(nonce));

    Ok((token_response, extra))
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

pub async fn refresh_xai(refresh_token: &str) -> Result<TokenResponse, String> {
    let (_auth_url, token_url) = discover_xai_config().await?;
    refresh_token_inner(&token_url, refresh_token).await
}

pub fn xai_needs_refresh(expires_at: &Option<String>) -> bool {
    crate::oauth::token_refresh::needs_refresh_with_lead(expires_at, REFRESH_LEAD_MS)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_xai_host_accepts_x_ai() {
        assert!(validate_xai_host("https://x.ai/.well-known/openid-configuration").is_ok());
    }

    #[test]
    fn test_validate_xai_host_accepts_subdomain() {
        assert!(validate_xai_host("https://auth.x.ai/oauth2/authorize").is_ok());
    }

    #[test]
    fn test_validate_xai_host_rejects_other() {
        assert!(validate_xai_host("https://evil.com/authorize").is_err());
    }

    #[test]
    fn test_validate_xai_host_rejects_similar() {
        assert!(validate_xai_host("https://xai.com/authorize").is_err());
    }

    #[test]
    fn test_validate_xai_host_rejects_x_ai_substring() {
        assert!(validate_xai_host("https://notx.ai/authorize").is_err());
    }

    #[test]
    fn test_build_xai_auth_url_includes_params() {
        let url = build_xai_auth_url(
            "https://auth.x.ai/oauth2/authorize",
            "http://127.0.0.1:56121/callback",
            "test_state",
            "test_challenge",
            "test_nonce",
        );
        assert!(url.contains(&format!("client_id={}", XAI_CLIENT_ID)));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A56121%2Fcallback"));
        assert!(url.contains("code_challenge=test_challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("nonce=test_nonce"));
        assert!(url.contains("plan=generic"));
        assert!(url.contains("referrer=cli-proxy-api"));
        assert!(url.contains("scope=openid"));
        assert!(url.contains("grok-cli%3Aaccess"));
        assert!(url.contains("api%3Aaccess"));
        assert!(url.contains("offline_access"));
    }
}
