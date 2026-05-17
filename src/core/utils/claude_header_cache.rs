//! Port of `open-sse/utils/claudeHeaderCache.js`.
//!
//! Singleton cache for real Claude Code client headers. Captures headers
//! from authentic Claude Code requests so they can be replayed when
//! forwarding to `api.anthropic.com`, replacing static hardcoded values
//! that are easy for Anthropic to fingerprint.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::OnceLock;

/// The set of header names we capture from authentic Claude Code clients.
const CLAUDE_IDENTITY_HEADERS: &[&str] = &[
    "user-agent",
    "anthropic-beta",
    "anthropic-version",
    "anthropic-dangerous-direct-browser-access",
    "x-app",
    "x-stainless-helper-method",
    "x-stainless-retry-count",
    "x-stainless-runtime-version",
    "x-stainless-package-version",
    "x-stainless-runtime",
    "x-stainless-lang",
    "x-stainless-arch",
    "x-stainless-os",
    "x-stainless-timeout",
    "x-claude-code-session-id",
    "package-version",
    "runtime-version",
    "os",
    "arch",
];

fn cache() -> &'static RwLock<Option<HashMap<String, String>>> {
    static CELL: OnceLock<RwLock<Option<HashMap<String, String>>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(None))
}

/// Detect if request headers look like a real Claude Code client.
fn is_claude_code_client(headers: &HashMap<String, String>) -> bool {
    let ua = headers
        .get("user-agent")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let x_app = headers
        .get("x-app")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    ua.contains("claude-cli") || ua.contains("claude-code") || x_app == "cli"
}

/// Capture this request's identity headers if it looks like an authentic
/// Claude Code client. Subsequent forwarded calls can replay them via
/// [`get_cached_claude_headers`].
pub fn cache_claude_headers(headers: &HashMap<String, String>) {
    if !is_claude_code_client(headers) {
        return;
    }
    let captured: HashMap<String, String> = CLAUDE_IDENTITY_HEADERS
        .iter()
        .filter_map(|name| headers.get(*name).map(|v| ((*name).to_string(), v.clone())))
        .collect();
    if !captured.is_empty() {
        *cache().write() = Some(captured);
    }
}

/// Get the most recently cached Claude Code identity headers, or `None`
/// if no authentic client request has been seen yet.
pub fn get_cached_claude_headers() -> Option<HashMap<String, String>> {
    cache().read().clone()
}

/// Clear the cache. Mainly useful for tests.
#[allow(dead_code)]
pub fn clear_cache() {
    *cache().write() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn cache_only_for_authentic_claude_clients() {
        clear_cache();
        cache_claude_headers(&h(&[("user-agent", "curl/8.0")]));
        assert!(get_cached_claude_headers().is_none());

        cache_claude_headers(&h(&[
            ("user-agent", "claude-cli/2.1.92"),
            ("anthropic-version", "2023-06-01"),
            ("x-app", "cli"),
            ("totally-unrelated", "foo"),
        ]));
        let cached = get_cached_claude_headers().expect("cached");
        assert_eq!(
            cached.get("anthropic-version").map(String::as_str),
            Some("2023-06-01")
        );
        assert!(!cached.contains_key("totally-unrelated"));
    }
}
