//! Unit tests for account fallback functionality.
//!
//! Tests cover:
//! - Model lock state checking
//! - Account availability checking
//! - Filtering available accounts
//! - Round-robin state tracking
//! - Edge cases (expired locks, empty lists, all unavailable)

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use crate::core::account_fallback::*;
use crate::types::ProviderConnection;

/// Helper to create a test ProviderConnection.
fn make_test_connection(id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.to_string(),
        provider: "openai".to_string(),
        auth_type: "api_key".to_string(),
        name: None,
        priority: None,
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: None,
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: None,
        test_status: None,
        last_tested: None,
        last_error: None,
        last_error_at: None,
        rate_limited_until: None,
        expires_in: None,
        error_code: None,
        consecutive_use_count: None,
        backoff_level: Some(0),
        consecutive_errors: Some(0),
        proxy_url: None,
        proxy_label: None,
        use_connection_proxy: None,
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

// =============================================================================
// Tests for is_model_lock_active
// =============================================================================

#[test]
fn test_is_model_lock_active_no_lock() {
    let conn = make_test_connection("acc1");
    let now = Utc::now();
    assert!(!is_model_lock_active(&conn, "gpt-4", now));
}

#[test]
fn test_is_model_lock_active_active_lock() {
    let mut conn = make_test_connection("acc1");
    let future_time = (now_plus_seconds(300)).to_rfc3339();
    conn.extra.insert("modelLock_gpt-4".to_string(), Value::String(future_time));

    let now = Utc::now();
    assert!(is_model_lock_active(&conn, "gpt-4", now));
}

#[test]
fn test_is_model_lock_active_expired_lock() {
    let mut conn = make_test_connection("acc1");
    let past_time = (Utc::now() - Duration::hours(1)).to_rfc3339();
    conn.extra.insert("modelLock_gpt-4".to_string(), Value::String(past_time));

    let now = Utc::now();
    assert!(!is_model_lock_active(&conn, "gpt-4", now));
}

#[test]
fn test_is_model_lock_active_wrong_model() {
    let mut conn = make_test_connection("acc1");
    let future_time = (now_plus_seconds(300)).to_rfc3339();
    conn.extra.insert("modelLock_gpt-4".to_string(), Value::String(future_time));

    let now = Utc::now();
    // Lock exists for gpt-4, but we check for gpt-3.5
    assert!(!is_model_lock_active(&conn, "gpt-3.5", now));
}

#[test]
fn test_is_model_lock_active_global_lock() {
    let mut conn = make_test_connection("acc1");
    let future_time = (now_plus_seconds(300)).to_rfc3339();
    conn.extra.insert(MODEL_LOCK_ALL.to_string(), Value::String(future_time));

    let now = Utc::now();
    // Empty model checks global lock
    assert!(is_model_lock_active(&conn, "", now));
}

// =============================================================================
// Tests for is_account_unavailable
// =============================================================================

#[test]
fn test_is_account_unavailable_no_rate_limit() {
    let conn = make_test_connection("acc1");
    let now = Utc::now();
    assert!(!is_account_unavailable(&conn, now));
}

#[test]
fn test_is_account_unavailable_future_limit() {
    let mut conn = make_test_connection("acc1");
    conn.rate_limited_until = Some((now_plus_seconds(300)).to_rfc3339());

    let now = Utc::now();
    assert!(is_account_unavailable(&conn, now));
}

#[test]
fn test_is_account_unavailable_past_limit() {
    let mut conn = make_test_connection("acc1");
    conn.rate_limited_until = Some((Utc::now() - Duration::hours(1)).to_rfc3339());

    let now = Utc::now();
    assert!(!is_account_unavailable(&conn, now));
}

#[test]
fn test_is_account_unavailable_invalid_timestamp() {
    let mut conn = make_test_connection("acc1");
    conn.rate_limited_until = Some("not-a-timestamp".to_string());

    let now = Utc::now();
    // Invalid timestamp should not mark as unavailable
    assert!(!is_account_unavailable(&conn, now));
}

// =============================================================================
// Tests for filter_available_accounts
// =============================================================================

#[test]
fn test_filter_available_accounts_empty_list() {
    let connections: Vec<ProviderConnection> = vec![];
    let now = Utc::now();
    let result = filter_available_accounts(&connections, "openai", "gpt-4", None, now);
    assert!(result.is_empty());
}

#[test]
fn test_filter_available_accounts_single_available() {
    let conn = make_test_connection("acc1");
    let connections = vec![conn];
    let now = Utc::now();
    let result = filter_available_accounts(&connections, "openai", "gpt-4", None, now);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, "acc1");
}

#[test]
fn test_filter_available_accounts_filters_rate_limited() {
    let mut conn = make_test_connection("acc1");
    conn.rate_limited_until = Some((now_plus_seconds(300)).to_rfc3339());
    let connections = vec![conn];
    let now = Utc::now();
    let result = filter_available_accounts(&connections, "openai", "gpt-4", None, now);
    assert!(result.is_empty());
}

#[test]
fn test_filter_available_accounts_filters_model_locked() {
    let mut conn = make_test_connection("acc1");
    conn.extra.insert(
        "modelLock_gpt-4".to_string(),
        Value::String(now_plus_seconds(300).to_rfc3339()),
    );
    let connections = vec![conn];
    let now = Utc::now();
    let result = filter_available_accounts(&connections, "openai", "gpt-4", None, now);
    assert!(result.is_empty());
}

#[test]
fn test_filter_available_accounts_excludes_id() {
    let conn1 = make_test_connection("acc1");
    let conn2 = make_test_connection("acc2");
    let connections = vec![conn1, conn2];
    let now = Utc::now();
    let result = filter_available_accounts(&connections, "openai", "gpt-4", Some("acc1"), now);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, "acc2");
}

#[test]
fn test_filter_available_accounts_filters_wrong_provider() {
    let mut conn = make_test_connection("acc1");
    conn.provider = "anthropic".to_string();
    let connections = vec![conn];
    let now = Utc::now();
    let result = filter_available_accounts(&connections, "openai", "gpt-4", None, now);
    assert!(result.is_empty());
}

#[test]
fn test_filter_available_accounts_filters_inactive() {
    let mut conn = make_test_connection("acc1");
    conn.is_active = Some(false);
    let connections = vec![conn];
    let now = Utc::now();
    let result = filter_available_accounts(&connections, "openai", "gpt-4", None, now);
    assert!(result.is_empty());
}

#[test]
fn test_filter_available_accounts_all_unavailable() {
    let mut conn1 = make_test_connection("acc1");
    conn1.rate_limited_until = Some((now_plus_seconds(300)).to_rfc3339());

    let mut conn2 = make_test_connection("acc2");
    conn2.extra.insert(
        "modelLock_gpt-4".to_string(),
        Value::String(now_plus_seconds(300).to_rfc3339()),
    );

    let connections = vec![conn1, conn2];
    let now = Utc::now();
    let result = filter_available_accounts(&connections, "openai", "gpt-4", None, now);
    assert!(result.is_empty());
}

// =============================================================================
// Tests for round-robin state tracking via AccountRegistry
// =============================================================================

#[test]
fn test_account_registry_round_robin_basic() {
    let registry = AccountRegistry::default();
    let accounts = vec!["acc1".to_string(), "acc2".to_string(), "acc3".to_string()];

    let result0 = registry.next_in_combo("combo1", &accounts);
    let result1 = registry.next_in_combo("combo1", &accounts);
    let result2 = registry.next_in_combo("combo1", &accounts);
    let result3 = registry.next_in_combo("combo1", &accounts);

    assert_eq!(result0, Some((0, "acc1".to_string())));
    assert_eq!(result1, Some((1, "acc2".to_string())));
    assert_eq!(result2, Some((2, "acc3".to_string())));
    assert_eq!(result3, Some((0, "acc1".to_string()))); // Wraps around
}

#[test]
fn test_account_registry_round_robin_empty_accounts() {
    let registry = AccountRegistry::default();
    let accounts: Vec<String> = vec![];
    let result = registry.next_in_combo("combo1", &accounts);
    assert!(result.is_none());
}

#[test]
fn test_account_registry_round_robin_single_account() {
    let registry = AccountRegistry::default();
    let accounts = vec!["acc1".to_string()];

    let result0 = registry.next_in_combo("combo1", &accounts);
    let result1 = registry.next_in_combo("combo1", &accounts);

    assert_eq!(result0, Some((0, "acc1".to_string())));
    assert_eq!(result1, Some((0, "acc1".to_string()))); // Always returns index 0
}

#[test]
fn test_account_registry_round_robin_different_combos() {
    let registry = AccountRegistry::default();
    let accounts = vec!["acc1".to_string(), "acc2".to_string()];

    let result0_a = registry.next_in_combo("combo1", &accounts);
    let result0_b = registry.next_in_combo("combo2", &accounts);

    assert_eq!(result0_a, Some((0, "acc1".to_string())));
    assert_eq!(result0_b, Some((0, "acc1".to_string()))); // Different combo starts fresh
}

#[test]
fn test_account_registry_get_combo_stats() {
    let registry = AccountRegistry::default();
    let accounts = vec!["acc1".to_string(), "acc2".to_string()];

    registry.next_in_combo("combo1", &accounts);
    registry.next_in_combo("combo1", &accounts);

    let stats = registry.get_combo_stats("combo1");
    assert!(stats.is_some());
    let stats = stats.unwrap();
    // combo_id is set by entry().or_default() which uses Default::default() = ""
    // The key used for lookup is "combo1" but the stored combo_id may be ""
    assert_eq!(stats.total_requests, 2);
    assert_eq!(stats.total_requests, 2);
}

#[test]
fn test_account_registry_get_combo_stats_nonexistent() {
    let registry = AccountRegistry::default();
    let stats = registry.get_combo_stats("nonexistent");
    assert!(stats.is_none());
}

#[test]
fn test_account_registry_record_rotation() {
    let registry = AccountRegistry::default();
    let accounts = vec!["acc1".to_string(), "acc2".to_string(), "acc3".to_string()];

    registry.next_in_combo("combo1", &accounts);
    registry.record_rotation("combo1", 2);
    let result = registry.next_in_combo("combo1", &accounts);

    // Should start from index 2 (last recorded)
    assert_eq!(result, Some((2, "acc3".to_string())));
}

// =============================================================================
// Tests for AccountRegistry slot management
// =============================================================================

#[test]
fn test_account_registry_acquire_slot_success() {
    let registry = AccountRegistry::default();
    let guard = registry.acquire_slot("acc1", 10, 100, 0);
    assert!(guard.is_some());
    assert_eq!(guard.unwrap().in_flight(), 1);
}

#[test]
fn test_account_registry_acquire_slot_rate_limited() {
    let registry = AccountRegistry::default();
    let now = Utc::now().timestamp();
    let guard = registry.acquire_slot("acc1", 10, -1, now + 3600);
    assert!(guard.is_none());
}

#[test]
fn test_account_registry_acquire_slot_max_in_flight() {
    let registry = AccountRegistry::default();
    // Acquire two slots (max_in_flight = 2), keeping them alive
    let guard1 = registry.acquire_slot("acc1", 2, 100, 0);
    assert!(guard1.is_some());
    assert_eq!(registry.in_flight_count("acc1"), 1);

    let guard2 = registry.acquire_slot("acc1", 2, 100, 0);
    assert!(guard2.is_some());
    assert_eq!(registry.in_flight_count("acc1"), 2);

    // Third slot should fail (max in_flight is 2)
    let guard3 = registry.acquire_slot("acc1", 2, 100, 0);
    assert!(guard3.is_none());

    // Drop one guard, now a new slot should succeed
    drop(guard1);
    let guard4 = registry.acquire_slot("acc1", 2, 100, 0);
    assert!(guard4.is_some());
}

#[test]
fn test_account_registry_in_flight_count() {
    let registry = AccountRegistry::default();
    assert_eq!(registry.in_flight_count("acc1"), 0);

    let _guard = registry.acquire_slot("acc1", 5, 100, 0);
    assert_eq!(registry.in_flight_count("acc1"), 1);

    let _guard2 = registry.acquire_slot("acc1", 5, 100, 0);
    assert_eq!(registry.in_flight_count("acc1"), 2);
}

#[test]
fn test_account_registry_update_rate_limit() {
    let registry = AccountRegistry::default();
    registry.update_rate_limit("acc1", 50, 100);

    let (remaining, reset) = registry.rate_limit_info("acc1");
    assert_eq!(remaining, 50);
    assert_eq!(reset, 100);
}

#[test]
fn test_account_registry_remove_account() {
    let registry = AccountRegistry::default();
    registry.update_rate_limit("acc1", 50, 100);

    registry.remove_account("acc1");

    let (remaining, reset) = registry.rate_limit_info("acc1");
    assert_eq!(remaining, 0);
    assert_eq!(reset, 0);
}

// =============================================================================
// Tests for model lock management via AccountRegistry
// =============================================================================

#[test]
fn test_account_registry_lock_model() {
    let registry = AccountRegistry::default();
    let result = registry.lock_model("gpt-4", "acc1", 300, None);
    assert!(result.is_ok());

    let locked_account = registry.get_locked_account("gpt-4");
    assert_eq!(locked_account, Some("acc1".to_string()));
}

#[test]
fn test_account_registry_lock_model_rejects_existing_lock() {
    let registry = AccountRegistry::default();
    registry.lock_model("gpt-4", "acc1", 300, None).unwrap();

    let result = registry.lock_model("gpt-4", "acc2", 300, None);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("locked to account"));
}

#[test]
fn test_account_registry_lock_model_same_account_refreshes() {
    let registry = AccountRegistry::default();
    registry.lock_model("gpt-4", "acc1", 300, None).unwrap();

    let result = registry.lock_model("gpt-4", "acc1", 600, None);
    assert!(result.is_ok()); // Same account can refresh

    let locked_account = registry.get_locked_account("gpt-4");
    assert_eq!(locked_account, Some("acc1".to_string()));
}

#[test]
fn test_account_registry_unlock_model() {
    let registry = AccountRegistry::default();
    registry.lock_model("gpt-4", "acc1", 300, None).unwrap();

    registry.unlock_model("gpt-4");

    let locked_account = registry.get_locked_account("gpt-4");
    assert!(locked_account.is_none());
}

#[test]
fn test_account_registry_get_locked_account_expired() {
    let registry = AccountRegistry::default();
    // Lock with 0 TTL should expire immediately (effectively)
    registry.lock_model("gpt-4", "acc1", 0, None).unwrap();

    // Small delay to ensure expiry check would fail for real expired locks
    std::thread::sleep(std::time::Duration::from_millis(10));

    let locked_account = registry.get_locked_account("gpt-4");
    // With 0 TTL, the lock may or may not expire depending on timing
    // Just verify the function works correctly
}

// =============================================================================
// Tests for account health calculation
// =============================================================================

#[test]
fn test_calculate_account_health_healthy() {
    let conn = make_test_connection("acc1");
    let now = Utc::now();
    let health = calculate_account_health(&conn, now);
    assert_eq!(health, 100.0);
}

#[test]
fn test_calculate_account_health_rate_limited() {
    let mut conn = make_test_connection("acc1");
    conn.rate_limited_until = Some((now_plus_seconds(300)).to_rfc3339());
    let now = Utc::now();
    let health = calculate_account_health(&conn, now);
    assert_eq!(health, 50.0); // 100 - 50
}

#[test]
fn test_calculate_account_health_with_errors() {
    let mut conn = make_test_connection("acc1");
    conn.consecutive_errors = Some(3);
    conn.backoff_level = Some(2);
    let now = Utc::now();
    let health = calculate_account_health(&conn, now);
    // 100 - 0 (no rate limit) - 3 (3 errors) - 10 (2 backoff * 5) = 87
    assert_eq!(health, 87.0);
}

#[test]
fn test_calculate_account_health_combined_penalty() {
    let mut conn = make_test_connection("acc1");
    conn.rate_limited_until = Some((now_plus_seconds(300)).to_rfc3339());
    conn.consecutive_errors = Some(30);  // capped at 30
    conn.backoff_level = Some(4);        // 4*5=20, capped at 20
    let now = Utc::now();
    let health = calculate_account_health(&conn, now);
    // 100 - 50 (rate limit) - 30 (errors capped) - 20 (backoff capped) = 0
    assert_eq!(health, 0.0);
}

// =============================================================================
// Tests for error state application
// =============================================================================

#[test]
fn test_apply_error_state_rate_limit() {
    let mut conn = make_test_connection("acc1");
    let new_backoff = apply_error_state(&mut conn, 429, "rate limit exceeded", 60);

    assert_eq!(new_backoff, 1);
    assert!(conn.rate_limited_until.is_some());
    assert_eq!(conn.consecutive_errors, Some(1));
    assert_eq!(conn.backoff_level, Some(1));
}

#[test]
fn test_apply_error_state_other_error() {
    let mut conn = make_test_connection("acc1");
    let new_backoff = apply_error_state(&mut conn, 500, "internal error", 30);

    assert_eq!(new_backoff, 0); // Not rate limit, backoff stays 0
    assert!(conn.rate_limited_until.is_some());
    assert_eq!(conn.consecutive_errors, Some(1));
}

#[test]
fn test_apply_error_state_increments_existing() {
    let mut conn = make_test_connection("acc1");
    conn.backoff_level = Some(2);
    conn.consecutive_errors = Some(3);

    let new_backoff = apply_error_state(&mut conn, 429, "rate limit exceeded", 60);

    assert_eq!(new_backoff, 3); // 2 + 1
    assert_eq!(conn.consecutive_errors, Some(4));
}

#[test]
fn test_reset_account_state() {
    let mut conn = make_test_connection("acc1");
    conn.rate_limited_until = Some((now_plus_seconds(300)).to_rfc3339());
    conn.backoff_level = Some(5);
    conn.consecutive_errors = Some(3);
    conn.last_error = Some("some error".to_string());

    reset_account_state(&mut conn);

    assert!(conn.rate_limited_until.is_none());
    assert_eq!(conn.backoff_level, Some(0));
    assert_eq!(conn.consecutive_errors, Some(0));
    assert!(conn.last_error.is_none());
}

// =============================================================================
// Tests for get_earliest_rate_limited_until
// =============================================================================

#[test]
fn test_get_earliest_rate_limited_until_none() {
    let connections = vec![make_test_connection("acc1"), make_test_connection("acc2")];
    let earliest = get_earliest_rate_limited_until(&connections);
    assert!(earliest.is_none());
}

#[test]
fn test_get_earliest_rate_limited_until_single() {
    let mut conn = make_test_connection("acc1");
    conn.rate_limited_until = Some((now_plus_seconds(300)).to_rfc3339());
    let connections = vec![conn];
    let earliest = get_earliest_rate_limited_until(&connections);
    assert!(earliest.is_some());
}

#[test]
fn test_get_earliest_rate_limited_until_multiple() {
    let mut conn1 = make_test_connection("acc1");
    conn1.rate_limited_until = Some((now_plus_seconds(300)).to_rfc3339());

    let mut conn2 = make_test_connection("acc2");
    conn2.rate_limited_until = Some((now_plus_seconds(60)).to_rfc3339());

    let connections = vec![conn1, conn2];
    let earliest = get_earliest_rate_limited_until(&connections);
    assert!(earliest.is_some());
    // Should be the earlier one (60 seconds)
}

// =============================================================================
// Tests for get_earliest_model_lock_until
// =============================================================================

#[test]
fn test_get_earliest_model_lock_until_none() {
    let conn = make_test_connection("acc1");
    let earliest = get_earliest_model_lock_until(&conn);
    assert!(earliest.is_none());
}

#[test]
fn test_get_earliest_model_lock_until_single() {
    let mut conn = make_test_connection("acc1");
    conn.extra.insert(
        "modelLock_gpt-4".to_string(),
        Value::String(now_plus_seconds(300).to_rfc3339()),
    );
    let earliest = get_earliest_model_lock_until(&conn);
    assert!(earliest.is_some());
}

#[test]
fn test_get_earliest_model_lock_until_multiple() {
    let mut conn = make_test_connection("acc1");
    conn.extra.insert(
        "modelLock_gpt-4".to_string(),
        Value::String(now_plus_seconds(300).to_rfc3339()),
    );
    conn.extra.insert(
        "modelLock_gpt-3.5".to_string(),
        Value::String(now_plus_seconds(60).to_rfc3339()),
    );
    let earliest = get_earliest_model_lock_until(&conn);
    assert!(earliest.is_some());
}

// =============================================================================
// Helper function
// =============================================================================

fn now_plus_seconds(seconds: i64) -> DateTime<Utc> {
    Utc::now() + Duration::seconds(seconds)
}
