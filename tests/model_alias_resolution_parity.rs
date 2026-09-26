//! Regression cover for bead `openproxy-e43o` — the bare-model-name routing
//! rules and the per-model `targetFormat` catalog rows.
//!
//! ## P66-001 — bare model names
//!
//! 9router's `inferProviderFromModelName` (`.tmp/9router/open-sse/services/model.js:126-142`)
//! is a five-entry table with an `"openai"` fallback:
//!
//! ```js
//! const MODEL_PREFIX_PROVIDERS = [
//!   [/^claude-/, "anthropic"],
//!   [/^gemini-/, "gemini"],
//!   [/^gpt-/, "openai"],
//!   [/^o[134]/, "openai"],
//!   [/^deepseek-/, "openrouter"],
//! ];
//! ```
//!
//! OpenProxy carried four extra arms (`mistral-*` → mistral, `command-*` →
//! cohere, `grok-*` → xai, `jamba-*` → ai21), so a bare `grok-4` or
//! `mistral-large` reached a different vendor here than there — and the
//! `jamba-*` arm named `ai21`, which is not a served provider at all.
//!
//! ## P274-001 — the three targetFormat catalog rows
//!
//! 9router ships per-model `targetFormat` / `upstreamModelId` overrides that
//! the reordered precedence chain exists to serve, and OpenProxy's merged
//! catalog had none of them. `chatCore.js:99` resolves
//! `useTransport?.format || modelTargetFormat || getTargetFormat(...)`, so
//! with the rows present a wire-format-matched transport stays lossless and
//! only a client with no matching transport falls through to the row's target.
//! The xiaomi-tokenplan row is the live one: that provider's default target is
//! `openai`, so a Gemini-source `mimo-v2.5-pro-claude` request used to go out
//! OpenAI-shaped with no upstream id rewrite at all.

use openproxy::core::chat::RequestPlan;
use openproxy::core::model::catalog::provider_catalog;
use openproxy::core::model::{get_model_info, ModelRouteKind};
use openproxy::core::translator::registry::Format;
use openproxy::types::AppDb;
use serde_json::json;

#[test]
fn bare_names_9router_sends_to_openai_reach_the_openai_fallback() {
    let db = AppDb::default();
    for model in [
        "grok-4",
        "command-r-plus",
        "mistral-large",
        "jamba-1.5-large",
        "llama-3.3-70b",
    ] {
        let resolved = get_model_info(model, &db);
        assert_eq!(
            resolved.provider.as_deref(),
            Some("openai"),
            "9router routes bare {model} to openai"
        );
        assert_eq!(resolved.model, model);
        assert_eq!(resolved.route_kind, ModelRouteKind::Direct);
    }
}

#[test]
fn the_9router_prefix_rules_still_win() {
    let db = AppDb::default();
    for (model, provider) in [
        ("claude-sonnet-4-5", "anthropic"),
        ("gemini-2.5-pro", "gemini"),
        ("gpt-4o", "openai"),
        ("o3-mini", "openai"),
        ("deepseek-chat", "openrouter"),
        ("codex-auto-review", "codex"),
    ] {
        assert_eq!(
            get_model_info(model, &db).provider.as_deref(),
            Some(provider),
            "{model} must stay on {provider}"
        );
    }
}

/// Guards the ordering dependency: with the `grok-` arm pruned, `grok-build`
/// must still reach grok-cli through the built-in alias, not the openai
/// fallback.
///
/// The target is `grok-cli`, not the `gcli` the built-in spells it with.
/// 9router feeds the target's provider half back through `resolveProviderAlias`
/// (model.js:73) and `grok-cli.js` declares `alias: "gcli"`, so the bare name
/// and `gcli/grok-build` land alike on the id a connection is stored under —
/// and the credential gate compares that id byte for byte.
#[test]
fn the_builtin_alias_still_wins_over_the_openai_fallback() {
    assert_eq!(
        get_model_info("grok-build", &AppDb::default())
            .provider
            .as_deref(),
        Some("grok-cli")
    );
}

#[test]
fn claude_target_format_catalog_rows_match_9r_registry() {
    let catalog = provider_catalog();

    let m3 = catalog
        .find_model("minimax", "MiniMax-M3")
        .expect("minimax MiniMax-M3 row");
    assert_eq!(m3.target_format.as_deref(), Some("claude"));

    let m3cn = catalog
        .find_model("minimax-cn", "MiniMax-M3")
        .expect("minimax-cn MiniMax-M3 row");
    assert_eq!(m3cn.target_format.as_deref(), Some("claude"));

    let mimo = catalog
        .find_model("xiaomi-tokenplan", "mimo-v2.5-pro-claude")
        .expect("xiaomi-tokenplan mimo-v2.5-pro-claude row");
    assert_eq!(mimo.target_format.as_deref(), Some("claude"));
    assert_eq!(mimo.upstream_model_id.as_deref(), Some("mimo-v2.5-pro"));
}

/// The precedence half: with the row present, a source-format-matched transport
/// outranks it. This is the assertion that would break if the two arms were
/// swapped back to model-target-first.
#[test]
fn minimax_m3_transport_outranks_the_model_target() {
    let openai_body = json!({"model": "MiniMax-M3", "messages": []});
    let openai_plan = RequestPlan::new(
        Some("/v1/chat/completions"),
        &openai_body,
        "minimax",
        "MiniMax-M3",
    );
    assert_eq!(
        openai_plan.target_format,
        Format::OpenAi,
        "the matched transport must win over the row's claude target"
    );
}

/// …and the model target is the fallback for a client whose wire format has no
/// supported transport on that provider.
#[test]
fn minimax_m3_falls_back_to_the_row_target_without_a_transport() {
    let gemini_body = json!({"model": "MiniMax-M3", "contents": []});
    let plan = RequestPlan::new(
        Some("/v1beta/models/MiniMax-M3:generateContent"),
        &gemini_body,
        "minimax",
        "MiniMax-M3",
    );
    assert_eq!(
        plan.target_format,
        Format::Claude,
        "no transport matched, so the model target is the fallback"
    );
}

/// The xiaomi-tokenplan row is the one that changes behaviour outright: the
/// provider's default target is `openai`, so without the row a Gemini-source
/// request for the Claude-native id would go out OpenAI-shaped under the
/// `mimo-v2.5-pro-claude` id, which no upstream serves.
#[test]
fn mimo_claude_native_row_retargets_and_rewrites_the_upstream_id() {
    let gemini_body = json!({"model": "mimo-v2.5-pro-claude", "contents": []});
    let plan = RequestPlan::new(
        Some("/v1beta/models/mimo-v2.5-pro-claude:generateContent"),
        &gemini_body,
        "xiaomi-tokenplan",
        "mimo-v2.5-pro-claude",
    );
    assert_eq!(plan.target_format, Format::Claude);
    assert_eq!(plan.upstream_model_id, "mimo-v2.5-pro");
}

/// The plain row keeps the id it is listed under, and the OpenAI transport
/// still wins for an OpenAI client.
#[test]
fn mimo_pro_row_is_untouched_by_the_claude_native_one() {
    let body = json!({"model": "mimo-v2.5-pro", "messages": []});
    let plan = RequestPlan::new(
        Some("/v1/chat/completions"),
        &body,
        "xiaomi-tokenplan",
        "mimo-v2.5-pro",
    );
    assert_eq!(plan.upstream_model_id, "mimo-v2.5-pro");
    assert_eq!(plan.target_format, Format::OpenAi);
}
