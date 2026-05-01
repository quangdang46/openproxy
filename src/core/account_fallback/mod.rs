//! Account fallback utilities for multi-account routing.
//!
//! Mirrors the functionality of `open-sse/services/accountFallback.js`.
//! Provides per-account state tracking, health scoring, and fallback routing logic.

use chrono::{DateTime, Utc};

use crate::types::ProviderConnection;

/// Prefix for model lock flat fields on connection record.
pub const MODEL_LOCK_PREFIX: &str = "modelLock_";

/// Special key used when no model is known (account-level lock).
pub const MODEL_LOCK_ALL: &str = "modelLock___all";

/// Maximum backoff level to prevent infinite growth.
pub const MAX_BACKOFF_LEVEL: u32 = 15;

/// Base cooldown in milliseconds for exponential backoff.
pub const BACKOFF_BASE_MS: u64 = 2_000;

/// Maximum cooldown in milliseconds (5 minutes).
pub const BACKOFF_MAX_MS: u64 = 5 * 60 * 1_000;

/// Transient error cooldown duration.
pub const TRANSIENT_COOLDOWN_SECS: i64 = 30;

/// Long cooldown for credential/auth errors.
pub const LONG_COOLDOWN_SECS: i64 = 120;

/// Short cooldown for minor errors.
pub const SHORT_COOLDOWN_SECS: i64 = 5;

/// Get the flat field key for a model lock.
pub fn get_model_lock_key(model: &str) -> String {
    if model.is_empty() {
        MODEL_LOCK_ALL.to_string()
    } else {
        format!("{}{}", MODEL_LOCK_PREFIX, model)
    }
}

/// Check if a model lock on a connection is still active.
/// Reads flat field `modelLock_${model}` (or `modelLock___all` when model="").
pub fn is_model_lock_active(connection: &ProviderConnection, model: &str, now: DateTime<Utc>) -> bool {
    let key = get_model_lock_key(model);
    connection
        .extra
        .get(&key)
        .and_then(|v| v.as_str())
        .and_then(|s| parse_timestamp(s))
        .is_some_and(|until| until > now)
}

/// Check if account is currently unavailable (cooldown not expired).
pub fn is_account_unavailable(connection: &ProviderConnection, now: DateTime<Utc>) -> bool {
    connection
        .rate_limited_until
        .as_deref()
        .and_then(parse_timestamp)
        .is_some_and(|until| until > now)
}

/// Get earliest active model lock expiry across all modelLock_* fields.
/// Used for UI cooldown display.
pub fn get_earliest_model_lock_until(connection: &ProviderConnection) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    let mut earliest: Option<DateTime<Utc>> = None;

    for (key, value) in connection.extra.iter() {
        if !key.starts_with(MODEL_LOCK_PREFIX) {
            continue;
        }
        let Some(ts) = value.as_str().and_then(parse_timestamp) else {
            continue;
        };
        if ts <= now {
            continue;
        }
        earliest = Some(match earliest {
            Some(current) if current <= ts => current,
            _ => ts,
        });
    }
    earliest
}

/// Filter available accounts (not in cooldown).
/// Returns accounts that are not rate-limited and not in the excluded set.
pub fn filter_available_accounts<'a>(
    connections: &'a [ProviderConnection],
    provider: &str,
    model: &str,
    exclude_id: Option<&str>,
    now: DateTime<Utc>,
) -> Vec<&'a ProviderConnection> {
    connections
        .iter()
        .filter(|conn| {
            // Must match provider
            if conn.provider != provider {
                return false;
            }
            // Must be active
            if !conn.is_active() {
                return false;
            }
            // Must not be in excluded set
            if let Some(exclude) = exclude_id {
                if &conn.id == exclude {
                    return false;
                }
            }
            // Must not be rate limited
            if is_account_unavailable(conn, now) {
                return false;
            }
            // Must not have model lock
            if is_model_lock_active(conn, model, now) {
                return false;
            }
            true
        })
        .collect()
}

/// Calculate account health score based on error state.
/// Higher score = healthier account.
/// Score ranges from 0-100.
pub fn calculate_account_health(connection: &ProviderConnection, now: DateTime<Utc>) -> f64 {
    let mut score = 100.0;

    // Penalize if rate limited
    if is_account_unavailable(connection, now) {
        score -= 50.0;
    }

    // Penalize based on consecutive errors
    let errors = connection.consecutive_errors.unwrap_or(0);
    score -= (errors as f64).min(30.0);

    // Penalize based on backoff level
    let backoff = connection.backoff_level.unwrap_or(0);
    score -= (backoff as f64 * 5.0).min(20.0);

    score.max(0.0)
}

/// Get the earliest rateLimitedUntil from a list of connections.
/// Returns the earliest future rate-limit expiry, or None if none are rate-limited.
pub fn get_earliest_rate_limited_until(connections: &[ProviderConnection]) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    let mut earliest: Option<DateTime<Utc>> = None;

    for conn in connections {
        let Some(until) = conn
            .rate_limited_until
            .as_deref()
            .and_then(parse_timestamp)
        else {
            continue;
        };
        if until <= now {
            continue;
        }
        earliest = Some(match earliest {
            Some(current) if current <= until => current,
            _ => until,
        });
    }
    earliest
}

/// Reset account state when request succeeds.
/// Clears cooldown and resets backoff level and consecutive errors.
pub fn reset_account_state(connection: &mut ProviderConnection) {
    connection.rate_limited_until = None;
    connection.backoff_level = Some(0);
    connection.consecutive_errors = Some(0);
    connection.last_error = None;
    connection.last_error_at = None;
    connection.error_code = None;
    connection.test_status = None;
}

/// Apply error state to account, incrementing error counters and setting cooldown.
/// Returns the new backoff level.
pub fn apply_error_state(
    connection: &mut ProviderConnection,
    status: u16,
    error_text: &str,
    cooldown_seconds: i64,
) -> u32 {
    let current_backoff = connection.backoff_level.unwrap_or(0);
    let current_errors = connection.consecutive_errors.unwrap_or(0);

    // Calculate new backoff level based on error type
    let new_backoff = if status == 429 || error_text.to_lowercase().contains("rate limit") {
        (current_backoff + 1).min(MAX_BACKOFF_LEVEL)
    } else {
        current_backoff
    };

    connection.rate_limited_until = Some(
        (Utc::now() + chrono::Duration::seconds(cooldown_seconds)).to_rfc3339(),
    );
    connection.backoff_level = Some(new_backoff);
    connection.consecutive_errors = Some(current_errors.saturating_add(1));
    connection.last_error = Some(error_text.chars().take(200).collect());
    connection.last_error_at = Some(Utc::now().to_rfc3339());
    connection.error_code = Some(status.to_string());
    connection.test_status = Some("unavailable".to_string());

    new_backoff
}

/// Build update object to set a model lock on a connection.
pub fn build_model_lock_update(model: &str, cooldown_seconds: i64) -> (String, String) {
    let key = get_model_lock_key(model);
    let until = (Utc::now() + chrono::Duration::seconds(cooldown_seconds)).to_rfc3339();
    (key, until)
}

/// Build update object to clear all model locks on a connection.
pub fn build_clear_model_locks_update() -> Vec<(String, Option<String>)> {
    Vec::new()
}

/// Parse RFC3339 timestamp string into DateTime<Utc>.
fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_connection(id: &str) -> ProviderConnection {
        use std::collections::BTreeMap;
        use serde_json::Value;

        ProviderConnection {
            id: id.to_string(),
            provider: "test".to_string(),
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
            provider_specific_data: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn test_get_model_lock_key() {
        assert_eq!(get_model_lock_key("gpt-4"), "modelLock_gpt-4");
        assert_eq!(get_model_lock_key(""), "modelLock___all");
    }

    #[test]
    fn test_filter_available_accounts_empty() {
        let connections = vec![];
        let now = Utc::now();
        let result = filter_available_accounts(&connections, "test", "model", None, now);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_available_accounts_filters_rate_limited() {
        let mut conn = make_connection("conn1");
        conn.provider = "openai".to_string();
        conn.rate_limited_until = Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339());

        let connections = vec![conn];
        let now = Utc::now();
        let result = filter_available_accounts(&connections, "openai", "gpt-4", None, now);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_available_accounts_excludes_id() {
        let conn1 = make_connection("conn1");
        let conn2 = make_connection("conn2");

        let connections = vec![conn1, conn2];
        let now = Utc::now();
        let result = filter_available_accounts(&connections, "test", "model", Some("conn1"), now);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "conn2");
    }

    #[test]
    fn test_calculate_account_health_no_errors() {
        let conn = make_connection("healthy");
        let now = Utc::now();
        let health = calculate_account_health(&conn, now);
        assert_eq!(health, 100.0);
    }

    #[test]
    fn test_calculate_account_health_with_errors() {
        let mut conn = make_connection("unhealthy");
        conn.consecutive_errors = Some(5);
        conn.backoff_level = Some(3);

        let now = Utc::now();
        let health = calculate_account_health(&conn, now);
        assert_eq!(health, 80.0);
    }

    #[test]
    fn test_calculate_account_health_rate_limited() {
        let mut conn = make_connection("rate-limited");
        conn.rate_limited_until = Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339());

        let now = Utc::now();
        let health = calculate_account_health(&conn, now);
        // 100 - 50 (rate limited) = 50
        assert_eq!(health, 50.0);
    }

    #[test]
    fn test_reset_account_state() {
        let mut conn = make_connection("test");
        conn.rate_limited_until = Some("2025-01-01T00:00:00Z".to_string());
        conn.backoff_level = Some(5);
        conn.consecutive_errors = Some(3);
        conn.last_error = Some("some error".to_string());

        reset_account_state(&mut conn);

        assert!(conn.rate_limited_until.is_none());
        assert_eq!(conn.backoff_level, Some(0));
        assert_eq!(conn.consecutive_errors, Some(0));
        assert!(conn.last_error.is_none());
    }

    #[test]
    fn test_apply_error_state() {
        let mut conn = make_connection("test");
        conn.backoff_level = Some(2);

        let new_backoff = apply_error_state(&mut conn, 429, "rate limit exceeded", 60);

        assert_eq!(new_backoff, 3); // incremented from 2
        assert!(conn.rate_limited_until.is_some());
        assert_eq!(conn.consecutive_errors, Some(1));
        assert!(conn.last_error.is_some());
    }

    #[test]
    fn test_is_account_unavailable() {
        let mut conn = make_connection("test");
        let now = Utc::now();

        // No rate limit
        assert!(!is_account_unavailable(&conn, now));

        // Future rate limit
        conn.rate_limited_until = Some((now + chrono::Duration::hours(1)).to_rfc3339());
        assert!(is_account_unavailable(&conn, now));

        // Past rate limit
        conn.rate_limited_until = Some((now - chrono::Duration::hours(1)).to_rfc3339());
        assert!(!is_account_unavailable(&conn, now));
    }

    #[test]
    fn test_get_earliest_rate_limited_until() {
        let mut conn1 = make_connection("conn1");
        let mut conn2 = make_connection("conn2");
        let mut conn3 = make_connection("conn3");

        let now = Utc::now();
        conn1.rate_limited_until = Some((now + chrono::Duration::minutes(10)).to_rfc3339());
        conn2.rate_limited_until = Some((now + chrono::Duration::minutes(5)).to_rfc3339());
        conn3.rate_limited_until = None; // not rate limited

        let connections = vec![conn1, conn2, conn3];
        let earliest = get_earliest_rate_limited_until(&connections);

        assert!(earliest.is_some());
        // Should be conn2's limit (5 minutes)
        let earliest_time = earliest.unwrap();
        let conn2_time = now + chrono::Duration::minutes(5);
        // Allow 1 second tolerance
        assert!((earliest_time - conn2_time).num_seconds().abs() <= 1);
    }
}