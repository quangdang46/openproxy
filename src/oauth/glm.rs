//! Z.ai GLM Coding OAuth — ZCode CLI polling flow.
//!
//! Port of 9router `src/lib/oauth/providers/glm.js` (commit 068ce87), which
//! mirrors the official ZCode CLI (`apps/zcode-cli` `cli-oauth.ts` +
//! `coding-plan-api-key.ts`). No PKCE and no local callback server:
//!
//!   1) POST {cliInitUrl}    Authorization: Bearer <pollToken>  {"provider":"zai"}
//!        → { code: 0, data: { authorize_url, flow_id, poll_interval_sec, expires_at } }
//!   2) Browser opens authorize_url; user signs in with the Z.ai account
//!   3) GET {cliPollUrl}/<flow_id>   Authorization: Bearer <pollToken>
//!        → { data: { status: "pending" } } until
//!          { data: { status: "ready", token, user, zai: { access_token, … } } }
//!   4) Z.AI OAuth token → POST {businessLoginUrl} {"token": …}
//!        → business JWT (`access_token`)
//!   5) Business JWT → coding-plan API key: getCustomerInfo → api_keys
//!      list/create("zcode-api-key") → copy → "{apiKey}.{secretKey}"
//!
//! The coding-plan API key is the long-lived model credential stored as the
//! connection's access token; the Z.AI OAuth token has no refresh grant, so
//! expiry means re-login (same as the official CLI). The zcode JWT, business
//! token and user fields ride along in `provider_specific_data`.

use std::collections::BTreeMap;

use rand::RngCore;
use reqwest::Client;
use serde_json::{json, Value};

use super::{DeviceCodeResponse, OAuthError, OAuthProviderConfig, TokenResponse};

/// Result of a successful GLM device-flow poll: the minted coding-plan
/// credential plus the extra `provider_specific_data` entries to persist.
pub struct GlmPollSuccess {
    pub token_response: TokenResponse,
    pub provider_specific_data: BTreeMap<String, Value>,
}

fn client() -> Client {
    Client::new()
}

fn oauth_error(error: &str, description: impl Into<String>) -> OAuthError {
    OAuthError {
        error: error.to_string(),
        error_description: Some(description.into()),
    }
}

/// 9router `isSuccessCode`: code 0/200 (or absent) = success.
fn is_success_code(code: Option<&Value>) -> bool {
    match code {
        None | Some(Value::Null) => true,
        Some(Value::Number(n)) => n.as_i64() == Some(0) || n.as_i64() == Some(200),
        Some(Value::String(s)) => s == "0" || s == "200",
        _ => false,
    }
}

/// Step 1 — ZCode CLI init. Mints a one-off poll token (Bearer on every
/// subsequent call) and returns the browser authorize URL plus the flow id
/// that doubles as the device code.
pub async fn glm_start_device_flow(
    provider_config: &OAuthProviderConfig,
) -> Result<(DeviceCodeResponse, String), OAuthError> {
    let cli_init_url = provider_config
        .get_param("cli_init_url")
        .ok_or_else(|| oauth_error("missing_cli_init_url", "GLM cli_init_url not configured"))?;

    let mut poll_token_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut poll_token_bytes);
    let poll_token = poll_token_bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    let provider_id = provider_config.get_param("provider_id").unwrap_or("zai");

    let response = client()
        .post(cli_init_url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {poll_token}"))
        .json(&json!({ "provider": provider_id }))
        .send()
        .await
        .map_err(|e| oauth_error("request_failed", e.to_string()))?;

    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(oauth_error(
            "init_failed",
            format!("ZCode OAuth init failed: {text}"),
        ));
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|e| oauth_error("parse_error", e.to_string()))?;
    if !is_success_code(payload.get("code")) {
        return Err(oauth_error(
            "init_failed",
            payload
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("ZCode OAuth init returned no data")
                .to_string(),
        ));
    }
    let data = payload
        .get("data")
        .ok_or_else(|| oauth_error("init_failed", "ZCode OAuth init returned no data"))?;
    let flow_id = data
        .get("flow_id")
        .and_then(Value::as_str)
        .ok_or_else(|| oauth_error("init_failed", "init response missing flow_id"))?
        .to_string();
    let authorize_url = data
        .get("authorize_url")
        .and_then(Value::as_str)
        .ok_or_else(|| oauth_error("init_failed", "init response missing authorize_url"))?
        .to_string();

    // expires_at is upstream-absolute (s or ms epoch); surface a relative
    // deadline, defaulting to 300s like the upstream UI fallback.
    let expires_in = data
        .get("expires_at")
        .and_then(Value::as_f64)
        .and_then(|raw| {
            if raw <= 0.0 {
                return None;
            }
            let ms = if raw > 1e12 { raw } else { raw * 1000.0 };
            let secs =
                ((ms - chrono::Utc::now().timestamp_millis() as f64) / 1000.0).floor() as i64;
            (secs > 0).then_some(secs)
        })
        .unwrap_or(300);
    let interval = data
        .get("poll_interval_sec")
        .and_then(Value::as_u64)
        .unwrap_or(3);

    Ok((
        DeviceCodeResponse {
            device_code: flow_id.clone(),
            // ZCode has no user code; the flow id is the only handle.
            user_code: flow_id.clone(),
            verification_uri: authorize_url.clone(),
            verification_uri_complete: Some(authorize_url),
            interval,
            expires_in: Some(expires_in),
        },
        poll_token,
    ))
}

/// Run the ZCode poll loop until the flow reports `ready` and return the
/// ready `data` payload.
async fn poll_until_ready(
    provider_config: &OAuthProviderConfig,
    poll_token: &str,
    flow_id: &str,
    interval_secs: u64,
) -> Result<Value, OAuthError> {
    let cli_poll_url = provider_config
        .get_param("cli_poll_url")
        .ok_or_else(|| oauth_error("missing_cli_poll_url", "GLM cli_poll_url not configured"))?;

    let client = client();
    let mut current_interval = interval_secs.max(1);

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(current_interval)).await;

        let response = client
            .get(format!(
                "{}/{}",
                cli_poll_url.trim_end_matches('/'),
                urlencoding::encode(flow_id)
            ))
            .header("Authorization", format!("Bearer {poll_token}"))
            .send()
            .await
            .map_err(|e| oauth_error("request_failed", e.to_string()))?;

        if !response.status().is_success() {
            return Err(oauth_error(
                "access_denied",
                format!("ZCode poll failed (HTTP {})", response.status()),
            ));
        }

        let payload: Value = response
            .json()
            .await
            .map_err(|e| oauth_error("parse_error", e.to_string()))?;
        if !is_success_code(payload.get("code")) {
            return Err(oauth_error(
                "access_denied",
                payload
                    .get("msg")
                    .and_then(Value::as_str)
                    .unwrap_or("ZCode poll failed")
                    .to_string(),
            ));
        }

        let data = payload.get("data").cloned().unwrap_or(Value::Null);
        match data.get("status").and_then(Value::as_str).unwrap_or("") {
            "pending" => continue,
            "failed" => {
                return Err(oauth_error(
                    "access_denied",
                    "ZCode authorization failed or was cancelled",
                ))
            }
            "ready" => return Ok(data),
            _ => continue,
        }
    }
}

/// Extract the Z.AI OAuth access token from a ready payload. The tokens nest
/// under `data[providerId]` (e.g. `data.zai.access_token`); fall back to
/// top-level fields for resilience against payload drift.
fn zai_access_token(data: &Value, provider_id: &str) -> Option<String> {
    let nested = data.get(provider_id).and_then(|v| {
        v.get("access_token")
            .or_else(|| v.get("accessToken"))
            .and_then(Value::as_str)
    });
    nested
        .or_else(|| {
            data.get("accessToken")
                .or_else(|| data.get("access_token"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn zai_refresh_token(data: &Value, provider_id: &str) -> Option<String> {
    let nested = data.get(provider_id).and_then(|v| {
        v.get("refresh_token")
            .or_else(|| v.get("refreshToken"))
            .and_then(Value::as_str)
    });
    nested
        .or_else(|| {
            data.get("refresh_token")
                .or_else(|| data.get("refreshToken"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// POST {businessLoginUrl} {"token": <zai oauth token>} → business JWT.
async fn exchange_business_token(
    provider_config: &OAuthProviderConfig,
    zai_access_token: &str,
) -> Result<String, OAuthError> {
    let url = provider_config
        .get_param("business_login_url")
        .ok_or_else(|| {
            oauth_error(
                "missing_business_login_url",
                "GLM business_login_url not configured",
            )
        })?;

    let payload = fetch_business_json(
        client()
            .post(url)
            .header("Content-Type", "application/json")
            .json(&json!({ "token": zai_access_token })),
        "Z.ai business login",
    )
    .await?;

    // fetch_business_json already unwraps `data`; the token may sit on the
    // unwrapped object or one level deeper depending on endpoint shape.
    let token = payload
        .get("access_token")
        .or_else(|| payload.get("accessToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("data")
                .and_then(|d| d.get("access_token").or_else(|| d.get("accessToken")))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| {
            oauth_error(
                "business_login_failed",
                "Z.ai business login response is missing access_token",
            )
        })?;
    Ok(token)
}

/// Business endpoints answer `{code, msg, data}`; code 0/200 (or absent) is
/// success. Returns the unwrapped `data` (or the whole payload when absent).
async fn fetch_business_json(
    request: reqwest::RequestBuilder,
    label: &str,
) -> Result<Value, OAuthError> {
    let response = request
        .send()
        .await
        .map_err(|e| oauth_error("request_failed", format!("Z.ai {label}: {e}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| oauth_error("request_failed", e.to_string()))?;
    if !status.is_success() {
        return Err(oauth_error(
            "business_error",
            format!(
                "Z.ai {label} request failed (HTTP {status}): {}",
                &text[..text.len().min(200)]
            ),
        ));
    }
    let payload: Value = serde_json::from_str(&text).map_err(|_| {
        oauth_error(
            "parse_error",
            format!("Z.ai {label} response is not valid JSON"),
        )
    })?;
    if payload.get("success").and_then(Value::as_bool) == Some(false)
        || !is_success_code(payload.get("code"))
    {
        return Err(oauth_error(
            "business_error",
            payload
                .get("msg")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("Z.ai {label} returned business error")),
        ));
    }
    Ok(payload.get("data").cloned().unwrap_or(payload))
}

/// Pick the default org + non-team project (projectType "2" = team),
/// preferring names containing 默认机构/默认项目 or "default".
fn pick_org_and_project(customer_info: &Value) -> Option<(String, String)> {
    let orgs = customer_info
        .get("organizations")
        .and_then(Value::as_array)?;

    let mut fallback: Option<(String, String)> = None;
    for org in orgs {
        let org_id = org
            .get("organizationId")
            .and_then(Value::as_str)?
            .to_string();
        let projects: Vec<&Value> = org
            .get("projects")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        let personal: Vec<&&Value> = projects
            .iter()
            .filter(|p| {
                p.get("projectType")
                    .map(|t| t.as_str().map(str::trim) != Some("2"))
                    .unwrap_or(true)
            })
            .collect();
        if personal.is_empty() {
            continue;
        }
        let org_name = org
            .get("organizationName")
            .and_then(Value::as_str)
            .unwrap_or("");
        let project = personal
            .iter()
            .find(|p| is_default_name(p.get("projectName").and_then(Value::as_str).unwrap_or("")))
            .or_else(|| personal.first())
            .unwrap();
        let project_id = project
            .get("projectId")
            .and_then(Value::as_str)?
            .to_string();
        let pair = (org_id, project_id);
        if is_default_name(org_name) {
            return Some(pair);
        }
        if fallback.is_none() {
            fallback = Some(pair);
        }
    }
    fallback
}

fn is_default_name(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    n.contains("默认机构") || n.contains("默认项目") || n == "default"
}

/// Business JWT → coding-plan API key ("{apiKey}.{secretKey}"). Mirrors the
/// ZCode CLI: getCustomerInfo → api_keys list/create("zcode-api-key") →
/// copy → secretKey.
async fn resolve_coding_plan_api_key(
    provider_config: &OAuthProviderConfig,
    business_token: &str,
) -> Result<String, OAuthError> {
    let api_base = provider_config
        .get_param("api_base_url")
        .unwrap_or("https://api.z.ai")
        .trim_end_matches('/');
    let plan_key_name = provider_config
        .get_param("plan_api_key_name")
        .unwrap_or("zcode-api-key");

    let auth = || {
        client()
            .header("Authorization", format!("Bearer {business_token}"))
            .header("Content-Type", "application/json")
    };

    let customer_info = fetch_business_json(
        auth().get(format!("{api_base}/api/biz/customer/getCustomerInfo")),
        "customer info",
    )
    .await?;

    let (org_id, project_id) = pick_org_and_project(&customer_info).ok_or_else(|| {
        oauth_error(
            "no_org_project",
            "Unable to resolve Z.ai organization and project for the coding plan",
        )
    })?;

    let list_url =
        format!("{api_base}/api/biz/v1/organization/{org_id}/projects/{project_id}/api_keys");

    let keys = fetch_business_json(auth().get(&list_url), "api keys").await?;
    let key_entry = keys.as_array().and_then(|arr| {
        arr.iter()
            .find(|item| item.get("name").and_then(Value::as_str) == Some(plan_key_name))
            .cloned()
    });

    let key_entry = match key_entry {
        Some(entry) => entry,
        None => {
            fetch_business_json(
                auth()
                    .post(&list_url)
                    .json(&json!({ "name": plan_key_name })),
                "api key create",
            )
            .await?
        }
    };

    let api_key = key_entry
        .get("apiKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            oauth_error(
                "api_key_missing",
                "Z.ai api_keys response is missing apiKey",
            )
        })?
        .to_string();

    let secret = fetch_business_json(
        auth().get(format!("{list_url}/copy/{}", urlencoding::encode(&api_key))),
        "api key copy",
    )
    .await?;
    let secret_key = secret
        .get("secretKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            oauth_error(
                "api_key_missing",
                "Z.ai api key copy response is missing secretKey",
            )
        })?;

    Ok(format!("{api_key}.{secret_key}"))
}

/// Full poll: ZCode poll → business JWT → coding-plan API key mint.
pub async fn glm_poll_for_token(
    provider_config: &OAuthProviderConfig,
    poll_token: &str,
    flow_id: &str,
    interval_secs: u64,
) -> Result<GlmPollSuccess, OAuthError> {
    let data = poll_until_ready(provider_config, poll_token, flow_id, interval_secs).await?;

    let provider_id = provider_config.get_param("provider_id").unwrap_or("zai");
    let zai_token = zai_access_token(&data, provider_id)
        .ok_or_else(|| oauth_error("access_denied", "ZCode poll response missing access token"))?;

    let business_token = exchange_business_token(provider_config, &zai_token).await?;
    let plan_api_key = resolve_coding_plan_api_key(provider_config, &business_token).await?;

    let user = data.get("user").cloned().unwrap_or(Value::Null);
    let user_name = user
        .get("name")
        .or_else(|| user.get("email"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let user_email = user
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string);
    let zcode_jwt = data
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut psd = BTreeMap::new();
    psd.insert("authMethod".to_string(), json!("cli_poll"));
    if let Some(name) = user.get("name").and_then(Value::as_str).map(str::to_string) {
        psd.insert("username".to_string(), json!(name));
    }
    if let Some(uid) = user.get("user_id").and_then(Value::as_str) {
        psd.insert("userId".to_string(), json!(uid));
    }
    if !zcode_jwt.is_empty() {
        psd.insert("zcodeJwtToken".to_string(), json!(zcode_jwt));
    }
    psd.insert("zaiBusinessToken".to_string(), json!(business_token));
    if let Some(rt) = zai_refresh_token(&data, provider_id) {
        psd.insert("zaiRefreshToken".to_string(), json!(rt));
    }
    if let Some(email) = &user_email {
        psd.insert("email".to_string(), json!(email));
    }
    if let Some(name) = &user_name {
        psd.insert("displayName".to_string(), json!(name));
    }

    Ok(GlmPollSuccess {
        token_response: TokenResponse {
            access_token: plan_api_key,
            // The Z.AI OAuth token has no refresh grant — re-login on expiry.
            refresh_token: None,
            expires_in: None,
            id_token: None,
            token_type: Some("Bearer".to_string()),
            scope: None,
        },
        provider_specific_data: psd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_code_accepts_upstream_shapes() {
        assert!(is_success_code(None));
        assert!(is_success_code(Some(&json!(0))));
        assert!(is_success_code(Some(&json!(200))));
        assert!(is_success_code(Some(&json!("0"))));
        assert!(!is_success_code(Some(&json!(1))));
        assert!(!is_success_code(Some(&json!("failed"))));
    }

    #[test]
    fn pick_org_prefers_default_names() {
        let info = json!({
            "organizations": [
                {
                    "organizationId": "org-team",
                    "organizationName": "Team Org",
                    "projects": [{"projectId": "p1", "projectName": "默认项目", "projectType": "1"}]
                },
                {
                    "organizationId": "org-def",
                    "organizationName": "默认机构",
                    "projects": [
                        {"projectId": "p-team", "projectName": "Team", "projectType": "2"},
                        {"projectId": "p-def", "projectName": "Default Project", "projectType": "1"}
                    ]
                }
            ]
        });
        // 默认机构 wins over the first org; its non-team project is chosen.
        assert_eq!(
            pick_org_and_project(&info),
            Some(("org-def".into(), "p-def".into()))
        );
    }

    #[test]
    fn pick_org_skips_team_only_orgs() {
        let info = json!({
            "organizations": [
                {
                    "organizationId": "org-team",
                    "organizationName": "Team",
                    "projects": [{"projectId": "p1", "projectName": "x", "projectType": "2"}]
                },
                {
                    "organizationId": "org-ok",
                    "organizationName": "Org",
                    "projects": [{"projectId": "p9", "projectName": "y", "projectType": "1"}]
                }
            ]
        });
        assert_eq!(
            pick_org_and_project(&info),
            Some(("org-ok".into(), "p9".into()))
        );
    }

    #[test]
    fn zai_token_prefers_nested_provider_data() {
        let data = json!({
            "status": "ready",
            "token": "jwt",
            "zai": {"access_token": "zai-at", "refresh_token": "zai-rt"},
            "accessToken": "top-level"
        });
        assert_eq!(zai_access_token(&data, "zai").as_deref(), Some("zai-at"));
        assert_eq!(zai_refresh_token(&data, "zai").as_deref(), Some("zai-rt"));

        let flat = json!({"status":"ready","accessToken":"flat-at"});
        assert_eq!(zai_access_token(&flat, "zai").as_deref(), Some("flat-at"));
    }
}
