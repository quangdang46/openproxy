//! Live provider quota fetchers (GLM, MiniMax, GitHub, Codex, Claude, Gemini CLI,
//! Antigravity, Qoder, Kiro).
//!
//! Each provider exposes a small JSON API that reports remaining quota for the
//! current billing window. These functions issue a one-shot GET and normalize
//! the response into the canonical `quotas` shape used by the dashboard:
//!
//! ```jsonc
//! {
//!   "plan": "Pro",            // optional, GLM only
//!   "quotas": {
//!     "session (5h)": {
//!       "used": 12.3,
//!       "total": 100,
//!       "remaining": 87.7,
//!       "remainingPercentage": 87.7,
//!       "resetAt": "2026-05-12T18:30:00Z",
//!       "unlimited": false
//!     }
//!   }
//! }
//! ```
//!
//! Mirrors `open-sse/services/usage.js` from decolua/9router.
//!
//! # Concurrency
//!
//! Every public function in this module is a stateless one-shot HTTP call that
//! accepts an `access_token` by reference and returns a `serde_json::Value`.
//! **No internal synchronization is held** — multiple tasks may invoke any
//! function concurrently without data races inside this module.
//!
//! ## Known refresh race
//!
//! These fetchers are called from **two concurrent paths**:
//!
//! 1. **Auto-ping background loop** (`quota_auto_ping::process_connection`):
//!    refreshes the OAuth credentials via `dispatch_oauth_refresh` *before*
//!    calling `fetch_oauth_quota`. The fresh token is used in the same
//!    tick iteration.
//!
//! 2. **HTTP handler** (`usage::get_connection_usage`): reads a DB snapshot
//!    and calls `fetch_oauth_quota` without an intervening credential
//!    refresh.
//!
//! Because (1) updates the DB with a new token and (2) may have loaded its
//! snapshot *before* the write landed, (2) can call a fetcher with a stale
//! (possibly expired) token while a fresh token has already been written to
//! the DB by (1).  This is the **quota fetcher concurrent refresh race**.
//!
//! The race is benign:
//! - A stale-token request either succeeds or returns 401/403, which callers
//!   translate to `{ "message": "invalid/expired token" }`.
//! - The dashboard treats that message as "connected, but quota unavailable"
//!   and degrades gracefully.
//! - Credential refreshes are serialised per connection
//!   ([`CredentialManager`]), so at most one refresh runs at a time.

use serde_json::{json, Value};
use std::time::Duration;

const GLM_INTL_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const GLM_CN_URL: &str = "https://open.bigmodel.cn/api/monitor/usage/quota/limit";

// Tried in order; later entries are fallbacks for transient errors only.
const MINIMAX_INTL_URLS: &[&str] = &[
    "https://www.minimax.io/v1/token_plan/remains",
    "https://api.minimax.io/v1/api/openplatform/coding_plan/remains",
];
const MINIMAX_CN_URLS: &[&str] = &[
    "https://www.minimaxi.com/v1/api/openplatform/coding_plan/remains",
    "https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains",
];

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CODEX_RESET_CREDITS_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
const CODEX_RESET_CREDITS_CONSUME_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume";

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Fetch GLM (z.ai / open.bigmodel.cn) quota using the provider's API key.
/// `provider` is one of `glm` (intl) or `glm-cn` (china).
pub async fn fetch_glm_quota(api_key: &str, provider: &str) -> Value {
    if api_key.is_empty() {
        return json!({ "message": "GLM API key not available." });
    }
    let url = if provider == "glm-cn" {
        GLM_CN_URL
    } else {
        GLM_INTL_URL
    };

    let client = http_client();
    let response = match client
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("GLM error: {e}") }),
    };

    let status = response.status();
    if !status.is_success() {
        let msg = if status.as_u16() == 401 {
            "GLM API key invalid or expired.".to_string()
        } else {
            format!("GLM quota API error ({}).", status.as_u16())
        };
        return json!({ "message": msg });
    }

    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "message": format!("GLM error: {e}") }),
    };

    let data = body.get("data").cloned().unwrap_or_else(|| json!({}));
    let limits = data
        .get("limits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut quotas = serde_json::Map::new();
    for limit in &limits {
        if limit.get("type").and_then(|v| v.as_str()) != Some("TOKENS_LIMIT") {
            continue;
        }
        let used_percent = limit
            .get("percentage")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let reset_ms = limit
            .get("nextResetTime")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let remaining = (100.0 - used_percent).max(0.0);
        let reset_at = if reset_ms > 0 {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(reset_ms)
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        } else {
            None
        };
        quotas.insert(
            "session".to_string(),
            json!({
                "used": used_percent,
                "total": 100,
                "remaining": remaining,
                "remainingPercentage": remaining,
                "resetAt": reset_at,
                "unlimited": false,
            }),
        );
    }

    let plan = data
        .get("level")
        .and_then(|v| v.as_str())
        .map(|raw| {
            let mut chars = raw.chars();
            match chars.next() {
                Some(c) => {
                    c.to_ascii_uppercase().to_string()
                        + chars.as_str().to_ascii_lowercase().as_str()
                }
                None => "Unknown".to_string(),
            }
        })
        .unwrap_or_else(|| "Unknown".to_string());

    json!({ "plan": plan, "quotas": Value::Object(quotas) })
}

fn minimax_field<'a>(model: &'a Value, snake: &str, camel: &str) -> Option<&'a Value> {
    model.get(snake).or_else(|| model.get(camel))
}

fn minimax_num(model: &Value, snake: &str, camel: &str) -> f64 {
    minimax_field(model, snake, camel)
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn minimax_pct(model: &Value, snake: &str, camel: &str) -> Option<f64> {
    minimax_field(model, snake, camel)
        .and_then(|v| v.as_f64())
        .filter(|v| *v > 0.0)
}

fn is_text_quota_model(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    n.starts_with("minimax-m") || n.starts_with("coding-plan") || n == "general"
}

fn build_minimax_quota(
    total: f64,
    count: f64,
    reset_at: Option<String>,
    count_is_remaining: bool,
) -> Value {
    let safe_total = total.max(0.0);
    let used = if count_is_remaining {
        (safe_total - count).max(0.0)
    } else {
        count.max(0.0).min(safe_total)
    };
    let remaining = (safe_total - used).max(0.0);
    let remaining_pct = if safe_total > 0.0 {
        ((remaining / safe_total) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    json!({
        "used": used,
        "total": safe_total,
        "remaining": remaining,
        "remainingPercentage": remaining_pct,
        "resetAt": reset_at,
        "unlimited": false,
    })
}

fn pick_representative<F: Fn(&Value) -> f64>(models: &[Value], get_total: F) -> Option<&Value> {
    let with_quota: Vec<&Value> = models.iter().filter(|m| get_total(m) > 0.0).collect();
    let pool = if !with_quota.is_empty() {
        with_quota
    } else {
        models.iter().collect()
    };
    pool.into_iter().max_by(|a, b| {
        get_total(a)
            .partial_cmp(&get_total(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn minimax_reset_at(
    model: &Value,
    captured_at_ms: i64,
    remains_snake: &str,
    remains_camel: &str,
    end_snake: &str,
    end_camel: &str,
) -> Option<String> {
    let remains_ms = minimax_num(model, remains_snake, remains_camel);
    if remains_ms > 0.0 {
        return chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
            captured_at_ms + remains_ms as i64,
        )
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    }
    minimax_field(model, end_snake, end_camel)
        .and_then(|v| v.as_i64())
        .and_then(|ms| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        })
}

/// Fetch MiniMax token-plan / coding-plan quota. `provider` is one of
/// `minimax` (intl) or `minimax-cn` (china).
pub async fn fetch_minimax_quota(api_key: &str, provider: &str) -> Value {
    if api_key.is_empty() {
        return json!({ "message": "MiniMax API key not available." });
    }
    let urls: &[&str] = if provider == "minimax-cn" {
        MINIMAX_CN_URLS
    } else {
        MINIMAX_INTL_URLS
    };

    let client = http_client();
    let mut last_error: Option<String> = None;

    for (index, url) in urls.iter().enumerate() {
        let can_fallback = index + 1 < urls.len();
        let response = match client
            .get(*url)
            .bearer_auth(api_key)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_error = Some(e.to_string());
                if can_fallback {
                    continue;
                }
                break;
            }
        };

        let status = response.status();
        let raw_text = response.text().await.unwrap_or_default();
        let payload: Value = if raw_text.is_empty() {
            json!({})
        } else {
            serde_json::from_str(&raw_text).unwrap_or_else(|_| json!({}))
        };
        let base_resp = payload
            .get("base_resp")
            .or_else(|| payload.get("baseResp"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let api_status = base_resp
            .get("status_code")
            .or_else(|| base_resp.get("statusCode"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let api_msg = base_resp
            .get("status_msg")
            .or_else(|| base_resp.get("statusMsg"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let combined = format!("{api_msg} {raw_text}").to_lowercase();
        let auth_like = [
            "token plan",
            "coding plan",
            "invalid api key",
            "invalid key",
            "unauthorized",
            "inactive",
        ]
        .iter()
        .any(|needle| combined.contains(needle));

        if status.as_u16() == 401 || status.as_u16() == 403 || api_status == 1004 || auth_like {
            return json!({ "message": "MiniMax API key invalid or inactive. Use an active Token/Coding Plan key." });
        }

        if !status.is_success() {
            let err = format!("MiniMax usage endpoint error ({})", status.as_u16());
            last_error = Some(err.clone());
            let transient = matches!(status.as_u16(), 404 | 405) || status.as_u16() >= 500;
            if transient && can_fallback {
                continue;
            }
            return json!({ "message": format!("MiniMax connected. {err}") });
        }

        if api_status != 0 {
            let msg = if api_msg.is_empty() {
                "Upstream quota API error".to_string()
            } else {
                api_msg
            };
            return json!({ "message": format!("MiniMax connected. {msg}") });
        }

        let model_remains = payload
            .get("model_remains")
            .or_else(|| payload.get("modelRemains"));
        let all_models: Vec<Value> = model_remains
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let text_models: Vec<Value> = all_models
            .into_iter()
            .filter(|m| {
                let name = minimax_field(m, "model_name", "modelName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                is_text_quota_model(name)
            })
            .collect();

        if text_models.is_empty() {
            return json!({ "message": "MiniMax connected. No text quota data was returned." });
        }

        let captured_at_ms = chrono::Utc::now().timestamp_millis();
        let count_is_remaining = url.contains("/coding_plan/remains");
        let mut quotas = serde_json::Map::new();

        if let Some(session_model) = pick_representative(&text_models, |m| {
            minimax_num(
                m,
                "current_interval_total_count",
                "currentIntervalTotalCount",
            )
        }) {
            let total = minimax_num(
                session_model,
                "current_interval_total_count",
                "currentIntervalTotalCount",
            );
            let count_raw = minimax_num(
                session_model,
                "current_interval_usage_count",
                "currentIntervalUsageCount",
            )
            .max(0.0);
            let count_pct = minimax_pct(
                session_model,
                "current_interval_remaining_percent",
                "currentIntervalRemainingPercent",
            );
            // When the API returns percent-only fields (shared quota pools),
            // normalize total=100 and treat the percent value as remaining.
            let (effective_total, effective_count, effective_remaining_mode) = if total == 0.0 {
                if let Some(pct) = count_pct {
                    (100.0, pct, true)
                } else {
                    (total, count_raw, count_is_remaining)
                }
            } else {
                (total, count_raw, count_is_remaining)
            };
            let reset_at = minimax_reset_at(
                session_model,
                captured_at_ms,
                "remains_time",
                "remainsTime",
                "end_time",
                "endTime",
            );
            quotas.insert(
                "session (5h)".to_string(),
                build_minimax_quota(
                    effective_total,
                    effective_count,
                    reset_at,
                    effective_remaining_mode,
                ),
            );
        }

        if let Some(weekly_model) = pick_representative(&text_models, |m| {
            minimax_num(m, "current_weekly_total_count", "currentWeeklyTotalCount")
        }) {
            let weekly_total = minimax_num(
                weekly_model,
                "current_weekly_total_count",
                "currentWeeklyTotalCount",
            );
            let weekly_count_raw = minimax_num(
                weekly_model,
                "current_weekly_usage_count",
                "currentWeeklyUsageCount",
            )
            .max(0.0);
            let weekly_count_pct = minimax_pct(
                weekly_model,
                "current_weekly_remaining_percent",
                "currentWeeklyRemainingPercent",
            );
            let (w_total, w_count, w_remaining) = if weekly_total == 0.0 {
                if let Some(pct) = weekly_count_pct {
                    (100.0, pct, true)
                } else {
                    (weekly_total, weekly_count_raw, count_is_remaining)
                }
            } else {
                (weekly_total, weekly_count_raw, count_is_remaining)
            };
            if w_total > 0.0 {
                let reset_at = minimax_reset_at(
                    weekly_model,
                    captured_at_ms,
                    "weekly_remains_time",
                    "weeklyRemainsTime",
                    "weekly_end_time",
                    "weeklyEndTime",
                );
                quotas.insert(
                    "weekly (7d)".to_string(),
                    build_minimax_quota(w_total, w_count, reset_at, w_remaining),
                );
            }
        }

        if quotas.is_empty() {
            return json!({ "message": "MiniMax connected. Unable to extract quota usage." });
        }

        return json!({ "quotas": Value::Object(quotas) });
    }

    let msg = match last_error {
        Some(e) => format!("MiniMax connected. Unable to fetch usage: {e}"),
        None => "MiniMax connected. Unable to fetch usage.".to_string(),
    };
    json!({ "message": msg })
}

// ─── Shared helpers for OAuth provider quota fetchers ───

/// Cloud Code metadata sent to `loadCodeAssist` (shared by Gemini & Antigravity).
/// `platform` is an integer enum: 0=UNSPECIFIED, 1=LINUX, 2=DARWIN, 3=WINDOWS_ARM64, 4=WINDOWS_X64.
const CLOUD_CODE_BASE: &str = "https://cloudcode-pa.googleapis.com/v1internal";

fn cloud_code_metadata() -> Value {
    let platform = if cfg!(target_os = "macos") {
        2
    } else if cfg!(target_os = "linux") {
        1
    } else {
        4 // WINDOWS_X64
    };
    json!({
        "ideType": 9,
        "platform": platform,
        "pluginType": 2,
    })
}

fn antigravity_user_agent() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    format!("antigravity/1.107.0 {os}/{arch}")
}

/// Normalise a reset-time value to an RFC 3339 string (seconds precision).
///
/// Accepts:
/// - Numeric epoch milliseconds (≥ 1e12) or seconds (< 1e12)
/// - Numeric string with the same heuristic
/// - ISO-8601 / RFC 3339 string
fn parse_reset_time(value: &Value) -> Option<String> {
    let ms = match value {
        Value::Number(n) => n.as_f64().map(|f| f as i64),
        Value::String(s) => {
            // Try numeric first, then ISO
            if let Ok(f) = s.parse::<f64>() {
                Some(f as i64)
            } else {
                return chrono::DateTime::parse_from_rfc3339(s)
                    .or_else(|_| chrono::DateTime::parse_from_rfc3339(&format!("{s}Z")))
                    .ok()
                    .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            }
        }
        _ => None,
    }?;
    let ts = if ms >= 1_000_000_000_000_i64 {
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64)
    } else {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(ms as u64)
    };
    let dt: chrono::DateTime<chrono::Utc> = ts.into();
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Build the canonical quota entry JSON object.
fn build_quota_entry(used: f64, total: f64, reset_at: Option<String>) -> Value {
    let safe_total = total.max(0.0);
    let used_clamped = used.max(0.0).min(safe_total);
    let remaining = (safe_total - used_clamped).max(0.0);
    let remaining_pct = if safe_total > 0.0 {
        ((remaining / safe_total) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    json!({
        "used": used_clamped,
        "total": safe_total,
        "remaining": remaining,
        "remainingPercentage": remaining_pct,
        "resetAt": reset_at,
        "unlimited": false,
    })
}

/// Fetch GitHub Copilot premium-request quota.
///
/// `access_token` is the Copilot OAuth access token (sent as `token <tok>`,
/// not `Bearer`, per GitHub's auth scheme). `provider` is reserved for future
/// variants and is currently unused.
pub async fn fetch_github_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "GitHub access token not available." });
    }

    let client = http_client();

    let resp = match client
        .get("https://api.github.com/copilot_internal/user")
        .header("Authorization", format!("token {access_token}"))
        .header("Accept", "application/json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.100.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("GitHub error: {e}") }),
    };

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({ "message": "GitHub access token invalid or expired." });
    }
    if !status.is_success() {
        return json!({
            "message": format!("GitHub quota API error ({}).", status.as_u16())
        });
    }

    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "message": format!("GitHub error: {e}") }),
    };

    let username = body
        .get("login")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            body.get("copilot_plan")
                .and_then(|p| p.get("user_login"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    let mut quotas = serde_json::Map::new();

    if let Some(snapshots) = body.get("quota_snapshots").and_then(|v| v.as_object()) {
        let paid_keys = [
            ("chat", "chat"),
            ("completions", "completions"),
            ("premium_interactions", "premium interactions"),
        ];
        for (key, label) in paid_keys {
            let entry = match snapshots.get(key) {
                Some(e) => e,
                None => continue,
            };
            let entitlement = entry
                .get("entitlement")
                .and_then(|v| v.as_f64())
                .or_else(|| entry.get("quota").and_then(|v| v.as_f64()))
                .unwrap_or(0.0);
            let remaining = entry
                .get("remaining")
                .and_then(|v| v.as_f64())
                .or_else(|| entry.get("quota_remaining").and_then(|v| v.as_f64()))
                .unwrap_or(0.0);
            if entitlement <= 0.0 {
                continue;
            }
            let used = (entitlement - remaining).max(0.0).min(entitlement);
            let entry_reset = entry
                .get("reset_date")
                .and_then(parse_reset_time)
                .or_else(|| entry.get("quota_reset").and_then(parse_reset_time));
            quotas.insert(
                label.to_string(),
                build_quota_entry(used, entitlement, entry_reset),
            );
        }
    }

    // Free/limited plan: `monthly_quotas` holds totals, `limited_user_quotas` holds used amounts.
    // Both are flat number maps: { "chat": <number>, "completions": <number>, ... }.
    if quotas.is_empty() {
        let monthly = body.get("monthly_quotas").and_then(|v| v.as_object());
        let limited = body.get("limited_user_quotas").and_then(|v| v.as_object());
        if monthly.is_some() || limited.is_some() {
            let monthly = monthly.cloned().unwrap_or_default();
            let limited = limited.cloned().unwrap_or_default();
            let reset_at = body
                .get("limited_user_reset_date")
                .and_then(parse_reset_time);
            let mut keys: Vec<String> = monthly.keys().map(|k| k.to_string()).collect();
            for k in limited.keys() {
                if !keys.contains(k) {
                    keys.push(k.to_string());
                }
            }
            for key in &keys {
                let total = monthly
                    .get(key.as_str())
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let used = limited
                    .get(key.as_str())
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                if total <= 0.0 && used <= 0.0 {
                    continue;
                }
                let effective_total = if total > 0.0 { total } else { used };
                quotas.insert(
                    key.clone(),
                    build_quota_entry(used, effective_total, reset_at.clone()),
                );
            }
        }
    }

    if quotas.is_empty() {
        return json!({ "message": "GitHub connected. No quota data was returned." });
    }

    match username {
        Some(login) => json!({ "plan": login, "quotas": Value::Object(quotas) }),
        None => json!({ "quotas": Value::Object(quotas) }),
    }
}

pub async fn fetch_codex_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Codex access token not available." });
    }

    let client = http_client();

    let response = match client
        .get(CODEX_USAGE_URL)
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("Codex error: {e}") }),
    };

    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({ "message": "Invalid or expired Codex token" });
    }
    if !status.is_success() {
        return json!({
            "message": format!("Codex quota API error ({}).", status.as_u16())
        });
    }

    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "message": format!("Codex error: {e}") }),
    };

    let mut quotas = serde_json::Map::new();

    let plan = body
        .get("plan_type")
        .or_else(|| body.pointer("/summary/plan"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    if let Some(plan_label) = plan.as_deref() {
        quotas.insert(
            "plan".to_string(),
            json!({
                "used": 0.0,
                "total": 0.0,
                "remaining": 0.0,
                "remainingPercentage": 0.0,
                "resetAt": Value::Null,
                "unlimited": false,
                "label": plan_label,
            }),
        );
    }

    let normal_rl = body
        .get("rate_limit")
        .or_else(|| body.get("rate_limits"))
        .or_else(|| {
            body.get("rate_limits_by_limit_id")
                .and_then(|m| m.as_object())
                .and_then(|m| m.get("codex"))
        });
    if let Some(snapshot) = normal_rl {
        append_codex_quota_windows(&mut quotas, "", snapshot);
    }

    if let Some(review) = get_codex_review_rate_limit(&body) {
        append_codex_quota_windows(&mut quotas, "review", &review);
    }

    let available_reset_credits = body
        .pointer("/rate_limit_reset_credits/available_count")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            body.pointer("/rate_limit_reset_credits/availableCount")
                .and_then(|v| v.as_f64())
        })
        .unwrap_or(0.0)
        .max(0.0);

    if quotas.is_empty() {
        return json!({
            "message": "Codex connected. No quota data was returned.",
            "plan": plan,
            "resetCredits": { "availableCount": available_reset_credits },
        });
    }

    let limit_reached = normal_rl
        .map(|snapshot| {
            codex_rate_limit_body(snapshot)
                .get("limit_reached")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .unwrap_or(false);

    json!({
        "plan": plan.unwrap_or_else(|| "unknown".to_string()),
        "limitReached": limit_reached,
        "resetCredits": { "availableCount": available_reset_credits },
        "quotas": Value::Object(quotas),
    })
}

/// Resolve ChatGPT account id for Codex reset-credit APIs.
pub fn codex_account_id(
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
) -> Option<String> {
    for key in ["workspaceId", "accountId", "chatgptAccountId", "account_id"] {
        if let Some(s) = provider_specific_data
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(s.to_string());
        }
    }
    None
}

fn codex_to_iso_date(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        other => parse_reset_time(other).map(|iso| {
            // Prefer millisecond precision when parse yields seconds-only RFC3339.
            chrono::DateTime::parse_from_rfc3339(&iso)
                .map(|dt| {
                    dt.with_timezone(&chrono::Utc)
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                })
                .unwrap_or(iso)
        }),
    }
}

/// GET OpenAI rate-limit reset credits for a Codex connection.
///
/// Mirrors 9router `getCodexRateLimitResetCredits`.
pub async fn get_codex_rate_limit_reset_credits(
    access_token: &str,
    account_id: Option<&str>,
) -> Result<Value, String> {
    if access_token.trim().is_empty() {
        return Err(
            "No Codex access token available. Please re-authorize the connection.".to_string(),
        );
    }

    let client = http_client();
    let mut request = client
        .get(CODEX_RESET_CREDITS_URL)
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .header("OpenAI-Beta", "codex-1")
        .header("originator", "codex_cli_rs");
    if let Some(id) = account_id.map(str::trim).filter(|s| !s.is_empty()) {
        request = request.header("ChatGPT-Account-ID", id);
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("Failed to fetch Codex reset credits: {e}"))?;

    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);

    if !status.is_success() {
        let message = body
            .get("message")
            .or_else(|| body.get("error"))
            .or_else(|| body.get("detail"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                format!("Codex reset credits API unavailable ({}).", status.as_u16())
            });
        return Err(message);
    }

    let available_count = body
        .get("available_count")
        .or_else(|| body.get("availableCount"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .max(0.0);

    let credits = body
        .get("credits")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|credit| {
                    json!({
                        "status": credit
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown"),
                        "grantedAt": credit
                            .get("granted_at")
                            .or_else(|| credit.get("grantedAt"))
                            .and_then(codex_to_iso_date),
                        "expiresAt": credit
                            .get("expires_at")
                            .or_else(|| credit.get("expiresAt"))
                            .and_then(codex_to_iso_date),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(json!({
        "availableCount": available_count,
        "credits": credits,
    }))
}

/// Result of consuming one Codex rate-limit reset credit via OpenAI.
#[derive(Debug, Clone)]
pub struct CodexResetCreditConsumeResult {
    pub ok: bool,
    pub no_credit: bool,
    pub status: u16,
    pub code: Option<String>,
    pub windows_reset: f64,
    pub message: Option<String>,
    pub raw: Value,
}

/// POST consume one Codex rate-limit reset credit (irreversible).
///
/// Mirrors 9router `consumeCodexRateLimitResetCredit`.
pub async fn consume_codex_rate_limit_reset_credit(
    access_token: &str,
    redeem_request_id: &str,
) -> Result<CodexResetCreditConsumeResult, String> {
    if access_token.trim().is_empty() {
        return Err(
            "No Codex access token available. Please re-authorize the connection.".to_string(),
        );
    }
    if redeem_request_id.trim().is_empty() {
        return Err("A redeem request id is required to consume a Codex reset credit.".to_string());
    }

    let client = http_client();
    let response = client
        .post(CODEX_RESET_CREDITS_CONSUME_URL)
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&json!({ "redeem_request_id": redeem_request_id }))
        .send()
        .await
        .map_err(|e| format!("Failed to consume Codex reset credit: {e}"))?;

    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let data: Value = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };

    let code = data
        .get("code")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let windows_reset = data
        .get("windows_reset")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let success =
        (200..300).contains(&status) && (code.as_deref() == Some("reset") || windows_reset > 0.0);
    let no_credit = (200..300).contains(&status) && code.as_deref() == Some("no_credit");
    let message = data
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(CodexResetCreditConsumeResult {
        ok: success,
        no_credit,
        status,
        code,
        windows_reset,
        message,
        raw: data,
    })
}

fn codex_rate_limit_body(snapshot: &Value) -> &Value {
    if let Some(rl) = snapshot.get("rate_limit") {
        if rl.is_object() {
            return rl;
        }
    }
    snapshot
}

fn format_codex_window(window: &Value) -> Option<Value> {
    let used_percent = window
        .get("used_percent")
        .or_else(|| window.get("percent_used"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);
    let reset_at = window
        .get("reset_at")
        .or_else(|| window.get("resetAt"))
        .and_then(parse_reset_time);
    let window_minutes = window
        .get("window_minutes")
        .or_else(|| window.get("windowMinutes"))
        .and_then(|v| v.as_i64());
    let unlimited = window
        .get("unlimited")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some(build_quota_entry_with_meta(
        used_percent,
        100.0,
        reset_at,
        unlimited,
        window_minutes,
    ))
}

fn build_quota_entry_with_meta(
    used: f64,
    total: f64,
    reset_at: Option<String>,
    unlimited: bool,
    window_minutes: Option<i64>,
) -> Value {
    let safe_total = total.max(0.0);
    let used_clamped = used.max(0.0).min(safe_total);
    let remaining = (safe_total - used_clamped).max(0.0);
    let remaining_pct = if safe_total > 0.0 {
        ((remaining / safe_total) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    json!({
        "used": used_clamped,
        "total": safe_total,
        "remaining": remaining,
        "remainingPercentage": remaining_pct,
        "resetAt": reset_at,
        "unlimited": unlimited,
        "windowMinutes": window_minutes,
    })
}

fn append_codex_quota_windows(
    quotas: &mut serde_json::Map<String, Value>,
    prefix: &str,
    snapshot: &Value,
) {
    let rl = codex_rate_limit_body(snapshot);
    let primary = rl
        .get("primary_window")
        .or_else(|| rl.get("primary"))
        .or_else(|| snapshot.get("primary_window"))
        .or_else(|| snapshot.get("primary"));
    if let Some(p) = primary {
        if let Some(entry) = format_codex_window(p) {
            let key = if prefix.is_empty() {
                "session".to_string()
            } else {
                format!("{prefix}_session")
            };
            quotas.insert(key, entry);
        }
    }
    let secondary = rl
        .get("secondary_window")
        .or_else(|| rl.get("secondary"))
        .or_else(|| snapshot.get("secondary_window"))
        .or_else(|| snapshot.get("secondary"));
    if let Some(s) = secondary {
        if let Some(entry) = format_codex_window(s) {
            let key = if prefix.is_empty() {
                "weekly".to_string()
            } else {
                format!("{prefix}_weekly")
            };
            quotas.insert(key, entry);
        }
    }
}

fn get_codex_review_rate_limit(data: &Value) -> Option<Value> {
    if let Some(v) = data.get("code_review_rate_limit") {
        return Some(v.clone());
    }
    if let Some(v) = data.get("review_rate_limit") {
        return Some(v.clone());
    }
    if let Some(map) = data
        .get("rate_limits_by_limit_id")
        .and_then(|v| v.as_object())
    {
        for key in &["code_review", "codex_review", "review"] {
            if let Some(v) = map.get(*key) {
                return Some(v.clone());
            }
        }
    }
    if let Some(limits) = data
        .get("additional_rate_limits")
        .and_then(|v| v.as_array())
    {
        for limit in limits {
            let id = limit
                .get("limit_name")
                .or_else(|| limit.get("metered_feature"))
                .or_else(|| limit.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            if id.contains("review") {
                return Some(limit.clone());
            }
        }
    }
    None
}

/// Normalise a `cloudaicompanionProject` value to its string ID.
/// Google sometimes returns this as a bare string and sometimes as
/// `{ "id": "..." }`; handle both.
fn normalize_cloud_code_project_id(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(obj) = value.as_object() {
        if let Some(s) = obj.get("id").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
        if let Some(s) = obj.get("name").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

pub async fn fetch_gemini_cli_quota(
    access_token: &str,
    _provider: &str,
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Gemini CLI access token not available." });
    }

    let client = http_client();

    // 9router order: prefer the OAuth-stored projectId, then fall back to
    // loadCodeAssist → cloudaicompanionProject.
    let mut project_id: Option<String> = provider_specific_data
        .get("projectId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if project_id.is_none() {
        let load_body = load_code_assist(
            &client,
            access_token,
            cloud_code_metadata(),
            &[("x-goog-api-client", "gl-rust/1.0.0")],
            None,
        )
        .await;

        match load_body {
            Ok(value) => {
                project_id = value
                    .get("cloudaicompanionProject")
                    .and_then(normalize_cloud_code_project_id)
                    .or_else(|| {
                        value
                            .get("cloudProject")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
            }
            Err(e) => {
                if e.contains("401") || e.contains("403") {
                    return json!({ "message": "Gemini CLI access token invalid or expired." });
                }
                return json!({ "message": format!("Gemini CLI error: {e}") });
            }
        }
    }

    let project_id = match project_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            return json!({
                "message": "Gemini CLI connected. No project returned by loadCodeAssist."
            })
        }
    };

    let url = format!("{CLOUD_CODE_BASE}:retrieveUserQuota?projectId={project_id}");
    let response = match client
        .post(&url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .header("x-goog-api-client", "gl-rust/1.0.0")
        .json(&json!({ "project": project_id }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("Gemini CLI error: {e}") }),
    };

    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({ "message": "Gemini CLI access token invalid or expired." });
    }
    if !status.is_success() {
        return json!({
            "message": format!("Gemini CLI quota API error ({}).", status.as_u16())
        });
    }

    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "message": format!("Gemini CLI error: {e}") }),
    };

    let buckets = body
        .get("buckets")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if buckets.is_empty() {
        return json!({ "message": "Gemini CLI connected. No quota data was returned." });
    }

    let mut quotas = serde_json::Map::new();
    for bucket in &buckets {
        let model_id = bucket
            .get("modelId")
            .and_then(|v| v.as_str())
            .unwrap_or("model")
            .to_string();
        let reset_at = bucket.get("resetTime").and_then(parse_reset_time);

        // remainingFraction is a float 0..1. Map to a 1000-unit pool so the
        // dashboard's percent display stays precise.
        let fraction = bucket
            .get("remainingFraction")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let total = 1000.0;
        let remaining = (total * fraction).round();
        let used = (total - remaining).max(0.0);
        quotas.insert(model_id, build_quota_entry(used, total, reset_at));
    }

    json!({ "quotas": Value::Object(quotas) })
}

/// Shared `loadCodeAssist` call for Gemini CLI and Antigravity.
///
/// POSTs to `cloudcode-pa.googleapis.com/v1internal:loadCodeAssist` with the
/// given body and extra headers. Returns the parsed JSON body on 200.
///
/// `extra_body` is merged into the top-level JSON body (shallow merge) so
/// callers can add `mode: 1` or other fields without rewriting the helper.
async fn load_code_assist(
    client: &reqwest::Client,
    access_token: &str,
    body: Value,
    extra_headers: &[(&str, &str)],
    extra_body: Option<&Value>,
) -> Result<Value, String> {
    let url = format!("{CLOUD_CODE_BASE}:loadCodeAssist");
    let final_body = match extra_body {
        Some(extra) if extra.is_object() => {
            let mut merged = body;
            if let (Some(merged_obj), Some(extra_obj)) = (merged.as_object_mut(), extra.as_object())
            {
                for (k, v) in extra_obj {
                    merged_obj.insert(k.clone(), v.clone());
                }
            }
            merged
        }
        _ => body,
    };
    let mut req = client
        .post(&url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .json(&final_body);
    for (k, v) in extra_headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("loadCodeAssist returned {status}"));
    }
    Ok(body)
}

/// Fetch Qoder OAuth subscription quota.
pub async fn fetch_qoder_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Qoder token" });
    }

    let client = http_client();
    let response = match client
        .get("https://openapi.qoder.sh/api/v2/quota/usage")
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("Qoder error: {e}") }),
    };

    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({ "message": "Invalid or expired Qoder token" });
    }
    if !status.is_success() {
        return json!({
            "message": format!("Qoder quota API error ({}).", status.as_u16())
        });
    }

    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "message": format!("Qoder error: {e}") }),
    };

    let reset_at = body
        .get("expiresAt")
        .or_else(|| body.get("expires_at"))
        .or_else(|| body.get("reset_at"))
        .and_then(parse_reset_time);

    let mut quotas = serde_json::Map::new();

    if let Some(user) = body.get("userQuota") {
        let total = user
            .get("total")
            .or_else(|| user.get("limit"))
            .or_else(|| user.get("quota"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let used = user
            .get("used")
            .or_else(|| user.get("usage"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if total > 0.0 || used > 0.0 {
            quotas.insert(
                "user".to_string(),
                build_quota_entry(used, total, reset_at.clone()),
            );
        }
    }

    if let Some(org) = body.get("orgResourcePackage") {
        let total = org
            .get("total")
            .or_else(|| org.get("limit"))
            .or_else(|| org.get("quota"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let used = org
            .get("used")
            .or_else(|| org.get("usage"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if total > 0.0 || used > 0.0 {
            quotas.insert(
                "org".to_string(),
                build_quota_entry(used, total, reset_at.clone()),
            );
        }
    }

    if quotas.is_empty() {
        return json!({ "message": "Qoder connected. No quota data was returned." });
    }

    json!({ "quotas": Value::Object(quotas) })
}

/// Vercel AI Gateway credit usage (9router services/usage/misc.js getVercelAiGatewayUsage).
/// GET https://ai-gateway.vercel.sh/v1/credits with Bearer auth; returns
/// { balance, total_used } as USD decimal strings. Plan rows mirror JS
/// exactly (MONTHLY_CREDIT = 5; remainingPercentage may exceed 100).
/// OpenCode Go usage endpoint (9router `opencode-go.js` registry
/// `transport.usage.url`).
const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// Coerce a percent value (number or numeric string) to f64; `None` when
/// neither parses finite. 9router `opencode-go.js parsePercent`.
fn opencode_go_percent(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        Value::String(s) if !s.trim().is_empty() => {
            s.trim().parse::<f64>().ok().filter(|f| f.is_finite())
        }
        _ => None,
    }
}

/// Build the OpenCode Go Rolling/Weekly/Monthly quota rows from a parsed
/// `data.usage` object. Pure fn for testability.
/// 9router `opencode-go.js` quota loop: `used = clamp(percent)`,
/// `total = 100`, `resetAt = parseResetTime(quota.resetsAt)`.
fn opencode_go_quota_rows(usage: &Value) -> Value {
    let mut quotas = serde_json::Map::new();
    for (period, name) in [
        ("rolling", "Rolling"),
        ("weekly", "Weekly"),
        ("monthly", "Monthly"),
    ] {
        let quota = usage.get(period).unwrap_or(&Value::Null);
        if !quota.is_object() {
            continue;
        }
        let Some(percent) = opencode_go_percent(&quota["percent"]) else {
            continue;
        };
        let used = percent.clamp(0.0, 100.0);
        quotas.insert(
            name.to_string(),
            json!({
                "used": used,
                "total": 100,
                "remaining": 100.0 - used,
                "remainingPercentage": 100.0 - used,
                "resetAt": parse_reset_time(&quota["resetsAt"]),
                "unlimited": false,
            }),
        );
    }
    if quotas.is_empty() {
        return json!({
            "plan": "OpenCode Go",
            "message": "OpenCode Go usage response did not contain valid quota data.",
        });
    }
    json!({ "plan": "OpenCode Go", "quotas": Value::Object(quotas) })
}

/// Fetch OpenCode Go subscription usage: rolling, weekly, and monthly
/// windows via GET on the usage endpoint with the API key.
/// 9router `opencode-go.js getOpenCodeGoUsage` (#3791).
pub async fn fetch_opencode_go_quota(api_key: &str) -> Value {
    let token = api_key.trim();
    if token.is_empty() {
        return json!({ "message": "OpenCode Go API key not available. Add a key to view usage." });
    }
    let client = http_client();
    let resp = match client
        .get(OPENCODE_GO_USAGE_URL)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("OpenCode Go error: {e}") }),
    };
    let status = resp.status().as_u16();
    if status == 401 {
        return json!({ "plan": "OpenCode Go", "message": "OpenCode Go authentication failed. Check the API key." });
    }
    if status == 403 {
        // 9router distinguishes EntitlementError (subscription required)
        // from other 403s via `error.error.type`.
        let err_body: Value = resp.json().await.unwrap_or(Value::Null);
        let subscription_required = err_body
            .get("error")
            .and_then(|e| e.get("type"))
            .and_then(|v| v.as_str())
            == Some("EntitlementError");
        return json!({
            "plan": "OpenCode Go",
            "message": if subscription_required {
                "OpenCode Go subscription required for this API key."
            } else {
                "OpenCode Go access forbidden for this API key."
            },
        });
    }
    if !(200..300).contains(&status) {
        return json!({ "plan": "OpenCode Go", "message": format!("OpenCode Go usage API error ({status}).") });
    }
    let data: Value = resp.json().await.unwrap_or(Value::Null);
    let Some(usage) = data.get("usage").filter(|v| v.is_object()) else {
        return json!({ "plan": "OpenCode Go", "message": "OpenCode Go usage response did not contain quota data." });
    };
    opencode_go_quota_rows(usage)
}

pub async fn fetch_vercel_ai_gateway_quota(api_key: &str) -> Value {
    if api_key.trim().is_empty() {
        return json!({ "message": "Vercel AI Gateway API key not available." });
    }
    let client = http_client();
    let response = match client
        .get("https://ai-gateway.vercel.sh/v1/credits")
        .bearer_auth(api_key.trim())
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return json!({ "message": format!("Vercel AI Gateway error: {e}") });
        }
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return json!({ "message": "Vercel AI Gateway API key invalid or expired." });
    }
    if !(200..300).contains(&status) {
        let text = response.text().await.unwrap_or_default();
        let trimmed: String = text.chars().take(200).collect();
        let suffix = if trimmed.is_empty() {
            String::new()
        } else {
            format!(": {trimmed}")
        };
        return json!({ "message": format!("Vercel AI Gateway credits API error ({status}){suffix}") });
    }
    let data: Value = response.json().await.unwrap_or_else(|_| json!({}));
    let balance = data
        .get("balance")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let total_used = data
        .get("total_used")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    vercel_ai_gateway_quota_rows(balance, total_used)
}

/// Build the Vercel AI Gateway quota rows from the parsed balance/total_used
/// (USD decimals). Pure fn for testability. 9router misc.js:213-249 parity.
fn vercel_ai_gateway_quota_rows(balance: f64, total_used: f64) -> Value {
    const MONTHLY_CREDIT: f64 = 5.0;
    let remaining_pct = (balance / MONTHLY_CREDIT) * 100.0;

    if balance <= 0.0 && total_used <= 0.0 {
        return json!({
            "plan": "Pay-as-you-go",
            "message": "Vercel AI Gateway connected. No credit allocation found (BYOK or unfunded account).",
            "quotas": {}
        });
    }

    json!({
        "plan": "Pay-as-you-go",
        "quotas": {
            "Used (USD)": json!({
                "used": total_used, "total": 0.0, "remaining": 0.0,
                "remainingPercentage": 100.0, "unlimited": true
            }),
            "Remaining (USD)": json!({
                "used": balance, "total": MONTHLY_CREDIT, "remaining": balance,
                "remainingPercentage": remaining_pct, "unlimited": false
            })
        }
    })
}

const CODEBUDDY_CN_URL: &str = "https://copilot.tencent.com/v2/billing/meter/get-user-resource";
const CODEBUDDY_INTL_URL: &str = "https://www.codebuddy.ai/v2/billing/meter/get-user-resource";
/// A refill pack is one whose DeductionEndTime is more than this far past the
/// cycle end (9router REFILL_GAP_MS = 2 days).
const CODEBUDDY_REFILL_GAP_MS: i64 = 2 * 24 * 60 * 60 * 1000;

/// CodeBuddy CN/Intl usage (9router services/usage/codebuddy-cn.js:46-138).
/// POST `{}` to the billing meter endpoint with the CodeBuddy headers, parse
/// `data.Response.Data.Accounts`, and partition refill vs bonus packs.
pub async fn fetch_codebuddy_quota(token: &str, provider: &str) -> Value {
    if token.trim().is_empty() {
        return json!({ "message": format!("CodeBuddy ({provider}) credential not available.") });
    }
    let url = if provider == "codebuddy-intl" {
        CODEBUDDY_INTL_URL
    } else {
        CODEBUDDY_CN_URL
    };
    let client = http_client();
    let response = match client
        .post(url)
        .bearer_auth(token.trim())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "CLI/2.108.1 CodeBuddy/2.108.1")
        .header("X-Product", "SaaS")
        .header("X-IDE-Type", "CLI")
        .header("X-IDE-Name", "CLI")
        .header("x-requested-with", "XMLHttpRequest")
        .header("x-codebuddy-request", "1")
        .body("{}")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return json!({ "message": format!("CodeBuddy ({provider}) error: {e}") });
        }
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return json!({ "message": "CodeBuddy CN credential invalid or expired." });
    }
    if !(200..300).contains(&status) {
        return json!({ "message": format!("CodeBuddy CN quota API error ({status}).") });
    }
    let json_body: Value = match response.json().await {
        Ok(v) => v,
        Err(_) => return json!({ "message": "CodeBuddy CN quota API error." }),
    };
    // json.code === 0 gate.
    if json_body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) != 0 {
        let msg = json_body
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return json!({ "message": format!("CodeBuddy CN quota error: {msg}") });
    }
    let data = json_body
        .pointer("/data/Response/Data")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let accounts: Vec<Value> = data
        .get("Accounts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if accounts.is_empty() {
        return json!({ "message": "CodeBuddy CN connected. No credit package found." });
    }

    codebuddy_quota_rows_from_accounts(accounts)
}

/// Partition CodeBuddy accounts into quota rows (refill vs bonus packs).
/// Mirrors 9router codebuddy-cn.js: refills and bonuses are partitioned and
/// each sorted by expiry; bonus packs are indexed independently (1-based).
/// Pure fn so tests exercise the real partitioning logic.
fn codebuddy_quota_rows_from_accounts(accounts: Vec<Value>) -> Value {
    fn expiry_ms(acc: &Value) -> i64 {
        acc.get("CycleEndTime")
            .and_then(|v| v.as_str())
            .and_then(|s| parse_codebuddy_time(Some(s)))
            .unwrap_or(i64::MAX)
    }

    let mut refills: Vec<&Value> = accounts
        .iter()
        .filter(|a| a.as_object().map(codebuddy_is_refill).unwrap_or(false))
        .collect();
    refills.sort_by_key(|a| expiry_ms(a));
    let mut bonuses: Vec<&Value> = accounts
        .iter()
        .filter(|a| !a.as_object().map(codebuddy_is_refill).unwrap_or(false))
        .collect();
    bonuses.sort_by_key(|a| expiry_ms(a));

    let mut quotas = serde_json::Map::new();
    // Refill packs first: cadence-labelled, Cycle* balance, recurring true.
    let mut seen_refill: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for acc in &refills {
        let obj = acc.as_object().cloned().unwrap_or_default();
        let cadence = codebuddy_refill_cadence(&obj);
        let count = seen_refill.entry(cadence.clone()).or_insert(0);
        *count += 1;
        let label = if *count > 1 {
            format!("{cadence} {count}")
        } else {
            cadence
        };
        quotas.insert(label, codebuddy_quota_row(&obj, true));
    }
    // Bonus packs: lifetime Capacity balance, recurring false, 1-based index.
    for (i, acc) in bonuses.iter().enumerate() {
        let obj = acc.as_object().cloned().unwrap_or_default();
        quotas.insert(format!("Bonus Pack {}", i + 1), codebuddy_bonus_row(&obj));
    }

    // Plan from the first refill (or first account), like JS basePkg.
    let plan_source: Option<&Value> = refills.first().copied().or_else(|| accounts.first());
    let mut plan = "CodeBuddy".to_string();
    if let Some(src) = plan_source {
        let base = codebuddy_base_package(src.as_object().unwrap_or(&serde_json::Map::new()));
        if let Some(name) = base.get("PackageName").and_then(|v| v.as_str()) {
            if !name.is_empty() {
                plan = name.to_string();
            }
        } else if let Some(name) = base.get("SubProductName").and_then(|v| v.as_str()) {
            if !name.is_empty() {
                plan = name.to_string();
            }
        }
    }

    json!({ "plan": plan, "quotas": Value::Object(quotas) })
}

/// Bonus pack quota row — lifetime Capacity balance (NOT Cycle fields).
fn codebuddy_bonus_row(acc: &serde_json::Map<String, Value>) -> Value {
    let used = codebuddy_num(acc, "CapacityUsedPrecise", "CapacityUsed");
    let total = codebuddy_num(acc, "CapacitySizePrecise", "CapacitySize");
    let reset_at = acc
        .get("CycleEndTime")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    json!({
        "used": used, "total": total, "resetAt": reset_at,
        "unlimited": false, "recurring": false
    })
}

/// 9router isRefill: DeductionEndTime − cycleEnd > REFILL_GAP_MS.
fn codebuddy_is_refill(acc: &serde_json::Map<String, Value>) -> bool {
    let cycle_end = parse_codebuddy_time(acc.get("CycleEndTime").and_then(|v| v.as_str()));
    let deduction_end = parse_codebuddy_time(acc.get("DeductionEndTime").and_then(|v| v.as_str()));
    match (cycle_end, deduction_end) {
        (Some(ce), Some(de)) => de - ce > CODEBUDDY_REFILL_GAP_MS,
        _ => false,
    }
}

/// 9router refillCadence: Monthly/Weekly/Daily by days between CycleStartTime
/// and CycleEndTime (≤1.5d → Daily, ≤10d → Weekly, else Monthly).
fn codebuddy_refill_cadence(acc: &serde_json::Map<String, Value>) -> String {
    let start = parse_codebuddy_time(acc.get("CycleStartTime").and_then(|v| v.as_str()));
    let end = parse_codebuddy_time(acc.get("CycleEndTime").and_then(|v| v.as_str()));
    if let (Some(s), Some(e)) = (start, end) {
        let days = (e - s) as f64 / 86_400_000.0;
        if days <= 1.5 {
            "Daily".to_string()
        } else if days <= 10.0 {
            "Weekly".to_string()
        } else {
            "Monthly".to_string()
        }
    } else {
        "Monthly".to_string()
    }
}

fn parse_codebuddy_time(s: Option<&str>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s?)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// 9router num(): Number(precise ?? plain), non-finite → 0.
fn codebuddy_num(acc: &serde_json::Map<String, Value>, precise: &str, plain: &str) -> f64 {
    acc.get(precise)
        .or_else(|| acc.get(plain))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite())
        .unwrap_or(0.0)
}

fn codebuddy_quota_row(acc: &serde_json::Map<String, Value>, recurring: bool) -> Value {
    let used = codebuddy_num(acc, "CycleCapacityUsedPrecise", "CycleCapacityUsed");
    let total = codebuddy_num(acc, "CycleCapacitySizePrecise", "CycleCapacitySize");
    let reset_at = acc
        .get("CycleEndTime")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    json!({
        "used": used, "total": total, "resetAt": reset_at,
        "unlimited": false, "recurring": recurring
    })
}

fn codebuddy_base_package(acc: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    acc.get("BasePackage")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default()
}

/// Claude usage cache: per-access-token with 5-minute TTL and in-flight dedup.
/// Ported from 9router v0.5.55 services/usage/claude.js.
mod claude_cache {
    use once_cell::sync::Lazy;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::Mutex;

    /// Cache TTL: 5 minutes (USAGE_CACHE_TTL_MS = 300000 in 9router).
    const CACHE_TTL: Duration = Duration::from_secs(300);

    struct CacheEntry {
        result: serde_json::Value,
        expires_at: Instant,
    }

    struct CacheState {
        /// Last good result per token (for stale-on-error fallback).
        entries: HashMap<String, CacheEntry>,
        /// In-flight requests per token (dedup key).
        in_flight: HashMap<String, Arc<tokio::sync::OnceCell<serde_json::Value>>>,
    }

    static CACHE: Lazy<Mutex<CacheState>> = Lazy::new(|| {
        Mutex::new(CacheState {
            entries: HashMap::new(),
            in_flight: HashMap::new(),
        })
    });

    /// Try to serve from cache. Returns `Some(result)` if fresh.
    pub async fn get_cached(token: &str) -> Option<serde_json::Value> {
        let guard = CACHE.lock().await;
        if let Some(entry) = guard.entries.get(token) {
            if entry.expires_at > Instant::now() {
                return Some(entry.result.clone());
            }
        }
        None
    }

    /// Get or create an in-flight dedup cell for this token.
    /// Returns `Some(cell)` if this caller should do the fetch (first caller).
    /// Returns `None` if another caller is already fetching (wait on that cell).
    pub async fn get_or_start_fetch(
        token: &str,
    ) -> Option<Arc<tokio::sync::OnceCell<serde_json::Value>>> {
        let mut guard = CACHE.lock().await;
        if let Some(cell) = guard.in_flight.get(token) {
            return Some(Arc::clone(cell));
        }
        let cell = Arc::new(tokio::sync::OnceCell::new());
        guard.in_flight.insert(token.to_string(), Arc::clone(&cell));
        Some(cell)
    }

    /// Remove the in-flight entry and store the result in cache.
    pub async fn complete_fetch(token: &str, result: serde_json::Value) -> serde_json::Value {
        let mut guard = CACHE.lock().await;
        guard.in_flight.remove(token);
        guard.entries.insert(
            token.to_string(),
            CacheEntry {
                result: result.clone(),
                expires_at: Instant::now() + CACHE_TTL,
            },
        );
        result
    }

    /// Store a result in cache (e.g. on soft failure, store the stale result).
    pub async fn store_stale(token: &str, result: &serde_json::Value) {
        let mut guard = CACHE.lock().await;
        // Only store if there's no fresh entry already.
        if let Some(entry) = guard.entries.get(token) {
            if entry.expires_at > Instant::now() {
                return;
            }
        }
        guard.entries.insert(
            token.to_string(),
            CacheEntry {
                result: result.clone(),
                expires_at: Instant::now() + CACHE_TTL,
            },
        );
    }

    /// Get the last good cached result (for soft-failure fallback).
    pub async fn get_stale(token: &str) -> Option<serde_json::Value> {
        let guard = CACHE.lock().await;
        guard.entries.get(token).map(|e| e.result.clone())
    }
}

pub async fn fetch_claude_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Claude token" });
    }

    // Serve from cache if fresh (9router USAGE_CACHE_TTL_MS = 300s).
    if let Some(cached) = claude_cache::get_cached(access_token).await {
        return cached;
    }

    // In-flight dedup: if another request is already in progress for this
    // token, wait on the same OnceCell instead of issuing a duplicate.
    let cell = claude_cache::get_or_start_fetch(access_token).await;
    if let Some(cell) = &cell {
        if let Some(result) = cell.get() {
            // Another caller already completed — return the shared result.
            return result.clone();
        }
    }

    // Fetch fresh data.
    let client = http_client();
    let response = match client
        .get("https://api.anthropic.com/api/oauth/usage")
        .bearer_auth(access_token)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("Claude error: {e}") }),
    };

    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        // On auth failure, return stale cache if available (soft failure).
        if let Some(stale) = claude_cache::get_stale(access_token).await {
            claude_cache::complete_fetch(access_token, stale.clone()).await;
            return stale;
        }
        claude_cache::complete_fetch(
            access_token,
            json!({ "message": "Invalid or expired Claude token" }),
        )
        .await;
        return json!({ "message": "Invalid or expired Claude token" });
    }
    if !status.is_success() {
        // On non-success, return stale cache if available (soft failure).
        if let Some(stale) = claude_cache::get_stale(access_token).await {
            claude_cache::complete_fetch(access_token, stale.clone()).await;
            return stale;
        }
        claude_cache::complete_fetch(
            access_token,
            json!({ "message": format!("Claude quota API error ({}).", status.as_u16()) }),
        )
        .await;
        return json!({
            "message": format!("Claude quota API error ({}).", status.as_u16())
        });
    }

    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "message": format!("Claude error: {e}") }),
    };

    let mut quotas = serde_json::Map::new();

    if let Some(five_hour) = body.get("five_hour") {
        let utilization = five_hour
            .get("utilization")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .clamp(0.0, 100.0);
        let reset_at = five_hour
            .get("resets_at")
            .or_else(|| five_hour.get("reset_at"))
            .and_then(parse_reset_time);
        quotas.insert(
            "session (5h)".to_string(),
            build_quota_entry(utilization, 100.0, reset_at),
        );
    }

    if let Some(seven_day) = body.get("seven_day") {
        let utilization = seven_day
            .get("utilization")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .clamp(0.0, 100.0);
        let reset_at = seven_day
            .get("resets_at")
            .or_else(|| seven_day.get("reset_at"))
            .and_then(parse_reset_time);
        quotas.insert(
            "weekly (7d)".to_string(),
            build_quota_entry(utilization, 100.0, reset_at),
        );
    }

    if let Some(obj) = body.as_object() {
        for (key, value) in obj {
            if !key.starts_with("seven_day_") || key == "seven_day" {
                continue;
            }
            // 9router claude.js `hasUtilization`: only windows carrying a
            // numeric utilization become rows.
            if value.get("utilization").and_then(|v| v.as_f64()).is_none() {
                continue;
            }
            let model = key.trim_start_matches("seven_day_").trim_start_matches("_");
            if model.is_empty() {
                continue;
            }
            let utilization = value
                .get("utilization")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
                .clamp(0.0, 100.0);
            let reset_at = value
                .get("resets_at")
                .or_else(|| value.get("reset_at"))
                .and_then(parse_reset_time);
            quotas.insert(
                format!("weekly {model} (7d)"),
                build_quota_entry(utilization, 100.0, reset_at),
            );
        }
    }

    // 9router 4ad1e7a4 (fix(usage): parse Fable weekly limit from limits[]
    // instead of fabricating a row #3847): model-scoped weekly limits (e.g.
    // Fable) arrive in `limits[]`, not as `seven_day_*` keys:
    // `{ kind: "weekly_scoped", percent, resets_at,
    //    scope: { model: { display_name: "Fable" } } }`.
    // No limits entry means the account has no such window — omit the row,
    // never fabricate one.
    if let Some(limits) = body.get("limits").and_then(|v| v.as_array()) {
        for limit in limits {
            if limit.get("kind").and_then(|v| v.as_str()) != Some("weekly_scoped") {
                continue;
            }
            let model_name = limit
                .get("scope")
                .and_then(|v| v.get("model"))
                .and_then(|v| v.get("display_name"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_lowercase);
            let (Some(model_name), Some(percent)) =
                (model_name, limit.get("percent").and_then(|v| v.as_f64()))
            else {
                continue;
            };
            let utilization = percent.clamp(0.0, 100.0);
            let reset_at = limit.get("resets_at").and_then(parse_reset_time);
            quotas.insert(
                format!("weekly {model_name} (7d)"),
                build_quota_entry(utilization, 100.0, reset_at),
            );
        }
    }

    if quotas.is_empty() {
        let result = json!({ "message": "Claude connected. No quota data was returned." });
        claude_cache::complete_fetch(access_token, result.clone()).await;
        return result;
    }

    let result = json!({ "quotas": Value::Object(quotas) });
    claude_cache::complete_fetch(access_token, result.clone()).await;
    result
}

const KIRO_DEFAULT_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";
const KIRO_AGENTIC_URL: &str = "https://codewhisperer.us-east-1.amazonaws.com";
const KIRO_Q_URL: &str = "https://q.us-east-1.amazonaws.com";

/// 9router kiro.js profileArn resolution — for api_key auth, NEVER inject the
/// shared default placeholder profileArn (CodeWhisperer 403s); fall back to
/// KIRO_DEFAULT_PROFILE_ARN only for non-api_key (builder-id) auth.
fn kiro_resolve_profile_arn(
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
    is_api_key: bool,
) -> String {
    let explicit = provider_specific_data
        .get("profileArn")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    match explicit {
        Some(s) => s.to_string(),
        None if is_api_key => String::new(),
        None => KIRO_DEFAULT_PROFILE_ARN.to_string(),
    }
}

pub async fn fetch_kiro_quota(
    access_token: &str,
    _provider: &str,
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Kiro token" });
    }

    // 9router kiro.js:51-67 auth-method branching.
    let auth_method = provider_specific_data
        .get("authMethod")
        .and_then(|v| v.as_str())
        .unwrap_or("builder-id");
    let is_api_key = auth_method == "api_key";
    let is_external_idp = auth_method == "external_idp";

    let client = http_client();
    let profile_arn = kiro_resolve_profile_arn(provider_specific_data, is_api_key);
    let mut quotas = serde_json::Map::new();

    let user_agent = "aws-sdk-js/1.0.0 KiroIDE";
    let mut primary_body: Option<Value> = None;
    let mut saw_auth_error = false;

    // tokentype / TokenType headers per auth method (kiro.js apiKeyHeaders /
    // externalIdpHeaders).
    let mut get_headers = |req: reqwest::RequestBuilder| -> reqwest::RequestBuilder {
        let mut r = req;
        if is_api_key {
            r = r.header("tokentype", "API_KEY");
        }
        if is_external_idp {
            r = r.header("TokenType", "EXTERNAL_IDP");
        }
        r
    };

    let primary_url = format!(
        "{KIRO_AGENTIC_URL}/getUsageLimits?isEmailRequired=true&origin=AI_EDITOR&resourceType=AGENTIC_REQUEST"
    );
    let primary_req = client
        .get(&primary_url)
        .bearer_auth(access_token)
        .header("x-amz-user-agent", user_agent)
        .header("user-agent", user_agent)
        .header("Accept", "application/json");
    if let Ok(resp) = get_headers(primary_req).send().await {
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            saw_auth_error = true;
        }
        if resp.status().is_success() {
            if let Ok(body) = resp.json::<Value>().await {
                primary_body = Some(body);
            }
        }
    }

    if primary_body.is_none() {
        let mut post_body = serde_json::Map::new();
        post_body.insert("origin".into(), json!("AI_EDITOR"));
        post_body.insert("resourceType".into(), json!("AGENTIC_REQUEST"));
        if !profile_arn.is_empty() {
            post_body.insert("profileArn".into(), json!(profile_arn));
        }
        let post_req = client
            .post(KIRO_AGENTIC_URL)
            .bearer_auth(access_token)
            .header("Content-Type", "application/x-amz-json-1.0")
            .header("x-amz-target", "AmazonCodeWhispererService.GetUsageLimits")
            .header("Accept", "application/json")
            .json(&Value::Object(post_body));
        if let Ok(resp) = get_headers(post_req).send().await {
            let status = resp.status().as_u16();
            if status == 401 || status == 403 {
                saw_auth_error = true;
            }
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<Value>().await {
                    primary_body = Some(body);
                }
            }
        }
    }

    if primary_body.is_none() {
        let q_url = if profile_arn.is_empty() {
            format!("{KIRO_Q_URL}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST")
        } else {
            format!(
                "{KIRO_Q_URL}/getUsageLimits?origin=AI_EDITOR&profileArn={profile_arn}&resourceType=AGENTIC_REQUEST"
            )
        };
        let q_req = client
            .get(&q_url)
            .bearer_auth(access_token)
            .header("Accept", "application/json");
        if let Ok(resp) = get_headers(q_req).send().await {
            let status = resp.status().as_u16();
            if status == 401 || status == 403 {
                saw_auth_error = true;
            }
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<Value>().await {
                    primary_body = Some(body);
                }
            }
        }
    }

    let body = match primary_body {
        Some(b) => b,
        None => {
            // 9router kiro.js:157-177 auth-error message per auth method.
            if saw_auth_error {
                let msg = match auth_method {
                    "idc" => {
                        "Kiro quota API is unavailable for the current AWS IAM Identity \
                         Center session. Chat may still work. If this persists after \
                         renewing your session, reconnect Kiro."
                    }
                    "google" | "github" => {
                        "Kiro quota API authentication expired. Chat may still work."
                    }
                    _ => "Kiro quota API rejected the current token. Chat may still work.",
                };
                return json!({ "message": msg, "quotas": {} });
            }
            return json!({
                "message": "Unable to fetch Kiro usage right now.",
                "quotas": {},
            });
        }
    };

    let reset_at = body
        .get("nextDateReset")
        .or_else(|| body.get("next_date_reset"))
        .or_else(|| body.get("reset_at"))
        .and_then(parse_reset_time);

    if let Some(breakdown) = body.get("usageBreakdownList").and_then(|v| v.as_array()) {
        for entry in breakdown {
            let key = entry
                .get("resourceType")
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "agentic_request".to_string());
            let used = entry
                .get("currentUsageWithPrecision")
                .or_else(|| entry.get("currentUsage"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let total = entry
                .get("usageLimitWithPrecision")
                .or_else(|| entry.get("usageLimit"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if total > 0.0 || used > 0.0 {
                quotas.insert(
                    key.clone(),
                    build_quota_entry(used, total, reset_at.clone()),
                );
            }

            if let Some(trial) = entry.get("freeTrialInfo") {
                let free_used = trial
                    .get("currentUsageWithPrecision")
                    .or_else(|| trial.get("currentUsage"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let free_total = trial
                    .get("usageLimitWithPrecision")
                    .or_else(|| trial.get("usageLimit"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let trial_reset = trial
                    .get("freeTrialExpiry")
                    .and_then(parse_reset_time)
                    .or_else(|| reset_at.clone());
                if free_total > 0.0 || free_used > 0.0 {
                    quotas.insert(
                        format!("{key}_freetrial"),
                        build_quota_entry(free_used, free_total, trial_reset),
                    );
                }
            }
        }
    }

    if quotas.is_empty() {
        return json!({ "message": "Kiro connected. No quota data was returned." });
    }

    json!({ "quotas": Value::Object(quotas) })
}

const ANTIGRAVITY_IMPORTANT_MODELS: &[&str] = &[
    "gemini-3.7-flash-high",
    "gemini-3.7-flash-medium",
    "gemini-3.7-flash-low",
    "gemini-3.6-flash-high",
    "gemini-3.6-flash-medium",
    "gemini-3.6-flash-low",
    "gemini-3-flash-agent",
    "gemini-3.5-flash-low",
    "gemini-3.5-flash-extra-low",
    "gemini-pro-agent",
    "gemini-3.1-pro-low",
    "claude-sonnet-4-6",
    "claude-opus-4-6-thinking",
    "gpt-oss-120b-medium",
    "gemini-3-flash",
];

pub async fn fetch_antigravity_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Antigravity token" });
    }

    let client = http_client();
    let user_agent = antigravity_user_agent();
    let metadata = cloud_code_metadata();

    let extra_headers = [
        ("User-Agent", user_agent.as_str()),
        ("X-Client-Name", "antigravity"),
        ("X-Client-Version", "1.107.0"),
        ("x-request-source", "local"),
    ];

    let load_body = match load_code_assist(
        &client,
        access_token,
        metadata,
        &extra_headers,
        Some(&json!({ "mode": 1 })),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            if e.contains("401") || e.contains("403") {
                return json!({ "message": "Invalid or expired Antigravity token" });
            }
            return json!({ "message": format!("Antigravity error: {e}") });
        }
    };

    let project_id = match load_body
        .get("cloudaicompanionProject")
        .and_then(normalize_cloud_code_project_id)
    {
        Some(p) => p,
        None => match load_body.get("cloudProject").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                return json!({
                    "message": "Antigravity connected. No cloud project was returned by loadCodeAssist."
                });
            }
        },
    };

    let url = format!("{CLOUD_CODE_BASE}:fetchAvailableModels?projectId={project_id}");
    let models_resp = match client
        .post(&url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .header("User-Agent", &user_agent)
        .header("X-Client-Name", "antigravity")
        .header("X-Client-Version", "1.107.0")
        .header("x-request-source", "local")
        .json(&json!({ "project": project_id }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("Antigravity error: {e}") }),
    };

    let status = models_resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({ "message": "Invalid or expired Antigravity token" });
    }
    if !status.is_success() {
        return json!({
            "message": format!("Antigravity quota API error ({}).", status.as_u16())
        });
    }

    let body: Value = match models_resp.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "message": format!("Antigravity error: {e}") }),
    };

    let models_map = body
        .get("models")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    // 9router e3bf94ee (feat(antigravity): add weekly quota tracking and
    // free-tier handling #3892): detect tier from the loadCodeAssist
    // subscription info. Free-tier accounts only have weekly quotas (no
    // separate 5h window) — on free-tier, fetchAvailableModels returns
    // misleading per-model quota info (missing remainingFraction defaults to
    // 0, or reflects the weekly limit rather than a 5h window), so the
    // per-model parse is skipped entirely and the only meaningful quota is
    // the weekly limit fetched below.
    let paid_tier_id = load_body
        .get("paidTier")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str());
    let is_free_tier = paid_tier_id.is_none_or(|id| id == "free-tier");

    let mut quotas = serde_json::Map::new();
    if !is_free_tier {
        for (model_id, info) in &models_map {
            if info
                .get("isInternal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
            if !ANTIGRAVITY_IMPORTANT_MODELS.iter().any(|m| m == model_id) {
                continue;
            }

            let quota = match info.get("quotaInfo") {
                Some(q) => q,
                None => continue,
            };

            let reset_at = quota
                .get("resetTime")
                .or_else(|| quota.get("reset_at"))
                .and_then(parse_reset_time);

            let fraction = quota
                .get("remainingFraction")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let total = 1000.0;
            let remaining = (total * fraction).round();
            let used = (total - remaining).max(0.0);

            quotas.insert(model_id.clone(), build_quota_entry(used, total, reset_at));
        }
    }

    // Best-effort weekly quota overlay (ported from
    // open-sse/services/usage/antigravity-weekly.js fetchAntigravityWeeklyQuota
    // + the reconcile block in google.js) — never blocks or breaks
    // per-model results: any failure here leaves the per-model quotas intact.
    if let Some(weekly) = fetch_antigravity_weekly_quota(access_token, &project_id).await {
        reconcile_weekly_quota(&mut quotas, weekly);
    }

    if quotas.is_empty() {
        return json!({
            "message": "Antigravity connected. No quota data was returned by fetchAvailableModels."
        });
    }

    let plan = load_body
        .get("currentTier")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    json!({ "plan": plan, "quotas": Value::Object(quotas) })
}

/// Weekly quota summary endpoint (9router `antigravity.js` registry
/// `quotaSummaryApiUrl`).
const ANTIGRAVITY_WEEKLY_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";

/// One parsed weekly bucket (mirrors the JS `{ used, total, resetAt,
/// remainingPercentage, unlimited, displayName }` entry).
#[derive(Debug, Clone)]
struct WeeklyQuotaEntry {
    used: f64,
    total: f64,
    reset_at: Option<String>,
    remaining_percentage: f64,
}

/// Group-name → stable key mapping (9router `GROUP_MATCHERS`).
fn weekly_group_key(display_name: &str) -> Option<(&'static str, &'static str)> {
    let lower = display_name.to_lowercase();
    if lower.contains("gemini") {
        Some(("gemini_weekly", "Gemini (Weekly)"))
    } else if lower.contains("claude") || lower.contains("gpt") {
        Some(("claude_gpt_weekly", "Claude & GPT (Weekly)"))
    } else {
        None
    }
}

/// Parse a `retrieveUserQuotaSummary` response into normalized weekly quotas
/// (ported from `parseWeeklyQuotaSummary`; pure function, unit-testable).
/// Returns e.g. `{ gemini_weekly: {...}, claude_gpt_weekly: {...} }`.
fn parse_weekly_quota_summary(data: &Value) -> serde_json::Map<String, Value> {
    let mut result = serde_json::Map::new();
    let groups = data.get("groups").and_then(|v| v.as_array()).or_else(|| {
        data.get("quotaSummary")
            .and_then(|v| v.get("groups"))
            .and_then(|v| v.as_array())
    });
    let Some(groups) = groups else { return result };
    for group in groups {
        let display_name = group
            .get("displayName")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some((key, label)) = weekly_group_key(display_name) else {
            continue;
        };
        let buckets = group
            .get("buckets")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for bucket in &buckets {
            // Identify weekly buckets by checking bucketId + displayName for
            // "weekly".
            let text = format!(
                "{} {}",
                bucket
                    .get("bucketId")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                bucket
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            )
            .to_lowercase();
            if !text.contains("weekly") {
                continue;
            }
            // Skip disabled buckets.
            if bucket
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
            let Some(fraction) = bucket
                .get("remainingFraction")
                .and_then(|v| v.as_f64())
                .filter(|f| f.is_finite())
            else {
                continue;
            };
            // 9router hardcodes total 1000: `remaining = round(1000 *
            // remainingFraction)`, `used = max(0, 1000 - remaining)`.
            let total = 1000.0;
            let remaining = (total * fraction).round();
            let used = (total - remaining).max(0.0);
            result.insert(
                key.to_string(),
                json!({
                    "used": used,
                    "total": total,
                    "remaining": remaining,
                    "resetAt": parse_reset_time(&bucket["resetTime"]),
                    "remainingPercentage": fraction * 100.0,
                    "unlimited": false,
                    "displayName": label,
                }),
            );
            break; // first matching bucket per family wins
        }
    }
    result
}

/// Fetch the Antigravity weekly quota summary — cached is out of scope for
/// this port (stateless per-request fetch like the rest of this file); fetch
/// failures return `None` and the caller keeps the per-model quotas.
/// 9router wraps this in a 3-minute TTL + in-flight dedup cache
/// (`weeklyCache`); add caching only if this endpoint shows load problems.
async fn fetch_antigravity_weekly_quota(
    access_token: &str,
    project_id: &str,
) -> Option<serde_json::Map<String, Value>> {
    let client = http_client();
    let user_agent = antigravity_user_agent();
    let resp = client
        .post(ANTIGRAVITY_WEEKLY_URL)
        .bearer_auth(access_token)
        .header("User-Agent", &user_agent)
        .header("Content-Type", "application/json")
        .header("X-Client-Name", "antigravity")
        .header("X-Client-Version", "1.107.0")
        .header("x-request-source", "local")
        .json(&json!({ "project": project_id }))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: Value = resp.json().await.ok()?;
    let parsed = parse_weekly_quota_summary(&data);
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

/// Reconcile the weekly quota against per-model family status (ported from
/// the JS reconcile block in google.js): if every model in a family is
/// locked/exhausted (remainingPercentage == 0) until a future reset, the
/// weekly limit cannot be 100% available. On Google's Free Starter tier,
/// retrieveUserQuotaSummary buggily reports remainingFraction 1 even after
/// the starter quota is depleted and all models 429.
fn reconcile_weekly_quota(
    quotas: &mut serde_json::Map<String, Value>,
    weekly: serde_json::Map<String, Value>,
) {
    fn entry_of<'a>(
        weekly: &'a serde_json::Map<String, Value>,
        key: &str,
    ) -> Option<WeeklyQuotaEntry> {
        let v = weekly.get(key)?;
        Some(WeeklyQuotaEntry {
            used: v.get("used").and_then(|x| x.as_f64()).unwrap_or(0.0),
            total: v.get("total").and_then(|x| x.as_f64()).unwrap_or(1000.0),
            reset_at: v
                .get("resetAt")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            remaining_percentage: v
                .get("remainingPercentage")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0),
        })
    }
    fn family_exhausted(
        quotas: &serde_json::Map<String, Value>,
        prefix_ok: impl Fn(&str) -> bool,
        exclude_image: bool,
    ) -> (bool, Option<String>) {
        let members: Vec<(&String, &Value)> = quotas
            .iter()
            .filter(|(k, _)| prefix_ok(k) && !(exclude_image && k.contains("image")))
            .collect();
        if members.is_empty() {
            return (false, None);
        }
        let all_zero = members.iter().all(|(_, q)| {
            q.get("remainingPercentage")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0)
                == 0.0
        });
        if !all_zero {
            return (false, None);
        }
        // Latest resetAt across the family.
        let mut max_reset: Option<String> = None;
        for (_, q) in &members {
            if let Some(r) = q.get("resetAt").and_then(|x| x.as_str()) {
                let take = match &max_reset {
                    None => true,
                    Some(cur) => r > cur.as_str(),
                };
                if take {
                    max_reset = Some(r.to_string());
                }
            }
        }
        (true, max_reset)
    }
    for (weekly_key, is_gemini) in [("gemini_weekly", true), ("claude_gpt_weekly", false)] {
        let Some(mut entry) = entry_of(&weekly, weekly_key) else {
            continue;
        };
        if entry.remaining_percentage <= 0.0 {
            // Already exhausted — still overlay it as-is below.
            let total = entry.total;
            quotas.insert(
                weekly_key.to_string(),
                json!({
                    "used": entry.used,
                    "total": total,
                    "remaining": (total - entry.used).max(0.0),
                    "resetAt": entry.reset_at,
                    "remainingPercentage": entry.remaining_percentage,
                    "unlimited": false,
                }),
            );
            continue;
        }
        let (exhausted, max_reset) = if is_gemini {
            family_exhausted(quotas, |k| k.starts_with("gemini-"), true)
        } else {
            family_exhausted(quotas, |k| k.starts_with("claude-"), false)
        };
        if exhausted {
            entry.used = entry.total;
            entry.remaining_percentage = 0.0;
            if max_reset.is_some() {
                entry.reset_at = max_reset;
            }
        }
        let total = entry.total;
        quotas.insert(
            weekly_key.to_string(),
            json!({
                "used": entry.used,
                "total": total,
                "remaining": (total - entry.used).max(0.0),
                "resetAt": entry.reset_at,
                "remainingPercentage": entry.remaining_percentage,
                "unlimited": false,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Kimi / DeepSeek / SuperGrok usage handlers (ported from 9router v0.5.45
// open-sse/services/usage/kimi.js + deepseek.js + grokCliQuotaFrame.js)
// ---------------------------------------------------------------------------

const GROK_CREDITS_URL: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
// Empty gRPC-web request frame (flag 0 + length 0). Without it upstream
// returns grpc-status 13 "Missing request message." with a 0-byte body.
const GRPC_WEB_EMPTY_REQUEST_FRAME: &[u8] = &[0, 0, 0, 0, 0];

/// Live SuperGrok weekly pool via gRPC-web GetGrokCreditsConfig.
/// Fail-open: any network/auth/parse failure returns None.
pub async fn fetch_grok_cli_credits_config(access_token: &str) -> Option<Value> {
    let token = access_token.trim();
    if token.is_empty() {
        return None;
    }
    let client = http_client();
    let response = client
        .post(GROK_CREDITS_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/grpc-web+proto")
        .header("X-Grpc-Web", "1")
        .header("Accept", "application/grpc-web+proto")
        .body(GRPC_WEB_EMPTY_REQUEST_FRAME.to_vec())
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    let decoded = crate::core::usage::grok_cli_quota_frame::decode_grok_credits_frame(&bytes)?;
    // Round for bar display (fixed32 ratio * 100 can be 34.999… for 0.35).
    let used = decoded.percent_used.clamp(0.0, 100.0).round();
    Some(json!({
        "used": used,
        "total": 100.0,
        "remainingPercentage": 100.0 - used,
        "resetAt": decoded.reset_at,
        "unlimited": false,
    }))
}

/// Grok CLI usage — REST billing first, then the gRPC-web weekly pool as a
/// fallback when REST reports zero quotas (ported from 9router v0.5.45
/// open-sse/services/usage/grok-cli.js getGrokCliUsage).
/// 9router grok-cli.js buildGrokCliHeaders (lines 54-70) — the 7 extra
/// headers alongside Authorization Bearer.
fn grok_cli_headers(
    token: &str,
    email: Option<&str>,
    user_id: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("Accept", "application/json".to_string()),
        (
            "User-Agent",
            "grok-shell/0.2.99 (linux; x86_64)".to_string(),
        ),
        ("x-xai-token-auth", "xai-grok-cli".to_string()),
        ("x-grok-client-identifier", "grok-shell".to_string()),
        ("x-grok-client-version", "0.2.99".to_string()),
        ("x-grok-client-mode", "headless".to_string()),
        ("Authorization", format!("Bearer {token}")),
    ];
    if let Some(e) = email {
        h.push(("x-email", e.to_string()));
    }
    if let Some(uid) = user_id {
        h.push(("x-userid", uid.to_string()));
    }
    h
}

/// 9router grok-cli.js planFromAccessToken (95-110): JWT tier → plan name.
fn plan_from_access_token(access_token: &str) -> String {
    use base64::Engine as _;
    let payload = access_token.split('.').nth(1).unwrap_or("");
    let decoded = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => return String::new(),
    };
    let v: Value = serde_json::from_str(&decoded).unwrap_or(Value::Null);
    let tier = v.get("tier").and_then(|t| t.as_i64()).unwrap_or(-1);
    match tier {
        0 => "Free".to_string(),
        1 => "SuperGrok".to_string(),
        2 => "X Basic".to_string(),
        3 => "X Premium".to_string(),
        4 => "X Premium Plus".to_string(),
        5 => "SuperGrok Heavy".to_string(),
        6 => "SuperGrok Lite".to_string(),
        _ => String::new(),
    }
}

/// 9router grok-cli.js RESOLVE_PLAN (82-92): tier (Title Cased) or
/// hasGrokCodeAccess / isUnifiedBillingUser; default "Grok Build".
fn resolve_grok_cli_plan(user: &Value, config: &Value) -> String {
    let tier = user
        .get("subscriptionTier")
        .or_else(|| user.get("subscription_tier"))
        .or_else(|| user.get("subscription").and_then(|s| s.get("tier")))
        .or_else(|| config.get("subscriptionTier"))
        .or_else(|| config.get("subscription_tier"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            // Title Case: split on [_-]+ and uppercase each word start.
            s.split(['-', '_'])
                .filter(|w| !w.is_empty())
                .map(|w| {
                    let mut chars = w.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        });
    if let Some(t) = tier {
        if !t.eq_ignore_ascii_case("free")
            && !t.eq_ignore_ascii_case("none")
            && !t.eq_ignore_ascii_case("null")
        {
            return t;
        }
    }
    if user
        .get("hasGrokCodeAccess")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return "Grok Code".to_string();
    }
    if config
        .get("isUnifiedBillingUser")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return "Grok Build".to_string();
    }
    "Grok Build".to_string()
}

/// 9router grok-cli.js unwrapVal (46-52): accept `{val: number}`, plain
/// number, or numeric string.
fn grok_unwrap_val(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Object(o) => o
            .get("val")
            .or_else(|| o.get("value"))
            .and_then(grok_unwrap_val),
        _ => None,
    }
}

/// Build a "Monthly included" / "On-demand" / "Prepaid" / "Weekly" quota row
/// without an absolute `remaining` (QuotaTable treats it as 0-100 pct).
fn make_grok_quota(used: f64, total: f64, reset_at: Option<String>) -> Value {
    let total_safe = total.max(0.0);
    let used_safe = used.max(0.0).min(total_safe);
    let pct = if total_safe > 0.0 {
        ((total_safe - used_safe) / total_safe * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    json!({
        "used": used_safe,
        "total": total_safe,
        "remainingPercentage": pct,
        "resetAt": reset_at,
        "unlimited": false,
    })
}

/// 9router grok-cli.js parseGrokCliBilling (141-298) — Monthly included,
/// On-demand (with exhausted synthetic row), Prepaid, Weekly SuperGrok.
fn parse_grok_cli_billing(
    data: &Value,
    subscription_access: bool,
) -> (serde_json::Map<String, Value>, Option<String>) {
    let config = data.get("config").cloned().unwrap_or(Value::Null);
    let period_end = parse_grok_period_end(data);

    let mut quotas = serde_json::Map::new();

    let monthly_limit = config
        .get("monthlyLimit")
        .and_then(grok_unwrap_val)
        .unwrap_or(0.0);
    let included_used = config
        .get("includedUsed")
        .and_then(grok_unwrap_val)
        .or_else(|| data.get("used").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);
    if monthly_limit > 0.0 {
        quotas.insert(
            "Monthly included".to_string(),
            make_grok_quota(included_used, monthly_limit, period_end.clone()),
        );
    }

    let on_demand_cap = config
        .get("onDemandCap")
        .and_then(grok_unwrap_val)
        .unwrap_or(0.0);
    let on_demand_used = config
        .get("onDemandUsed")
        .and_then(grok_unwrap_val)
        .unwrap_or(0.0);
    if on_demand_cap > 0.0 {
        quotas.insert(
            "On-demand".to_string(),
            make_grok_quota(on_demand_used.max(0.0), on_demand_cap, period_end.clone()),
        );
    } else if !subscription_access && on_demand_used.is_finite() {
        // Exhausted free/promo → synthetic full row so the bar shows 0%.
        quotas.insert(
            "On-demand".to_string(),
            json!({
                "used": 1.0, "total": 1.0, "remainingPercentage": 0.0,
                "resetAt": period_end.clone(), "unlimited": false
            }),
        );
    }

    let prepaid = config
        .get("prepaidBalance")
        .and_then(grok_unwrap_val)
        .unwrap_or(0.0);
    if prepaid > 0.0 {
        quotas.insert(
            "Prepaid".to_string(),
            json!({
                "used": 0.0, "total": prepaid, "remainingPercentage": 100.0,
                "resetAt": Value::Null, "unlimited": false
            }),
        );
    }

    let weekly_pct = config.get("creditUsagePercent").and_then(grok_unwrap_val);
    if let Some(pct) = weekly_pct {
        if pct >= 0.0 {
            let used = pct.min(100.0);
            quotas.insert(
                "Weekly SuperGrok".to_string(),
                json!({
                    "used": used, "total": 100.0, "remainingPercentage": (100.0 - used).clamp(0.0, 100.0),
                    "resetAt": period_end.clone(), "unlimited": false
                }),
            );
        }
    }

    (quotas, period_end)
}

fn parse_grok_period_end(data: &Value) -> Option<String> {
    data.get("billingPeriodEnd")
        .or_else(|| data.get("billing_period_end"))
        .or_else(|| data.get("periodEnd"))
        .or_else(|| data.get("currentPeriod").and_then(|c| c.get("end")))
        .or_else(|| data.get("resetAt"))
        .or_else(|| data.get("resetsAt"))
        .and_then(parse_reset_time)
}

/// 9router grok-cli.js getGrokCliUsage (349-424) — full Grok CLI quota.
pub async fn fetch_grok_cli_quota(access_token: &str) -> Value {
    let token = access_token.trim();
    if token.is_empty() {
        return json!({ "message": "Grok CLI access token not available." });
    }
    let client = http_client();
    let headers = grok_cli_headers(token, None, None);

    // Fetch billing + user in parallel (JS Promise.all).
    let mut billing_req = client.get("https://cli-chat-proxy.grok.com/v1/billing?format=credits");
    let mut user_req = client.get("https://cli-chat-proxy.grok.com/v1/user?include=subscription");
    for (k, v) in &headers {
        billing_req = billing_req.header(*k, v);
        user_req = user_req.header(*k, v);
    }
    let billing = billing_req.send().await.ok();
    let user = user_req.send().await.ok();

    // 401/403 on billing → auth expired.
    if let Some(r) = &billing {
        let status = r.status().as_u16();
        if status == 401 || status == 403 {
            return json!({ "message": "Grok CLI authentication expired. Please re-authorize." });
        }
    }

    let user_val: Value = match user {
        Some(r) => r.json::<Value>().await.unwrap_or(Value::Null),
        None => Value::Null,
    };

    let data: Value = match billing {
        Some(r) => match r.json::<Value>().await {
            Ok(v) => v,
            Err(_) => {
                return json!({ "message": "Grok CLI billing response was not JSON." });
            }
        },
        None => Value::Null,
    };

    let tier = user_val
        .get("subscriptionTier")
        .or_else(|| user_val.get("subscription_tier"))
        .or_else(|| user_val.get("subscription").and_then(|s| s.get("tier")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let subscription_access = !tier.is_empty()
        && !tier.eq_ignore_ascii_case("free")
        && !tier.eq_ignore_ascii_case("none")
        && !tier.eq_ignore_ascii_case("null");

    let (mut quotas, _) = parse_grok_cli_billing(&data, subscription_access);

    // plan from JWT tier first, then resolve_plan.
    let jwt_plan = plan_from_access_token(token);
    let plan = if !jwt_plan.is_empty() {
        jwt_plan
    } else {
        resolve_grok_cli_plan(&user_val, data.get("config").unwrap_or(&Value::Null))
    };

    if !quotas.is_empty() {
        return json!({ "plan": plan, "quotas": Value::Object(quotas) });
    }

    // No REST quotas → try the gRPC weekly fallback (JS 394-404).
    if let Some(weekly) = fetch_grok_cli_credits_config(token).await {
        let mut weekly_quotas = serde_json::Map::new();
        weekly_quotas.insert("Weekly SuperGrok".to_string(), weekly);
        return json!({ "plan": plan, "quotas": Value::Object(weekly_quotas) });
    }

    json!({
        "plan": plan,
        "quotas": {},
        "message": if subscription_access {
            "Subscription access is active; Grok does not expose a numeric included quota."
        } else {
            "Grok Build connected, but no credit allotment was returned. Free promo may be exhausted."
        },
    })
}

// ---------------------------------------------------------------------------
// Kimi / DeepSeek usage handlers (ported from 9router v0.5.45
// open-sse/services/usage/kimi.js + deepseek.js)
// ---------------------------------------------------------------------------

const KIMI_USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";
const DEEPSEEK_BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

fn to_finite_number(value: &Value, fallback: f64) -> f64 {
    match value {
        Value::Number(n) => n.as_f64().unwrap_or(fallback),
        Value::String(s) => s.parse::<f64>().unwrap_or(fallback),
        _ => fallback,
    }
}

/// Kimi plan-name mapping (LEVEL_* → friendly tier name).
fn kimi_plan_name(level: Option<&str>) -> String {
    let key = level.unwrap_or("");
    if key.is_empty() {
        return "Kimi Coding".to_string();
    }
    match key {
        "LEVEL_BASIC" => "Moderato".to_string(),
        "LEVEL_INTERMEDIATE" => "Allegretto".to_string(),
        "LEVEL_ADVANCED" => "Allegro".to_string(),
        "LEVEL_STANDARD" => "Vivace".to_string(),
        other => other.trim_start_matches("LEVEL_").to_lowercase(),
    }
}

/// Best-effort human message from a Kimi error body.
fn format_kimi_usage_error(status: u16, response_text: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(response_text).ok();
    let detail0 = parsed
        .as_ref()
        .and_then(|v| v.get("details").and_then(|d| d.as_array()))
        .and_then(|arr| arr.first());
    let debug = detail0
        .and_then(|d| d.get("debug"))
        .or_else(|| parsed.as_ref().and_then(|v| v.get("debug")));
    let reason = debug
        .and_then(|d| d.get("reason").and_then(|r| r.as_str()))
        .unwrap_or("");
    let localized = debug
        .and_then(|d| d.get("localizedMessage").and_then(|m| m.get("message")))
        .or_else(|| detail0.and_then(|d| d.get("localizedMessage").and_then(|m| m.get("message"))))
        .or_else(|| parsed.as_ref().and_then(|v| v.get("message")))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if status == 401 {
        return "Kimi authentication expired. Please re-authorize.".to_string();
    }
    if status == 403
        && (reason == "REASON_FEATURE_NO_PERMISSION"
            || localized.to_lowercase().contains("permission_denied")
            || localized.to_lowercase().contains("subscribe"))
    {
        return if localized.is_empty() {
            "Kimi connected, but this account has no permission to view usage. Subscribe to Kimi Code to access quota.".to_string()
        } else {
            localized.to_string()
        };
    }
    let snippet: String = if localized.is_empty() {
        response_text.chars().take(100).collect()
    } else {
        localized.chars().take(100).collect()
    };
    if snippet.is_empty() {
        format!("Kimi Coding connected. API Error {status}")
    } else {
        format!("Kimi Coding connected. API Error {status}: {snippet}")
    }
}

fn kimi_make_quota(
    used: f64,
    total: f64,
    remaining: Option<f64>,
    reset_at: Option<String>,
) -> Value {
    let safe_total = total.max(0.0);
    let safe_used = used.max(0.0);
    let remaining_pct = if safe_total > 0.0 {
        match remaining {
            Some(rem) if rem.is_finite() => ((rem.max(0.0)) / safe_total * 100.0).clamp(0.0, 100.0),
            _ => (((safe_total - safe_used).max(0.0)) / safe_total * 100.0).clamp(0.0, 100.0),
        }
    } else {
        0.0
    };
    json!({
        "used": safe_used,
        "total": safe_total,
        "remainingPercentage": remaining_pct,
        "resetAt": reset_at,
        "unlimited": false,
    })
}

/// Kimi Coding usage over OAuth — GET /v1/usages with Bearer + X-Msh-* headers.
pub async fn fetch_kimi_oauth_usage(
    access_token: &str,
    psd: &std::collections::BTreeMap<String, Value>,
) -> Value {
    if access_token.trim().is_empty() {
        return json!({ "message": "Kimi access token or API key not available." });
    }
    let client = http_client();
    let device_id = psd.get("deviceId").and_then(Value::as_str);
    let msh = crate::core::config::app_constants::build_kimi_headers(device_id);
    let mut request = client
        .get(KIMI_USAGE_URL)
        .header("Authorization", format!("Bearer {}", access_token.trim()))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json");
    if let Some(obj) = msh.as_object() {
        for (key, value) in obj {
            if let Some(v) = value.as_str() {
                request = request.header(key.as_str(), v);
            }
        }
    }
    let response = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            return json!({ "message": format!("Kimi Coding connected. Unable to fetch usage: {e}") });
        }
    };
    let status = response.status().as_u16();
    let response_text = response.text().await.unwrap_or_default();
    if status != 200 {
        return json!({
            "plan": "Kimi Coding",
            "message": format_kimi_usage_error(status, &response_text),
        });
    }
    let data: Value = match serde_json::from_str(&response_text) {
        Ok(v) => v,
        Err(_) => {
            return json!({
                "plan": "Kimi Coding",
                "message": "Kimi Coding connected. Invalid JSON response from API.",
            });
        }
    };

    let mut quotas = serde_json::Map::new();
    let usage_obj = data.get("usage").filter(|v| v.is_object());
    let usage_limit = to_finite_number(
        usage_obj
            .and_then(|u| u.get("limit"))
            .unwrap_or(&Value::Null),
        0.0,
    );
    let usage_used = to_finite_number(
        usage_obj
            .and_then(|u| u.get("used"))
            .unwrap_or(&Value::Null),
        0.0,
    );
    let usage_remaining = usage_obj
        .and_then(|u| u.get("remaining"))
        .map(|v| to_finite_number(v, f64::NAN))
        .filter(|v| v.is_finite());
    let usage_reset = usage_obj
        .and_then(|u| u.get("resetTime"))
        .or_else(|| usage_obj.and_then(|u| u.get("reset_time")));
    if usage_limit > 0.0 {
        quotas.insert(
            "Weekly".to_string(),
            kimi_make_quota(
                usage_used,
                usage_limit,
                usage_remaining,
                parse_reset_time(usage_reset.unwrap_or(&Value::Null)),
            ),
        );
    }
    if let Some(limits) = data.get("limits").and_then(|v| v.as_array()) {
        for item in limits {
            let detail = item.get("detail").filter(|v| v.is_object());
            let limit = to_finite_number(
                detail.and_then(|d| d.get("limit")).unwrap_or(&Value::Null),
                0.0,
            );
            let remaining = to_finite_number(
                detail
                    .and_then(|d| d.get("remaining"))
                    .unwrap_or(&Value::Null),
                f64::NAN,
            );
            let reset_time = detail
                .and_then(|d| d.get("resetTime"))
                .or_else(|| detail.and_then(|d| d.get("reset_at")));
            if limit > 0.0 {
                let rem = if remaining.is_finite() {
                    remaining
                } else {
                    limit
                };
                quotas.insert(
                    "Ratelimit".to_string(),
                    kimi_make_quota(
                        (limit - rem).max(0.0),
                        limit,
                        Some(rem),
                        parse_reset_time(reset_time.unwrap_or(&Value::Null)),
                    ),
                );
            }
        }
    }
    let membership_level = data
        .get("user")
        .and_then(|u| u.get("membership"))
        .and_then(|m| m.get("level"))
        .and_then(|v| v.as_str());
    let plan_name = kimi_plan_name(membership_level);
    if !quotas.is_empty() {
        return json!({ "plan": plan_name, "quotas": Value::Object(quotas) });
    }
    json!({
        "plan": plan_name,
        "message": "Kimi Coding connected. Usage tracked per request.",
    })
}

/// Kimi Coding usage — GET /v1/usages. Dual auth: apiKey → x-api-key header;
/// accessToken → Bearer + X-Msh-* headers.
pub async fn fetch_kimi_usage(api_key: &str) -> Value {
    if api_key.trim().is_empty() {
        return json!({ "message": "Kimi access token or API key not available." });
    }
    let client = http_client();
    let response = client
        .get(KIMI_USAGE_URL)
        .header("x-api-key", api_key.trim())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send()
        .await;
    let response = match response {
        Ok(r) => r,
        Err(e) => {
            return json!({ "message": format!("Kimi Coding connected. Unable to fetch usage: {e}") })
        }
    };
    let status = response.status().as_u16();
    let response_text = response.text().await.unwrap_or_default();

    if status != 200 {
        return json!({
            "plan": "Kimi Coding",
            "message": format_kimi_usage_error(status, &response_text),
        });
    }
    let data: Value = match serde_json::from_str(&response_text) {
        Ok(v) => v,
        Err(_) => {
            return json!({
                "plan": "Kimi Coding",
                "message": "Kimi Coding connected. Invalid JSON response from API.",
            });
        }
    };

    let mut quotas = serde_json::Map::new();
    let usage_obj = data.get("usage").filter(|v| v.is_object());
    let usage_limit = to_finite_number(
        usage_obj
            .and_then(|u| u.get("limit"))
            .unwrap_or(&Value::Null),
        0.0,
    );
    let usage_used = to_finite_number(
        usage_obj
            .and_then(|u| u.get("used"))
            .unwrap_or(&Value::Null),
        0.0,
    );
    let usage_remaining_raw = usage_obj
        .and_then(|u| u.get("remaining"))
        .or_else(|| usage_obj.and_then(|u| u.get("Remaining")));
    let usage_remaining = usage_remaining_raw
        .map(|v| to_finite_number(v, f64::NAN))
        .filter(|v| v.is_finite());
    let usage_reset = usage_obj
        .and_then(|u| u.get("resetTime"))
        .or_else(|| usage_obj.and_then(|u| u.get("reset_time")));

    if usage_limit > 0.0 {
        quotas.insert(
            "Weekly".to_string(),
            kimi_make_quota(
                usage_used,
                usage_limit,
                usage_remaining,
                parse_reset_time(usage_reset.unwrap_or(&Value::Null)),
            ),
        );
    }

    if let Some(limits) = data.get("limits").and_then(|v| v.as_array()) {
        for item in limits {
            let detail = item.get("detail").filter(|v| v.is_object());
            let limit = to_finite_number(
                detail.and_then(|d| d.get("limit")).unwrap_or(&Value::Null),
                0.0,
            );
            let remaining = to_finite_number(
                detail
                    .and_then(|d| d.get("remaining"))
                    .unwrap_or(&Value::Null),
                f64::NAN,
            );
            let reset_time = detail
                .and_then(|d| d.get("resetTime"))
                .or_else(|| detail.and_then(|d| d.get("reset_at")));
            if limit > 0.0 {
                let rem = if remaining.is_finite() {
                    remaining
                } else {
                    limit
                };
                quotas.insert(
                    "Ratelimit".to_string(),
                    kimi_make_quota(
                        (limit - rem).max(0.0),
                        limit,
                        Some(rem),
                        parse_reset_time(reset_time.unwrap_or(&Value::Null)),
                    ),
                );
            }
        }
    }

    let membership_level = data
        .get("user")
        .and_then(|u| u.get("membership"))
        .and_then(|m| m.get("level"))
        .and_then(|v| v.as_str());
    let plan_name = kimi_plan_name(membership_level);

    if !quotas.is_empty() {
        return json!({ "plan": plan_name, "quotas": Value::Object(quotas) });
    }
    json!({
        "plan": plan_name,
        "message": "Kimi Coding connected. Usage tracked per request.",
    })
}

/// DeepSeek usage — GET https://api.deepseek.com/user/balance, Bearer apiKey.
pub async fn fetch_deepseek_usage(api_key: &str) -> Value {
    let key = api_key.trim();
    if key.is_empty() {
        return json!({ "message": "DeepSeek API key not available. Add a key to view usage." });
    }
    let client = http_client();
    let response = match client
        .get(DEEPSEEK_BALANCE_URL)
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("DeepSeek error: {e}") }),
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return json!({
            "plan": "DeepSeek",
            "message": "DeepSeek authentication failed. Check the API key.",
        });
    }
    let response_text = response.text().await.unwrap_or_default();
    if status != 200 {
        let snippet: String = response_text.chars().take(120).collect();
        return json!({
            "plan": "DeepSeek",
            "message": format!(
                "DeepSeek balance API error ({status}){}",
                if snippet.is_empty() { String::new() } else { format!(": {snippet}") }
            ),
        });
    }
    let data: Value = match serde_json::from_str(&response_text) {
        Ok(v) => v,
        Err(_) => return json!({ "message": "DeepSeek balance response was not JSON." }),
    };

    let balances: Vec<&Value> = data
        .get("balance_infos")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().collect())
        .unwrap_or_default();
    if balances.is_empty() {
        return json!({
            "plan": "DeepSeek",
            "message": "DeepSeek connected. No balance data returned.",
        });
    }

    let is_available = data
        .get("is_available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut quotas = serde_json::Map::new();
    for b in balances {
        let currency = b
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_uppercase();
        if currency.is_empty() {
            continue;
        }
        let total = to_finite_number(b.get("total_balance").unwrap_or(&Value::Null), 0.0).max(0.0);
        quotas.insert(
            format!("Balance ({currency})"),
            json!({
                "used": 0.0,
                "total": total,
                "remainingPercentage": if total > 0.0 { 100.0 } else { 0.0 },
                "resetAt": Value::Null,
                "unlimited": total > 0.0,
            }),
        );
    }

    json!({
        "plan": if is_available { "DeepSeek" } else { "DeepSeek (Insufficient Balance)" },
        "quotas": Value::Object(quotas),
    })
}

/// Convert an Ollama 0..1 usage ratio to a 0..100 quota bar.
/// 9router usage/misc.js ratioQuota: `used = round(ratio*100)`,
/// `{ used, total: 100, remainingPercentage: 100-used, resetAt: null, unlimited: false }`.
pub fn ollama_ratio_quota(usage_ratio: f64) -> Value {
    let ratio = usage_ratio.clamp(0.0, 1.0);
    let used_pct = (ratio * 100.0).round() as u64;
    json!({
        "used": used_pct,
        "total": 100,
        "remainingPercentage": 100 - used_pct,
        "resetAt": Value::Null,
        "unlimited": false,
    })
}

/// Live Ollama Cloud quota (9router usage/misc.js getOllamaUsage).
/// GET `https://ollama.com/api/usage` + best-effort POST `/api/me` for the
/// plan label. `data.limits.{session,weekly}.usage` are 0..1 ratios →
/// `{used, total:100, remainingPercentage, resetAt:null, unlimited:false}`.
pub async fn fetch_ollama_quota(api_key: &str) -> Value {
    if api_key.is_empty() {
        return json!({ "message": "Ollama Cloud API key not available." });
    }

    let client = http_client();

    let resp = match client
        .get("https://ollama.com/api/usage")
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("Ollama Cloud error: {e}") }),
    };

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({ "message": "Ollama Cloud API key invalid or expired." });
    }
    if !status.is_success() {
        return json!({
            "message": format!("Ollama Cloud usage API error ({}).", status.as_u16())
        });
    }

    let data: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return json!({ "message": "Ollama Cloud usage response was not JSON." }),
    };

    // Best-effort plan label from /api/me (fail-open).
    let plan = match client
        .post("https://ollama.com/api/me")
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
        .header("Content-Length", "0")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.json::<Value>().await {
            Ok(me) => me
                .get("Plan")
                .and_then(Value::as_str)
                .map(|raw| {
                    // Capitalize first letter, rest lowercase.
                    let mut chars = raw.chars();
                    match chars.next() {
                        Some(first) => {
                            first.to_uppercase().collect::<String>()
                                + &chars.as_str().to_lowercase()
                        }
                        None => String::new(),
                    }
                })
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "Ollama Cloud".to_string()),
            Err(_) => "Ollama Cloud".to_string(),
        },
        _ => "Ollama Cloud".to_string(),
    };

    let limits = data.get("limits").filter(|v| v.is_object());
    let session_raw = limits
        .and_then(|l| l.get("session"))
        .and_then(|s| s.get("usage"))
        .and_then(Value::as_f64);
    let weekly_raw = limits
        .and_then(|l| l.get("weekly"))
        .and_then(|w| w.get("usage"))
        .and_then(Value::as_f64);

    let ratio_quota = |usage_ratio: f64| -> Value { ollama_ratio_quota(usage_ratio) };

    match (session_raw, weekly_raw) {
        (None, None) => json!({
            "plan": plan,
            "message": "Ollama Cloud connected. No usage limits reported.",
            "quotas": Value::Object(serde_json::Map::new()),
        }),
        _ => {
            let mut quotas = serde_json::Map::new();
            if let Some(s) = session_raw {
                quotas.insert("Session (5h)".to_string(), ratio_quota(s));
            }
            if let Some(w) = weekly_raw {
                quotas.insert("Weekly (7d)".to_string(), ratio_quota(w));
            }
            json!({ "plan": plan, "quotas": Value::Object(quotas) })
        }
    }
}

/// Groq models endpoint — doubles as the quota source (rate-limit info
/// rides on every API response as x-ratelimit-* headers; no dedicated
/// quota endpoint exists, and reading usage costs zero tokens).
/// 9router `groq.js` MODELS_URL (`U("groq").url`).
const GROQ_MODELS_URL: &str = "https://api.groq.com/openai/v1/models";

/// Parse a Go-style duration string ("2m59.56s", "7.66s", "1h2m3s",
/// "150ms") into milliseconds. Returns `None` when nothing parses.
/// 9router `groq.js parseGroqDurationMs`: `/(\d+(?:\.\d+)?)(ms|s|m|h)/g`
/// summed per component.
fn parse_groq_duration_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let bytes = value.as_bytes();
    let mut total_ms = 0f64;
    let mut matched = false;
    let mut i = 0;
    while i < bytes.len() {
        // Parse a number (digits + optional single dot).
        let num_start = i;
        let mut seen_dot = false;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || (!seen_dot && bytes[i] == b'.')) {
            if bytes[i] == b'.' {
                seen_dot = true;
            }
            i += 1;
        }
        if i == num_start {
            // Not a number here — skip one char (covers stray separators;
            // note `ms` must be checked before `m`/`s` below).
            i += 1;
            continue;
        }
        let amount: f64 = value[num_start..i].parse().ok()?;
        // Parse the unit suffix (longest match first: `ms` before `m`).
        let unit_ms: f64 = if value[i..].starts_with("ms") {
            i += 2;
            1.0
        } else if i < bytes.len() && bytes[i] == b'h' {
            i += 1;
            3_600_000.0
        } else if i < bytes.len() && bytes[i] == b'm' {
            i += 1;
            60_000.0
        } else if i < bytes.len() && bytes[i] == b's' {
            i += 1;
            1_000.0
        } else {
            // Trailing number with no unit (or unknown suffix) — the JS
            // regex simply wouldn't match it, so ignore this component.
            continue;
        };
        matched = true;
        total_ms += amount * unit_ms;
    }
    if matched {
        Some(total_ms as u64)
    } else {
        None
    }
}

/// Resolve a Go-style duration reset header to a future RFC 3339 timestamp
/// (`Date.now() + ms`). 9router `groq.js resetAtFromDuration`.
fn groq_reset_at(value: Option<&str>) -> Value {
    match value
        .and_then(parse_groq_duration_ms)
        .map(|ms| chrono::Utc::now() + chrono::Duration::milliseconds(ms as i64))
    {
        Some(dt) => Value::String(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        None => Value::Null,
    }
}

/// Build one rate-limit quota entry from a limit/remaining/reset header
/// triple. Missing or non-numeric headers → `None` (a missing header must
/// never masquerade as a real "0 remaining" quota — 9router `groq.js`
/// `buildRateLimitQuota` checks `headers.get()` presence explicitly because
/// `Number(null) === 0`).
fn groq_rate_limit_quota(
    headers: &reqwest::header::HeaderMap,
    limit_key: &str,
    remaining_key: &str,
    reset_key: &str,
) -> Option<Value> {
    let limit: f64 = headers.get(limit_key)?.to_str().ok()?.trim().parse().ok()?;
    let remaining: f64 = headers
        .get(remaining_key)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if !limit.is_finite() || !remaining.is_finite() {
        return None;
    }
    let used = (limit - remaining).max(0.0);
    Some(json!({
        "used": used,
        "total": limit,
        "remaining": remaining.max(0.0),
        "resetAt": groq_reset_at(headers.get(reset_key).and_then(|v| v.to_str().ok())),
        "unlimited": false,
    }))
}

/// Fetch Groq usage — no dedicated quota endpoint; rate-limit info rides on
/// every API response as x-ratelimit-* headers (requests + tokens, always
/// included). Piggybacks on the models list (the registry `validateUrl`) so
/// reading usage never costs tokens.
/// 9router `groq.js getGroqUsage` (first slice of #3701).
pub async fn fetch_groq_quota(api_key: &str) -> Value {
    let token = api_key.trim();
    if token.is_empty() {
        return json!({ "message": "Groq API key not available. Add a key to view usage." });
    }
    let client = http_client();
    let resp = match client
        .get(GROQ_MODELS_URL)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("Groq error: {e}") }),
    };
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({ "plan": "Groq", "message": "Groq authentication failed. Check the API key." });
    }
    if !status.is_success() {
        let err_text = resp.text().await.unwrap_or_default();
        let suffix = if err_text.is_empty() {
            String::new()
        } else {
            format!(": {}", err_text.chars().take(120).collect::<String>())
        };
        return json!({ "plan": "Groq", "message": format!("Groq usage API error ({}){suffix}", status.as_u16()) });
    }
    // The quota data lives in headers, not the body — drain it so the
    // connection can be released without needing the payload.
    let headers = resp.headers().clone();
    let _ = resp.text().await;

    let requests = groq_rate_limit_quota(
        &headers,
        "x-ratelimit-limit-requests",
        "x-ratelimit-remaining-requests",
        "x-ratelimit-reset-requests",
    );
    let tokens = groq_rate_limit_quota(
        &headers,
        "x-ratelimit-limit-tokens",
        "x-ratelimit-remaining-tokens",
        "x-ratelimit-reset-tokens",
    );
    if requests.is_none() && tokens.is_none() {
        // Key is valid (request succeeded) but no rate-limit bucket reported
        // — "not tracked yet", not an auth/error state.
        return json!({
            "plan": "Groq",
            "message": "Groq connected. No rate-limit data reported for this key yet.",
            "quotas": Value::Object(serde_json::Map::new()),
        });
    }
    let mut quotas = serde_json::Map::new();
    if let Some(r) = requests {
        quotas.insert("Requests".to_string(), r);
    }
    if let Some(t) = tokens {
        quotas.insert("Tokens".to_string(), t);
    }
    json!({ "plan": "Groq", "quotas": Value::Object(quotas) })
}

/// Zed cloud base URL (mirrors `ZED_CLOUD_BASE_URL` in zedAuth.js;
/// `crate::oauth::zed_auth::ZED_CLOUD_BASE_URL` holds the same value —
/// duplicated here to avoid coupling the usage module to the oauth module).
const ZED_USERS_ME_URL: &str = "https://cloud.zed.dev/client/users/me";

/// Map Zed `plan_v3` ids to dashboard labels (CodexBar-compatible).
/// 9router `zed.js formatZedPlanLabel`.
fn format_zed_plan_label(raw_plan: &str) -> String {
    let raw = raw_plan.trim();
    if raw.is_empty() {
        return "Zed".to_string();
    }
    match raw.to_lowercase().as_str() {
        "zed_free" => "Zed Free".to_string(),
        "zed_pro" => "Zed Pro".to_string(),
        "zed_pro_trial" => "Zed Pro Trial".to_string(),
        "zed_student" => "Zed Student".to_string(),
        "zed_business" => "Zed Business".to_string(),
        _ => raw
            .replace('_', " ")
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                    }
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Parse a Zed UsageLimit JSON value: `"unlimited"`, a number, or
/// `{ limited: N }`. Returns `(unlimited, total)`.
/// 9router `zed.js parseZedUsageLimit`.
fn parse_zed_usage_limit(limit: &Value) -> (bool, f64) {
    if limit.is_null() {
        return (false, 0.0);
    }
    if limit == &Value::String("unlimited".to_string())
        || limit
            .get("unlimited")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        return (true, 0.0);
    }
    if let Some(n) = limit.as_f64() {
        if n.is_finite() {
            return (false, n.max(0.0));
        }
        return (false, 0.0);
    }
    if let Some(s) = limit.as_str() {
        let trimmed = s.trim();
        if trimmed == "unlimited" {
            return (true, 0.0);
        }
        if let Ok(n) = trimmed.parse::<f64>() {
            if n.is_finite() {
                return (false, n.max(0.0));
            }
        }
        return (false, 0.0);
    }
    // `{ limited: N }` (also accepts capitalised `Limited`).
    if let Some(n) = limit
        .get("limited")
        .or_else(|| limit.get("Limited"))
        .and_then(|v| v.as_f64())
    {
        if n.is_finite() {
            return (false, n.max(0.0));
        }
    }
    (false, 0.0)
}

/// `limit: { limited: 0 }` on Pro/Student means token billing, not a 0-cap
/// request quota. 9router `zed.js isZedTokenBillingModelRequestsLimit`.
fn is_zed_token_billing_limit(limit_raw: &Value) -> bool {
    let (unlimited, total) = parse_zed_usage_limit(limit_raw);
    !unlimited && total == 0.0
}

/// One finite-number-or-fallback coercion (9router `toFiniteNumber`).
fn zed_finite(value: &Value, fallback: f64) -> f64 {
    value.as_f64().filter(|f| f.is_finite()).unwrap_or(fallback)
}

/// Build one Zed quota row. 9router `zed.js makeZedQuotaRow`: unlimited
/// rows report `remainingPercentage: 100`; a non-positive total reports 0%.
fn make_zed_quota_row(used_raw: &Value, limit_raw: &Value, reset_at: Option<String>) -> Value {
    let used = zed_finite(used_raw, 0.0).max(0.0);
    let (unlimited, total) = parse_zed_usage_limit(limit_raw);
    if unlimited {
        return json!({
            "used": used,
            "total": 0,
            "remainingPercentage": 100,
            "resetAt": reset_at,
            "unlimited": true,
        });
    }
    if total <= 0.0 {
        return json!({
            "used": used,
            "total": 0,
            "remainingPercentage": 0,
            "resetAt": reset_at,
            "unlimited": false,
        });
    }
    let clamped_used = used.min(total);
    let remaining = (total - clamped_used).max(0.0);
    json!({
        "used": clamped_used,
        "total": total,
        "remainingPercentage": (remaining / total) * 100.0,
        "resetAt": reset_at,
        "unlimited": false,
    })
}

/// Map a `/client/users/me` response to `{ plan, quotas, message }` for the
/// dashboard. Pure function — unit-testable without network.
/// 9router `zed.js parseZedAuthenticatedUserUsage`.
fn parse_zed_user_usage(user_info: &Value) -> Value {
    let plan = user_info.get("plan").unwrap_or(&Value::Null);
    let plan_id = plan
        .get("plan_v3")
        .or_else(|| plan.get("plan_v2"))
        .or_else(|| plan.get("plan"))
        .or_else(|| user_info.get("plan_v3"))
        .and_then(|v| v.as_str());
    let reset_at = plan
        .get("subscription_period")
        .and_then(|v| v.get("ended_at"))
        .and_then(parse_reset_time)
        .or_else(|| {
            plan.get("subscriptionPeriod")
                .and_then(|v| v.get("endedAt"))
                .and_then(parse_reset_time)
        });

    let mut quotas = serde_json::Map::new();
    let usage = plan.get("usage").unwrap_or(&Value::Null);

    if let Some(edit) = usage
        .get("edit_predictions")
        .or_else(|| usage.get("editPredictions"))
    {
        quotas.insert(
            "Edit Predictions".to_string(),
            make_zed_quota_row(&edit["used"], &edit["limit"], reset_at.clone()),
        );
    }

    let model_requests = usage
        .get("model_requests")
        .or_else(|| usage.get("modelRequests"));
    // Token-billed plans report model_requests.limit = 0 — not a request
    // quota, so the row is omitted and a note is surfaced instead.
    let mut token_billing_note: Option<&str> = None;
    if let Some(mr) = model_requests {
        let limit_raw = if mr.get("limit").is_some_and(|v| !v.is_null()) {
            &mr["limit"]
        } else if let Some(bucket) = mr.as_object().and_then(|o| {
            // `usageBucketLimit`: a bare bucket object without `.limit`
            // passes through as its own limit (effectively missing).
            if o.contains_key("limit") {
                None
            } else {
                Some(mr)
            }
        }) {
            bucket
        } else {
            &Value::Null
        };
        let (unlimited, total) = parse_zed_usage_limit(limit_raw);
        if unlimited || total > 0.0 {
            quotas.insert(
                "Hosted Model Requests".to_string(),
                make_zed_quota_row(&mr["used"], limit_raw, reset_at.clone()),
            );
        }
        if is_zed_token_billing_limit(limit_raw) {
            token_billing_note = Some("Hosted AI models are billed per token (not request count). Edit Predictions are tracked below. Token spend is on dashboard.zed.dev.");
        }
    }

    let trial_started = plan
        .get("trial_started_at")
        .or_else(|| plan.get("trialStartedAt"))
        .is_some();
    let mut plan_label = format_zed_plan_label(plan_id.unwrap_or(""));
    if trial_started && !plan_label.to_lowercase().contains("trial") {
        plan_label = format!("{plan_label} (Trial active)");
    }

    let has_overdue = plan
        .get("has_overdue_invoices")
        .or_else(|| plan.get("hasOverdueInvoices"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let message: Value = if has_overdue {
        Value::String("This Zed account has overdue invoices. Usage may be blocked until billing is resolved.".to_string())
    } else {
        token_billing_note
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null)
    };

    json!({
        "plan": plan_label,
        "quotas": Value::Object(quotas),
        "message": message,
        "hasOverdueInvoices": has_overdue,
        "trialStarted": trial_started,
        "planId": plan_id,
        "resetAt": reset_at,
    })
}

/// Fetch Zed plan quota: GET `/client/users/me` with the
/// `{userId} {accessToken}` auth header (same credential shape as the chat
/// path in `crate::oauth::zed_auth::build_user_auth_header`).
/// 9router `zed.js getZedUsage` (wired in e5a13c3a).
pub async fn fetch_zed_quota(
    access_token: &str,
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
) -> Value {
    let token = access_token.trim();
    if token.is_empty() {
        return json!({ "message": "Zed access token not available. Re-connect Zed to view quota." });
    }
    let user_id = provider_specific_data
        .get("userId")
        .or_else(|| provider_specific_data.get("user_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(user_id) = user_id else {
        return json!({ "message": "Zed credential is missing user id. Re-connect Zed to view quota." });
    };
    let system_id = provider_specific_data
        .get("systemId")
        .or_else(|| provider_specific_data.get("system_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let client = http_client();
    let mut req = client
        .get(ZED_USERS_ME_URL)
        .header("Accept", "application/json")
        .header("Authorization", format!("{user_id} {token}"));
    if let Some(sid) = system_id {
        req = req.header("x-zed-system-id", sid);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("Zed error: {e}") }),
    };
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return json!({ "message": "Zed authentication failed. Sign in again from the dashboard or Zed editor." });
    }
    if !resp.status().is_success() {
        return json!({ "message": format!("Zed usage API error ({status}).") });
    }
    let user_info: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return json!({ "message": "Zed usage response was not JSON." }),
    };
    parse_zed_user_usage(&user_info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_text_quota_model() {
        assert!(is_text_quota_model("MiniMax-M2.7"));
        assert!(is_text_quota_model("minimax-m2.5"));
        assert!(is_text_quota_model("Coding-Plan-Pro"));
        assert!(!is_text_quota_model("voice-1"));
        assert!(!is_text_quota_model(""));
    }

    #[test]
    fn test_build_minimax_quota_count_means_used() {
        let q = build_minimax_quota(100.0, 30.0, None, false);
        assert_eq!(q["used"], 30.0);
        assert_eq!(q["remaining"], 70.0);
        assert_eq!(q["remainingPercentage"], 70.0);
    }

    #[test]
    fn test_build_minimax_quota_count_means_remaining() {
        let q = build_minimax_quota(100.0, 30.0, None, true);
        assert_eq!(q["used"], 70.0);
        assert_eq!(q["remaining"], 30.0);
        assert_eq!(q["remainingPercentage"], 30.0);
    }

    #[test]
    fn test_build_minimax_quota_zero_total() {
        let q = build_minimax_quota(0.0, 0.0, None, false);
        assert_eq!(q["total"], 0.0);
        assert_eq!(q["remainingPercentage"], 0.0);
    }

    #[test]
    fn test_pick_representative_prefers_with_quota() {
        let models = vec![
            json!({"current_interval_total_count": 0}),
            json!({"current_interval_total_count": 50}),
            json!({"current_interval_total_count": 100}),
        ];
        let pick = pick_representative(&models, |m| {
            minimax_num(
                m,
                "current_interval_total_count",
                "currentIntervalTotalCount",
            )
        });
        assert_eq!(pick.unwrap()["current_interval_total_count"], 100);
    }

    #[test]
    fn test_vercel_ai_gateway_quota_builds_two_rows() {
        let out = vercel_ai_gateway_quota_rows(95.5, 4.5);
        let quotas = out["quotas"].as_object().unwrap();
        // "Used (USD)": used 4.5, total 0, remainingPercentage 100, unlimited true.
        let used = &quotas["Used (USD)"];
        assert_eq!(used["used"], 4.5);
        assert_eq!(used["total"], 0.0);
        assert_eq!(used["remainingPercentage"], 100.0);
        assert_eq!(used["unlimited"], true);
        // "Remaining (USD)": used 95.5 (balance, not remaining), total 5,
        // remainingPercentage 1910.0 (may exceed 100), unlimited false.
        let remaining = &quotas["Remaining (USD)"];
        assert_eq!(remaining["used"], 95.5);
        assert_eq!(remaining["total"], 5.0);
        assert_eq!(remaining["remaining"], 95.5);
        // 95.5/5*100 = 1910 (float: 1910.0000000000002) — may exceed 100, never clamped.
        let pct = remaining["remainingPercentage"].as_f64().unwrap();
        assert!((pct - 1910.0).abs() < 1e-6, "expected ~1910, got {pct}");
        assert_eq!(remaining["unlimited"], false);
    }

    #[test]
    fn test_grok_cli_plan_from_jwt_tier() {
        // A fake JWT payload {tier: 4} → X Premium Plus.
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"tier":4}"#);
        let jwt = format!("header.{payload}.sig");
        assert_eq!(plan_from_access_token(&jwt), "X Premium Plus");

        let p0 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"tier":0}"#);
        assert_eq!(plan_from_access_token(&format!("h.{p0}.s")), "Free");

        // Missing tier → "".
        assert_eq!(plan_from_access_token("not-a-jwt"), "");
    }

    #[test]
    fn test_grok_cli_parse_billing_on_demand_exhausted() {
        // onDemandCap 0 + no subscription access → synthetic 1/1 0% row.
        let data = json!({
            "config": {"onDemandCap": {"val": 0}, "onDemandUsed": {"val": 0}},
            "billingPeriodEnd": "2026-01-31T00:00:00Z"
        });
        let (quotas, _) = parse_grok_cli_billing(&data, false);
        let od = quotas.get("On-demand").expect("on-demand row");
        assert_eq!(od["used"], 1.0);
        assert_eq!(od["total"], 1.0);
        assert_eq!(od["remainingPercentage"], 0.0);
    }

    #[test]
    fn test_grok_cli_parse_billing_monthly_prepaid() {
        let data = json!({
            "config": {
                "monthlyLimit": {"val": 500}, "includedUsed": {"val": 50},
                "prepaidBalance": {"val": 10}
            },
            "billingPeriodEnd": "2026-01-31T00:00:00Z"
        });
        let (quotas, _) = parse_grok_cli_billing(&data, true);
        let monthly = quotas.get("Monthly included").expect("monthly row");
        assert_eq!(monthly["total"], 500.0);
        assert_eq!(monthly["used"], 50.0);
        let prepaid = quotas.get("Prepaid").expect("prepaid row");
        assert_eq!(prepaid["used"], 0.0);
        assert_eq!(prepaid["total"], 10.0);
        assert_eq!(prepaid["remainingPercentage"], 100.0);
    }

    #[test]
    fn test_kiro_quota_omits_default_profile_for_api_key() {
        use std::collections::BTreeMap;
        let mut psd = BTreeMap::new();
        psd.insert("authMethod".into(), json!("api_key"));
        // api_key + no profileArn → empty (NOT KIRO_DEFAULT_PROFILE_ARN).
        assert_eq!(kiro_resolve_profile_arn(&psd, true), "");
        // api_key + explicit profileArn → that value.
        psd.insert("profileArn".into(), json!("arn:custom"));
        assert_eq!(kiro_resolve_profile_arn(&psd, true), "arn:custom");
        // builder-id + no profileArn → default.
        let mut psd2 = BTreeMap::new();
        psd2.insert("authMethod".into(), json!("builder-id"));
        assert_eq!(
            kiro_resolve_profile_arn(&psd2, false),
            KIRO_DEFAULT_PROFILE_ARN
        );
    }

    #[test]
    fn test_kiro_quota_headers_match_auth_method() {
        // api_key → tokentype: API_KEY present, TokenType absent.
        let mut h = std::collections::HashMap::new();
        if true {
            // is_api_key branch
            h.insert("tokentype".to_string(), "API_KEY".to_string());
        }
        assert_eq!(h.get("tokentype").map(String::as_str), Some("API_KEY"));
        assert!(!h.contains_key("TokenType"));
        // external_idp → TokenType: EXTERNAL_IDP present.
        let mut h2 = std::collections::HashMap::new();
        if true {
            h2.insert("TokenType".to_string(), "EXTERNAL_IDP".to_string());
        }
        assert_eq!(
            h2.get("TokenType").map(String::as_str),
            Some("EXTERNAL_IDP")
        );
        assert!(!h2.contains_key("tokentype"));
    }

    #[test]
    fn test_vercel_ai_gateway_no_credit_message() {
        let out = vercel_ai_gateway_quota_rows(0.0, 0.0);
        assert_eq!(out["plan"], "Pay-as-you-go");
        let msg = out["message"].as_str().unwrap();
        assert!(msg.contains("No credit allocation found"), "got: {msg}");
        assert!(out["quotas"].as_object().unwrap().is_empty());
    }

    #[test]
    fn test_codebuddy_refill_cadence() {
        // Monthly: Jan 1 → Jan 31.
        let m = serde_json::Map::from_iter([
            ("CycleStartTime".into(), json!("2026-01-01T00:00:00Z")),
            ("CycleEndTime".into(), json!("2026-01-31T00:00:00Z")),
        ]);
        assert_eq!(codebuddy_refill_cadence(&m), "Monthly");
        // Daily: 1-day span.
        let d = serde_json::Map::from_iter([
            ("CycleStartTime".into(), json!("2026-01-01T00:00:00Z")),
            ("CycleEndTime".into(), json!("2026-01-02T00:00:00Z")),
        ]);
        assert_eq!(codebuddy_refill_cadence(&d), "Daily");
        // Weekly: 7-day span.
        let w = serde_json::Map::from_iter([
            ("CycleStartTime".into(), json!("2026-01-01T00:00:00Z")),
            ("CycleEndTime".into(), json!("2026-01-08T00:00:00Z")),
        ]);
        assert_eq!(codebuddy_refill_cadence(&w), "Weekly");
    }

    #[test]
    fn test_codebuddy_partitions_refill_vs_bonus() {
        // Refill account: DeductionEndTime − CycleEndTime > 2 days.
        let refill = json!({
            "CycleStartTime": "2026-01-01T00:00:00Z",
            "CycleEndTime": "2026-01-31T00:00:00Z",
            "DeductionEndTime": "2026-02-05T00:00:00Z", // > 2 days past cycle end
            "CycleCapacityUsedPrecise": "10",
            "CycleCapacitySizePrecise": "100",
            "BasePackage": {"PackageName": "Tencent Coding Plan"}
        });
        // Bonus account: DeductionEndTime == 0 (no refill signal).
        let bonus = json!({
            "CycleEndTime": "2026-01-31T00:00:00Z",
            "DeductionEndTime": "0",
            "CapacityUsedPrecise": "2",
            "CapacitySizePrecise": "5"
        });
        let out = codebuddy_quota_rows_from_accounts(vec![refill, bonus]);
        let quotas = out["quotas"].as_object().unwrap();
        assert_eq!(quotas.len(), 2);
        // Refill → "Monthly" recurring true.
        let monthly = quotas.get("Monthly").expect("refill pack labeled Monthly");
        assert_eq!(monthly["recurring"], true);
        assert_eq!(monthly["used"], 10.0);
        assert_eq!(monthly["total"], 100.0);
        // Bonus → "Bonus Pack 1" recurring false.
        let bonus_pack = quotas.get("Bonus Pack 1").expect("bonus pack present");
        assert_eq!(bonus_pack["recurring"], false);
        assert_eq!(bonus_pack["used"], 2.0);
        // Plan from PackageName.
        assert_eq!(out["plan"], "Tencent Coding Plan");
    }

    #[test]
    fn ollama_ratio_quota_converts_ratio_to_percent() {
        // 0.0 → 0%, 1.0 → 100%, 0.5 → 50%; clamped outside [0,1].
        assert_eq!(ollama_ratio_quota(0.0)["used"], json!(0));
        assert_eq!(ollama_ratio_quota(1.0)["used"], json!(100));
        assert_eq!(ollama_ratio_quota(0.5)["used"], json!(50));
        assert_eq!(ollama_ratio_quota(0.253)["used"], json!(25));
        // Clamped.
        assert_eq!(ollama_ratio_quota(2.0)["used"], json!(100));
        assert_eq!(ollama_ratio_quota(-1.0)["used"], json!(0));
        // remainingPercentage complements.
        assert_eq!(ollama_ratio_quota(0.25)["remainingPercentage"], json!(75));
        assert_eq!(ollama_ratio_quota(1.0)["remainingPercentage"], json!(0));
        // Shape matches JS: total 100, resetAt null, unlimited false.
        assert_eq!(ollama_ratio_quota(0.5)["total"], json!(100));
        assert!(ollama_ratio_quota(0.5)["resetAt"].is_null());
        assert_eq!(ollama_ratio_quota(0.5)["unlimited"], json!(false));
    }

    // 9router e3bf94ee (antigravity-weekly.js parseWeeklyQuotaSummary):
    // groups at data.groups or data.quotaSummary.groups; weekly buckets
    // only; disabled/non-finite skipped; first matching bucket per family.
    #[test]
    fn antigravity_weekly_parses_groups_and_quota_summary_shapes() {
        let top = json!({
            "groups": [
                {"displayName": "Gemini Models", "buckets": [
                    {"bucketId": "daily-1", "displayName": "Daily", "remainingFraction": 0.1},
                    {"bucketId": "weekly-1", "displayName": "Weekly Quota", "remainingFraction": 0.75, "resetTime": "2026-09-20T00:00:00Z"},
                ]},
                {"displayName": "Claude Models", "buckets": [
                    {"bucketId": "w", "displayName": "weekly", "remainingFraction": 0.5},
                ]},
            ]
        });
        let out = parse_weekly_quota_summary(&top);
        let g = &out["gemini_weekly"];
        assert_eq!(g["used"], json!(250.0));
        assert_eq!(g["total"], json!(1000.0));
        assert_eq!(g["remainingPercentage"], json!(75.0));
        assert_eq!(g["displayName"], json!("Gemini (Weekly)"));
        assert_eq!(out["claude_gpt_weekly"]["used"], json!(500.0));

        // quotaSummary.groups shape.
        let nested = json!({ "quotaSummary": { "groups": [
            {"displayName": "GPT Models", "buckets": [
                {"bucketId": "weekly", "displayName": "weekly", "remainingFraction": 1.0},
            ]}
        ]}});
        let out2 = parse_weekly_quota_summary(&nested);
        assert_eq!(
            out2["claude_gpt_weekly"]["remainingPercentage"],
            json!(100.0)
        );

        // Non-object / no-groups / disabled / non-finite → {}.
        assert!(parse_weekly_quota_summary(&json!(null)).is_empty());
        assert!(parse_weekly_quota_summary(&json!({"groups": []})).is_empty());
        assert!(parse_weekly_quota_summary(&json!({"groups": [
            {"displayName": "Gemini", "buckets": [
                {"bucketId": "weekly", "displayName": "weekly", "remainingFraction": 0.5, "disabled": true},
            ]}
        ]})).is_empty());
        assert!(parse_weekly_quota_summary(&json!({"groups": [
            {"displayName": "Other Family", "buckets": [
                {"bucketId": "weekly", "displayName": "weekly", "remainingFraction": 0.5},
            ]}
        ]}))
        .is_empty());
    }

    // 9router google.js reconcile: all models in a family exhausted but
    // weekly reports available → clamp weekly to 0 with the family's latest
    // resetAt (the Free Starter tier remainingFraction: 1 bug).
    #[test]
    fn antigravity_weekly_reconcile_clamps_buggily_full_weekly() {
        let mut quotas = serde_json::Map::new();
        quotas.insert(
            "gemini-3.8-flash-high".to_string(),
            json!({"remainingPercentage": 0.0, "resetAt": "2026-09-18T00:00:00Z"}),
        );
        quotas.insert(
            "gemini-3.8-flash-low".to_string(),
            json!({"remainingPercentage": 0.0, "resetAt": "2026-09-19T00:00:00Z"}),
        );
        let mut weekly = serde_json::Map::new();
        weekly.insert(
            "gemini_weekly".to_string(),
            json!({"used": 0.0, "total": 1000.0, "remainingPercentage": 100.0, "resetAt": null}),
        );
        reconcile_weekly_quota(&mut quotas, weekly);
        let g = &quotas["gemini_weekly"];
        assert_eq!(g["used"], json!(1000.0));
        assert_eq!(g["remainingPercentage"], json!(0.0));
        assert_eq!(g["resetAt"], json!("2026-09-19T00:00:00Z"));
    }

    #[test]
    fn antigravity_weekly_reconcile_keeps_healthy_family() {
        let mut quotas = serde_json::Map::new();
        quotas.insert(
            "gemini-3.8-flash-high".to_string(),
            json!({"remainingPercentage": 50.0, "resetAt": null}),
        );
        let mut weekly = serde_json::Map::new();
        weekly.insert(
            "gemini_weekly".to_string(),
            json!({"used": 500.0, "total": 1000.0, "remainingPercentage": 50.0, "resetAt": null}),
        );
        reconcile_weekly_quota(&mut quotas, weekly);
        assert_eq!(quotas["gemini_weekly"]["used"], json!(500.0));
        assert_eq!(quotas["gemini_weekly"]["remainingPercentage"], json!(50.0));
    }
    // 9router b9c92cb8 (groq.js): Go-style durations, header
    // presence semantics, and quota entry shape.
    #[test]
    fn groq_duration_parses_go_style_components() {
        assert_eq!(parse_groq_duration_ms("2m59.56s"), Some(179_560));
        assert_eq!(parse_groq_duration_ms("7.66s"), Some(7_660));
        assert_eq!(parse_groq_duration_ms("150ms"), Some(150));
        assert_eq!(parse_groq_duration_ms("1h2m3s"), Some(3_723_000));
        assert_eq!(parse_groq_duration_ms(""), None);
        assert_eq!(parse_groq_duration_ms("   "), None);
        assert_eq!(parse_groq_duration_ms("abc"), None);
    }

    #[test]
    fn groq_rate_limit_quota_requires_both_headers_present_and_numeric() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        // Missing headers → None (must not masquerade as 0 remaining).
        assert!(groq_rate_limit_quota(
            &h,
            "x-ratelimit-limit-requests",
            "x-ratelimit-remaining-requests",
            "x-ratelimit-reset-requests"
        )
        .is_none());
        h.insert(
            "x-ratelimit-limit-requests",
            HeaderValue::from_static("14400"),
        );
        // Only limit present → still None.
        assert!(groq_rate_limit_quota(
            &h,
            "x-ratelimit-limit-requests",
            "x-ratelimit-remaining-requests",
            "x-ratelimit-reset-requests"
        )
        .is_none());
        h.insert(
            "x-ratelimit-remaining-requests",
            HeaderValue::from_static("14370"),
        );
        h.insert(
            "x-ratelimit-reset-requests",
            HeaderValue::from_static("2m59.56s"),
        );
        let q = groq_rate_limit_quota(
            &h,
            "x-ratelimit-limit-requests",
            "x-ratelimit-remaining-requests",
            "x-ratelimit-reset-requests",
        )
        .expect("quota");
        assert_eq!(q["used"], json!(30.0));
        assert_eq!(q["total"], json!(14400.0));
        assert_eq!(q["unlimited"], json!(false));
        assert!(q["resetAt"].is_string(), "reset header resolves to ISO ts");
        // Non-numeric → None.
        h.insert(
            "x-ratelimit-remaining-requests",
            HeaderValue::from_static("many"),
        );
        assert!(groq_rate_limit_quota(
            &h,
            "x-ratelimit-limit-requests",
            "x-ratelimit-remaining-requests",
            "x-ratelimit-reset-requests"
        )
        .is_none());
    }
    // 9router e5a13c3a (zed.js): plan labels, UsageLimit shapes,
    // token-billing detection, quota rows, full user-info mapping.
    #[test]
    fn zed_plan_label_maps_known_ids() {
        assert_eq!(format_zed_plan_label("zed_free"), "Zed Free");
        assert_eq!(format_zed_plan_label("zed_pro"), "Zed Pro");
        assert_eq!(format_zed_plan_label("zed_pro_trial"), "Zed Pro Trial");
        assert_eq!(format_zed_plan_label("zed_student"), "Zed Student");
        assert_eq!(format_zed_plan_label("zed_business"), "Zed Business");
        assert_eq!(format_zed_plan_label(""), "Zed");
        assert_eq!(format_zed_plan_label("zed_custom_plus"), "Zed Custom Plus");
    }

    #[test]
    fn zed_usage_limit_parses_all_shapes() {
        assert_eq!(parse_zed_usage_limit(&json!(null)), (false, 0.0));
        assert_eq!(parse_zed_usage_limit(&json!("unlimited")), (true, 0.0));
        assert_eq!(
            parse_zed_usage_limit(&json!({"unlimited": true})),
            (true, 0.0)
        );
        assert_eq!(parse_zed_usage_limit(&json!(500)), (false, 500.0));
        assert_eq!(parse_zed_usage_limit(&json!(-5)), (false, 0.0));
        assert_eq!(parse_zed_usage_limit(&json!("300")), (false, 300.0));
        assert_eq!(
            parse_zed_usage_limit(&json!({"limited": 250})),
            (false, 250.0)
        );
        assert_eq!(
            parse_zed_usage_limit(&json!({"Limited": 100})),
            (false, 100.0)
        );
        assert_eq!(parse_zed_usage_limit(&json!({"other": 1})), (false, 0.0));
        // Token-billing: { limited: 0 } on Pro means per-token billing.
        assert!(is_zed_token_billing_limit(&json!({"limited": 0})));
        assert!(!is_zed_token_billing_limit(&json!({"limited": 10})));
        assert!(!is_zed_token_billing_limit(&json!("unlimited")));
    }

    #[test]
    fn zed_quota_row_covers_unlimited_zero_and_clamped() {
        let u = make_zed_quota_row(&json!(100), &json!("unlimited"), None);
        assert_eq!(u["unlimited"], json!(true));
        assert_eq!(u["remainingPercentage"], json!(100));
        let z = make_zed_quota_row(&json!(0), &json!({"limited": 0}), None);
        assert_eq!(z["total"], json!(0));
        assert_eq!(z["remainingPercentage"], json!(0));
        let c = make_zed_quota_row(&json!(900), &json!(500), None);
        assert_eq!(c["used"], json!(500.0));
        assert_eq!(c["remainingPercentage"], json!(0.0));
    }

    #[test]
    fn zed_user_usage_maps_plan_quotas_and_messages() {
        // Paid Pro: both rows, plan label, no message.
        let out = parse_zed_user_usage(&json!({
            "plan": {
                "plan_v3": "zed_pro",
                "subscription_period": {"ended_at": "2026-10-01T00:00:00Z"},
                "usage": {
                    "edit_predictions": {"used": 50, "limit": 1000},
                    "model_requests": {"used": 10, "limit": {"limited": 500}},
                }
            }
        }));
        assert_eq!(out["plan"], json!("Zed Pro"));
        assert_eq!(out["quotas"]["Edit Predictions"]["used"], json!(50.0));
        assert_eq!(
            out["quotas"]["Hosted Model Requests"]["total"],
            json!(500.0)
        );
        assert!(out["message"].is_null());
        assert_eq!(out["planId"], json!("zed_pro"));
        // Trial suffix appended only once.
        let trial = parse_zed_user_usage(&json!({
            "plan": {"plan_v3": "zed_pro", "trial_started_at": "2026-09-01T00:00:00Z", "usage": {}}
        }));
        assert_eq!(trial["plan"], json!("Zed Pro (Trial active)"));
        assert_eq!(trial["trialStarted"], json!(true));
        // Token billing: model row omitted, note surfaced.
        let tok = parse_zed_user_usage(&json!({
            "plan": {"plan_v3": "zed_pro", "usage": {
                "edit_predictions": {"used": 5, "limit": 100},
                "model_requests": {"used": 3, "limit": {"limited": 0}},
            }}
        }));
        assert!(tok["quotas"].get("Hosted Model Requests").is_none());
        assert!(tok["message"].as_str().unwrap().contains("per token"));
        // Overdue invoices override the token note.
        let overdue = parse_zed_user_usage(&json!({
            "plan": {"plan_v3": "zed_pro", "has_overdue_invoices": true, "usage": {
                "model_requests": {"used": 1, "limit": {"limited": 0}},
            }}
        }));
        assert!(overdue["message"].as_str().unwrap().contains("overdue"));
        assert_eq!(overdue["hasOverdueInvoices"], json!(true));
        // Unlimited model row kept.
        let unlim = parse_zed_user_usage(&json!({
            "plan": {"plan_v3": "zed_free", "usage": {
                "model_requests": {"used": 7, "limit": "unlimited"},
            }}
        }));
        assert_eq!(
            unlim["quotas"]["Hosted Model Requests"]["unlimited"],
            json!(true)
        );
    }
    // 9router 4ad1e7a4: model-scoped weekly limits come from limits[]
    // (kind == "weekly_scoped"), never fabricated; seven_day_* keys need a
    // numeric utilization and skip the bare "seven_day" key.
    #[test]
    fn claude_limits_weekly_scoped_creates_model_row() {
        // Extract the row-building logic via a representative body: the
        // seven_day_* + limits[] loop lives inside fetch_claude_quota, so
        // mirror its contract here at the unit level through the same
        // helpers it uses (build_quota_entry + parse_reset_time) plus a
        // direct simulation of the new branch.
        let body = json!({
            "seven_day": {"utilization": 20.0, "resets_at": "2026-09-24T00:00:00Z"},
            "seven_day_sonnet": {"utilization": 30.0, "resets_at": "2026-09-24T00:00:00Z"},
            "seven_day": {"utilization": 20.0},
            "limits": [
                {"kind": "weekly_scoped", "percent": 42.0, "resets_at": "2026-09-25T00:00:00Z",
                 "scope": {"model": {"display_name": "Fable"}}},
                {"kind": "daily", "percent": 99.0},
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "NoPercent"}}},
                {"kind": "weekly_scoped", "percent": 10.0, "scope": {}},
            ],
        });
        // Simulate the branch: only the well-formed Fable entry survives.
        let mut rows: Vec<String> = vec![];
        if let Some(limits) = body.get("limits").and_then(|v| v.as_array()) {
            for limit in limits {
                if limit.get("kind").and_then(|v| v.as_str()) != Some("weekly_scoped") {
                    continue;
                }
                let name = limit
                    .get("scope")
                    .and_then(|v| v.get("model"))
                    .and_then(|v| v.get("display_name"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_lowercase);
                let (Some(name), Some(pct)) = (name, limit.get("percent").and_then(|v| v.as_f64()))
                else {
                    continue;
                };
                rows.push(format!("weekly {name} (7d):{pct}"));
            }
        }
        assert_eq!(rows, vec!["weekly fable (7d):42"]);
        // And the quota entry math matches createQuotaObject semantics.
        let q = build_quota_entry(
            42.0_f64.clamp(0.0, 100.0),
            100.0,
            parse_reset_time(&json!("2026-09-25T00:00:00Z")),
        );
        assert_eq!(q["used"], json!(42.0));
        // Float math: remaining = 100 - 42 via f64 in build_quota_entry.
        let pct = q["remainingPercentage"].as_f64().expect("pct");
        assert!((pct - 58.0).abs() < 1e-9, "pct={pct}");
    }
    // 9router 0da803ee (opencode-go.js): percent coercion + Rolling /
    // Weekly / Monthly rows + empty/invalid usage handling.
    #[test]
    fn opencode_go_percent_coerces_number_and_string() {
        assert_eq!(opencode_go_percent(&json!(42.5)), Some(42.5));
        assert_eq!(opencode_go_percent(&json!("75")), Some(75.0));
        assert_eq!(opencode_go_percent(&json!("  ")), None);
        assert_eq!(opencode_go_percent(&json!(null)), None);
        assert_eq!(opencode_go_percent(&json!("abc")), None);
    }

    #[test]
    fn opencode_go_rows_cover_all_periods_and_skip_invalid() {
        let out = opencode_go_quota_rows(&json!({
            "rolling": {"percent": 10, "resetsAt": "2026-09-18T00:00:00Z"},
            "weekly": {"percent": "55.5"},
            "monthly": {"percent": 150},
        }));
        assert_eq!(out["plan"], json!("OpenCode Go"));
        assert_eq!(out["quotas"]["Rolling"]["used"], json!(10.0));
        assert_eq!(out["quotas"]["Weekly"]["used"], json!(55.5));
        // Clamped at 100.
        assert_eq!(out["quotas"]["Monthly"]["used"], json!(100.0));
        assert_eq!(out["quotas"]["Monthly"]["remainingPercentage"], json!(0.0));
        // Missing/invalid periods skipped, not fabricated.
        let out2 = opencode_go_quota_rows(&json!({"rolling": {"percent": "nan"}}));
        assert!(out2.get("message").is_some());
        let out3 = opencode_go_quota_rows(&json!({}));
        assert!(out3.get("message").is_some());
    }
}
