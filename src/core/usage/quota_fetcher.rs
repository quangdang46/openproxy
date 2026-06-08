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
            quotas.insert(label.to_string(), build_quota_entry(used, entitlement, entry_reset));
        }
    }

    // Free/limited plan: `monthly_quotas` holds totals, `limited_user_quotas` holds used amounts.
    // Both are flat number maps: { "chat": <number>, "completions": <number>, ... }.
    if quotas.is_empty() {
        let monthly = body
            .get("monthly_quotas")
            .and_then(|v| v.as_object());
        let limited = body
            .get("limited_user_quotas")
            .and_then(|v| v.as_object());
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
                let total = monthly.get(key.as_str()).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let used = limited.get(key.as_str()).and_then(|v| v.as_f64()).unwrap_or(0.0);
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
        .get("https://chatgpt.com/backend-api/wham/usage")
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

    if let Some(plan) = body.get("plan_type").and_then(|v| v.as_str()) {
        if !plan.is_empty() {
            quotas.insert(
                "plan".to_string(),
                json!({
                    "used": 0.0,
                    "total": 0.0,
                    "remaining": 0.0,
                    "remainingPercentage": 0.0,
                    "resetAt": Value::Null,
                    "unlimited": false,
                    "label": plan,
                }),
            );
        }
    }

    let normal_rl = body.get("rate_limit")
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

    if quotas.is_empty() {
        return json!({ "message": "Codex connected. No quota data was returned." });
    }

    json!({ "quotas": Value::Object(quotas) })
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

fn append_codex_quota_windows(quotas: &mut serde_json::Map<String, Value>, prefix: &str, snapshot: &Value) {
    let rl = codex_rate_limit_body(snapshot);
    let primary = rl.get("primary_window")
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
    let secondary = rl.get("secondary_window")
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
    if let Some(map) = data.get("rate_limits_by_limit_id").and_then(|v| v.as_object()) {
        for key in &["code_review", "codex_review", "review"] {
            if let Some(v) = map.get(*key) {
                return Some(v.clone());
            }
        }
    }
    if let Some(limits) = data.get("additional_rate_limits").and_then(|v| v.as_array()) {
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
        quotas.insert(
            model_id,
            build_quota_entry(used, total, reset_at),
        );
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
            if let (Some(merged_obj), Some(extra_obj)) = (merged.as_object_mut(), extra.as_object()) {
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
            quotas.insert("user".to_string(), build_quota_entry(used, total, reset_at.clone()));
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
            quotas.insert("org".to_string(), build_quota_entry(used, total, reset_at.clone()));
        }
    }

    if quotas.is_empty() {
        return json!({ "message": "Qoder connected. No quota data was returned." });
    }

    json!({ "quotas": Value::Object(quotas) })
}

pub async fn fetch_claude_quota(access_token: &str, _provider: &str) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Claude token" });
    }

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
            if !key.starts_with("seven_day_") {
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

    if quotas.is_empty() {
        return json!({ "message": "Claude connected. No quota data was returned." });
    }

    json!({ "quotas": Value::Object(quotas) })
}

const KIRO_DEFAULT_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";
const KIRO_AGENTIC_URL: &str = "https://codewhisperer.us-east-1.amazonaws.com";
const KIRO_Q_URL: &str = "https://q.us-east-1.amazonaws.com";

fn kiro_resolve_profile_arn(
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
) -> String {
    provider_specific_data
        .get("profileArn")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(KIRO_DEFAULT_PROFILE_ARN)
        .to_string()
}

pub async fn fetch_kiro_quota(
    access_token: &str,
    _provider: &str,
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
) -> Value {
    if access_token.is_empty() {
        return json!({ "message": "Invalid or expired Kiro token" });
    }

    let client = http_client();
    let profile_arn = kiro_resolve_profile_arn(provider_specific_data);
    let mut quotas = serde_json::Map::new();

    let user_agent = "aws-sdk-js/1.0.0 KiroIDE";
    let mut tried_post = false;
    let mut tried_q = false;
    let mut primary_body: Option<Value> = None;

    let primary_url = format!(
        "{KIRO_AGENTIC_URL}/getUsageLimits?isEmailRequired=true&origin=AI_EDITOR&resourceType=AGENTIC_REQUEST"
    );
    if let Ok(resp) = client
        .get(&primary_url)
        .bearer_auth(access_token)
        .header("x-amz-user-agent", user_agent)
        .header("user-agent", user_agent)
        .header("Accept", "application/json")
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(body) = resp.json::<Value>().await {
                primary_body = Some(body);
            }
        }
    }

    if primary_body.is_none() {
        tried_post = true;
        let post_body = json!({
            "origin": "AI_EDITOR",
            "profileArn": profile_arn,
            "resourceType": "AGENTIC_REQUEST",
        });
        if let Ok(resp) = client
            .post(KIRO_AGENTIC_URL)
            .bearer_auth(access_token)
            .header("Content-Type", "application/x-amz-json-1.0")
            .header("x-amz-target", "AmazonCodeWhispererService.GetUsageLimits")
            .header("Accept", "application/json")
            .json(&post_body)
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<Value>().await {
                    primary_body = Some(body);
                }
            }
        }
    }

    if primary_body.is_none() {
        tried_q = true;
        let q_url = format!(
            "{KIRO_Q_URL}/getUsageLimits?origin=AI_EDITOR&profileArn={profile_arn}&resourceType=AGENTIC_REQUEST"
        );
        if let Ok(resp) = client
            .get(&q_url)
            .bearer_auth(access_token)
            .header("Accept", "application/json")
            .send()
            .await
        {
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
            return json!({
                "message": format!(
                    "Kiro connected. Quota endpoints unreachable (tried primary={} post={} q={}).",
                    !tried_post, tried_post, tried_q
                )
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
                quotas.insert(key.clone(), build_quota_entry(used, total, reset_at.clone()));
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

    let project_id = match load_body.get("cloudaicompanionProject").and_then(normalize_cloud_code_project_id) {
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

    let mut quotas = serde_json::Map::new();
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

        quotas.insert(
            model_id.clone(),
            build_quota_entry(used, total, reset_at),
        );
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
