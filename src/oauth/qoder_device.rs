//! Qoder device-token OAuth flow — port of 9router
//! `src/lib/oauth/services/qoder.js` + `src/lib/oauth/providers/qoder.js`.
//!
//! Unlike a standard device-code flow there is NO server-issued device code:
//! the client generates a PKCE pair + nonce + machineId locally, the user
//! lands on `{loginUrl}?challenge&challenge_method&machine_id&nonce` in the
//! browser, and we poll `{deviceTokenUrl}?nonce&verifier&challenge_method=S256`
//! until a `dt-`/token payload appears (202/404 = pending, 200 = done).
//!
//! Region-aware: `qoder-cn` uses the *.qoder.com.cn hosts; intl keeps the
//! *.qoder.sh hosts. Only the hosts differ — paths are identical
//! (open-sse/shared/qoder/constants.js).

use serde_json::Value;

use crate::oauth::TokenResponse;

/// Per-region endpoint set (mirrors `QODER_REGION_BASES`).
#[derive(Debug, Clone, Copy)]
pub struct QoderRegion {
    pub login_url: &'static str,
    pub device_token_url: &'static str,
    pub user_info_url: &'static str,
}

pub const QODER_INTL: QoderRegion = QoderRegion {
    login_url: "https://qoder.com/device/selectAccounts",
    device_token_url: "https://openapi.qoder.sh/api/v1/deviceToken/poll",
    user_info_url: "https://openapi.qoder.sh/api/v1/userinfo",
};

/// 9router registry/qoder-cn.js oauth block.
pub const QODER_CN: QoderRegion = QoderRegion {
    login_url: "https://qoder.com.cn/device/selectAccounts",
    device_token_url: "https://openapi.qoder.com.cn/api/v1/deviceToken/poll",
    user_info_url: "https://openapi.qoder.com.cn/api/v1/userinfo",
};

/// Everything the pending flow must carry between start and poll.
#[derive(Debug, Clone)]
pub struct QoderDeviceStart {
    /// nonce — doubles as the device_code the dashboard posts back.
    pub nonce: String,
    pub code_verifier: String,
    pub machine_id: String,
    pub verification_uri_complete: String,
    pub user_code: String,
    pub interval: u64,
    pub expires_in: u64,
}

/// Profile fields fetched after the token lands (best-effort upstream too).
#[derive(Debug, Clone, Default)]
pub struct QoderUserInfo {
    pub name: String,
    pub email: String,
    pub organization_id: String,
    pub user_id: String,
}

/// Outcome of one poll attempt.
pub enum QoderPollOutcome {
    Pending,
    Success {
        token: TokenResponse,
        user_id: String,
        expire_ms: i64,
    },
    Failed(String),
}

fn generate_pkce_pair() -> (String, String) {
    use base64::Engine;
    let verifier_bytes: [u8; 32] = {
        use rand::RngCore;
        let mut b = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut b);
        b
    };
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);
    use sha2::Digest;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Step 1 — purely client-side (upstream `initiateDeviceFlow` makes no
/// network call). Returns the verification URL the user opens.
pub fn initiate_device_flow(region: &QoderRegion) -> QoderDeviceStart {
    let (verifier, challenge) = generate_pkce_pair();
    let nonce = uuid::Uuid::new_v4().to_string();
    let machine_id = uuid::Uuid::new_v4().to_string();

    let params = format!(
        "challenge={}&challenge_method=S256&machine_id={}&nonce={}",
        urlencoding(&challenge),
        urlencoding(&machine_id),
        urlencoding(&nonce),
    );
    QoderDeviceStart {
        user_code: nonce[..8.min(nonce.len())].to_uppercase(),
        verification_uri_complete: format!("{}?{}", region.login_url, params),
        nonce: nonce.clone(),
        code_verifier: verifier,
        machine_id,
        interval: 2,
        expires_in: 300,
    }
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

/// Parse the upstream expiry hint into unix-ms (upstream `parseExpiry`):
/// numeric ms-epoch, numeric string, RFC3339, else now + 30 days.
fn parse_expiry_ms(expires_at: Option<&Value>, expires_in: Option<&Value>) -> i64 {
    if let Some(v) = expires_at {
        if let Some(n) = v
            .as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        {
            if n > 0 {
                return n;
            }
        }
        if let Some(s) = v.as_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                if let Ok(ms) = trimmed.parse::<i64>() {
                    if ms > 0 {
                        return ms;
                    }
                }
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
                    return dt.timestamp_millis();
                }
            }
        }
    }
    if let Some(secs) = expires_in.and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    }) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        if secs == 0 {
            return now_ms;
        }
        return now_ms + secs * 1000;
    }
    chrono::Utc::now().timestamp_millis() + 30 * 24 * 60 * 60 * 1000
}

/// Step 2 — ONE poll attempt. 202/404 = pending; 200 + `token` = success.
pub async fn poll_device_token(
    region: &QoderRegion,
    nonce: &str,
    code_verifier: &str,
) -> Result<QoderPollOutcome, String> {
    if nonce.is_empty() || code_verifier.is_empty() {
        return Err("pollDeviceToken: missing nonce or code verifier".to_string());
    }
    let url = format!(
        "{}?nonce={}&verifier={}&challenge_method=S256",
        region.device_token_url,
        urlencoding(nonce),
        urlencoding(code_verifier),
    );
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .header("User-Agent", "Go-http-client/2.0")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Qoder device token poll failed: {e}"))?;

    let status = response.status();
    if status == reqwest::StatusCode::ACCEPTED || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(QoderPollOutcome::Pending);
    }
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let mut message = format!("Qoder device token poll failed: HTTP {status}");
        if let Ok(body) = serde_json::from_str::<Value>(&text) {
            if let Some(m) = body.get("message").and_then(Value::as_str) {
                message = format!("Qoder device token poll failed: {m}");
            }
        }
        return Err(message);
    }
    let body: Value = serde_json::from_str(&text)
        .map_err(|e| format!("Qoder device token poll: invalid JSON response ({e})"))?;
    let Some(token) = body.get("token").and_then(Value::as_str) else {
        return Err("Qoder device token poll returned 200 but no token".to_string());
    };
    let expire_ms = parse_expiry_ms(
        body.get("expires_at"),
        body.get("expires_in").or_else(|| body.get("expiresIn")),
    );
    // Upstream floors expires_in at 1 day so a skewed upstream timestamp does
    // not truncate the stored token below something useful.
    let min_seconds = 24 * 60 * 60i64;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let remaining = ((expire_ms - now_ms) / 1000).max(min_seconds);
    Ok(QoderPollOutcome::Success {
        token: TokenResponse {
            access_token: token.to_string(),
            refresh_token: body
                .get("refresh_token")
                .and_then(Value::as_str)
                .map(str::to_string),
            expires_in: Some(remaining),
            id_token: None,
            token_type: None,
            scope: None,
        },
        user_id: body
            .get("user_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        expire_ms,
    })
}

/// Best-effort profile lookup so we have a name/email to display.
pub async fn fetch_user_info(region: &QoderRegion, access_token: &str) -> QoderUserInfo {
    let client = reqwest::Client::new();
    let response = client
        .get(region.user_info_url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "Go-http-client/2.0")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;
    let Ok(r) = response else {
        return QoderUserInfo::default();
    };
    if !r.status().is_success() {
        return QoderUserInfo::default();
    }
    let Ok(body) = r.json::<Value>().await else {
        return QoderUserInfo::default();
    };
    let s = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| body.get(*k).and_then(Value::as_str))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    QoderUserInfo {
        name: s(&["name", "username"]),
        email: s(&["email"]),
        organization_id: s(&["organization_id", "organizationId"]),
        user_id: s(&["id", "userId", "user_id"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cn_region_endpoints() {
        assert_eq!(
            QODER_CN.device_token_url,
            "https://openapi.qoder.com.cn/api/v1/deviceToken/poll"
        );
        assert_eq!(
            QODER_CN.user_info_url,
            "https://openapi.qoder.com.cn/api/v1/userinfo"
        );
        assert_eq!(
            QODER_CN.login_url,
            "https://qoder.com.cn/device/selectAccounts"
        );
    }

    #[test]
    fn initiate_flow_is_client_side() {
        let start = initiate_device_flow(&QODER_CN);
        assert!(!start.nonce.is_empty());
        assert!(!start.code_verifier.is_empty());
        assert!(!start.machine_id.is_empty());
        assert!(start
            .verification_uri_complete
            .starts_with("https://qoder.com.cn/device/selectAccounts?"));
        assert!(start
            .verification_uri_complete
            .contains("challenge_method=S256"));
        assert_eq!(start.interval, 2);
        assert_eq!(start.expires_in, 300);
        // user_code is the first 8 chars of the nonce, uppercased
        assert_eq!(start.user_code, start.nonce[..8].to_uppercase());
    }

    #[test]
    fn pkce_verifier_challenge_roundtrip() {
        let (verifier, challenge) = generate_pkce_pair();
        assert_eq!(verifier.len(), 43); // base64url of 32 bytes
        assert_eq!(challenge.len(), 43);
        assert_ne!(verifier, challenge);
    }

    #[test]
    fn parse_expiry_numeric_ms() {
        let v = serde_json::json!(1893456000000i64);
        assert_eq!(parse_expiry_ms(Some(&v), None), 1893456000000);
    }

    #[test]
    fn parse_expiry_falls_back_to_30d() {
        let ms = parse_expiry_ms(None, None);
        let now = chrono::Utc::now().timestamp_millis();
        assert!(ms > now + 29 * 24 * 3600 * 1000);
    }
}
