//! Budget caps & hard kill-switch tests (free-tier Feature 3).
//!
//! Exercises `budget_guard::enforce_budget` directly:
//! - over budget  → 429 with `X-Budget-Remaining: 0` + `openproxy.v1.budget.exceeded`
//! - under budget → `Ok(Some(remaining))`
//! - no budget     → `Ok(None)` (unlimited)
//! - no key presented → `Ok(None)` (nothing to enforce)

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::to_bytes;
use axum::http::{header::HeaderValue, StatusCode};
use axum::response::IntoResponse;
use openproxy::db::Db;
use openproxy::server::api::budget_guard::{
    enforce_budget, with_budget_header, BUDGET_EXCEEDED_SCHEMA, BUDGET_REMAINING_HEADER,
};
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, TokenUsage, UsageEntry};
use tempfile::tempdir;

const BUDGET_KEY: &str = "budget-key";

fn key_with_budget(budget: Option<f64>) -> ApiKey {
    ApiKey {
        id: "k1".into(),
        name: "budgeted".into(),
        key: BUDGET_KEY.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: Some("2026-01-01T00:00:00Z".into()),
        monthly_budget_usd: budget,
        extra: BTreeMap::new(),
    }
}

fn usage_entry(api_key: &str, cost: f64) -> UsageEntry {
    UsageEntry {
        timestamp: Some(chrono::Utc::now().to_rfc3339()),
        provider: Some("openai".into()),
        model: "gpt-4o".into(),
        connection_id: Some("c1".into()),
        api_key: Some(api_key.into()),
        tokens: Some(TokenUsage {
            prompt_tokens: Some(1),
            input_tokens: None,
            completion_tokens: Some(1),
            output_tokens: None,
            total_tokens: None,
            reasoning_tokens: None,
            cached_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            extra: BTreeMap::new(),
        }),
        cost: Some(cost),
        status: None,
        endpoint: None,
        bytes_before: 0,
        bytes_after: 0,
        bytes_saved: 0,
        image_prompts: 0,
        extra: BTreeMap::new(),
    }
}

async fn build_state(key: ApiKey, usage: Vec<UsageEntry>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys.push(key);
    })
    .await
    .expect("seed key");
    if !usage.is_empty() {
        db.update_usage(|usage_db| {
            for entry in usage {
                usage_db.history.push(entry);
            }
        })
        .await
        .expect("seed usage");
    }
    AppState::new(db)
}

#[tokio::test]
async fn over_budget_returns_429_with_header_and_schema() {
    let state = build_state(
        key_with_budget(Some(10.0)),
        vec![usage_entry(BUDGET_KEY, 20.0)],
    )
    .await;

    let result = enforce_budget(&state, Some(BUDGET_KEY));
    let Err(response) = result else {
        panic!("expected a 429 budget-exceeded response");
    };

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response.headers().get(BUDGET_REMAINING_HEADER),
        Some(&HeaderValue::from_static("0"))
    );

    let bytes = to_bytes(response.into_body(), 4096)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    assert_eq!(body["schema"], BUDGET_EXCEEDED_SCHEMA);
    assert_eq!(body["error"]["code"], "budget_exceeded");
}

#[tokio::test]
async fn under_budget_passes_with_remaining() {
    let state = build_state(
        key_with_budget(Some(10.0)),
        vec![usage_entry(BUDGET_KEY, 3.0)],
    )
    .await;

    let result = enforce_budget(&state, Some(BUDGET_KEY));
    let Ok(remaining) = result else {
        panic!("expected Ok with remaining budget");
    };
    assert_eq!(remaining, Some(7.0));
}

#[tokio::test]
async fn no_budget_is_unlimited() {
    let state = build_state(key_with_budget(None), vec![usage_entry(BUDGET_KEY, 999.0)]).await;

    let result = enforce_budget(&state, Some(BUDGET_KEY));
    assert_eq!(result.expect("Ok"), None);
}

#[tokio::test]
async fn no_key_presented_skips_enforcement() {
    let state = build_state(key_with_budget(Some(5.0)), vec![]).await;

    let result = enforce_budget(&state, None);
    assert_eq!(result.expect("Ok"), None);
}

#[tokio::test]
async fn unknown_key_skips_enforcement() {
    let state = build_state(key_with_budget(Some(5.0)), vec![]).await;

    let result = enforce_budget(&state, Some("not-a-real-key"));
    assert_eq!(result.expect("Ok"), None);
}

#[test]
fn with_budget_header_stamps_when_set_and_omits_when_none() {
    let ok = (StatusCode::OK, "ok").into_response();

    let stamped = with_budget_header(ok, Some(42.12));
    assert_eq!(
        stamped.headers().get(BUDGET_REMAINING_HEADER),
        Some(&HeaderValue::from_static("42.12"))
    );

    let none = (StatusCode::OK, "ok").into_response();
    let unstamped = with_budget_header(none, None);
    assert!(unstamped.headers().get(BUDGET_REMAINING_HEADER).is_none());
}
