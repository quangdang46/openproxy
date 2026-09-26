//! Bead openproxy-7lnq — the cooldown table and the activation reset, pinned
//! against 9router's two rule lists.
//!
//! 9router has no "permanent error" tier anywhere. `ERROR_RULES`
//! (`open-sse/config/errorConfig.js:59-76`) declares 401/402/403/404 as long
//! cooldowns and 429 as backoff; `checkFallbackError`
//! (`open-sse/services/accountFallback.js:23-50`) walks it and, for anything
//! unmatched, returns the 30 s `TRANSIENT_COOLDOWN_MS`. A 400 is the most
//! common recoverable 4xx on these upstreams and it clears on the next
//! well-formed request, so giving it a longer lock than a 502 is a pure
//! over-penalty.
//!
//! The second half is `resetHealthStateOnActivation`
//! (`src/lib/db/repos/connectionsRepo.js:15-33`): 9router expands *any* write
//! that lands a connection on `testStatus: "active"` into a full health reset,
//! so re-enabling a connection is a routing decision, not a label. The unit
//! tests next to each call site cover the per-site plumbing; this file pins the
//! shared contract — the table itself, and the fact that a reset makes a locked
//! account selectable again through the predicates dispatch actually calls.

use std::time::Duration;

use chrono::Utc;
use openproxy::core::account_fallback::{
    is_account_unavailable, is_model_lock_active, reset_health_state_on_activation,
    MODEL_LOCK_PREFIX,
};
use openproxy::core::combo::{check_fallback_error, get_quota_cooldown};
use openproxy::core::config::error_config::{
    classify_error, ErrorClassification, COOLDOWN_LONG_MS, COOLDOWN_SHORT_MS, ERROR_RULES,
};
use openproxy::types::ProviderConnection;

const FAR_FUTURE: &str = "2099-01-01T00:00:00Z";

/// Every status rule 9router declares, with the cooldown it must produce.
/// Anything not listed here must land on the 30 s transient default.
#[test]
fn cooldown_table_matches_the_js_rule_list() {
    for status in [401u16, 402, 403, 404] {
        assert_eq!(
            check_fallback_error(status, "boom", 0).cooldown,
            Duration::from_secs(120),
            "status {status} takes the long cooldown"
        );
    }
    for status in [400u16, 406, 418, 500, 502, 503, 504] {
        assert_eq!(
            check_fallback_error(status, "boom", 0).cooldown,
            Duration::from_secs(30),
            "status {status} has no rule and takes the transient default"
        );
    }
}

#[test]
fn every_status_rule_is_backoff_or_long_cooldown() {
    for status in [401u16, 402, 403, 404] {
        let rule = ERROR_RULES
            .iter()
            .find(|r| r.status == Some(status))
            .unwrap_or_else(|| panic!("no status rule for {status}"));
        assert!(!rule.backoff, "status {status} is a fixed cooldown");
        assert_eq!(
            rule.cooldown,
            Some(Duration::from_millis(COOLDOWN_LONG_MS)),
            "status {status}"
        );
    }
    let rule = ERROR_RULES
        .iter()
        .find(|r| r.status == Some(429))
        .expect("no status rule for 429");
    assert!(rule.backoff, "429 is the one backoff status rule");
    assert_eq!(rule.cooldown, None);
}

#[test]
fn an_unmatched_400_is_classified_as_a_plain_no_match() {
    // There is no `Permanent` variant to resolve into — the caller sees
    // `NoMatch` and applies its own 30 s default.
    assert_eq!(
        classify_error(Some("unsupported parameter: max_tokens"), Some(400)),
        ErrorClassification::NoMatch
    );
    assert_eq!(
        classify_error(Some("The model `gpt-9` does not exist"), Some(404)),
        ErrorClassification::Cooldown(Duration::from_millis(COOLDOWN_LONG_MS))
    );
}

/// A text rule still wins over the status default, and it fires even when the
/// status would otherwise take a different tier.
#[test]
fn text_rules_take_priority_over_status_rules() {
    assert_eq!(
        check_fallback_error(400, "improperly formed request", 0).cooldown,
        Duration::from_millis(COOLDOWN_LONG_MS)
    );
    assert_eq!(
        check_fallback_error(400, "request not allowed", 0).cooldown,
        Duration::from_millis(COOLDOWN_SHORT_MS)
    );
    // "no credentials" outranks a 429 on the same response.
    let decision = check_fallback_error(429, "no credentials found for this account", 0);
    assert_eq!(decision.cooldown, Duration::from_millis(COOLDOWN_LONG_MS));
    assert!(decision.new_backoff_level.is_none());
}

/// `newBackoffLevel ?? backoffLevel` (accountFallback.js:211) — only a rule
/// carrying `backoff: true` returns a level, so nothing else escalates.
#[test]
fn only_backoff_failures_advance_the_level() {
    assert_eq!(
        check_fallback_error(429, "boom", 4).new_backoff_level,
        Some(5)
    );
    assert_eq!(
        check_fallback_error(429, "boom", 4).cooldown,
        get_quota_cooldown(5)
    );
    for status in [400u16, 401, 402, 403, 404, 500] {
        assert_eq!(
            check_fallback_error(status, "boom", 4).new_backoff_level,
            None,
            "status {status} must leave the level alone"
        );
    }
}

#[test]
fn every_failure_falls_back_to_the_next_member() {
    for status in [400u16, 401, 402, 403, 404, 429, 500, 503] {
        assert!(
            check_fallback_error(status, "boom", 0).should_fallback,
            "9router always falls back, status {status} included"
        );
    }
}

fn cooled_connection() -> ProviderConnection {
    let mut conn = ProviderConnection {
        id: "c1".into(),
        provider: "openai".into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        api_key: Some("sk-test".into()),
        test_status: Some("unavailable".into()),
        last_error: Some("rate limited".into()),
        last_error_at: Some("2026-01-01T00:00:00+00:00".into()),
        error_code: Some("429".into()),
        rate_limited_until: Some(FAR_FUTURE.into()),
        backoff_level: Some(6),
        consecutive_errors: Some(7),
        ..Default::default()
    };
    conn.extra.insert(
        format!("{MODEL_LOCK_PREFIX}gpt-4.1"),
        serde_json::Value::String(FAR_FUTURE.into()),
    );
    conn.extra.insert(
        format!("{MODEL_LOCK_PREFIX}claude-opus-4"),
        serde_json::Value::String(FAR_FUTURE.into()),
    );
    conn.extra.insert(
        "proxyPoolId".into(),
        serde_json::Value::String("pool-7".into()),
    );
    conn
}

/// The behavioural core of `resetHealthStateOnActivation`: after a write lands
/// the connection on "active", the predicates dispatch calls both say the
/// account is usable again. Before the reset existed they still said otherwise,
/// so the dashboard showed a healthy provider that nothing could route to.
#[test]
fn a_reset_makes_a_cooled_account_selectable_again() {
    let mut conn = cooled_connection();

    assert!(
        is_model_lock_active(&conn, "gpt-4.1", Utc::now()),
        "precondition: the model lock is live"
    );
    assert!(
        is_account_unavailable(&conn, Utc::now()),
        "precondition: the rate-limit window is open"
    );

    // The dashboard "Test" button's write: test_status lands on "active".
    conn.test_status = Some("active".into());
    assert!(reset_health_state_on_activation(&mut conn));

    assert!(!is_model_lock_active(&conn, "gpt-4.1", Utc::now()));
    assert!(!is_model_lock_active(&conn, "claude-opus-4", Utc::now()));
    assert!(!is_account_unavailable(&conn, Utc::now()));
    assert_eq!(conn.backoff_level, Some(0));
    assert_eq!(conn.consecutive_errors, Some(0));
    assert!(conn.error_code.is_none());
    assert_eq!(
        conn.extra
            .get("proxyPoolId")
            .and_then(serde_json::Value::as_str),
        Some("pool-7"),
        "the reset must not touch unrelated connection data"
    );
}

/// The error path is the one write that must not clear its own state.
#[test]
fn a_failed_write_leaves_the_lock_in_place() {
    let mut conn = cooled_connection();
    conn.test_status = Some("unavailable".into());

    assert!(!reset_health_state_on_activation(&mut conn));
    assert!(is_model_lock_active(&conn, "gpt-4.1", Utc::now()));
    assert_eq!(conn.error_code.as_deref(), Some("429"));
    assert_eq!(conn.backoff_level, Some(6));
}
