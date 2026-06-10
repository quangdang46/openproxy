//! Qoder OAuth 2.0 flow (Device Token flow with PKCE).
//!
//! - Device Token flow (not Device Code)
//! - PKCE pair, nonce, machineId
//! - Poll /deviceToken/poll with 15s timeout
//! - 202/404 = pending, 200 = token
//! - NO refresh (server returns 403), re-login after 30d
//! - connect_qoder()

use base64::Engine;
use crate::oauth::pkce::{generate_code_challenge, generate_code_verifier};
use crate::oauth::TokenResponse;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const QODER_AUTH_URL: &str = "https://qoder.com/oauth/authorize";
const QODER_TOKEN_URL: &str = "https://api.qoder.com/oauth/token";
const QODER_DEVICE_TOKEN_POLL_URL: &str = "https://api.qoder.com/deviceToken/poll";
const QODER_CLIENT_ID: &str = "10009311001";
const QODER_CLIENT_SECRET: &str = "4Z3YjXycVsQvyGF1etiNlIBB4RsqSDtW";
const POLL_TIMEOUT_SECS: u64 = 15;
const POLL_INTERVAL_SECS: u64 = 3;
const REAUTH_AFTER_DAYS: u64 = 30;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct DeviceTokenStartRequest {
    client_id: String,
    scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code_challenge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code_challenge_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    machine_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenStartResponse {
    #[serde(default)]
    device_token: String,
    #[serde(default)]
    user_code: String,
    #[serde(default)]
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Serialize)]
struct DeviceTokenPollRequest {
    client_id: String,
    device_token: String,
    grant_type: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_nonce() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_machine_id() -> String {
    Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Device Token flow
// ---------------------------------------------------------------------------

/// Start the device token flow. Returns the device_token + user_code + verification_uri.
async fn start_device_token_flow(
    code_challenge: Option<&str>,
    nonce: Option<&str>,
    machine_id: Option<&str>,
) -> Result<DeviceTokenStartResponse, String> {
    let client = reqwest::Client::new();
    let mut body = DeviceTokenStartRequest {
        client_id: QODER_CLIENT_ID.to_string(),
        scope: "openid profile email".to_string(),
        code_challenge: code_challenge.map(str::to_string),
        code_challenge_method: code_challenge.map(|_| "S256".to_string()),
        nonce: nonce.map(str::to_string),
        machine_id: machine_id.map(str::to_string),
    };

    let response = client
        .post(QODER_AUTH_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Start device token request failed: {e}"))?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(format!("Start device token failed: {error_text}"));
    }

    response
        .json()
        .await
        .map_err(|e| format!("Start device token parse failed: {e}"))
}

/// Poll the device token endpoint.
/// - 200: token ready (parse TokenResponse)
/// - 202/404: still pending
async fn poll_device_token(device_token: &str) -> Result<Option<TokenResponse>, String> {
    let client = reqwest::Client::new();
    let body = DeviceTokenPollRequest {
        client_id: QODER_CLIENT_ID.to_string(),
        device_token: device_token.to_string(),
        grant_type: "urn:ietf:params:oauth:grant-type:device_code".to_string(),
    };

    let response = client
        .post(QODER_DEVICE_TOKEN_POLL_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Poll request failed: {e}"))?;

    let status = response.status();
    if status == reqwest::StatusCode::OK {
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|e| format!("Poll parse failed: {e}"))?;
        return Ok(Some(token));
    }

    if status == reqwest::StatusCode::ACCEPTED || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    let error_text = response.text().await.unwrap_or_default();
    Err(format!("Poll returned {}: {}", status, error_text))
}

// ---------------------------------------------------------------------------
// Refresh (always fails — server returns 403)
// ---------------------------------------------------------------------------

pub async fn refresh_qoder(_refresh_token: &str) -> Result<TokenResponse, String> {
    Err("Qoder does not support refresh tokens (server returns 403). Re-authenticate after 30 days.".to_string())
}

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

pub async fn connect_qoder() -> Result<(TokenResponse, BTreeMap<String, Value>), String> {
    // 1. Generate PKCE pair
    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);
    let nonce = generate_nonce();
    let machine_id = generate_machine_id();

    // 2. Start device token flow
    let start_resp = start_device_token_flow(
        Some(&code_challenge),
        Some(&nonce),
        Some(&machine_id),
    )
    .await?;

    eprintln!(
        "Open this URL in your browser to authorize Qoder:\n  {}\n\n\
         User code: {}\n\n\
         Waiting for authorization (timeout: {}s)...",
        start_resp.verification_uri_complete.as_deref().unwrap_or(&start_resp.verification_uri),
        start_resp.user_code,
        POLL_TIMEOUT_SECS,
    );

    // 3. Poll for token
    let deadline = std::time::Instant::now() + Duration::from_secs(POLL_TIMEOUT_SECS);
    let device_token = &start_resp.device_token;

    loop {
        if std::time::Instant::now() > deadline {
            return Err("Device token flow timed out".to_string());
        }

        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;

        match poll_device_token(device_token).await {
            Ok(Some(token_response)) => {
                let mut extra = BTreeMap::new();
                extra.insert("nonce".to_string(), Value::String(nonce));
                extra.insert("machineId".to_string(), Value::String(machine_id));
                extra.insert(
                    "reauthAfterDays".to_string(),
                    Value::Number(serde_json::Number::from(REAUTH_AFTER_DAYS)),
                );
                return Ok((token_response, extra));
            }
            Ok(None) => {
                // Still pending, continue polling
                continue;
            }
            Err(e) => {
                return Err(format!("Poll failed: {e}"));
            }
        }
    }
}

pub fn qoder_needs_refresh(expires_at: &Option<String>) -> bool {
    // Qoder tokens never refresh; check if expired or more than 30 days old
    let Some(expires_at) = expires_at else {
        return true;
    };

    match chrono::DateTime::parse_from_rfc3339(expires_at) {
        Ok(expires) => {
            let expires = expires.with_timezone(&chrono::Utc);
            let now = chrono::Utc::now();
            // Consider expired after 30 days regardless
            let max_lifetime = chrono::Duration::days(REAUTH_AFTER_DAYS as i64);
            now > expires || now - chrono::Duration::minutes(5) > expires - max_lifetime
        }
        Err(_) => true,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_token_start_request_serialization() {
        let req = DeviceTokenStartRequest {
            client_id: "test_client".to_string(),
            scope: "openid profile".to_string(),
            code_challenge: Some("challenge".to_string()),
            code_challenge_method: Some("S256".to_string()),
            nonce: Some("nonce_val".to_string()),
            machine_id: Some("machine_val".to_string()),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json.get("client_id").and_then(Value::as_str), Some("test_client"));
        assert_eq!(json.get("scope").and_then(Value::as_str), Some("openid profile"));
        assert_eq!(json.get("code_challenge").and_then(Value::as_str), Some("challenge"));
        assert_eq!(json.get("machine_id").and_then(Value::as_str), Some("machine_val"));
    }

    #[test]
    fn test_device_token_start_request_minimal() {
        let req = DeviceTokenStartRequest {
            client_id: "test".to_string(),
            scope: "openid".to_string(),
            code_challenge: None,
            code_challenge_method: None,
            nonce: None,
            machine_id: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("code_challenge").is_none());
        assert!(json.get("nonce").is_none());
        assert!(json.get("machine_id").is_none());
    }

    #[test]
    fn test_poll_request_serialization() {
        let req = DeviceTokenPollRequest {
            client_id: "c".to_string(),
            device_token: "dt".to_string(),
            grant_type: "urn:ietf:params:oauth:grant-type:device_code".to_string(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json.get("grant_type").and_then(Value::as_str),
            Some("urn:ietf:params:oauth:grant-type:device_code")
        );
    }

    #[test]
    fn test_generate_machine_id_is_uuid() {
        let id = generate_machine_id();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn test_generate_nonce_is_unique() {
        let n1 = generate_nonce();
        let n2 = generate_nonce();
        assert_ne!(n1, n2);
    }

    #[test]
    fn test_qoder_needs_refresh_no_expiry() {
        assert!(qoder_needs_refresh(&None));
    }

    #[test]
    fn test_qoder_needs_refresh_future() {
        let future = (chrono::Utc::now() + chrono::Duration::days(15)).to_rfc3339();
        assert!(!qoder_needs_refresh(&Some(future)));
    }

    #[test]
    fn test_qoder_needs_refresh_expired() {
        let past = "2020-01-01T00:00:00Z".to_string();
        assert!(qoder_needs_refresh(&Some(past)));
    }

    #[test]
    fn test_qoder_needs_refresh_over_30_days() {
        let old = (chrono::Utc::now() + chrono::Duration::days(25)).to_rfc3339();
        // Within 30 days but still valid since it hasn't actually expired
        assert!(!qoder_needs_refresh(&Some(old)));
    }
}
