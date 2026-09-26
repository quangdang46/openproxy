//! Audit findings N19 (cost model) and N21 (`/api/usage/chart` shape) against
//! `.tmp/9router`:
//!
//! - `open-sse/providers/pricing.js:418-449` — `calculateCostFromTokens`
//!   subtracts the cache subsets from `prompt_tokens` before charging the input
//!   rate, and bills reasoning tokens at their own rate.
//! - `src/app/api/usage/chart/route.js:9` — an omitted `period` is `7d`, and the
//!   response is the bucket array itself, keyed `label` (`usageRepo.js:661`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::core::usage::{Pricing, UsageTracker};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderNode, TokenUsage, UsageEntry};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "usage-chart-cost-test-key";

/// Token usage on a model whose rates mirror a cache-discounting provider:
/// input 3.0, cached reads 0.3, cache writes 3.75, output 15.0.
fn anthropic_style_pricing() -> Value {
    json!({
        "input": 3.0,
        "output": 15.0,
        "cached": 0.3,
        "cache_creation": 3.75,
    })
}

fn token_usage(prompt: u64, cache_read: u64) -> TokenUsage {
    TokenUsage {
        prompt_tokens: Some(prompt),
        input_tokens: None,
        completion_tokens: Some(0),
        output_tokens: None,
        total_tokens: Some(prompt),
        reasoning_tokens: None,
        cached_tokens: None,
        cache_read_input_tokens: Some(cache_read),
        cache_creation_input_tokens: None,
        extra: BTreeMap::new(),
    }
}

async fn build_test_app() -> axum::Router {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));

    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "test-key-id".to_string(),
            name: "test".to_string(),
            key: TEST_KEY.to_string(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: Default::default(),
            monthly_budget_usd: None,
        }];
        state.settings.require_login = false;
        state.provider_nodes = vec![ProviderNode {
            id: "openai".to_string(),
            r#type: "provider".to_string(),
            name: "OpenAI".to_string(),
            prefix: None,
            api_type: None,
            base_url: None,
            created_at: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }];
    })
    .await
    .expect("seed auth");

    db.update_usage(|usage| {
        usage.history = vec![UsageEntry {
            timestamp: Some("2026-05-06T10:15:00Z".to_string()),
            provider: Some("openai".to_string()),
            model: "gpt-4.1".to_string(),
            tokens: Some(token_usage(100, 0)),
            connection_id: Some("conn-1".to_string()),
            api_key: None,
            endpoint: Some("/v1/chat/completions".to_string()),
            cost: Some(0.5),
            status: Some("success".to_string()),
            bytes_before: 0,
            bytes_after: 0,
            bytes_saved: 0,
            image_prompts: 0,
            extra: BTreeMap::new(),
        }];
        usage.total_requests_lifetime = 1;
    })
    .await
    .expect("seed usage");

    openproxy::build_app(AppState::new(db))
}

async fn get_chart(period: Option<&str>) -> Value {
    let app = build_test_app().await;
    let uri = match period {
        Some(p) => format!("/api/usage/chart?period={p}"),
        None => "/api/usage/chart".to_string(),
    };
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).expect("chart json")
}

// ---------------------------------------------------------------- N21 shape

/// 9router answers the bare array. The dashboard reads `label` off each bucket
/// and unwraps nothing, so an envelope or a `date` key silently blanks the
/// chart — this pins both.
#[tokio::test]
async fn usage_chart_returns_a_bare_label_array() {
    let json = get_chart(Some("7d")).await;

    let buckets = json.as_array().expect("chart body is a bare JSON array");
    assert_eq!(buckets.len(), 7);
    assert!(
        json.get("data").is_none(),
        "the `{{ data: [...] }}` envelope is gone"
    );

    let first = &buckets[0];
    assert!(first["label"].is_string(), "got {first}");
    assert!(first["tokens"].is_number(), "got {first}");
    assert!(first["cost"].is_number(), "got {first}");
    assert!(
        first.get("date").is_none(),
        "the bucket key is `label`, not `date`: {first}"
    );
}

/// chart/route.js:9 — `searchParams.get("period") || "7d"`. An omitted period
/// used to mean `today`, which is 24 hourly buckets; a request that asks for
/// nothing gets a week of daily buckets, so it must not get 24 of them.
#[tokio::test]
async fn usage_chart_defaults_to_seven_days() {
    let defaulted = get_chart(None).await;
    let explicit = get_chart(Some("7d")).await;
    assert_eq!(defaulted, explicit);
    assert_eq!(defaulted.as_array().expect("array").len(), 7);

    let today = get_chart(Some("today")).await;
    assert_eq!(
        today.as_array().expect("array").len(),
        24,
        "`today` is still the 24-hourly-bucket view"
    );
    assert_ne!(defaulted, today);
}

// ----------------------------------------------------------------- N19 cost

/// 9router's `Math.max(0, input - cached - cacheCreation)` guards the
/// subtraction for a provider whose `input_tokens` excludes its cache counters
/// (Anthropic's does). Without the floor the turn under-counts to nothing.
#[test]
fn a_cache_count_larger_than_the_input_still_bills_the_cache_rate() {
    let pricing = Pricing::from_db(&BTreeMap::from([(
        "anthropic".to_string(),
        BTreeMap::from([("claude-test".to_string(), anthropic_style_pricing())]),
    )]));

    let cost = pricing.calculate_cost("anthropic", "claude-test", 100, 0, 0, 1_000_000);
    // 1M cache reads @ 0.3/1M, and the input rate covers nothing.
    assert!(
        (cost - 0.3).abs() < 0.0001,
        "expected the 1M cached tokens to bill $0.30, got {cost}"
    );
}

/// A half-cached turn must be billed once: 500k of the 1M prompt at the input
/// rate, 500k at the cache-read rate. The pre-fix sum charged all 1M at the
/// input rate as well, roughly tripling the figure.
#[test]
fn a_half_cached_turn_is_billed_once_through_the_tracker() {
    let temp = tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let db = rt.block_on(async {
        let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
        db.update(|state| {
            state.pricing = BTreeMap::from([(
                "anthropic".to_string(),
                BTreeMap::from([("claude-test".to_string(), anthropic_style_pricing())]),
            )]);
        })
        .await
        .expect("seed pricing");
        db
    });

    let tracker = UsageTracker::new(db.clone());
    rt.block_on(tracker.track_request(
        "anthropic",
        "claude-test",
        Some(&token_usage(1_000_000, 500_000)),
        Some("conn-1"),
        None,
        Some("/v1/messages"),
        None,
    ));

    let usage = tracker.get_usage_db();
    let entry = usage
        .history
        .last()
        .expect("track_request wrote a history row");
    let cost = entry.cost.expect("a cost was persisted");

    // 500k @ 3.0/1M = 1.5, plus 500k @ 0.3/1M = 0.15.
    assert!(
        (cost - 1.65).abs() < 0.001,
        "expected $1.65, got {cost} — the cache subset is being billed twice"
    );
}
