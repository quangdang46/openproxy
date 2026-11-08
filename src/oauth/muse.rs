//! Muse (Meta Model API) OAuth helpers.
//!
//! 9router parity: `src/lib/oauth/providers/muse.js` +
//! `open-sse/providers/registry/muse.js` (v0.5.112).
//!
//! Dual auth, same shape as kimi/nous:
//!   - `oauth`  = Muse Code subscription — Meta account device-code flow to
//!     auth.meta.com, then a key mint at api.meta.ai that yields the `LLM|…`
//!     Model API key the chat transport actually uses.
//!   - `apikey` = pay-as-you-go Model API key from dev.meta.ai.
//!
//! Meta issues no refresh token for the subscription flow → re-login on 401.
//! The device-code endpoints require the `x-api-version` header on every call,
//! which the generic device-code helpers do not send, so this module owns the
//! whole flow rather than reusing `device_code::start_device_flow`.

use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::oauth::{DeviceCodeResponse, OAuthError, TokenResponse};

pub const CLIENT_ID: &str = "1031625952748946";
pub const DEVICE_CODE_URL: &str = "https://auth.meta.com/oidc/device/authorization/";
pub const TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";
/// Mint endpoint: turns the Meta account access token into the `LLM|…` key.
pub const KEY_URL: &str = "https://api.meta.ai/muse-code/key";
pub const MODELS_URL: &str = "https://api.meta.ai/v1/models";
pub const API_VERSION: &str = "1.0.0";
/// 9router mapTokens stores the minted key as the connection credential; the
/// Meta account token is kept in providerSpecificData for a future re-mint.
pub const PSD_OAUTH_ACCESS_TOKEN: &str = "oauthAccessToken";
pub const PSD_AUTH_METHOD: &str = "authMethod";
pub const PSD_SUBSCRIPTION_TIER: &str = "subscriptionTier";
pub const PSD_AUTH_METHOD_VALUE: &str = "device_code";

fn meta_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if let Ok(v) = HeaderValue::from_str(API_VERSION) {
        headers.insert("x-api-version", v);
    }
    headers
}

/// POST the device-authorization request (upstream `requestDeviceCode`).
/// Body is exactly `client_id` — no scope, matching muse.js.
pub async fn request_device_code() -> Result<DeviceCodeResponse, OAuthError> {
    let client = reqwest::Client::new();
    let response = client
        .post(DEVICE_CODE_URL)
        .headers(meta_headers())
        .form(&[("client_id", CLIENT_ID)])
        .send()
        .await
        .map_err(|e| OAuthError {
            error: "request_failed".to_string(),
            error_description: Some(e.to_string()),
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(OAuthError {
            error: "device_code_request_failed".to_string(),
            error_description: Some(format!("{}: {}", status, text)),
        });
    }

    response.json().await.map_err(|e| OAuthError {
        error: "parse_error".to_string(),
        error_description: Some(e.to_string()),
    })
}

/// Poll the device-token endpoint once (upstream `pollToken`).
///
/// Returns the raw Meta token payload on success; pending states surface as
/// `authorization_pending` / `slow_down` errors exactly like the generic
/// helper so the caller's pending classification keeps working.
pub async fn poll_token_once(device_code: &str) -> Result<Value, OAuthError> {
    let client = reqwest::Client::new();
    let response = client
        .post(TOKEN_URL)
        .headers(meta_headers())
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| OAuthError {
            error: "request_failed".to_string(),
            error_description: Some(e.to_string()),
        })?;

    let status = response.status();
    let data: Value = response
        .json()
        .await
        .unwrap_or_else(|_| json!({ "error": "invalid_response" }));

    let pending = matches!(
        data.get("error").and_then(Value::as_str),
        Some("authorization_pending") | Some("slow_down")
    );
    if status.is_success() || pending {
        return Ok(data);
    }

    Err(OAuthError {
        error: data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("token_request_failed")
            .to_string(),
        error_description: data
            .get("error_description")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Poll until Meta returns an access token (upstream `pollToken` + the
/// provider-index pending loop). `slow_down` doubles the interval (cap 60s),
/// matching the generic `device_code::poll_for_token` semantics.
pub async fn poll_for_token(device_code: &str, interval_secs: u64) -> Result<Value, OAuthError> {
    let mut interval = interval_secs.max(1);
    // Safety bound: Meta device codes live ~15 minutes; the pending-flow TTL
    // is the real expiry, this just prevents an orphaned poll from looping
    // forever server-side.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20 * 60);
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(OAuthError {
                error: "poll_timeout".to_string(),
                error_description: Some("Muse device code poll timed out".to_string()),
            });
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let data = poll_token_once(device_code).await?;
        let error = data.get("error").and_then(Value::as_str);
        match error {
            Some("authorization_pending") => {}
            Some("slow_down") => {
                interval = (interval * 2).min(60);
                continue;
            }
            Some("access_denied") => {
                return Err(OAuthError {
                    error: "access_denied".to_string(),
                    error_description: Some("User denied the authorization request".to_string()),
                });
            }
            Some("expired_token") => {
                return Err(OAuthError {
                    error: "expired_token".to_string(),
                    error_description: Some("The device code has expired".to_string()),
                });
            }
            _ => {
                if data.get("access_token").is_some() {
                    return Ok(data);
                }
            }
        }
    }
}

/// Minted Muse Code subscription key (`postExchange` + `mapTokens` upstream).
#[derive(Debug, Clone)]
pub struct MuseKey {
    /// The `LLM|…` Model API key sent as the Bearer credential upstream.
    pub api_key: String,
    pub user_email: Option<String>,
    pub subscription_tier: Option<String>,
}

/// POST `https://api.meta.ai/muse-code/key` with the Meta account access
/// token. The endpoint is aggressively rate-limited (429) and the device code
/// is one-shot, so transient failures retry with backoff like upstream
/// (attempts 0..=2, 5s·(attempt+1) waits).
pub async fn mint_key(oauth_access_token: &str) -> Result<MuseKey, String> {
    // No-redirect client (upstream redirect: "error"): a redirect would
    // replay the bearer token at whatever origin the 3xx names.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Muse Code key mint client build failed: {e}"))?;
    let mut last_status = 0u16;
    let mut last_text = String::new();

    for attempt in 0..=2u32 {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Ok(v) = HeaderValue::from_str(API_VERSION) {
            headers.insert("x-api-version", v);
        }
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {oauth_access_token}")) {
            headers.insert(AUTHORIZATION, v);
        }

        let response = client
            .post(KEY_URL)
            .headers(headers)
            .json(&json!({ "onboard": true }))
            .send()
            .await
            .map_err(|e| format!("Muse Code key mint request failed: {e}"))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let transient = status.as_u16() == 429 || status.as_u16() >= 500;
        if !transient || attempt == 2 {
            if !status.is_success() {
                // Meta's error envelope (e.g. 403 code 4705002) carries the
                // fix-it URL — surface title/detail/action_url when present.
                let mut msg = format!("{} {}", status.as_u16(), &text[..text.len().min(200)]);
                if let Ok(err) = serde_json::from_str::<Value>(&text) {
                    let parts: Vec<&str> = [
                        err.get("title").and_then(Value::as_str),
                        err.get("detail").and_then(Value::as_str),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    if !parts.is_empty() {
                        msg = parts.join(": ");
                        if let Some(action) = err.get("action_url").and_then(Value::as_str) {
                            msg = format!("{msg} — {action}");
                        }
                    }
                }
                return Err(format!("Muse Code key mint failed: {msg}"));
            }
            last_status = status.as_u16();
            last_text = text;
            break;
        }
        last_status = status.as_u16();
        last_text = text;
        tokio::time::sleep(std::time::Duration::from_secs(5 * (attempt as u64 + 1))).await;
    }

    let _ = (last_status, &last_text);
    let payload: Value = serde_json::from_str(&last_text)
        .map_err(|_| "Muse Code key mint returned invalid JSON".to_string())?;

    if payload
        .get("is_subs_active")
        .and_then(Value::as_bool)
        .map(|active| !active)
        .unwrap_or(false)
    {
        return Err(
            "Muse Code subscription is inactive — activate it on muse.ai first".to_string(),
        );
    }
    let action_url = payload
        .get("action_url")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("require_payment_action_url")
                .and_then(Value::as_str)
        });
    let api_key = payload
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|k| !k.is_empty());
    if api_key.is_none() {
        if payload
            .get("require_payment")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || action_url.is_some()
        {
            return Err(format!(
                "Muse Code subscription required{}",
                action_url.map(|u| format!(": {u}")).unwrap_or_default()
            ));
        }
        return Err("Muse Code key response is missing api_key".to_string());
    }

    Ok(MuseKey {
        api_key: api_key.unwrap_or_default().to_string(),
        user_email: payload
            .get("user_email")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_string),
        subscription_tier: payload
            .get("subs_tier_name")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Upstream `mapTokens`: the minted `LLM|…` key becomes the connection
/// credential (upstream stores it in `accessToken`); the Meta account token
/// moves to providerSpecificData so a future re-mint needs no new login.
/// Meta gives no expiry/refresh — a dead key means re-login.
pub fn apply_muse_key_to_token_response(
    token: &mut TokenResponse,
    muse_key: &MuseKey,
    oauth_access_token: &str,
    extra_psd: &mut std::collections::BTreeMap<String, Value>,
) {
    token.access_token = muse_key.api_key.clone();
    token.refresh_token = None;
    token.expires_in = None;
    extra_psd.insert(
        PSD_AUTH_METHOD.to_string(),
        Value::String(PSD_AUTH_METHOD_VALUE.to_string()),
    );
    extra_psd.insert(
        PSD_OAUTH_ACCESS_TOKEN.to_string(),
        Value::String(oauth_access_token.to_string()),
    );
    if let Some(tier) = &muse_key.subscription_tier {
        extra_psd.insert(
            PSD_SUBSCRIPTION_TIER.to_string(),
            Value::String(tier.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_upstream_registry() {
        assert_eq!(CLIENT_ID, "1031625952748946");
        assert_eq!(
            DEVICE_CODE_URL,
            "https://auth.meta.com/oidc/device/authorization/"
        );
        assert_eq!(TOKEN_URL, "https://auth.meta.com/oidc/device/token/");
        assert_eq!(KEY_URL, "https://api.meta.ai/muse-code/key");
        assert_eq!(MODELS_URL, "https://api.meta.ai/v1/models");
        assert_eq!(API_VERSION, "1.0.0");
    }

    #[test]
    fn apply_muse_key_replaces_access_token_and_clears_refresh() {
        let mut token = TokenResponse {
            access_token: "meta-account-token".into(),
            refresh_token: Some("should-be-dropped".into()),
            expires_in: Some(3600),
            id_token: None,
            token_type: None,
            scope: None,
        };
        let key = MuseKey {
            api_key: "LLM|abc123".into(),
            user_email: Some("user@meta.com".into()),
            subscription_tier: Some("contributor".into()),
        };
        let mut psd = std::collections::BTreeMap::new();
        apply_muse_key_to_token_response(&mut token, &key, "meta-account-token", &mut psd);
        assert_eq!(token.access_token, "LLM|abc123");
        assert!(token.refresh_token.is_none());
        assert!(token.expires_in.is_none());
        assert_eq!(
            psd.get(PSD_OAUTH_ACCESS_TOKEN).and_then(Value::as_str),
            Some("meta-account-token")
        );
        assert_eq!(
            psd.get(PSD_SUBSCRIPTION_TIER).and_then(Value::as_str),
            Some("contributor")
        );
    }
}
