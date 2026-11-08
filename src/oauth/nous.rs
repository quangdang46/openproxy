//! Nous Research OAuth helpers.
//!
//! 9router parity: `src/lib/oauth/providers/nous.js` +
//! `open-sse/services/nous.js`.

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::oauth::TokenResponse;

pub const PORTAL_BASE_URL: &str = "https://portal.nousresearch.com";
pub const INFERENCE_BASE_URL: &str = "https://inference-api.nousresearch.com/v1";
pub const CHAT_COMPLETIONS_URL: &str = "https://inference-api.nousresearch.com/v1/chat/completions";
pub const MODELS_URL: &str = "https://inference-api.nousresearch.com/v1/models";
pub const DEVICE_CODE_URL: &str = "https://portal.nousresearch.com/api/oauth/device/code";
pub const TOKEN_URL: &str = "https://portal.nousresearch.com/api/oauth/token";
pub const AGENT_KEY_URL: &str = "https://portal.nousresearch.com/api/oauth/agent-key";
pub const CLIENT_ID: &str = "hermes-cli";
pub const SCOPE: &str = "inference:invoke inference:mint_agent_key";
pub const VALIDATION_MODEL: &str = "poolside/laguna-xs-2.1:free";

#[derive(Debug, Clone)]
pub struct AgentKey {
    pub api_key: String,
    pub key_id: Option<String>,
    pub expires_at: Option<String>,
    pub expires_in: Option<i64>,
}

/// Mint the short-lived inference key Nous hands out after Portal OAuth.
/// Failures are non-fatal for callers: the OAuth access token remains a
/// valid fallback credential, matching `postExchange`'s null-agent path.
pub async fn mint_agent_key(access_token: &str) -> Result<Option<AgentKey>, String> {
    let access_token = access_token.trim();
    if access_token.is_empty() {
        return Ok(None);
    }

    let client = reqwest::Client::new();
    let response = client
        .post(AGENT_KEY_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .header(
            AUTHORIZATION,
            format!("Bearer {access_token}")
                .parse()
                .map_err(|_| "invalid Nous access token".to_string())?,
        )
        .json(&json!({ "min_ttl_seconds": 7200 }))
        .send()
        .await
        .map_err(|e| format!("Nous agent key request failed: {e}"))?;

    if !response.status().is_success() {
        // Do not include the response body: upstream errors can echo request
        // metadata and we do not need it for the fallback path.
        return Err(format!(
            "Nous agent key mint failed: HTTP {}",
            response.status()
        ));
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("Nous agent key response parse failed: {e}"))?;

    let api_key = payload
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    Ok(api_key.map(|api_key| AgentKey {
        api_key,
        key_id: payload
            .get("key_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_at: payload.get("expires_at").and_then(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| (!value.is_null()).then(|| value.to_string()))
        }),
        expires_in: payload.get("expires_in").and_then(Value::as_i64),
    }))
}

fn expires_in_from_timestamp(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some((parsed.timestamp() - chrono::Utc::now().timestamp()).max(1));
    }
    if let Ok(seconds) = value.parse::<i64>() {
        // Numeric expires_at values are normally epoch seconds. Values far in
        // the past are already-expired keys and must not become negative TTLs.
        let remaining = seconds - chrono::Utc::now().timestamp();
        return (remaining > 0).then_some(remaining);
    }
    None
}

pub(crate) fn agent_key_ttl(agent_key: &AgentKey) -> Option<i64> {
    agent_key.expires_in.or_else(|| {
        agent_key
            .expires_at
            .as_deref()
            .and_then(expires_in_from_timestamp)
    })
}

/// Prefer the minted agent key for inference, keeping the OAuth access token
/// available for future refresh calls via `refresh_token`.
pub fn apply_agent_key_to_token_response(token: &mut TokenResponse, agent_key: Option<&AgentKey>) {
    if let Some(agent_key) = agent_key {
        token.access_token = agent_key.api_key.clone();
        if let Some(expires_in) = agent_key_ttl(agent_key) {
            token.expires_in = Some(match token.expires_in {
                Some(existing) => existing.min(expires_in),
                None => expires_in,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_key_replaces_access_token_and_preserves_shorter_expiry() {
        let mut token = TokenResponse {
            access_token: "oauth-access".into(),
            refresh_token: Some("refresh".into()),
            expires_in: Some(86_400),
            id_token: None,
            token_type: None,
            scope: None,
        };
        let agent = AgentKey {
            api_key: "nous-agent".into(),
            key_id: Some("key-1".into()),
            expires_at: None,
            expires_in: Some(7_200),
        };
        apply_agent_key_to_token_response(&mut token, Some(&agent));
        assert_eq!(token.access_token, "nous-agent");
        assert_eq!(token.expires_in, Some(7_200));
        assert_eq!(token.refresh_token.as_deref(), Some("refresh"));
    }
}
