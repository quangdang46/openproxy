//! Regression cover for the provider-alias routing table in
//! `core::model::ALIAS_TO_PROVIDER_ID` — the twelve registry aliases whose
//! targets are served by a dedicated executor or by the media path.
//!
//! ## Why the table and not the dispatch arm
//!
//! Several of these look already-handled. `chat.rs` matches
//! `provider == "grok-cli" || provider == "gcli" || provider == "gb" ||
//! provider == "grok-build"`; the TTS adapter accepts `"aws-polly" | "polly"`;
//! the image adapter has a `"stability-ai"` arm. None of that makes the alias
//! route. The dispatch match sits inside the per-attempt loop in
//! `forward_with_provider_fallback`, and the first thing that loop does is
//! pick a credential — gated on the connection's provider string being equal to
//! the resolved one, byte for byte (`conn.provider != provider` in
//! `core::account_fallback::filter_available_accounts`, the filter
//! `select_connection` is built on; the same byte comparison in
//! `select_media_connections`). Connections are stored under the canonical id
//! — OAuth writes `provider: "grok-cli"`, and the dashboard posts
//! `AI_PROVIDERS[id].id`.
//!
//! So an unmapped alias never reaches the arm that would have served it: the
//! request dies with `404 No active credentials for provider: gcli`. The alias table
//! is the routing step; the arm is downstream of it.
//!
//! `filter_available_accounts` is the public form of that gate, so these tests
//! assert against it directly rather than restating the rule.

use chrono::Utc;
use openproxy::core::account_fallback::filter_available_accounts;
use openproxy::core::model::{get_model_info, parse_model, resolve_provider_alias};
use openproxy::types::{AppDb, ProviderConnection};

/// Every alias in the group, with the canonical id 9router's registry folds it
/// into (`alias:` / `aliases:` on the matching `providers/registry/*.js`).
const ALIASES: &[(&str, &str, &str)] = &[
    ("brave", "brave-search", "web/search"),
    ("cmc", "commandcode", "deepseek/deepseek-v4-pro"),
    ("devin", "devin-cli", "swe-1.6"),
    ("fish", "fish-audio", "s2-pro"),
    ("gb", "grok-cli", "grok-build"),
    ("gcli", "grok-cli", "grok-build"),
    ("grok-build", "grok-cli", "grok-build"),
    ("marscode", "trae", "ultimate"),
    ("polly", "aws-polly", "neural"),
    ("qd", "qoder", "ultimate"),
    ("stability", "stability-ai", "sd3.5-large"),
    ("tr", "trae", "ultimate"),
];

#[test]
fn an_addressed_alias_resolves_to_the_canonical_provider() {
    for &(alias, provider, model) in ALIASES {
        assert_eq!(
            resolve_provider_alias(alias),
            provider,
            "alias {alias} must resolve to {provider}"
        );
        assert_eq!(
            resolve_provider_alias(provider),
            provider,
            "{provider} is also its own id and must round-trip"
        );

        let parsed = parse_model(&format!("{alias}/{model}"));
        assert_eq!(
            parsed.provider.as_deref(),
            Some(provider),
            "{alias}/{model} must address {provider}"
        );
        assert_eq!(parsed.model.as_deref(), Some(model));
        assert_eq!(parsed.provider_alias.as_deref(), Some(alias));
    }
}

/// The umbrella guard: resolving the alias is only half the job — the canonical
/// id it produces has to be a value a connection is actually stored under, or
/// the credential gate finds nothing and 400s. Before the mappings landed,
/// every one of these returned an empty account list.
#[test]
fn a_remapped_alias_reaches_the_stored_connection() {
    for &(alias, provider, model) in ALIASES {
        let db = AppDb {
            provider_connections: vec![ProviderConnection {
                id: format!("{alias}-conn"),
                provider: provider.to_string(),
                ..ProviderConnection::default()
            }],
            ..AppDb::default()
        };

        let resolved = get_model_info(&format!("{alias}/{model}"), &db);
        let routed = resolved
            .provider
            .as_deref()
            .unwrap_or_else(|| panic!("{alias}/{model} resolved to no provider"));

        let available = filter_available_accounts(
            &db.provider_connections,
            routed,
            &resolved.model,
            None,
            Utc::now(),
        );
        assert_eq!(
            available.len(),
            1,
            "{alias}/{model} routed to {routed}, but the stored connection is {provider}"
        );
    }
}

/// `grok-build` names one provider through two unrelated paths: the built-in
/// model alias for the bare name, and the alias table for the addressed form.
/// 9router feeds the builtin's target back through `resolveProviderAlias`
/// (model.js:73), so both land on `grok-cli`; hardcoding `gcli` in the
/// built-in left them disagreeing, and `gcli` is a display alias no connection
/// is stored under.
#[test]
fn the_bare_grok_build_builtin_agrees_with_the_addressed_spelling() {
    let db = AppDb::default();

    let bare = get_model_info("grok-build", &db);
    assert_eq!(bare.provider.as_deref(), Some("grok-cli"));
    assert_eq!(bare.model, "grok-build");

    for alias in ["gcli", "gb", "grok-build"] {
        let addressed = get_model_info(&format!("{alias}/grok-build"), &db);
        assert_eq!(
            addressed.provider.as_deref(),
            bare.provider.as_deref(),
            "{alias}/grok-build and the bare name must resolve alike"
        );
        assert_eq!(addressed.model, "grok-build");
    }
}
