pub mod catalog;
pub mod catalog_overlay;

use std::collections::{BTreeMap, HashMap};

use once_cell::sync::Lazy;

use crate::types::{AppDb, ModelAliasTarget, ProviderModelRef};

static ALIAS_TO_PROVIDER_ID: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    HashMap::from([
        ("cc", "claude"),
        ("cx", "codex"),
        ("gc", "gemini-cli"),
        ("qw", "qwen"),
        ("if", "iflow"),
        ("ag", "antigravity"),
        ("gh", "github"),
        ("kr", "kiro"),
        ("cu", "cursor"),
        ("kc", "kilocode"),
        ("kmc", "kimi-coding"),
        ("cl", "cline"),
        ("oc", "opencode"),
        ("ocg", "opencode-go"),
        ("el", "elevenlabs"),
        ("openai", "openai"),
        ("anthropic", "anthropic"),
        ("gemini", "gemini"),
        ("openrouter", "openrouter"),
        ("glm", "glm"),
        ("kimi", "kimi"),
        ("minimax", "minimax"),
        ("minimax-cn", "minimax-cn"),
        ("ds", "deepseek"),
        ("deepseek", "deepseek"),
        ("groq", "groq"),
        ("xai", "xai"),
        ("mistral", "mistral"),
        ("pplx", "perplexity"),
        ("perplexity", "perplexity"),
        ("together", "together"),
        ("fireworks", "fireworks"),
        ("cerebras", "cerebras"),
        ("cohere", "cohere"),
        ("nvidia", "nvidia"),
        ("nebius", "nebius"),
        ("siliconflow", "siliconflow"),
        ("hyp", "hyperbolic"),
        ("hyperbolic", "hyperbolic"),
        ("dg", "deepgram"),
        ("deepgram", "deepgram"),
        ("aai", "assemblyai"),
        ("assemblyai", "assemblyai"),
        ("nb", "nanobanana"),
        ("nanobanana", "nanobanana"),
        ("ch", "chutes"),
        ("chutes", "chutes"),
        ("ark", "volcengine-ark"),
        ("volcengine-ark", "volcengine-ark"),
        ("byteplus", "byteplus"),
        ("bpm", "byteplus"),
        ("cursor", "cursor"),
        ("vx", "vertex"),
        ("vertex", "vertex"),
        ("vxp", "vertex-partner"),
        ("vertex-partner", "vertex-partner"),
        ("gw", "grok-web"),
        ("grok-web", "grok-web"),
        ("pw", "perplexity-web"),
        ("perplexity-web", "perplexity-web"),
        ("ds-web", "deepseek-web"),
        ("deepseek-web", "deepseek-web"),
        // ── Enterprise & Cloud ──
        ("databricks", "databricks"),
        ("snowflake", "snowflake"),
        ("heroku", "heroku"),
        ("lambda-ai", "lambda-ai"),
        ("ovhcloud", "ovhcloud"),
        ("wandb", "wandb"),
        // ── Gateway / Bridge ──
        ("kilo-gateway", "kilo-gateway"),
        ("v0-vercel", "v0-vercel"),
        // ── Regional CN ──
        ("alibaba", "alibaba"),
        ("ali", "alibaba"),
        ("alibaba-cn", "alibaba-cn"),
        ("ali-cn", "alibaba-cn"),
        ("moonshot", "moonshot"),
        ("qianfan", "qianfan"),
        ("volcengine", "volcengine"),
        ("zai", "zai"),
        // ── Regional international ──
        ("gigachat", "gigachat"),
        ("upstage", "upstage"),
        ("maritalk", "maritalk"),
        // ── Inference APIs ──
        ("venice", "venice"),
        ("featherless-ai", "featherless-ai"),
        ("friendliai", "friendliai"),
        ("galadriel", "galadriel"),
        ("llamagate", "llamagate"),
        ("nanogpt", "nanogpt"),
        ("synthetic", "synthetic"),
        ("pollinations", "pollinations"),
        ("meta-llama", "meta-llama"),
        // ── Coding / CLI ──
        ("opencode-zen", "opencode-zen"),
        ("kimi-coding-apikey", "kimi-coding-apikey"),
        ("kmca", "kimi-coding-apikey"),
        ("devin-cli", "devin-cli"),
        ("dv", "devin-cli"),
        ("windsurf", "windsurf"),
        ("ws", "windsurf"),
        ("crof", "crof"),
        // ── Media ──
        ("haiper", "haiper"),
        ("hp", "haiper"),
        ("leonardo", "leonardo"),
        ("leo", "leonardo"),
        ("ideogram", "ideogram"),
        ("ideo", "ideogram"),
        ("suno", "suno"),
        ("udio", "udio"),
        // ── Web / Chat ──
        ("chatgpt-web", "chatgpt-web"),
        ("gemini-web", "gemini-web"),
        ("gweb", "gemini-web"),
        ("muse-spark-web", "muse-spark-web"),
        ("ms-web", "muse-spark-web"),
        // ── 9router registry aliases not previously mapped ──────────────
        // Each entry mirrors `alias:` / `aliases:` on the matching
        // 9router providers/registry/*.js file, and every target is verified
        // to exist as a PROVIDER_CONFIGS key. Without these, a request
        // addressed as `cbcn/glm-5.2` or `kgw/some-model` passed the alias
        // through as the provider name and routed to a provider that does
        // not exist.
        ("af", "api-airforce"),
        ("airforce", "api-airforce"),
        ("baidu-qianfan", "baidu"),
        ("bazaar-link", "bazaarlink"),
        ("bb", "blackbox"),
        ("bfl", "black-forest-labs"),
        ("blue-sminds", "bluesminds"),
        ("bm", "bluesminds"),
        ("bzl", "bazaarlink"),
        ("cbai", "codebuddy-intl"),
        ("cbcn", "codebuddy-cn"),
        ("cf", "cloudflare-ai"),
        ("ernie", "baidu"),
        ("fal", "fal-ai"),
        ("fl", "featherless"),
        ("gpse", "google-pse"),
        ("hf", "huggingface"),
        ("hunyuan", "tencent"),
        ("jina", "jina-ai"),
        ("kgw", "kilo-gateway"),
        ("kilogateway", "kilo-gateway"),
        ("kimi-coding", "kimi"),
        ("llm-7", "llm7"),
        ("mimo", "xiaomi-mimo"),
        ("mimo-desktop", "xiaomi-mimo"),
        ("morphllm", "morph"),
        ("pplx-agent", "perplexity-agent"),
        ("pplx-responses", "perplexity-agent"),
        ("ps", "poolside"),
        ("runway", "runwayml"),
        ("samba", "sambanova"),
        ("sambanova-ai", "sambanova"),
        ("tencent-hunyuan", "tencent"),
        ("vercel", "vercel-ai-gateway"),
        ("vn", "venice"),
        ("xmd", "xiaomi-mimo"),
        ("xmtp", "xiaomi-tokenplan"),
        ("zd", "zed"),
        // ── Aliases served by a dedicated executor or the media path ────
        // None of these targets is a PROVIDER_CONFIGS key: each is dispatched
        // by its own arm in chat.rs / media.rs, or by a media adapter. That
        // dispatch is not what makes them reachable, though — the credential
        // gate runs first and compares the connection's provider byte for byte
        // (`conn.provider != provider`, account_fallback; the same test in
        // select_media_connections). An unmapped alias 400s with "No
        // credentials for provider" before the arm that would serve it is ever
        // entered, so the `|| provider == "gcli"` arm beside `grok-cli` does
        // not make `gcli/` work on its own. The table is the routing step.
        ("brave", "brave-search"),
        ("cmc", "commandcode"),
        ("devin", "devin-cli"),
        ("fish", "fish-audio"),
        ("gb", "grok-cli"),
        ("gcli", "grok-cli"),
        ("grok-build", "grok-cli"),
        ("marscode", "trae"),
        ("polly", "aws-polly"),
        ("qd", "qoder"),
        ("stability", "stability-ai"),
        ("tr", "trae"),
    ])
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModel {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub is_alias: bool,
    pub provider_alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRouteKind {
    Direct,
    Combo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub provider: Option<String>,
    pub model: String,
    pub route_kind: ModelRouteKind,
}

pub fn resolve_provider_alias(alias_or_id: &str) -> String {
    ALIAS_TO_PROVIDER_ID
        .get(alias_or_id)
        .copied()
        .unwrap_or(alias_or_id)
        .to_string()
}

pub fn parse_model(model_str: &str) -> ParsedModel {
    if model_str.is_empty() {
        return ParsedModel {
            provider: None,
            model: None,
            is_alias: false,
            provider_alias: None,
        };
    }

    if let Some(first_slash) = model_str.find('/') {
        let provider_or_alias = &model_str[..first_slash];
        let model = &model_str[first_slash + 1..];
        return ParsedModel {
            provider: Some(resolve_provider_alias(provider_or_alias)),
            model: Some(model.to_string()),
            is_alias: false,
            provider_alias: Some(provider_or_alias.to_string()),
        };
    }

    ParsedModel {
        provider: None,
        model: Some(model_str.to_string()),
        is_alias: true,
        provider_alias: None,
    }
}

/// 9router's `BUILTIN_MODEL_ALIASES` (open-sse/services/model.js:20-22), applied
/// after the config-driven map exactly as 9router does.
///
/// Without it `grok-build` fell through to prefix inference, which OpenProxy
/// routes to `xai` — a different provider than the one the alias names.
///
/// The builtin's target is the string `"gcli/grok-build"`, and 9router feeds
/// that target's provider half back through `resolveProviderAlias`
/// (model.js:73), so the bare name lands on the canonical `grok-cli`. Spelling
/// the alias out here left the two forms of one provider disagreeing, and
/// `gcli` is a display alias that no connection is ever stored under.
fn builtin_model_alias(model: &str) -> Option<ProviderModelRef> {
    match model {
        "grok-build" => Some(ProviderModelRef {
            provider: resolve_provider_alias("gcli"),
            model: "grok-build".to_string(),
            extra: BTreeMap::new(),
        }),
        _ => None,
    }
}

pub fn resolve_model_alias_from_map(
    alias: &str,
    aliases: &BTreeMap<String, ModelAliasTarget>,
) -> Option<ProviderModelRef> {
    let resolved = aliases.get(alias)?;
    match resolved {
        ModelAliasTarget::Path(path) => {
            path.split_once('/')
                .map(|(provider_or_alias, model)| ProviderModelRef {
                    provider: resolve_provider_alias(provider_or_alias),
                    model: model.to_string(),
                    extra: BTreeMap::new(),
                })
        }
        ModelAliasTarget::Mapping(mapping) => Some(ProviderModelRef {
            provider: resolve_provider_alias(&mapping.provider),
            model: mapping.model.clone(),
            extra: mapping.extra.clone(),
        }),
    }
}

pub fn get_model_info(model_str: &str, db: &AppDb) -> ResolvedModel {
    let (explicit_combo, normalized_model) = model_str
        .strip_prefix("combo:")
        .map(|value| (true, value))
        .unwrap_or((false, model_str));

    if explicit_combo {
        return ResolvedModel {
            provider: None,
            model: normalized_model.to_string(),
            route_kind: ModelRouteKind::Combo,
        };
    }

    let parsed = parse_model(normalized_model);

    if !parsed.is_alias {
        if let (Some(provider), Some(provider_alias), Some(model)) = (
            parsed.provider.clone(),
            parsed.provider_alias.clone(),
            parsed.model.clone(),
        ) {
            if provider == provider_alias {
                for node_type in [
                    "openai-compatible",
                    "anthropic-compatible",
                    "custom-embedding",
                ] {
                    if let Some(node) = db.provider_nodes.iter().find(|node| {
                        node.r#type == node_type
                            && node.prefix.as_deref() == Some(provider_alias.as_str())
                    }) {
                        return ResolvedModel {
                            // JS parity: credentials are keyed by the node's
                            // id/prefix (the connection `provider` value), not
                            // the display name.
                            provider: node.id.clone().into(),
                            model,
                            route_kind: ModelRouteKind::Direct,
                        };
                    }
                }
            }

            return ResolvedModel {
                provider: Some(provider),
                model,
                route_kind: ModelRouteKind::Direct,
            };
        }
    }

    let alias_name = parsed.model.unwrap_or_default();
    if db.combos.iter().any(|combo| combo.name == alias_name) {
        return ResolvedModel {
            provider: None,
            model: alias_name,
            route_kind: ModelRouteKind::Combo,
        };
    }

    if let Some(resolved) = resolve_model_alias_from_map(&alias_name, &db.model_aliases)
        .or_else(|| builtin_model_alias(&alias_name))
    {
        return ResolvedModel {
            provider: Some(resolved.provider),
            model: resolved.model,
            route_kind: ModelRouteKind::Direct,
        };
    }

    let fallback = infer_provider_from_model_name(&alias_name).to_string();
    ResolvedModel {
        provider: Some(fallback),
        model: alias_name,
        route_kind: ModelRouteKind::Direct,
    }
}

/// Infer the target provider from a bare model name string, based on known
/// model-family prefixes.  This is the last-resort fallback used when no
/// explicit alias or combo maps the model — it avoids forcing every unknown
/// model to "openai".
///
/// Known model-family prefix → provider mappings, mirroring 9router's
/// `MODEL_PREFIX_PROVIDERS` (open-sse/services/model.js:126-133) rule for rule
/// — first match wins, `"openai"` is the fallback:
///
/// | Prefix(es)                  | Provider     |
/// |-----------------------------|--------------|
/// | `claude-`                   | `anthropic`  |
/// | `gemini-`                   | `gemini`     |
/// | `gpt-`, `o1`, `o3`, `o4`   | `openai`     |
/// | `deepseek-`                 | `openrouter` |
/// | Everything else (llama, …)  | `openai`     |
///
/// Four further families used to be inferred to their native vendor here
/// (`mistral-*` → mistral, `command-*` → cohere, `grok-*` → xai, `jamba-*` →
/// ai21). 9router has no such rule, so a bare name in any of them reaches its
/// `openai` fallback there. The `jamba-*` arm in particular could only
/// dead-end: `ai21` is a configurable API-key provider but has no entry in the
/// merged provider catalog, so nothing could resolve a `jamba-*` name through
/// it. The one bare name that does need a non-openai route, `grok-build`, is
/// covered by `builtin_model_alias` above. See docs/parity-9router.md.
fn infer_provider_from_model_name(model_name: &str) -> &'static str {
    let model_name = model_name.to_lowercase();

    // Checked before the family prefixes: the Codex CLI sends this bare virtual
    // model for auto-review, and it must stay on OAuth Codex rather than
    // falling through to the openai fallback. Not a 9router rule — kept because
    // the CLI depends on it.
    if model_name == "codex-auto-review" {
        return "codex";
    }

    if model_name.starts_with("claude-") {
        "anthropic"
    } else if model_name.starts_with("gemini-") {
        "gemini"
    } else if model_name.starts_with("gpt-")
        || model_name.starts_with("o1")
        || model_name.starts_with("o3")
        || model_name.starts_with("o4")
    {
        "openai"
    } else if model_name.starts_with("deepseek-") {
        "openrouter"
    } else {
        // Unknown model prefixes route to "openai" as the generic fallback.
        // Common model families that land here: llama-*, codellama-*, phi-*,
        // nemotron-*, dbrx-*, qwen-*, yi-*, gemma-* — and, matching 9router,
        // mistral-*, command-*, grok-* and jamba-*.
        "openai"
    }
}

#[cfg(test)]
mod alias_parity_tests {
    /// Regression (audit finding #48): 9router declares `alias` / `aliases`
    /// on its provider registry entries and accepts an addressed model of the
    /// form `<alias>/<model>`. OpenProxy's hand table carried only a subset,
    /// so `cbcn/glm-5.2`, `kgw/some-model`, `mimo/x` and 35 others passed the
    /// alias straight through as the provider name and routed to a provider
    /// that does not exist.
    #[test]
    fn added_aliases_resolve_to_the_9router_canonical_id() {
        for (alias, expected) in [
            ("cbcn", "codebuddy-cn"),
            ("cbai", "codebuddy-intl"),
            ("kimi-coding", "kimi"),
            ("kgw", "kilo-gateway"),
            ("kilogateway", "kilo-gateway"),
            ("mimo", "xiaomi-mimo"),
            ("mimo-desktop", "xiaomi-mimo"),
            ("xmd", "xiaomi-mimo"),
            ("xmtp", "xiaomi-tokenplan"),
            ("baidu-qianfan", "baidu"),
            ("ernie", "baidu"),
            ("sambanova-ai", "sambanova"),
            ("pplx-agent", "perplexity-agent"),
            ("pplx-responses", "perplexity-agent"),
            ("af", "api-airforce"),
            ("airforce", "api-airforce"),
            ("bfl", "black-forest-labs"),
            ("bm", "bluesminds"),
            ("cf", "cloudflare-ai"),
            ("gpse", "google-pse"),
            ("hf", "huggingface"),
            ("hunyuan", "tencent"),
            ("tencent-hunyuan", "tencent"),
            ("jina", "jina-ai"),
            ("llm-7", "llm7"),
            ("morphllm", "morph"),
            ("ps", "poolside"),
            ("runway", "runwayml"),
            ("vercel", "vercel-ai-gateway"),
            ("vn", "venice"),
            ("zd", "zed"),
            ("brave", "brave-search"),
            ("cmc", "commandcode"),
            ("devin", "devin-cli"),
            ("fish", "fish-audio"),
            ("gb", "grok-cli"),
            ("gcli", "grok-cli"),
            ("grok-build", "grok-cli"),
            ("marscode", "trae"),
            ("polly", "aws-polly"),
            ("qd", "qoder"),
            ("stability", "stability-ai"),
            ("tr", "trae"),
        ] {
            assert_eq!(
                super::resolve_provider_alias(alias),
                expected,
                "alias {alias} must resolve to {expected}"
            );
        }
    }

    /// An addressable model string must route to the canonical id, not the
    /// alias — that is the whole point of the table.
    #[test]
    fn parsing_an_addressed_model_normalises_the_provider() {
        let parsed = super::parse_model("cbcn/glm-5.2");
        assert_eq!(parsed.provider.as_deref(), Some("codebuddy-cn"));
        assert_eq!(parsed.model.as_deref(), Some("glm-5.2"));
        assert_eq!(parsed.provider_alias.as_deref(), Some("cbcn"));

        let kgw = super::parse_model("kgw/some-model");
        assert_eq!(kgw.provider.as_deref(), Some("kilo-gateway"));
        assert_eq!(kgw.model.as_deref(), Some("some-model"));
    }
}

#[cfg(test)]
mod builtin_alias_parity {
    use super::*;

    /// openproxy-e43o: 9router declares a built-in alias applied after the
    /// config map (model.js:20-22, 111-113). Without it `grok-build` fell to
    /// prefix inference, which OpenProxy routes to xai — a different provider
    /// from the one the alias names.
    #[test]
    fn built_in_grok_build_alias_resolves_to_grok_cli() {
        let db = AppDb::default();
        let resolved = get_model_info("grok-build", &db);
        assert_eq!(resolved.provider.as_deref(), Some("grok-cli"));
        assert_eq!(resolved.model, "grok-build");
        assert_eq!(resolved.route_kind, ModelRouteKind::Direct);
    }

    /// The config-driven map still wins over the built-in, as in 9router where
    /// the user map is tried first.
    #[test]
    fn config_alias_still_overrides_the_built_in() {
        let mut db = AppDb::default();
        db.model_aliases.insert(
            "grok-build".to_string(),
            ModelAliasTarget::Mapping(ProviderModelRef {
                provider: "my-gw".to_string(),
                model: "grok-4".to_string(),
                extra: BTreeMap::new(),
            }),
        );
        let resolved = get_model_info("grok-build", &db);
        assert_eq!(resolved.provider.as_deref(), Some("my-gw"));
        assert_eq!(resolved.model, "grok-4");
    }

    /// 9router checks the Codex auto-review virtual model before the family
    /// prefixes, specifically so it stays on OAuth Codex instead of falling
    /// through to the openai default.
    #[test]
    fn codex_auto_review_routes_to_codex_not_the_openai_fallback() {
        let db = AppDb::default();
        let resolved = get_model_info("codex-auto-review", &db);
        assert_eq!(resolved.provider.as_deref(), Some("codex"));
    }
}

/// openproxy-e43o: `infer_provider_from_model_name` used to carry four
/// provider-inference rules 9router does not have (mistral → mistral,
/// command → cohere, grok → xai, jamba → ai21), so a bare model name reached a
/// different vendor here than there. model.js:126-142 has exactly five entries
/// and falls back to "openai".
#[cfg(test)]
mod prefix_inference_parity {
    use super::*;

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
    /// reaches grok-cli through the built-in alias rather than the openai
    /// fallback.
    #[test]
    fn the_builtin_alias_still_wins_over_the_openai_fallback() {
        let db = AppDb::default();
        assert_eq!(
            get_model_info("grok-build", &db).provider.as_deref(),
            Some("grok-cli")
        );
    }
}
