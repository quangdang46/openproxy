//! Account fallback utilities for multi-account routing.
//!
//! Mirrors the functionality of `open-sse/services/accountFallback.js`.
//! Provides per-account state tracking, health scoring, and fallback routing logic.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use crate::types::ProviderConnection;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountLockState {
    pub in_flight: usize,
    pub rate_limit_remaining: i64,
    pub rate_limit_reset: i64,
}

/// Tracks round-robin state for a combo's account rotation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComboRotationState {
    /// The combo identifier.
    pub combo_id: String,
    /// Last used account index (wraps around).
    pub last_index: usize,
    /// Unix timestamp of last rotation.
    pub last_rotation: i64,
    /// Total requests through this combo.
    pub total_requests: u64,
}

/// Model lock state for sticky routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelLockState {
    pub model: String,
    pub account_id: String,
    pub locked_at: i64,
    pub ttl_secs: i64,
    pub combo_id: Option<String>,
}

#[derive(Debug)]
pub struct AccountSlotGuard<'a> {
    registry: &'a AccountRegistry,
    account_id: String,
}

impl Drop for AccountSlotGuard<'_> {
    fn drop(&mut self) {
        let mut states = self.registry.states.write();
        if let Some(state) = states.get_mut(&self.account_id) {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }
}

impl<'a> AccountSlotGuard<'a> {
    pub fn in_flight(&self) -> usize {
        self.registry.get_state(&self.account_id).in_flight
    }
}

#[derive(Debug, Default)]
pub struct AccountRegistry {
    states: RwLock<HashMap<String, AccountLockState>>,
    combo_rotation: RwLock<HashMap<String, ComboRotationState>>,
    model_locks: RwLock<HashMap<String, ModelLockState>>,
}

impl AccountRegistry {
    pub fn get_state(&self, account_id: &str) -> AccountLockState {
        self.states
            .read()
            .get(account_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn acquire_slot(
        &self,
        account_id: &str,
        max_in_flight: usize,
        rate_limit_remaining: i64,
        rate_limit_reset: i64,
    ) -> Option<AccountSlotGuard<'_>> {
        if rate_limit_remaining <= 0 {
            let now = Utc::now().timestamp();
            if rate_limit_reset > now {
                return None;
            }
        }

        self.acquire_slot_internal(account_id, max_in_flight)
    }

    fn acquire_slot_internal(
        &self,
        account_id: &str,
        max_in_flight: usize,
    ) -> Option<AccountSlotGuard<'_>> {
        let mut states = self.states.write();
        let state = states
            .entry(account_id.to_string())
            .or_default();

        if state.in_flight >= max_in_flight {
            return None;
        }

        state.in_flight += 1;
        Some(AccountSlotGuard {
            registry: self,
            account_id: account_id.to_string(),
        })
    }

    pub fn update_rate_limit(&self, account_id: &str, remaining: i64, reset: i64) {
        let mut states = self.states.write();
        let state = states
            .entry(account_id.to_string())
            .or_default();
        state.rate_limit_remaining = remaining;
        state.rate_limit_reset = reset;
    }

    pub fn rate_limit_info(&self, account_id: &str) -> (i64, i64) {
        let state = self.states.read();
        state
            .get(account_id)
            .map(|s| (s.rate_limit_remaining, s.rate_limit_reset))
            .unwrap_or((0, 0))
    }

    pub fn remove_account(&self, account_id: &str) {
        let mut states = self.states.write();
        states.remove(account_id);
    }

    pub fn in_flight_count(&self, account_id: &str) -> usize {
        let state = self.states.read();
        state.get(account_id).map(|s| s.in_flight).unwrap_or(0)
    }

    pub fn get_sticky_session(&self, combo_id: &str) -> Option<StickySession> {
        let locks = self.model_locks.read();
        let key = format!("sticky_{}", combo_id);
        locks.get(&key).and_then(|lock| {
            let now = Utc::now().timestamp();
            if lock.locked_at + lock.ttl_secs > now {
                Some(StickySession {
                    account_id: lock.account_id.clone(),
                    expires_at: DateTime::from_timestamp(lock.locked_at + lock.ttl_secs, 0)
                        .unwrap_or_else(Utc::now),
                })
            } else {
                None
            }
        })
    }

    pub fn set_sticky_session(
        &self,
        combo_id: &str,
        account_id: &str,
        duration_secs: i64,
    ) {
        let mut locks = self.model_locks.write();
        let key = format!("sticky_{}", combo_id);
        let now = Utc::now().timestamp();
        let lock = ModelLockState {
            model: key.clone(),
            account_id: account_id.to_string(),
            locked_at: now,
            ttl_secs: duration_secs,
            combo_id: Some(combo_id.to_string()),
        };
        locks.insert(key, lock);
    }

    pub fn clear_sticky_session(&self, combo_id: &str) {
        let mut locks = self.model_locks.write();
        let key = format!("sticky_{}", combo_id);
        locks.remove(&key);
    }

    pub fn select_account_by_strategy(
        &self,
        available: &[&ProviderConnection],
        strategy: StrategyType,
        combo_id: Option<&str>,
        sticky_duration_secs: i64,
    ) -> Option<usize> {
        if available.is_empty() {
            return None;
        }

        match strategy {
            StrategyType::FillFirst => {
                available
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, conn)| self.get_state(&conn.id).rate_limit_remaining)
                    .map(|(i, _)| i)
            }
            StrategyType::RoundRobin => {
                let combo_id = combo_id?;
                let mut rotation = self.combo_rotation.write();
                let state = rotation.entry(combo_id.to_string()).or_default();
                let idx = state.last_index;
                state.last_index = (idx + 1) % available.len();
                state.total_requests += 1;
                state.last_rotation = Utc::now().timestamp();
                Some(idx)
            }
            StrategyType::Sticky => {
                let combo_id = combo_id?;
                if let Some(sticky) = self.get_sticky_session(combo_id) {
                    if !sticky.is_expired(Utc::now()) {
                        if let Some(pos) = available
                            .iter()
                            .position(|conn| conn.id == sticky.account_id)
                        {
                            return Some(pos);
                        }
                    }
                }
                let idx = 0;
                if !available.is_empty() {
                    let first_account = &available[idx].id;
                    self.set_sticky_session(combo_id, first_account, sticky_duration_secs);
                }
                Some(idx)
            }
            StrategyType::LeastLoaded => {
                available
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, conn)| self.in_flight_count(&conn.id))
                    .map(|(i, _)| i)
            }
        }
    }

    pub fn lock_model(
        &self,
        model: &str,
        account_id: &str,
        ttl_secs: i64,
        combo_id: Option<String>,
    ) -> Result<(), String> {
        let mut locks = self.model_locks.write();
        let now = Utc::now().timestamp();

        if let Some(existing) = locks.get(model) {
            if existing.account_id == account_id {
                let updated = ModelLockState {
                    model: model.to_string(),
                    account_id: account_id.to_string(),
                    locked_at: now,
                    ttl_secs,
                    combo_id,
                };
                locks.insert(model.to_string(), updated);
                return Ok(());
            }
            if existing.locked_at + existing.ttl_secs > now {
                return Err(format!(
                    "model {} is locked to account {} until TTL expires",
                    model, existing.account_id
                ));
            }
        }

        let lock = ModelLockState {
            model: model.to_string(),
            account_id: account_id.to_string(),
            locked_at: now,
            ttl_secs,
            combo_id,
        };
        locks.insert(model.to_string(), lock);
        Ok(())
    }

    pub fn unlock_model(&self, model: &str) {
        let mut locks = self.model_locks.write();
        locks.remove(model);
    }

    pub fn get_locked_account(&self, model: &str) -> Option<String> {
        let locks = self.model_locks.read();
        let now = Utc::now().timestamp();

        locks.get(model).and_then(|lock| {
            if lock.locked_at + lock.ttl_secs > now {
                Some(lock.account_id.clone())
            } else {
                None
            }
        })
    }

    pub fn next_in_combo(
        &self,
        combo_id: &str,
        accounts: &[String],
    ) -> Option<(usize, String)> {
        if accounts.is_empty() {
            return None;
        }
        let now = Utc::now().timestamp();
        let mut rotation = self.combo_rotation.write();
        let state = rotation.entry(combo_id.to_string()).or_default();
        let idx = state.last_index;
        state.total_requests += 1;
        state.last_index = (idx + 1) % accounts.len();
        state.last_rotation = now;
        Some((idx, accounts[idx].clone()))
    }

    pub fn record_rotation(&self, combo_id: &str, index: usize) {
        let now = Utc::now().timestamp();
        let mut rotation = self.combo_rotation.write();
        let state = rotation.entry(combo_id.to_string()).or_default();
        state.last_index = index;
        state.last_rotation = now;
    }

    pub fn get_combo_stats(&self, combo_id: &str) -> Option<ComboRotationState> {
        self.combo_rotation
            .read()
            .get(combo_id)
            .cloned()
    }
}

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

/// Default sticky duration in seconds.
pub const DEFAULT_STICKY_DURATION_SECS: i64 = 300;

/// Strategy type for provider account selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyType {
    /// Use account with most remaining quota first.
    FillFirst,
    /// Rotate through accounts in round-robin fashion.
    RoundRobin,
    /// Stick to first successful account for a duration.
    Sticky,
    /// Always pick account with fewest in-flight requests.
    LeastLoaded,
}

impl Default for StrategyType {
    fn default() -> Self {
        StrategyType::FillFirst
    }
}

impl std::fmt::Display for StrategyType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StrategyType::FillFirst => write!(f, "fillFirst"),
            StrategyType::RoundRobin => write!(f, "roundRobin"),
            StrategyType::Sticky => write!(f, "sticky"),
            StrategyType::LeastLoaded => write!(f, "leastLoaded"),
        }
    }
}

impl std::str::FromStr for StrategyType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "fillfirst" | "fill-first" | "fill_first" => Ok(StrategyType::FillFirst),
            "roundrobin" | "round-robin" | "round_robin" | "round_robin" => Ok(StrategyType::RoundRobin),
            "sticky" => Ok(StrategyType::Sticky),
            "leastloaded" | "least-loaded" | "least_loaded" => Ok(StrategyType::LeastLoaded),
            _ => Err(format!("Unknown strategy type: {}", s)),
        }
    }
}

/// Provider strategy settings for account selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStrategySettings {
    /// Strategy type for account selection.
    pub strategy: StrategyType,
    /// Duration in seconds to stick to a successful account (for sticky strategy).
    pub sticky_duration_secs: i64,
    /// Whether fallback to other accounts is enabled on failure.
    pub fallback_enabled: bool,
    /// Maximum number of fallback attempts before giving up.
    pub max_fallback_attempts: usize,
}

impl Default for ProviderStrategySettings {
    fn default() -> Self {
        Self {
            strategy: StrategyType::FillFirst,
            sticky_duration_secs: DEFAULT_STICKY_DURATION_SECS,
            fallback_enabled: true,
            max_fallback_attempts: 3,
        }
    }
}

impl ProviderStrategySettings {
    /// Parse strategy settings from a string value (e.g., from config).
    pub fn from_config_value(value: Option<&str>, fallback_enabled: bool, max_fallback_attempts: usize) -> Self {
        match value.and_then(|v| v.parse().ok()) {
            Some(strategy) => Self {
                strategy,
                sticky_duration_secs: DEFAULT_STICKY_DURATION_SECS,
                fallback_enabled,
                max_fallback_attempts,
            },
            None => Self::default(),
        }
    }
}

/// Sticky session state for tracking which account to stick to.
#[derive(Debug, Clone)]
pub struct StickySession {
    /// Account ID that was last successful.
    pub account_id: String,
    /// Timestamp when the sticky session expires.
    pub expires_at: DateTime<Utc>,
}

impl StickySession {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
}

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