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

/// Abort an upstream stream if no chunk arrives within this window (360s,
/// matching the 9router default).
pub const STREAM_STALL_TIMEOUT_MS: u64 = 6 * 60 * 1000;

/// Time-to-first-token: abort if the first SSE chunk does not arrive within
/// this window (200s). After the first chunk, the per-chunk stall timeout
/// (`STREAM_STALL_TIMEOUT_MS`) applies.
pub const STREAM_FIRST_CHUNK_TIMEOUT_MS: u64 = 200_000;

/// Keep-alive socket TTL for pooled upstream connections (4s default,
/// `FETCH_KEEPALIVE_TIMEOUT_MS` override). Capped below upstream/edge idle
/// windows (Cloudflare, AWS Global Accelerator / NVIDIA NIM) that drop idle
/// sockets without a TCP RST. Port of `DEFAULT_KEEPALIVE_TIMEOUT_MS` /
/// `resolveKeepAliveTimeouts` in 9router `open-sse/config/runtimeConfig.js`
/// (v0.5.116, commit 963787f7).
pub const DEFAULT_KEEPALIVE_TIMEOUT_MS: u64 = 4 * 1000;
/// Fresh-socket retry TTL (1ms) — guarantees a new TCP/TLS connection.
/// Port of `FRESH_RETRY_KEEPALIVE_TIMEOUT_MS`.
pub const FRESH_RETRY_KEEPALIVE_TIMEOUT_MS: u64 = 1;

/// Parse a positive-integer millisecond env override, falling back to `def`
/// (port of upstream `envMs`: unset/empty/non-numeric/<=0 → default).
fn env_ms(name: &str, def: u64) -> u64 {
    match std::env::var(name) {
        Ok(raw) if !raw.is_empty() => raw
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(def),
        _ => def,
    }
}

/// Effective keep-alive TTL for pooled upstream connections.
pub fn fetch_keepalive_timeout_ms() -> u64 {
    env_ms("FETCH_KEEPALIVE_TIMEOUT_MS", DEFAULT_KEEPALIVE_TIMEOUT_MS)
}

/// Escape hatch: `FETCH_FRESH_RETRY=off|0|false|no` disables the
/// transport-level stale-socket replay, leaving only the executor's own
/// retry path. Defaults to on. Port of `isFreshSocketReplayEnabled`.
pub fn fresh_socket_replay_enabled() -> bool {
    match std::env::var("FETCH_FRESH_RETRY") {
        Ok(raw) => !matches!(
            raw.trim().to_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        ),
        Err(_) => true,
    }
}

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
    let attempts = entry.get("attempts").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let delay_ms = entry
        .get("delayMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(RetryEntry::DEFAULT_DELAY_MS);
    RetryEntry { attempts, delay_ms }
}

/// Inbound prompts containing any of these substrings are treated as
/// disposable (filler) requests and may bypass the heavy provider routing
/// path. Mirrors `SKIP_PATTERNS` in 9router.
pub const SKIP_PATTERNS: &[&str] =
    &["Please write a 5-10 word title for the following conversation:"];

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

    #[test]
    fn env_ms_parses_positive_int_and_falls_back() {
        // Unique var name so parallel tests can't collide.
        let name = "OPENPROXY_TEST_ENV_MS_KEEPALIVE";
        std::env::set_var(name, "2500");
        assert_eq!(env_ms(name, 99), 2500);
        std::env::set_var(name, "0");
        assert_eq!(env_ms(name, 99), 99);
        std::env::set_var(name, "not-a-number");
        assert_eq!(env_ms(name, 99), 99);
        std::env::remove_var(name);
        assert_eq!(env_ms(name, 99), 99);
    }

    #[test]
    fn fresh_socket_replay_defaults_on_and_honors_kill_switch() {
        // Save/restore the real env var; nothing else in the test binary
        // reads FETCH_FRESH_RETRY.
        let saved = std::env::var("FETCH_FRESH_RETRY").ok();
        std::env::remove_var("FETCH_FRESH_RETRY");
        assert!(fresh_socket_replay_enabled());
        for off in ["off", "0", "false", "no", "OFF"] {
            std::env::set_var("FETCH_FRESH_RETRY", off);
            assert!(!fresh_socket_replay_enabled(), "{off} should disable");
        }
        std::env::set_var("FETCH_FRESH_RETRY", "on");
        assert!(fresh_socket_replay_enabled());
        match saved {
            Some(v) => std::env::set_var("FETCH_FRESH_RETRY", v),
            None => std::env::remove_var("FETCH_FRESH_RETRY"),
        }
    }
}
