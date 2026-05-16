//! Port of `open-sse/utils/sessionManager.js`.
//!
//! Per-connection session-id manager for the Antigravity Cloud Code path.
//! The upstream binary mints a session id once at startup (`randomUUID()
//! + Date.now()`); since the proxy is long-running, we simulate the same
//! "stable for the process lifetime" behaviour by caching one id per
//! connection-id (typically the OAuth account email).

use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::core::config::runtime_config::memory_config;

/// Hard cap on the number of cached session ids. Belt-and-suspenders against
/// runaway growth between cleanup ticks.
const MAX_SESSIONS: usize = 1000;

#[derive(Debug, Clone)]
struct Entry {
    session_id: String,
    last_used: Instant,
}

static STORE: Lazy<DashMap<String, Entry>> = Lazy::new(DashMap::new);
static CLEANUP_LOCK: Lazy<parking_lot::Mutex<Instant>> =
    Lazy::new(|| parking_lot::Mutex::new(Instant::now()));

/// Return a stable session id for `connection_id`, minting one on first use
/// and refreshing the LRU timestamp on subsequent calls. If `connection_id`
/// is empty, returns a fresh one-shot id (no caching).
pub fn derive_session_id(connection_id: &str) -> String {
    if connection_id.is_empty() {
        return generate_binary_style_id();
    }

    maybe_run_cleanup();

    if let Some(mut entry) = STORE.get_mut(connection_id) {
        entry.last_used = Instant::now();
        return entry.session_id.clone();
    }

    if STORE.len() >= MAX_SESSIONS {
        // Pop one arbitrary entry as a soft eviction.
        if let Some(victim) = STORE.iter().next().map(|r| r.key().clone()) {
            STORE.remove(&victim);
        }
    }

    let session_id = generate_binary_style_id();
    STORE.insert(
        connection_id.to_string(),
        Entry {
            session_id: session_id.clone(),
            last_used: Instant::now(),
        },
    );
    session_id
}

/// Generate a fresh session id matching the upstream binary's format.
pub fn generate_binary_style_id() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}{now_ms}", Uuid::new_v4())
}

/// Drop all cached session ids. Mainly useful for tests.
pub fn clear_session_store() {
    STORE.clear();
}

/// Number of cached entries. Useful for tests and observability.
pub fn cached_count() -> usize {
    STORE.len()
}

fn maybe_run_cleanup() {
    let interval = memory_config::SESSION_CLEANUP_INTERVAL;
    let mut last = match CLEANUP_LOCK.try_lock() {
        Some(g) => g,
        None => return,
    };
    if last.elapsed() < interval {
        return;
    }
    let ttl = memory_config::SESSION_TTL;
    let cutoff = Instant::now() - ttl;
    STORE.retain(|_, entry| entry.last_used >= cutoff);
    *last = Instant::now();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_connection_id_returns_uncached_value() {
        clear_session_store();
        let a = derive_session_id("");
        let b = derive_session_id("");
        assert_ne!(a, b);
        assert_eq!(cached_count(), 0);
    }

    #[test]
    fn same_connection_returns_same_id() {
        clear_session_store();
        let a = derive_session_id("conn-1");
        let b = derive_session_id("conn-1");
        assert_eq!(a, b);
        assert_eq!(cached_count(), 1);
    }

    #[test]
    fn different_connections_get_different_ids() {
        clear_session_store();
        let a = derive_session_id("conn-1");
        let b = derive_session_id("conn-2");
        assert_ne!(a, b);
        assert_eq!(cached_count(), 2);
    }

    #[test]
    fn binary_style_id_is_uuid_then_timestamp() {
        let id = generate_binary_style_id();
        // 36 hex+hyphen UUID followed by a ms timestamp (>= 13 chars in 2025+)
        assert!(id.len() >= 36 + 13);
    }

    #[test]
    fn ttl_durations_are_positive() {
        // sanity check the runtime_config wiring
        assert!(memory_config::SESSION_TTL > Duration::from_secs(0));
        assert!(memory_config::SESSION_CLEANUP_INTERVAL > Duration::from_secs(0));
    }
}
