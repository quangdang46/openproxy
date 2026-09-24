//! Routing-table completeness guards (bead openproxy-1wyb).
//!
//! `PROVIDER_CONFIGS` in `src/core/executor/default.rs` is the single source of
//! truth consulted by `DefaultExecutor::new`. A provider that reaches
//! `DefaultExecutor` but has no entry there fails with
//! `ExecutorError::UnsupportedProvider`, which the chat dispatch maps to an
//! opaque HTTP 500 on *every* request.
//!
//! Providers that are dispatched to a dedicated executor (`KiroExecutor`,
//! `CodexExecutor`, `GitHubExecutor`, ...) never consult `PROVIDER_CONFIGS`,
//! so they are exempt. The exemption list below is derived from the
//! `provider == "..."` branches in `src/server/api/chat.rs`; a new OAuth
//! provider must appear in exactly one of the two lists.

use openproxy::core::executor::{provider_config_base_url, ClientPool, DefaultExecutor};
use openproxy::oauth::providers::get_config;
use std::sync::Arc;

/// Every id the OAuth registry can resolve (`providers::get_config`).
/// Data-driven: adding an OAuth provider arm without a routing entry fails here.
const OAUTH_REGISTRY_IDS: &[&str] = &[
    "antigravity",
    "claude",
    "cline",
    "clinepass",
    "codebuddy",
    "codebuddy-cn",
    "codebuddy-intl",
    "codex",
    "cursor",
    "gemini-cli",
    "github",
    "gitlab",
    "iflow",
    "kilocode",
    "kimchi",
    "kimi",
    "kimi-coding",
    "kiro",
    "openai-native",
    "qoder",
    "qwen",
    "trae",
    "xai",
    "zed",
];

/// Ids accepted by the device-code OAuth flow (`is_device_code_provider`).
const DEVICE_CODE_IDS: &[&str] = &[
    "github",
    "kiro",
    "kimi",
    "kimi-coding",
    "kilocode",
    "codebuddy",
    "codebuddy-cn",
    "codebuddy-intl",
    "qoder",
    "grok-cli",
    "qwen",
];

/// OAuth providers routed to a dedicated executor instead of `DefaultExecutor`.
const DEDICATED_EXECUTOR_PROVIDERS: &[&str] = &[
    "codex",        // CodexExecutor      (chat.rs)
    "codebuddy-cn", // CodeBuddyCNExecutor
    "gemini-cli",   // GeminiCliExecutor
    "github",       // GithubExecutor
    "grok-cli",     // GrokCliExecutor
    "iflow",        // IFlowExecutor
    "kimchi",       // KimchiExecutor
    "kiro",         // KiroExecutor
    "qoder",        // QoderExecutor
    "qwen",         // QwenExecutor
    "trae",         // TraeExecutor
    "zed",          // ZedExecutor
    // OAuth config only: no dispatch branch and no connection may select it
    // as a provider id, so it never reaches the executor layer.
    "openai-native",
];

/// The registry, the device-code list, and the executor dispatch must agree:
/// every id a user can configure is either routable through
/// `PROVIDER_CONFIGS` or explicitly exempt via a dedicated executor.
fn assert_routable(id: &str, source: &str) {
    if DEDICATED_EXECUTOR_PROVIDERS.contains(&id) {
        return;
    }
    assert!(
        provider_config_base_url(id).is_some(),
        "{source} provider `{id}` has no PROVIDER_CONFIGS entry and no dedicated \
         executor, so DefaultExecutor::new returns UnsupportedProvider and every \
         request fails with HTTP 500"
    );
}

#[test]
fn oauth_registry_providers_all_have_a_routing_entry() {
    for &id in OAUTH_REGISTRY_IDS {
        // Guard the fixture itself: a stale list would silently weaken the test.
        assert!(
            get_config(id).is_some(),
            "OAUTH_REGISTRY_IDS lists `{id}` but providers::get_config cannot resolve it"
        );
        assert_routable(id, "OAuth-registry");
    }
}

#[test]
fn device_code_providers_all_have_a_routing_entry() {
    for &id in DEVICE_CODE_IDS {
        assert_routable(id, "device-code");
    }
}

#[test]
fn kimi_coding_is_known_to_the_oauth_registry() {
    assert_eq!(get_config("kimi-coding").map(|c| c.id), Some("kimi-coding"));
}

#[test]
fn kimi_coding_executor_constructs() {
    // Regression guard for the reported defect: a kimi-coding connection created
    // through the device-code OAuth path carries no provider_node, so
    // `DefaultExecutor::new` must resolve it from PROVIDER_CONFIGS.
    let exec = DefaultExecutor::new("kimi-coding", Arc::new(ClientPool::new()), None)
        .expect("kimi-coding must be a supported provider");
    // Construction alone is the regression guard: before the fix this returned
    // `ExecutorError::UnsupportedProvider("kimi-coding")`, which chat.rs maps
    // to HTTP 500 on every request.
    let _ = exec.pool();
}

#[test]
fn kimi_coding_routing_mirrors_kimi() {
    // kimi-coding is a dual-auth alias of kimi (same host, same Claude shape),
    // so its routing entry must resolve to the same upstream endpoint.
    let kimi = provider_config_base_url("kimi").expect("kimi routing entry");
    let kimi_coding = provider_config_base_url("kimi-coding").expect("kimi-coding routing entry");
    assert_eq!(
        kimi, kimi_coding,
        "kimi-coding must mirror the kimi endpoint"
    );
}
