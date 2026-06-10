//! Gemini CLI OAuth 2.0 flow (Authorization Code, no PKCE).
//!
//! - Uses Google OAuth Authorization Code flow (no PKCE)
//! - access_type=offline, prompt=consent
//! - Post-exchange: fetch user info + loadCodeAssist for projectId
//! - Numeric Client-Metadata enums (ideType:9, platform per OS, pluginType:2)
//! - connect_gemini_cli(), refresh_gemini_cli()

use crate::oauth::TokenResponse;
use base64::Engine;
use rand::RngCore;
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const GEMINI_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GEMINI_CLIENT_SECRET: &str = "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";
const GEMINI_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GEMINI_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GEMINI_USER_INFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
const GEMINI_LOAD_CODE_ASSIST_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const GEMINI_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform \
    https://www.googleapis.com/auth/userinfo.email \
    https://www.googleapis.com/auth/userinfo.profile";

pub const REFRESH_LEAD_MS: u64 = 5 * 60 * 1000;

// ---------------------------------------------------------------------------
// Platform helper - numeric enums matching Google's Client-Metadata spec
// ---------------------------------------------------------------------------

/// Returns the numeric platform enum for Google Client-Metadata.
/// 0=unspecified, 1=macOS_x86, 2=macOS_arm, 3=linux_x86, 4=linux_arm, 5=windows
fn google_oauth_platform_enum() -> i64 {
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

/// Numeric Client-Metadata for Gemini CLI (ideType:9, platform per OS, pluginType:2).
pub fn client_metadata() -> serde_json::Value {
    serde_json::json!({
        "ideType": 9,
        "platform": google_oauth_platform_enum(),
        "pluginType": 2,
    })
}

// ---------------------------------------------------------------------------
// State / verifier generation (used for CSRF, no PKCE)
// ---------------------------------------------------------------------------

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Auth URL builder
// ---------------------------------------------------------------------------

fn build_gemini_auth_url(redirect_uri: &str, state: &str) -> String {
    let pairs: Vec<(&str, String)> = vec![
        ("client_id", GEMINI_CLIENT_ID.to_string()),
        ("response_type", "code".to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("scope", GEMINI_SCOPE.to_string()),
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

    format!("{GEMINI_AUTHORIZE_URL}?{query}")
}

// ---------------------------------------------------------------------------
// Token exchange (form-urlencoded, no PKCE)
// ---------------------------------------------------------------------------

async fn exchange_code_for_token(code: &str, redirect_uri: &str) -> Result<Value, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", GEMINI_CLIENT_ID),
        ("client_secret", GEMINI_CLIENT_SECRET),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ];

    let response = client
        .post(GEMINI_TOKEN_URL)
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
// User info + loadCodeAssist
// ---------------------------------------------------------------------------

/// Fetch user info from Google's userinfo endpoint.
async fn fetch_user_info(access_token: &str) -> Value {
    let client = reqwest::Client::new();
    match client
        .get(format!("{}?alt=json", GEMINI_USER_INFO_URL))
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            response.json().await.unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

/// Call loadCodeAssist and extract the projectId.
async fn fetch_project_id(access_token: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let response = client
        .post(GEMINI_LOAD_CODE_ASSIST_ENDPOINT)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "metadata": client_metadata(),
            "mode": 1,
        }))
        .send()
        .await
        .ok()?;

    if !response.status().is_success() {
        return None;
    }

    let payload: Value = response.json().await.ok()?;
    extract_google_project_id(&payload)
}

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

// ---------------------------------------------------------------------------
// Token response -> ConnectResult
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

pub async fn refresh_gemini_cli(refresh_token: &str) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", GEMINI_CLIENT_ID),
        ("client_secret", GEMINI_CLIENT_SECRET),
    ];

    let response = client
        .post(GEMINI_TOKEN_URL)
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

pub async fn connect_gemini_cli() -> Result<(TokenResponse, BTreeMap<String, Value>), String> {
    let state = generate_state();
    let redirect_uri = "http://127.0.0.1:8080/callback".to_string();

    // 1. Build auth URL
    let auth_url = build_gemini_auth_url(&redirect_uri, &state);

    // 2. Start local callback server on port 8080
    let listener = TcpListener::bind(("127.0.0.1", 8080))
        .await
        .map_err(|e| format!("Failed to bind local callback: {e}"))?;

    eprintln!(
        "Open this URL in your browser to authorize Gemini CLI:\n  {}\n\n\
         Waiting for callback on http://127.0.0.1:8080/callback...",
        auth_url
    );

    // 3. Accept incoming connection
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

    // 4. Extract code and verify state
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
    let tokens = exchange_code_for_token(&code, &redirect_uri).await?;
    let token_response = parse_token_response(tokens)?;

    // 6. Fetch user info
    let user_info = fetch_user_info(&token_response.access_token).await;
    let email = user_info
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string);

    // 7. Fetch projectId via loadCodeAssist
    let project_id = fetch_project_id(&token_response.access_token).await;

    // 8. Build extra data
    let mut extra = BTreeMap::new();
    if let Some(email) = email {
        extra.insert("email".to_string(), Value::String(email));
    }
    if let Some(pid) = project_id {
        extra.insert("projectId".to_string(), Value::String(pid));
    }
    extra.insert("clientMetadata".to_string(), client_metadata());

    Ok((token_response, extra))
}

pub fn gemini_cli_needs_refresh(expires_at: &Option<String>) -> bool {
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
        assert!(platform.is_some(), "platform should be present");
        assert!(platform.unwrap() >= 0 && platform.unwrap() <= 5);
    }

    #[test]
    fn test_build_gemini_auth_url_includes_params() {
        let url = build_gemini_auth_url("http://127.0.0.1:8080/callback", "test_state");
        assert!(url.contains(&format!("client_id={}", GEMINI_CLIENT_ID)));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("state=test_state"));
        assert!(url.contains(
            "scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform"
        ));
    }

    #[test]
    fn test_extract_google_project_id_from_object() {
        let payload = serde_json::json!({
            "cloudaicompanionProject": {
                "id": "my-project-123"
            }
        });
        assert_eq!(
            extract_google_project_id(&payload),
            Some("my-project-123".to_string())
        );
    }

    #[test]
    fn test_extract_google_project_id_from_string() {
        let payload = serde_json::json!({
            "cloudaicompanionProject": "direct-project-id"
        });
        assert_eq!(
            extract_google_project_id(&payload),
            Some("direct-project-id".to_string())
        );
    }

    #[test]
    fn test_extract_google_project_id_missing() {
        let payload = serde_json::json!({});
        assert_eq!(extract_google_project_id(&payload), None);
    }

    #[test]
    fn test_extract_google_project_id_empty() {
        let payload = serde_json::json!({
            "cloudaicompanionProject": {
                "id": ""
            }
        });
        assert_eq!(extract_google_project_id(&payload), None);
    }

    #[test]
    fn test_parse_token_response_full() {
        let tokens = serde_json::json!({
            "access_token": "ya29.abc",
            "refresh_token": "1//xyz",
            "expires_in": 3600,
            "scope": "openid email",
        });
        let tr = parse_token_response(tokens).unwrap();
        assert_eq!(tr.access_token, "ya29.abc");
        assert_eq!(tr.refresh_token, Some("1//xyz".to_string()));
        assert_eq!(tr.expires_in, Some(3600));
    }

    #[test]
    fn test_parse_token_response_minimal() {
        let tokens = serde_json::json!({
            "access_token": "ya29.abc"
        });
        let tr = parse_token_response(tokens).unwrap();
        assert_eq!(tr.access_token, "ya29.abc");
        assert!(tr.refresh_token.is_none());
    }

    #[test]
    fn test_parse_token_response_missing_access() {
        let tokens = serde_json::json!({});
        assert!(parse_token_response(tokens).is_err());
    }
}
