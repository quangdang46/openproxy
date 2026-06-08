//! Live provider quota fetchers (GLM, MiniMax).
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
                build_minimax_quota(effective_total, effective_count, reset_at, effective_remaining_mode),
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

    // /copilot_internal/v2/usage reports the premium-request allowance and
    // monthly reset timestamp for the signed-in Copilot subscription.
    let usage_resp = match client
        .get("https://api.github.com/copilot_internal/v2/usage")
        .header("Authorization", format!("token {access_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "GitHubCopilotChat/0.38.0")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("GitHub error: {e}") }),
    };

    let status = usage_resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return json!({ "message": "GitHub access token invalid or expired." });
    }
    if !status.is_success() {
        return json!({
            "message": format!("GitHub quota API error ({}).", status.as_u16())
        });
    }

    let body: Value = match usage_resp.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "message": format!("GitHub error: {e}") }),
    };

    // /user is a cheap validity check that also surfaces the login handle.
    // Failure here is non-fatal: the dashboard still gets the quota numbers.
    let username = match async {
        let resp = client
            .get("https://api.github.com/user")
            .header("Authorization", format!("token {access_token}"))
            .header("Accept", "application/json")
            .header("User-Agent", "GitHubCopilotChat/0.38.0")
            .send()
            .await?;
        resp.json::<Value>().await
    }
    .await
    {
        Ok(user) => user
            .get("login")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        Err(_) => None,
    };

    let premium = body.get("premium_requests");
    let allowance = premium
        .and_then(|p| p.get("allowance"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let consumed = premium
        .and_then(|p| p.get("consumed"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    // Reset time appears at the top level as `quota_reset` (epoch seconds) and
    // is also mirrored on the premium_requests object. Prefer the explicit
    // top-level field when present.
    let reset_at = body
        .get("quota_reset")
        .and_then(parse_reset_time)
        .or_else(|| premium.and_then(|p| p.get("reset")).and_then(parse_reset_time));

    // Quota snapshot is reported as remaining, not used. Some users see
    // `quota_remaining` only on plans that have a separate chat pool.
    let chat_remaining = body.get("quota_remaining").and_then(|v| v.as_f64());

    let mut quotas = serde_json::Map::new();
    if allowance > 0.0 {
        quotas.insert(
            "premium requests".to_string(),
            build_quota_entry(consumed, allowance, reset_at.clone()),
        );
    }
    if let Some(remaining) = chat_remaining {
        // Chat pool: GitHub does not document a separate total, so we treat
        // `quota_remaining` as both remaining and effective total (1.0 unit
        // per request). The dashboard will surface a "1 / 1" cell that flips
        // to 0 / 1 once the user exhausts the chat pool.
        quotas.insert(
            "chat quota".to_string(),
            build_quota_entry(1.0 - remaining, 1.0, reset_at),
        );
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
    let mut last_error: Option<String> = None;

    let urls: &[&str] = &[
        "https://api.openai.com/v1/organization/usage",
        "https://api.openai.com/dashboard/billing/usage",
    ];

    for (index, url) in urls.iter().enumerate() {
        let can_fallback = index + 1 < urls.len();
        let response = match client
            .get(*url)
            .bearer_auth(access_token)
            .header("Accept", "application/json")
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
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return json!({ "message": "Invalid or expired Codex token" });
        }

        if !status.is_success() {
            let err = format!("Codex quota endpoint error ({})", status.as_u16());
            last_error = Some(err.clone());
            let transient = matches!(status.as_u16(), 404 | 405) || status.as_u16() >= 500;
            if transient && can_fallback {
                continue;
            }
            return json!({ "message": format!("Codex connected. {err}") });
        }

        let body: Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                last_error = Some(e.to_string());
                if can_fallback {
                    continue;
                }
                return json!({ "message": format!("Codex error: {e}") });
            }
        };

        let mut quotas = serde_json::Map::new();

        if let Some(total_granted) = body
            .get("total_granted")
            .and_then(|v| v.as_f64())
            .or_else(|| body.get("totalGranted").and_then(|v| v.as_f64()))
        {
            let total_used = body
                .get("total_used")
                .and_then(|v| v.as_f64())
                .or_else(|| body.get("totalUsed").and_then(|v| v.as_f64()))
                .unwrap_or(0.0);
            let reset_at = body
                .get("access_until")
                .and_then(parse_reset_time)
                .or_else(|| body.get("accessUntil").and_then(parse_reset_time));
            quotas.insert(
                "session".to_string(),
                build_quota_entry(total_used, total_granted, reset_at),
            );
        }

        if let Some(buckets) = body.get("bucket_limits").and_then(|v| v.as_array()) {
            for (i, bucket) in buckets.iter().enumerate() {
                let used = bucket
                    .get("used")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let total = bucket
                    .get("limit")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                if total <= 0.0 {
                    continue;
                }
                let reset_at = bucket
                    .get("reset_at")
                    .and_then(parse_reset_time)
                    .or_else(|| bucket.get("resetAt").and_then(parse_reset_time));
                let key = format!("bucket_{i}");
                quotas.insert(key, build_quota_entry(used, total, reset_at));
            }
        }

        if let Some(line_items) = body
            .get("line_items")
            .and_then(|v| v.as_array())
            .or_else(|| body.get("lineItems").and_then(|v| v.as_array()))
        {
            let mut total_cost: f64 = 0.0;
            for item in line_items {
                let cost = item
                    .get("cost")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                total_cost += cost;
            }
            if total_cost > 0.0 {
                let hard_limit = body
                    .get("hard_limit_usd")
                    .and_then(|v| v.as_f64())
                    .or_else(|| body.get("hardLimitUsd").and_then(|v| v.as_f64()))
                    .unwrap_or(0.0);
                let reset_at = body
                    .get("access_until")
                    .and_then(parse_reset_time)
                    .or_else(|| body.get("accessUntil").and_then(parse_reset_time));
                if hard_limit > 0.0 {
                    quotas.insert(
                        "billing".to_string(),
                        build_quota_entry(total_cost, hard_limit, reset_at),
                    );
                }
            }
        }

        if !quotas.is_empty() {
            return json!({ "quotas": Value::Object(quotas) });
        }

        if can_fallback {
            last_error = Some("no quota fields found in response".to_string());
            continue;
        }
        break;
    }

    let _ = last_error;
    json!({ "message": "Codex connected. Quota data not available via this endpoint." })
}

pub async fn fetch_gemini_cli_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Gemini CLI access token not available." });
    }

    let client = http_client();

    let load_body = load_code_assist(
        &client,
        access_token,
        cloud_code_metadata(),
        &[("x-goog-api-client", "gl-rust/1.0.0")],
    )
    .await;

    let project_id = match load_body {
        Ok(value) => value
            .get("cloudProject")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        Err(e) => {
            if e.contains("401") || e.contains("403") {
                return json!({ "message": "Gemini CLI access token invalid or expired." });
            }
            return json!({ "message": format!("Gemini CLI error: {e}") });
        }
    };

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
        .json(&json!({}))
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

    let limits = body
        .get("usageLimits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if limits.is_empty() {
        return json!({ "message": "Gemini CLI connected. No quota data was returned." });
    }

    let mut quotas = serde_json::Map::new();
    for limit in &limits {
        let quota_id = limit
            .get("quotaId")
            .and_then(|v| v.as_str())
            .unwrap_or("quota")
            .to_string();
        let used = limit
            .get("used")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let total = limit
            .get("limit")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let reset_at = limit.get("resetTime").and_then(parse_reset_time);
        quotas.insert(quota_id, build_quota_entry(used, total, reset_at));
    }

    json!({ "quotas": Value::Object(quotas) })
}

/// Shared `loadCodeAssist` call for Gemini CLI and Antigravity.
///
/// POSTs to `cloudcode-pa.googleapis.com/v1internal:loadCodeAssist` with the
/// given body and extra headers. Returns the parsed JSON body on 200.
async fn load_code_assist(
    client: &reqwest::Client,
    access_token: &str,
    body: Value,
    extra_headers: &[(&str, &str)],
) -> Result<Value, String> {
    let url = format!("{CLOUD_CODE_BASE}:loadCodeAssist");
    let mut req = client
        .post(&url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .json(&body);
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
///
/// Tries the most likely quota endpoint(s) on the Qoder API with the user's
/// OAuth bearer token. Qoder is a newer AI coding tool whose exact quota
/// contract is not yet stable, so we probe a small set of candidates and
/// return a helpful message if none of them expose quota data.
pub async fn fetch_qoder_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Qoder token" });
    }

    let client = http_client();
    const CANDIDATE_URLS: &[&str] = &[
        "https://api.qoder.ai/v1/usage",
        "https://api.qoder.ai/v1/quota",
        "https://api.qoder.ai/v1/account/quota",
    ];

    let mut last_status: Option<u16> = None;
    let mut last_body = String::new();

    for url in CANDIDATE_URLS {
        let response = match client
            .get(*url)
            .bearer_auth(access_token)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return json!({ "message": format!("Qoder error: {e}") }),
        };

        let status = response.status();
        let status_code = status.as_u16();
        let raw_text = response.text().await.unwrap_or_default();

        if status_code == 401 || status_code == 403 {
            return json!({ "message": "Invalid or expired Qoder token" });
        }

        if status_code == 404 || status_code == 405 {
            last_status = Some(status_code);
            last_body = raw_text;
            continue;
        }

        if !status.is_success() {
            return json!({
                "message": format!("Qoder quota API error ({}).", status_code)
            });
        }

        let body: Value = match serde_json::from_str(&raw_text) {
            Ok(v) => v,
            Err(_) => {
                last_status = Some(status_code);
                last_body = raw_text;
                continue;
            }
        };

        let data = body.get("data").cloned().unwrap_or_else(|| body.clone());

        let total = data
            .get("total")
            .or_else(|| data.get("quota"))
            .or_else(|| data.get("limit"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let used = data
            .get("used")
            .or_else(|| data.get("usage"))
            .or_else(|| data.get("consumed"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let remaining = data
            .get("remaining")
            .or_else(|| data.get("left"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let reset_at = parse_reset_time(
            data.get("reset_at")
                .or_else(|| data.get("resets_at"))
                .or_else(|| data.get("resetAt"))
                .unwrap_or(&Value::Null),
        );

        if total <= 0.0 && used <= 0.0 && remaining <= 0.0 {
            last_status = Some(status_code);
            last_body = raw_text;
            continue;
        }

        // Qoder's contract is not yet stable: some endpoints return only
        // (used, remaining) without an explicit total. When that happens,
        // derive total so build_quota_entry's remaining/percentage math
        // stays accurate.
        let effective_total = if total > 0.0 {
            total
        } else {
            used + remaining
        };
        let effective_used = if used > 0.0 {
            used
        } else if total > 0.0 && remaining >= 0.0 {
            (total - remaining).max(0.0)
        } else {
            0.0
        };

        let mut quotas = serde_json::Map::new();
        quotas.insert(
            "session".to_string(),
            build_quota_entry(effective_used, effective_total, reset_at),
        );

        return json!({ "quotas": Value::Object(quotas) });
    }

    let detail = match last_status {
        Some(code) if code == 404 || code == 405 => {
            "Qoder connected. Quota endpoint not available; the Qoder API contract may have changed."
                .to_string()
        }
        Some(code) => format!("Qoder quota API error ({}).", code),
        None if last_body.is_empty() => {
            "Qoder connected. No quota data was returned by the upstream API.".to_string()
        }
        None => format!("Qoder connected. No quota data was returned: {}", last_body),
    };
    json!({ "message": detail })
}

pub async fn fetch_claude_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Claude token" });
    }

    let client = http_client();
    let response = match client
        .get("https://api.anthropic.com/v1/oauth/token/quota")
        .bearer_auth(access_token)
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
        return json!({ "message": "Invalid or expired Claude token" });
    }
    if !status.is_success() {
        return json!({
            "message": format!("Claude quota API error ({}).", status.as_u16())
        });
    }

    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "message": format!("Claude error: {e}") }),
    };

    let total = body
        .get("quota")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let used = body
        .get("quota_usage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let reset_at = parse_reset_time(body.get("resets_at").unwrap_or(&Value::Null));

    if total <= 0.0 {
        return json!({ "message": "Claude connected. No quota data available." });
    }

    let mut quotas = serde_json::Map::new();
    quotas.insert(
        "session".to_string(),
        build_quota_entry(used, total, reset_at),
    );

    json!({ "quotas": Value::Object(quotas) })
}

const KIRO_ENDPOINTS: &[&str] = &[
    "https://kiro.ai/api/quota",
    "https://kiro.ai/api/usage",
    "https://kiro.ai/api/user",
];

pub async fn fetch_kiro_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Kiro token" });
    }

    let client = http_client();
    let mut last_error: Option<String> = None;

    for url in KIRO_ENDPOINTS {
        let response = match client
            .get(*url)
            .bearer_auth(access_token)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_error = Some(e.to_string());
                continue;
            }
        };

        let status = response.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return json!({ "message": "Invalid or expired Kiro token" });
        }
        if !status.is_success() {
            last_error = Some(format!("HTTP {}", status.as_u16()));
            continue;
        }

        let body: Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                last_error = Some(e.to_string());
                continue;
            }
        };

        let data = body.get("data").unwrap_or(&body);
        let total = data
            .get("quota")
            .or_else(|| data.get("quota_limit"))
            .or_else(|| data.get("total"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if total <= 0.0 {
            continue;
        }

        let used = data
            .get("usage")
            .or_else(|| data.get("used"))
            .or_else(|| data.get("quota_usage"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let reset_at = parse_reset_time(
            data.get("reset_at")
                .or_else(|| data.get("resets_at"))
                .or_else(|| data.get("resetAt"))
                .unwrap_or(&Value::Null),
        );
        let plan = data
            .get("subscription")
            .or_else(|| data.get("plan"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let mut quotas = serde_json::Map::new();
        quotas.insert(
            "session".to_string(),
            build_quota_entry(used, total, reset_at),
        );

        let mut result = serde_json::Map::new();
        result.insert("quotas".to_string(), Value::Object(quotas));
        if let Some(plan_name) = plan {
            result.insert("plan".to_string(), Value::String(plan_name));
        }
        return Value::Object(result);
    }

    json!({
        "message": last_error
            .map(|e| format!("Kiro quota API error ({e})."))
            .unwrap_or_else(|| "Kiro quota API error.".to_string())
    })
}

pub async fn fetch_antigravity_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Antigravity token" });
    }

    let client = http_client();
    let user_agent = antigravity_user_agent();
    let metadata = cloud_code_metadata();

    let load_body = match load_code_assist(
        &client,
        access_token,
        metadata,
        &[("User-Agent", &user_agent)],
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

    let project_id = match load_body.get("cloudProject").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            return json!({
                "message": "Antigravity connected. No cloud project was returned by loadCodeAssist."
            });
        }
    };

    let url = format!("{CLOUD_CODE_BASE}:fetchAvailableModels?projectId={project_id}");
    let models_resp = match client
        .post(&url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .header("User-Agent", &user_agent)
        .json(&json!({}))
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

    let models = body
        .get("models")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut quotas = serde_json::Map::new();
    for model in &models {
        let name = model
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| model.get("displayName").and_then(|v| v.as_str()))
            .or_else(|| model.get("id").and_then(|v| v.as_str()))
            .map(|s| s.to_string());
        let Some(name) = name else { continue };

        let quota = match model.get("quotaInfo").or_else(|| model.get("quota")) {
            Some(q) => q,
            None => continue,
        };

        let total = quota
            .get("totalCount")
            .or_else(|| quota.get("total"))
            .or_else(|| quota.get("limit"))
            .or_else(|| quota.get("quota"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let used = quota
            .get("usedCount")
            .or_else(|| quota.get("used"))
            .or_else(|| quota.get("usage"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let remaining = quota
            .get("remainingCount")
            .or_else(|| quota.get("remaining"))
            .and_then(|v| v.as_f64());
        let reset_at = parse_reset_time(
            quota
                .get("resetTime")
                .or_else(|| quota.get("reset_at"))
                .or_else(|| quota.get("resets_at"))
                .or_else(|| quota.get("resetAt"))
                .unwrap_or(&Value::Null),
        );

        let (effective_total, effective_used) = if let Some(remaining) = remaining {
            if total > 0.0 {
                (total, used.max(0.0).min(total))
            } else {
                (used + remaining, used.max(0.0))
            }
        } else {
            (total, used)
        };

        if effective_total <= 0.0 && effective_used <= 0.0 {
            continue;
        }

        quotas.insert(name, build_quota_entry(effective_used, effective_total, reset_at));
    }

    if quotas.is_empty() {
        return json!({
            "message": "Antigravity connected. No quota data was returned by fetchAvailableModels."
        });
    }

    json!({ "quotas": Value::Object(quotas) })
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
}
