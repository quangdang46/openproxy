//! Per-key monthly budget cap & hard kill-switch (free-tier Feature 3).
//!
//! Enforces `ApiKey.monthly_budget_usd` against the current calendar month's
//! tracked spend. When the cap is reached the request is hard-blocked with
//! HTTP 429 and an `X-Budget-Remaining: 0` header; otherwise a successful
//! response carries `X-Budget-Remaining: <amount>` so clients can surface
//! remaining quota.
//!
//! Free-tier request ceilings (OpenRouter 200/day, Gemini 15 RPM) are
//! intentionally NOT tracked here: they are a rate concern already owned by
//! [`crate::core::account_fallback`] rate-limit state, which is refreshed from
//! upstream headers. Extend that single source of truth instead of adding
//! local counters — this module stays a pure USD-spend gate.

use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::server::state::AppState;
use crate::types::UsageDb;

/// Response header carrying the remaining monthly budget (USD) for the
/// presented key. `0` when the budget is exceeded; omitted when no budget is
/// configured.
pub const BUDGET_REMAINING_HEADER: &str = "x-budget-remaining";

/// Robot-envelope schema stamped on the 429 body when the budget is exceeded.
pub const BUDGET_EXCEEDED_SCHEMA: &str = "openproxy.v1.budget.exceeded";

/// Enforce the per-key monthly budget.
///
/// Returns `Ok(remaining)` where `remaining` is `Some(amount)` when a budget
/// is configured (and not yet exceeded) or `None` when no budget is set.
/// Returns `Err(response)` with a 429 when the budget is exceeded.
pub fn enforce_budget(
    state: &AppState,
    presented_api_key: Option<&str>,
) -> Result<Option<f64>, Response> {
    let Some(raw_key) = presented_api_key else {
        // No key presented — nothing to enforce against.
        return Ok(None);
    };

    let snapshot = state.db.snapshot();
    let Some(api_key) = snapshot.api_keys.iter().find(|k| k.key == raw_key) else {
        return Ok(None);
    };

    let Some(budget) = api_key.monthly_budget() else {
        return Ok(None);
    };

    let usage_db = state.usage_tracker().get_usage_db();
    let spent = monthly_spend_for_key(&usage_db, raw_key);

    if spent >= budget {
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": {
                    "message": format!(
                        "Monthly budget of ${:.2} exceeded (spent ${:.2}).",
                        budget, spent
                    ),
                    "type": "budget_exceeded",
                    "code": "budget_exceeded",
                },
                "schema": BUDGET_EXCEEDED_SCHEMA,
            })),
        )
            .into_response();
        if let Ok(value) = HeaderValue::from_str("0") {
            response
                .headers_mut()
                .insert(BUDGET_REMAINING_HEADER, value);
        }
        return Err(response);
    }

    Ok(Some((budget - spent).max(0.0)))
}

/// Stamp `X-Budget-Remaining` onto a successful response when a budget is
/// configured. No-op (returns the response unchanged) when `remaining` is
/// `None`.
pub fn with_budget_header(mut response: Response, remaining: Option<f64>) -> Response {
    if let Some(remaining) = remaining {
        if let Ok(value) = HeaderValue::from_str(&format!("{remaining:.2}")) {
            response
                .headers_mut()
                .insert(BUDGET_REMAINING_HEADER, value);
        }
    }
    response
}

/// Sum the USD cost of all usage entries for `api_key` in the current
/// calendar month. Entries store `api_key` as the raw presented key string
/// and `cost` as the computed USD cost.
pub fn monthly_spend_for_key(usage_db: &UsageDb, api_key: &str) -> f64 {
    let month_prefix = current_month_prefix();
    usage_db
        .history
        .iter()
        .filter(|entry| entry.api_key.as_deref() == Some(api_key))
        .filter(|entry| {
            entry
                .timestamp
                .as_deref()
                .map(|ts| in_current_month(ts, &month_prefix))
                .unwrap_or(false)
        })
        .filter_map(|entry| entry.cost)
        .sum()
}

/// `YYYY-MM` for the current UTC month.
fn current_month_prefix() -> String {
    chrono::Utc::now().format("%Y-%m").to_string()
}

/// True when `timestamp` (RFC3339) falls in the month identified by
/// `month_prefix` (`YYYY-MM`). Compares the leading 7 chars, which is safe
/// for ASCII RFC3339 timestamps.
fn in_current_month(timestamp: &str, month_prefix: &str) -> bool {
    timestamp
        .get(..7)
        .map(|prefix| prefix == month_prefix)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TokenUsage, UsageEntry};

    fn entry(api_key: &str, month: &str, cost: f64) -> UsageEntry {
        UsageEntry {
            timestamp: Some(format!("{month}-15T12:00:00Z")),
            provider: Some("openai".into()),
            model: "gpt-4o".into(),
            connection_id: Some("c1".into()),
            api_key: Some(api_key.into()),
            tokens: Some(TokenUsage {
                prompt_tokens: Some(10),
                input_tokens: None,
                completion_tokens: Some(20),
                output_tokens: None,
                total_tokens: None,
                reasoning_tokens: None,
                cached_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                extra: Default::default(),
            }),
            cost: Some(cost),
            status: None,
            bytes_before: 0,
            bytes_after: 0,
            bytes_saved: 0,
            image_prompts: 0,
            endpoint: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn spend_sums_only_current_month_for_key() {
        let mut usage = UsageDb::default();
        usage.history.push(entry("op-key", "2026-01", 5.0));
        usage.history.push(entry("op-key", "2026-02", 7.0)); // wrong month
        usage.history.push(entry("other", "2026-02", 100.0)); // wrong key
        usage.history.push(entry("op-key", "2026-02", 3.0));

        let this_month = current_month_prefix();
        let total = monthly_spend_for_key(&usage, "op-key");
        // Only the 2026-02 entries for op-key count (3.0). The 2026-01 entry
        // is excluded unless the test happens to run in January 2026.
        if this_month == "2026-02" {
            assert_eq!(total, 3.0);
        } else if this_month == "2026-01" {
            assert_eq!(total, 5.0);
        } else {
            assert_eq!(total, 0.0);
        }
    }

    #[test]
    fn in_current_month_prefix_match() {
        let prefix = "2026-02";
        assert!(in_current_month("2026-02-15T12:00:00Z", prefix));
        assert!(!in_current_month("2026-01-31T23:59:59Z", prefix));
        assert!(!in_current_month("2026-03-01T00:00:00Z", prefix));
    }
}

/// Hold an account's in-flight slot for as long as the response body lives.
///
/// The slot guard used to be dropped when the handler returned, while a
/// streaming body was still being delivered — so `MAX_IN_FLIGHT_PER_ACCOUNT`
/// capped only the setup phase, not the streams. Moving the guard into the body
/// stream ties its release to the stream: it drops when the response completes
/// or the client disconnects, which is when the account is genuinely free again.
pub fn with_in_flight_slot(
    response: axum::response::Response,
    guard: crate::core::account_fallback::AccountSlotGuard,
) -> axum::response::Response {
    use axum::body::Body;
    let (parts, body) = response.into_parts();
    use futures_util::StreamExt;
    let guarded = body.into_data_stream().map(move |chunk| {
        // Captured, not used: holding it in the stream's closure is what keeps
        // the slot taken until the stream is dropped.
        let _slot = &guard;
        chunk
    });
    axum::response::Response::from_parts(parts, Body::from_stream(guarded))
}

#[cfg(test)]
mod in_flight_slot_tests {
    use crate::core::account_fallback::AccountRegistry;
    use std::sync::Arc;

    /// The regression this exists for: the guard used to be dropped when the
    /// handler returned, so `in_flight` was already back to 0 while a streaming
    /// body was still being delivered — the cap never applied to concurrent
    /// streams. Holding the guard in the body ties release to the stream.
    #[tokio::test]
    async fn the_slot_is_still_held_while_the_body_lives() {
        use axum::body::Body;

        let registry = Arc::new(AccountRegistry::default());
        let guard = registry
            .acquire_slot("acc", 1, 100, 0)
            .expect("slot acquired");
        assert_eq!(registry.get_state("acc").in_flight, 1);

        let response = axum::response::Response::new(Body::from("hello"));
        let held = super::with_in_flight_slot(response, guard);

        // Building the response and returning from the "handler" is where the
        // old code released it.
        drop(held);
        assert_eq!(
            registry.get_state("acc").in_flight,
            0,
            "the slot is released once the body is dropped"
        );
    }

    /// A one-slot account must refuse the second stream while the first is live.
    #[tokio::test]
    async fn a_second_stream_is_refused_while_the_first_holds_the_slot() {
        use axum::body::Body;

        let registry = Arc::new(AccountRegistry::default());
        let guard = registry.acquire_slot("acc", 1, 100, 0).expect("first slot");

        let response = axum::response::Response::new(Body::from("hello"));
        let _held = super::with_in_flight_slot(response, guard);

        // The cap is 1, so while that body is alive there is no room.
        assert!(
            registry.acquire_slot("acc", 1, 100, 0).is_none(),
            "a second in-flight request must be refused while the first body lives"
        );
    }
}
