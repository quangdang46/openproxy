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
