//! Port of `open-sse/utils/sessionManager.js`.
//!
//! Per-connection session-id manager for the Antigravity Cloud Code path.
//! The upstream binary mints a session id once at startup (`randomUUID()
//! + Date.now()`); since the proxy is long-running, we simulate the same
//!   "stable for the process lifetime" behaviour by caching one id per
//!   connection-id (typically the OAuth account email).
//!
//! Also provides conversation-stable session identity resolution used by
//! Kiro multi-turn prompt-cache (`resolve_session_identity`) and stable
//! `agentContinuationId` minting (`resolve_continuation_id`).

use dashmap::DashMap;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::core::config::runtime_config::memory_config;

/// Hard cap on the number of cached session ids. Belt-and-suspenders against
/// runaway growth between cleanup ticks.
const MAX_SESSIONS: usize = 1000;
const MAX_CONTINUATION_SESSIONS: usize = 5000;

/// Client headers that may carry an upstream session id (priority order).
const SESSION_HEADER_KEYS: &[&str] = &[
    "x-session-id",
    "session-id",
    "session_id",
    "x-amp-thread-id",
];

#[derive(Debug, Clone)]
struct Entry {
    session_id: String,
    last_used: Instant,
}

#[derive(Debug, Clone)]
struct ContinuationEntry {
    continuation_id: String,
    last_used: Instant,
}

/// Result of resolving a conversation-stable session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub session_id: String,
    /// When true the id is one-shot (do not cache continuation across turns).
    pub ephemeral: bool,
}

static STORE: Lazy<DashMap<String, Entry>> = Lazy::new(DashMap::new);
static CONTINUATION_STORE: Lazy<DashMap<String, ContinuationEntry>> = Lazy::new(DashMap::new);
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

/// Drop all session + continuation ids. Mainly useful for tests.
pub fn clear_session_store() {
    STORE.clear();
    CONTINUATION_STORE.clear();
}

/// Number of cached per-connection session entries. Useful for tests.
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
    CONTINUATION_STORE.retain(|_, entry| entry.last_used >= cutoff);
    *last = Instant::now();
}

fn normalize_session_id(value: Option<&str>) -> Option<String> {
    let v = value?.trim();
    if v.is_empty() || v.len() > 256 {
        return None;
    }
    Some(v.to_string())
}

fn extract_claude_code_session(user_id: &str) -> Option<String> {
    if user_id.is_empty() {
        return None;
    }
    // `_session_{uuid}` suffix
    if let Some(idx) = user_id.rfind("_session_") {
        let rest = &user_id[idx + "_session_".len()..];
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
    }
    // JSON `{ "session_id": "..." }`
    if user_id.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(user_id) {
            return normalize_session_id(v.get("session_id").and_then(|s| s.as_str()));
        }
    }
    None
}

fn header_value(headers: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    let headers = headers?;
    let want = key.to_lowercase();
    headers
        .iter()
        .find(|(k, _)| k.to_lowercase() == want)
        .and_then(|(_, v)| normalize_session_id(Some(v.as_str())))
}

/// Read client-provided session id from headers/body (no generation).
fn extract_client_session_id(
    headers: Option<&HashMap<String, String>>,
    body: Option<&Value>,
    scope: &str,
) -> Option<String> {
    if let Some(body) = body {
        if let Some(user_id) = body
            .get("metadata")
            .and_then(|m| m.get("user_id"))
            .and_then(|v| v.as_str())
        {
            if let Some(claude) = extract_claude_code_session(user_id) {
                return Some(format!("claude:{claude}"));
            }
        }
    }

    for key in SESSION_HEADER_KEYS {
        if let Some(v) = header_value(headers, key) {
            return Some(v);
        }
    }

    // For Kiro we intentionally ignore x-client-request-id (one-shot per request).
    if scope != "kiro" {
        if let Some(v) = header_value(headers, "x-client-request-id") {
            return Some(v);
        }
    }

    let body = body?;
    normalize_session_id(body.get("prompt_cache_key").and_then(|v| v.as_str()))
        .or_else(|| normalize_session_id(body.get("session_id").and_then(|v| v.as_str())))
        .or_else(|| normalize_session_id(body.get("conversation_id").and_then(|v| v.as_str())))
        .or_else(|| {
            if scope == "kiro" {
                None
            } else {
                normalize_session_id(
                    body.get("metadata")
                        .and_then(|m| m.get("user_id"))
                        .and_then(|v| v.as_str()),
                )
            }
        })
}

/// Resolve a conversation-stable session id (9router `resolveSessionIdentity`).
///
/// Priority: client session header/body → (non-kiro) per-connection cache →
/// for Kiro with no client id, mint an ephemeral one-shot id.
pub fn resolve_session_identity(
    headers: Option<&HashMap<String, String>>,
    body: Option<&Value>,
    connection_id: Option<&str>,
    scope: &str,
) -> SessionIdentity {
    if let Some(client) = extract_client_session_id(headers, body, scope) {
        return SessionIdentity {
            session_id: client,
            ephemeral: false,
        };
    }
    // 9router: for kiro, skip assistant-text hashing and mint ephemeral when no client id.
    if scope == "kiro" {
        return SessionIdentity {
            session_id: generate_binary_style_id(),
            ephemeral: true,
        };
    }
    SessionIdentity {
        session_id: derive_session_id(connection_id.unwrap_or("")),
        ephemeral: false,
    }
}

/// Resolve a stable `agentContinuationId` for multi-turn Kiro sessions.
/// Ephemeral sessions always get a fresh UUID.
pub fn resolve_continuation_id(
    session_id: &str,
    connection_id: Option<&str>,
    scope: &str,
    ephemeral: bool,
) -> String {
    if ephemeral {
        return Uuid::new_v4().to_string();
    }
    maybe_run_cleanup();
    let key = format!("{}:{}:{}", scope, connection_id.unwrap_or(""), session_id);
    if let Some(mut entry) = CONTINUATION_STORE.get_mut(&key) {
        entry.last_used = Instant::now();
        return entry.continuation_id.clone();
    }
    if CONTINUATION_STORE.len() >= MAX_CONTINUATION_SESSIONS {
        if let Some(victim) = CONTINUATION_STORE.iter().next().map(|r| r.key().clone()) {
            CONTINUATION_STORE.remove(&victim);
        }
    }
    let continuation_id = Uuid::new_v4().to_string();
    CONTINUATION_STORE.insert(
        key,
        ContinuationEntry {
            continuation_id: continuation_id.clone(),
            last_used: Instant::now(),
        },
    );
    continuation_id
}

/// Convenience: extract connectionId / rawHeaders from the translator credentials Value.
pub fn credentials_connection_id(credentials: Option<&Value>) -> Option<String> {
    credentials.and_then(|c| {
        c.get("connectionId")
            .or_else(|| c.get("connection_id"))
            .or_else(|| c.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

/// Parse `rawHeaders` from the translator credentials Value into a lowercase map.
pub fn credentials_raw_headers(credentials: Option<&Value>) -> Option<HashMap<String, String>> {
    let obj = credentials?.get("rawHeaders")?.as_object()?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            map.insert(k.to_lowercase(), s.to_string());
        }
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn empty_connection_id_returns_uncached_value() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_session_store();
        let a = derive_session_id("");
        let b = derive_session_id("");
        assert_ne!(a, b);
        // Empty connection ids are never cached.
        assert_eq!(cached_count(), 0);
    }

    #[test]
    fn same_connection_returns_same_id() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_session_store();
        let a = derive_session_id("test-conn-1");
        let b = derive_session_id("test-conn-1");
        assert_eq!(a, b);
        // Only one entry cached for this connection.
        assert!(cached_count() >= 1);
    }

    #[test]
    fn different_connections_get_different_ids() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_session_store();
        let a = derive_session_id("test-conn-a");
        let b = derive_session_id("test-conn-b");
        assert_ne!(a, b);
        // At least two entries (may be more from parallel tests).
        assert!(cached_count() >= 2);
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
