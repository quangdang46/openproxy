//! Port of `open-sse/config/runtimeConfig.js` — HTTP status codes, retry
//! policy defaults, cache TTLs, memory caps, and the catch-all "skip
//! patterns" used to short-circuit obvious filler requests (title summary,
//! …).

use std::collections::BTreeMap;

/// HTTP status codes referenced by the error/retry pipeline.
pub mod http_status {
    pub const BAD_REQUEST: u16 = 400;
    pub const UNAUTHORIZED: u16 = 401;
    pub const PAYMENT_REQUIRED: u16 = 402;
    pub const FORBIDDEN: u16 = 403;
    pub const NOT_FOUND: u16 = 404;
    pub const NOT_ACCEPTABLE: u16 = 406;
    pub const REQUEST_TIMEOUT: u16 = 408;
    pub const RATE_LIMITED: u16 = 429;
    pub const SERVER_ERROR: u16 = 500;
    pub const BAD_GATEWAY: u16 = 502;
    pub const SERVICE_UNAVAILABLE: u16 = 503;
    pub const GATEWAY_TIMEOUT: u16 = 504;
}

/// Cache TTLs in seconds.
pub mod cache_ttl {
    /// User info cache: 5 minutes.
    pub const USER_INFO: u64 = 300;
    /// Model alias cache: 1 hour.
    pub const MODEL_ALIAS: u64 = 3600;
}

/// Memory management knobs.
pub mod memory_config {
    use std::time::Duration;

    pub const SESSION_TTL: Duration = Duration::from_secs(2 * 60 * 60);
    pub const SESSION_CLEANUP_INTERVAL: Duration = Duration::from_secs(30 * 60);
    pub const DNS_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
    pub const PROXY_DISPATCHERS_MAX_SIZE: usize = 20;
}

/// Abort an upstream stream if no chunk arrives within this window.
pub const STREAM_STALL_TIMEOUT_MS: u64 = 3 * 60 * 1000;

/// Hard cap on `max_tokens` we forward upstream (clamped down from any
/// caller-supplied value above this).
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;
/// Floor used by the max-tokens helper when callers ask for "as much as
/// possible" without specifying a value.
pub const DEFAULT_MIN_TOKENS: u32 = 32_000;

/// Legacy retry config, kept for backward compatibility with code that
/// reads `RETRY_CONFIG.*` directly.
pub mod retry_config {
    pub const MAX_ATTEMPTS: u32 = 2;
    pub const DELAY_MS: u64 = 2000;
}

/// Parsed retry entry: how many additional attempts to make and how long
/// to sleep between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryEntry {
    pub attempts: u32,
    pub delay_ms: u64,
}

impl RetryEntry {
    pub const DEFAULT_DELAY_MS: u64 = retry_config::DELAY_MS;
}

/// Default retry policy keyed by HTTP status code.
///
/// Matches `DEFAULT_RETRY_CONFIG` in 9router. `429` retries are handled by
/// the rate-limit backoff path, not here, hence `attempts: 0`.
pub fn default_retry_config() -> BTreeMap<u16, RetryEntry> {
    let mut map = BTreeMap::new();
    map.insert(
        http_status::RATE_LIMITED,
        RetryEntry {
            attempts: 0,
            delay_ms: 0,
        },
    );
    map.insert(
        http_status::BAD_GATEWAY,
        RetryEntry {
            attempts: 3,
            delay_ms: 3000,
        },
    );
    map.insert(
        http_status::SERVICE_UNAVAILABLE,
        RetryEntry {
            attempts: 3,
            delay_ms: 2000,
        },
    );
    map.insert(
        http_status::GATEWAY_TIMEOUT,
        RetryEntry {
            attempts: 2,
            delay_ms: 3000,
        },
    );
    map
}

/// Normalize a JSON-shaped retry entry into `RetryEntry`.
///
/// Accepts:
///   - `null`            → `{attempts: 0, delay_ms: DEFAULT_DELAY_MS}`
///   - integer N         → `{attempts: N, delay_ms: DEFAULT_DELAY_MS}`
///   - object `{...}`    → fields mapped, missing values defaulted
pub fn resolve_retry_entry(entry: Option<&serde_json::Value>) -> RetryEntry {
    let Some(entry) = entry else {
        return RetryEntry {
            attempts: 0,
            delay_ms: RetryEntry::DEFAULT_DELAY_MS,
        };
    };
    if entry.is_null() {
        return RetryEntry {
            attempts: 0,
            delay_ms: RetryEntry::DEFAULT_DELAY_MS,
        };
    }
    if let Some(n) = entry.as_u64() {
        return RetryEntry {
            attempts: n as u32,
            delay_ms: RetryEntry::DEFAULT_DELAY_MS,
        };
    }
    let attempts = entry
        .get("attempts")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let delay_ms = entry
        .get("delayMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(RetryEntry::DEFAULT_DELAY_MS);
    RetryEntry { attempts, delay_ms }
}

/// Inbound prompts containing any of these substrings are treated as
/// disposable (filler) requests and may bypass the heavy provider routing
/// path. Mirrors `SKIP_PATTERNS` in 9router.
pub const SKIP_PATTERNS: &[&str] = &["Please write a 5-10 word title for the following conversation:"];

/// Returns `true` if the request body matches any [`SKIP_PATTERNS`] entry.
pub fn matches_skip_pattern(text: &str) -> bool {
    SKIP_PATTERNS.iter().any(|p| text.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_retry_entry_handles_all_shapes() {
        assert_eq!(
            resolve_retry_entry(None),
            RetryEntry {
                attempts: 0,
                delay_ms: retry_config::DELAY_MS
            }
        );
        assert_eq!(
            resolve_retry_entry(Some(&serde_json::Value::Null)),
            RetryEntry {
                attempts: 0,
                delay_ms: retry_config::DELAY_MS
            }
        );
        assert_eq!(
            resolve_retry_entry(Some(&json!(3))),
            RetryEntry {
                attempts: 3,
                delay_ms: retry_config::DELAY_MS
            }
        );
        assert_eq!(
            resolve_retry_entry(Some(&json!({"attempts": 5, "delayMs": 1000}))),
            RetryEntry {
                attempts: 5,
                delay_ms: 1000
            }
        );
        assert_eq!(
            resolve_retry_entry(Some(&json!({"attempts": 5}))),
            RetryEntry {
                attempts: 5,
                delay_ms: retry_config::DELAY_MS
            }
        );
    }

    #[test]
    fn default_retry_config_includes_5xx() {
        let cfg = default_retry_config();
        assert_eq!(cfg[&502].attempts, 3);
        assert_eq!(cfg[&503].delay_ms, 2000);
        assert_eq!(cfg[&429].attempts, 0);
    }

    #[test]
    fn skip_patterns_match_title_filler() {
        assert!(matches_skip_pattern(
            "Please write a 5-10 word title for the following conversation: ..."
        ));
        assert!(!matches_skip_pattern("hello world"));
    }
}
