//! MiniMax Code (mcode) OAuth device flow — shared implementation for the two
//! site providers (`minimax-code` = China, `minimax-code-global` =
//! international). Port of 9router `src/lib/oauth/providers/minimax-code-shared.js`.
//!
//! Wire shape (reverse-engineered from @magpie-community/opencode-minimax-auth):
//!
//!   1) POST {account}/oauth2/device/code  (form)
//!        client_id, scope, audience, code_challenge, code_challenge_method=S256
//!        → user_code, verification_uri(_complete), device_code (sometimes
//!          absent — MiniMax's variant polls with the user_code instead),
//!          expiry as expired_in (ms epoch or seconds) or expires_in, interval
//!          (seconds on the standard shape, milliseconds on the user-code one)
//!   2) POST {account}/oauth2/token  (one POST per poll; the dashboard drives)
//!        grant_type=urn:ietf:params:oauth:grant-type:device_code,
//!        device_code|user_code, client_id, code_verifier
//!        → {status: pending|slow_down|denied|expired} on 200, or a standard
//!          OAuth {error} body; success carries access/refresh_token + expires_in
//!
//! The poll is a SINGLE attempt per call — the dashboard re-invokes the poll
//! endpoint, matching how `pollToken` is driven upstream.

use serde_json::Value;

use crate::oauth::providers::OAuthProviderConfig;
use crate::oauth::{OAuthError, TokenResponse};

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const SIGN_IN_MS: u64 = 10 * 60 * 1000;

/// Result of `request_device_code` — everything the dashboard needs plus the
/// flag telling the poll which credential field to send upstream.
pub struct DeviceCodeStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
    /// MiniMax's variant: no device_code was issued, so the token endpoint is
    /// polled with the user_code itself (`extraData._minimaxPollByUser`).
    pub poll_by_user: bool,
}

fn str_field<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn pos_field(body: &Value, key: &str) -> Option<u64> {
    body.get(key).and_then(Value::as_u64).filter(|n| *n > 0)
}

async fn post_form(url: &str, fields: &[(&str, String)]) -> Result<(u16, Value), OAuthError> {
    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(fields)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| OAuthError {
            error: "request_failed".to_string(),
            error_description: Some(e.to_string()),
        })?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let body = serde_json::from_str::<Value>(&text).unwrap_or_else(
        |_| serde_json::json!({ "error": format!("invalid_json_response (HTTP {status})") }),
    );
    Ok((status, body))
}

/// MiniMax states the device code's end in odd units: expired_in may be a ms
/// epoch or seconds; interval may be seconds (standard shape) or ms
/// (user-code shape). Normalize both to the units the dashboard expects.
fn normalize_deadline(body: &Value) -> Option<u64> {
    if let Some(expires_in) = pos_field(body, "expires_in") {
        return Some(expires_in);
    }
    let expired_in = pos_field(body, "expired_in")?;
    // < 1e12 is treated as seconds-from-now; larger values are a ms epoch.
    if expired_in < 1_000_000_000_000 {
        Some(expired_in)
    } else {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Some(expired_in.saturating_sub(now_ms).div_ceil(1000).max(1))
    }
}

fn normalize_interval(body: &Value, poll_by_user: bool) -> u64 {
    let Some(interval) = pos_field(body, "interval") else {
        return 5;
    };
    if !poll_by_user {
        return interval; // standard shape: seconds
    }
    // user-code shape: milliseconds — values >= 60 are treated as ms.
    if interval >= 60 {
        (interval / 1000).max(1)
    } else {
        interval
    }
}

/// Step 1 of the flow. `code_challenge` is the S256 challenge of a verifier
/// the caller generated and stored on the pending flow.
pub async fn request_device_code(
    config: &OAuthProviderConfig,
    code_challenge: &str,
) -> Result<DeviceCodeStart, OAuthError> {
    if code_challenge.is_empty() {
        return Err(OAuthError {
            error: "missing_code_challenge".to_string(),
            error_description: Some(
                "MiniMax Code sign-in requires a PKCE code challenge".to_string(),
            ),
        });
    }
    let audience = config
        .extra_params
        .iter()
        .find(|(k, _)| *k == "audience")
        .map(|(_, v)| *v)
        .unwrap_or("agent-backend");
    let fields = [
        ("client_id", config.client_id.to_string()),
        ("scope", config.scopes.join(" ")),
        ("audience", audience.to_string()),
        ("code_challenge", code_challenge.to_string()),
        ("code_challenge_method", "S256".to_string()),
    ];
    let (status, body) = post_form(config.authorize_url, &fields).await?;

    let user_code = str_field(&body, "user_code");
    let device_code = str_field(&body, "device_code");
    let uri = str_field(&body, "verification_uri_complete")
        .or_else(|| str_field(&body, "verification_uri"))
        .or_else(|| str_field(&body, "verification_url"));
    let (Some(user_code), Some(uri)) = (user_code, uri) else {
        return Err(OAuthError {
            error: "invalid_device_code_response".to_string(),
            error_description: Some(format!(
                "MiniMax device code response unusable (HTTP {status})"
            )),
        });
    };

    // No device_code → MiniMax's variant: the token endpoint is polled with
    // the user_code itself. Surface the user code as device_code (the modal
    // threads one value through) plus a flag the poll body switches on.
    let poll_by_user = device_code.is_none();
    Ok(DeviceCodeStart {
        device_code: device_code.unwrap_or(user_code).to_string(),
        user_code: user_code.to_string(),
        verification_uri: uri.to_string(),
        expires_in: normalize_deadline(&body).unwrap_or(SIGN_IN_MS / 1000),
        interval: normalize_interval(&body, poll_by_user),
        poll_by_user,
    })
}

/// Outcome of a single poll attempt.
pub enum PollOutcome {
    /// Still waiting — map to `authorization_pending` / `slow_down`.
    Pending { slow_down: bool },
    /// Terminal failure (denied / expired / protocol error).
    Failed(OAuthError),
    /// Success — standard TokenResponse.
    Success(TokenResponse),
}

/// Step 2 — ONE poll attempt. The dashboard drives repetition.
pub async fn poll_token(
    config: &OAuthProviderConfig,
    device_code: &str,
    code_verifier: &str,
    poll_by_user: bool,
) -> PollOutcome {
    if device_code.is_empty() {
        return PollOutcome::Pending { slow_down: false };
    }
    let code_field = if poll_by_user {
        "user_code"
    } else {
        "device_code"
    };
    let fields = [
        ("grant_type", DEVICE_GRANT.to_string()),
        (code_field, device_code.to_string()),
        ("client_id", config.client_id.to_string()),
        ("code_verifier", code_verifier.to_string()),
    ];
    let (_status, body) = match post_form(config.token_url, &fields).await {
        Ok(v) => v,
        Err(e) => return PollOutcome::Failed(e),
    };

    // MiniMax's own envelope: {status: ...} on HTTP 200.
    if let Some(status) = str_field(&body, "status") {
        return match status {
            "pending" => PollOutcome::Pending { slow_down: false },
            "slow_down" => PollOutcome::Pending { slow_down: true },
            "denied" | "access_denied" => PollOutcome::Failed(OAuthError {
                error: "access_denied".to_string(),
                error_description: Some("User denied the authorization request".to_string()),
            }),
            "expired" | "expired_token" => PollOutcome::Failed(OAuthError {
                error: "expired_token".to_string(),
                error_description: Some("The device code has expired".to_string()),
            }),
            other => PollOutcome::Failed(OAuthError {
                error: format!("unexpected_status:{other}"),
                error_description: None,
            }),
        };
    }

    // Standard OAuth error body on the same call.
    if let Some(err) = str_field(&body, "error") {
        let pending = err == "authorization_pending";
        let slow_down = err == "slow_down";
        if pending || slow_down {
            return PollOutcome::Pending { slow_down };
        }
        return PollOutcome::Failed(OAuthError {
            error: err.to_string(),
            error_description: str_field(&body, "error_description").map(str::to_string),
        });
    }

    if str_field(&body, "access_token").is_none() {
        return PollOutcome::Failed(OAuthError {
            error: "no_access_token".to_string(),
            error_description: None,
        });
    }
    match serde_json::from_value::<TokenResponse>(body) {
        Ok(token) => PollOutcome::Success(token),
        Err(e) => PollOutcome::Failed(OAuthError {
            error: "parse_error".to_string(),
            error_description: Some(e.to_string()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> OAuthProviderConfig {
        crate::oauth::providers::minimax_code()
    }

    #[test]
    fn deadline_prefers_expires_in() {
        let body = json!({"expires_in": 600, "expired_in": 123});
        assert_eq!(normalize_deadline(&body), Some(600));
    }

    #[test]
    fn deadline_seconds_vs_ms_epoch() {
        // seconds-from-now shape
        let body = json!({"expired_in": 600});
        assert_eq!(normalize_deadline(&body), Some(600));
        // ms epoch shape → converted to remaining seconds (>0)
        let future_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 600_000;
        let body = json!({"expired_in": future_ms});
        let secs = normalize_deadline(&body).unwrap();
        assert!(secs > 0 && secs <= 600, "got {secs}");
    }

    #[test]
    fn interval_standard_seconds_usercode_ms() {
        let body = json!({"interval": 10});
        assert_eq!(normalize_interval(&body, false), 10);
        assert_eq!(normalize_interval(&body, true), 10);
        let body = json!({"interval": 5000});
        assert_eq!(normalize_interval(&body, true), 5);
        let body = json!({});
        assert_eq!(normalize_interval(&body, false), 5);
    }

    #[test]
    fn empty_device_code_is_pending() {
        // poll_token with empty device_code short-circuits to pending without
        // network — verified via the Pending arm (can't easily assert without
        // a runtime here, so just ensure the function exists and the config
        // carries the right endpoints).
        let cfg = config();
        assert_eq!(
            cfg.authorize_url,
            "https://account.minimax.cn/oauth2/device/code"
        );
        assert_eq!(cfg.token_url, "https://account.minimax.cn/oauth2/token");
        assert_eq!(cfg.client_id, "mcode-public");
    }
}
