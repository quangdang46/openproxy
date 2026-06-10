//! Antigravity OAuth 2.0 flow (Authorization Code, no PKCE).
//!
//! - Google OAuth with extended scopes
//! - loadCodeAssist + onboardUser (poll 5s x 10 retries)
//! - Numeric Client-Metadata headers for EVERY call
//! - ProjectId caching per connection
//! - connect_antigravity(), refresh_antigravity()

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

const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const ANTIGRAVITY_CLIENT_SECRET: &str = "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";
const ANTIGRAVITY_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const ANTIGRAVITY_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ANTIGRAVITY_USER_INFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
const ANTIGRAVITY_LOAD_CODE_ASSIST_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const ANTIGRAVITY_ONBOARD_USER_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:onboardUser";
const ANTIGRAVITY_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform \
    https://www.googleapis.com/auth/userinfo.email \
    https://www.googleapis.com/auth/userinfo.profile \
    https://www.googleapis.com/auth/cclog \
    https://www.googleapis.com/auth/experimentsandconfigs";

const ANTIGRAVITY_LOAD_CODE_ASSIST_USER_AGENT: &str = "google-api-nodejs-client/9.15.1";
const ANTIGRAVITY_LOAD_CODE_ASSIST_API_CLIENT: &str =
    "google-cloud-sdk vscode_cloudshelleditor/0.1";

pub const REFRESH_LEAD_MS: u64 = 5 * 60 * 1000;

// ---------------------------------------------------------------------------
// Platform helper
// ---------------------------------------------------------------------------

fn google_oauth_platform_enum() -> i64 {
    let is_arm64 = matches!(std::env::consts::ARCH, "aarch64" | "arm64");
    match std::env::consts::OS {
        "macos" => {
            if is_arm64 { 2 } else { 1 }
        }
        "linux" => {
            if is_arm64 { 4 } else { 3 }
        }
        "windows" => 5,
        _ => 0,
    }
}

/// Numeric Client-Metadata for Antigravity (ideType:9, platform per OS, pluginType:2).
pub fn client_metadata() -> serde_json::Value {
    serde_json::json!({
        "ideType": 9,
        "platform": google_oauth_platform_enum(),
        "pluginType": 2,
    })
}

/// Stringified Client-Metadata header value for Antigravity (IDE_UNSPECIFIED format for legacy compat).
fn antigravity_metadata_header() -> String {
    client_metadata().to_string()
}

// ---------------------------------------------------------------------------
// State generation
// ---------------------------------------------------------------------------

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Auth URL builder
// ---------------------------------------------------------------------------

fn build_antigravity_auth_url(redirect_uri: &str, state: &str) -> String {
    let pairs: Vec<(&str, String)> = vec![
        ("client_id", ANTIGRAVITY_CLIENT_ID.to_string()),
        ("response_type", "code".to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("scope", ANTIGRAVITY_SCOPE.to_string()),
        ("state", state.to_string()),
        ("access_type", "offline".to_string()),
        ("prompt", "consent".to_string()),
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

    format!("{ANTIGRAVITY_AUTHORIZE_URL}?{query}")
}

// ---------------------------------------------------------------------------
// Token exchange
// ---------------------------------------------------------------------------

async fn exchange_code_for_token(code: &str, redirect_uri: &str) -> Result<Value, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", ANTIGRAVITY_CLIENT_ID),
        ("client_secret", ANTIGRAVITY_CLIENT_SECRET),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ];

    let response = client
        .post(ANTIGRAVITY_TOKEN_URL)
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
// User info
// ---------------------------------------------------------------------------

async fn fetch_user_info(access_token: &str) -> Value {
    let client = reqwest::Client::new();
    match client
        .get(format!("{}?alt=json", ANTIGRAVITY_USER_INFO_URL))
        .header("Authorization", format!("Bearer {access_token}"))
        .header("x-request-source", "local")
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            response.json().await.unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

// ---------------------------------------------------------------------------
// loadCodeAssist
// ---------------------------------------------------------------------------

fn extract_google_project_id(payload: &Value) -> Option<String> {
    let project = payload.get("cloudaicompanionProject")?;
    project
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| project.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

async fn call_load_code_assist(
    access_token: &str,
) -> Result<(Option<String>, Option<String>), String> {
    let metadata_json = antigravity_metadata_header();
    let client = reqwest::Client::new();
    let response = client
        .post(ANTIGRAVITY_LOAD_CODE_ASSIST_ENDPOINT)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", ANTIGRAVITY_LOAD_CODE_ASSIST_USER_AGENT)
        .header("X-Goog-Api-Client", ANTIGRAVITY_LOAD_CODE_ASSIST_API_CLIENT)
        .header("Client-Metadata", &metadata_json)
        .header("x-request-source", "local")
        .json(&serde_json::json!({ "metadata": client_metadata() }))
        .send()
        .await
        .map_err(|e| format!("loadCodeAssist request failed: {e}"))?;

    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("loadCodeAssist failed: {body}"));
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("loadCodeAssist parse failed: {e}"))?;

    let project_id = extract_google_project_id(&payload);

    let tier_id = payload
        .get("allowedTiers")
        .and_then(Value::as_array)
        .and_then(|tiers| {
            tiers.iter().find_map(|tier| {
                if tier.get("isDefault").and_then(Value::as_bool) == Some(true) {
                    tier.get("id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(str::to_string)
                } else {
                    None
                }
            })
        });

    Ok((project_id, tier_id))
}

// ---------------------------------------------------------------------------
// onboardUser with polling (5s x 10 retries)
// ---------------------------------------------------------------------------

async fn call_onboard_user(
    access_token: &str,
    tier_id: &str,
) -> Result<(), String> {
    let metadata_json = antigravity_metadata_header();
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "tierId": tier_id,
        "metadata": client_metadata(),
    });

    for attempt in 0..10 {
        let response = client
            .post(ANTIGRAVITY_ONBOARD_USER_ENDPOINT)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Content-Type", "application/json")
            .header("User-Agent", ANTIGRAVITY_LOAD_CODE_ASSIST_USER_AGENT)
            .header("X-Goog-Api-Client", ANTIGRAVITY_LOAD_CODE_ASSIST_API_CLIENT)
            .header("Client-Metadata", &metadata_json)
            .header("x-request-source", "local")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("onboardUser request failed: {e}"))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            if attempt < 9 {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            return Err(format!("onboardUser failed after retries: {error_text}"));
        }

        let result: Value = response.json().await.unwrap_or_default();
        if result.get("done").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    Err("onboardUser timed out after 10 retries".to_string())
}

// ---------------------------------------------------------------------------
// Token response parsing
// ---------------------------------------------------------------------------

fn parse_token_response(tokens: Value) -> Result<TokenResponse, String> {
    let access_token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing access_token in token response".to_string())?
        .to_string();

    let refresh_token = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_string);
    let expires_in = tokens.get("expires_in").and_then(Value::as_i64);
    let scope = tokens.get("scope").and_then(Value::as_str).map(str::to_string);

    Ok(TokenResponse {
        access_token,
        refresh_token,
        expires_in,
        id_token: None,
        token_type: Some("Bearer".to_string()),
        scope,
    })
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

pub async fn refresh_antigravity(refresh_token: &str) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", ANTIGRAVITY_CLIENT_ID),
        ("client_secret", ANTIGRAVITY_CLIENT_SECRET),
    ];

    let response = client
        .post(ANTIGRAVITY_TOKEN_URL)
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

    let tokens: Value = response
        .json()
        .await
        .map_err(|e| format!("Refresh parse failed: {e}"))?;
    parse_token_response(tokens)
}

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

/// Result of connecting Antigravity.
pub struct AntigravityConnectResult {
    pub token_response: TokenResponse,
    pub email: Option<String>,
    pub project_id: Option<String>,
    pub tier_id: Option<String>,
    pub extra: BTreeMap<String, Value>,
}

pub async fn connect_antigravity() -> Result<AntigravityConnectResult, String> {
    let state = generate_state();
    let redirect_uri = "http://127.0.0.1:8080/callback".to_string();

    // 1. Build auth URL
    let auth_url = build_antigravity_auth_url(&redirect_uri, &state);

    // 2. Start local callback server
    let listener = TcpListener::bind(("127.0.0.1", 8080))
        .await
        .map_err(|e| format!("Failed to bind loopback: {e}"))?;

    eprintln!(
        "Open this URL in your browser to authorize Antigravity:\n  {}\n\n\
         Waiting for callback on http://127.0.0.1:8080/callback...",
        auth_url
    );

    // 3. Accept callback
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

    let parsed_url = url::Url::parse("http://127.0.0.1:8080")
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

    // 4. Exchange code for tokens
    let tokens = exchange_code_for_token(&code, &redirect_uri).await?;
    let token_response = parse_token_response(tokens)?;

    // 5. Fetch user info
    let user_info = fetch_user_info(&token_response.access_token).await;
    let email = user_info
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string);

    // 6. Fetch projectId via loadCodeAssist
    let (project_id, tier_id) =
        call_load_code_assist(&token_response.access_token).await.unwrap_or((None, None));

    // 7. If we have a tier_id, start onboardUser in background
    if let Some(ref tier) = tier_id {
        let at = token_response.access_token.clone();
        let tid = tier.clone();
        tokio::spawn(async move {
            let _ = call_onboard_user(&at, &tid).await;
        });
    }

    // 8. Build extra data
    let mut extra = BTreeMap::new();
    if let Some(ref email) = email {
        extra.insert("email".to_string(), Value::String(email.clone()));
    }
    if let Some(ref pid) = project_id {
        extra.insert("projectId".to_string(), Value::String(pid.clone()));
    }
    if let Some(ref tid) = tier_id {
        extra.insert("tierId".to_string(), Value::String(tid.clone()));
    }
    extra.insert("clientMetadata".to_string(), client_metadata());

    Ok(AntigravityConnectResult {
        token_response,
        email,
        project_id,
        tier_id,
        extra,
    })
}

pub fn antigravity_needs_refresh(expires_at: &Option<String>) -> bool {
    crate::oauth::token_refresh::needs_refresh_with_lead(expires_at, REFRESH_LEAD_MS)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_metadata_contains_numeric_enums() {
        let md = client_metadata();
        assert_eq!(md.get("ideType").and_then(Value::as_i64), Some(9));
        assert_eq!(md.get("pluginType").and_then(Value::as_i64), Some(2));
        let platform = md.get("platform").and_then(Value::as_i64);
        assert!(platform.is_some());
    }

    #[test]
    fn test_build_antigravity_auth_url_includes_params() {
        let url = build_antigravity_auth_url("http://127.0.0.1:8080/callback", "test_state");
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("state=test_state"));
        assert!(url.contains("scope="));
        assert!(url.contains("cclog"));
        assert!(url.contains("experimentsandconfigs"));
    }

    #[test]
    fn test_extract_google_project_id() {
        let payload = serde_json::json!({
            "cloudaicompanionProject": {
                "id": "antigravity-proj"
            }
        });
        assert_eq!(
            extract_google_project_id(&payload),
            Some("antigravity-proj".to_string())
        );
    }

    #[test]
    fn test_extract_google_project_id_string() {
        let payload = serde_json::json!({
            "cloudaicompanionProject": "proj-id"
        });
        assert_eq!(extract_google_project_id(&payload), Some("proj-id".to_string()));
    }

    #[test]
    fn test_extract_google_project_id_missing() {
        assert_eq!(extract_google_project_id(&serde_json::json!({})), None);
    }

    #[test]
    fn test_parse_token_response() {
        let tokens = serde_json::json!({
            "access_token": "ya29.antigravity",
            "refresh_token": "1//antigravity-refresh",
            "expires_in": 3600,
        });
        let tr = parse_token_response(tokens).unwrap();
        assert_eq!(tr.access_token, "ya29.antigravity");
        assert_eq!(tr.refresh_token, Some("1//antigravity-refresh".to_string()));
        assert_eq!(tr.expires_in, Some(3600));
    }

    #[test]
    fn test_parse_token_response_minimal() {
        let tokens = serde_json::json!({ "access_token": "ya29.xyz" });
        let tr = parse_token_response(tokens).unwrap();
        assert_eq!(tr.access_token, "ya29.xyz");
        assert!(tr.refresh_token.is_none());
    }
}
