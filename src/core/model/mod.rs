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
fn builtin_model_alias(model: &str) -> Option<ProviderModelRef> {
    match model {
        "grok-build" => Some(ProviderModelRef {
            provider: "gcli".to_string(),
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
/// Known model-family prefix → provider mappings:
///
/// | Prefix(es)                        | Provider      | In 9router? |
/// |-----------------------------------|---------------|-------------|
/// | `codex-auto-review` (exact)       | `codex`       | yes         |
/// | `claude-`                         | `anthropic`   | yes         |
/// | `gemini-`                         | `gemini`      | yes         |
/// | `gpt-`, `o1`, `o3`, `o4`         | `openai`      | yes         |
/// | `deepseek-`                       | `openrouter`  | yes         |
/// | `mistral-`, `open-mistral-`, …   | `mistral`     | no          |
/// | `command-`, `command-r`           | `cohere`      | no          |
/// | `grok-`                           | `xai`         | no          |
/// | `jamba-`                          | `ai21`        | no          |
/// | Everything else (llama, phi, …)   | `openai`      | yes         |
///
/// The four marked "no" are OpenProxy's own: 9router has no rule for those
/// families, so an unqualified `mistral-large` or `grok-4` would fall to its
/// `openai` default. Sending them to a provider that actually serves them is
/// the better behaviour and the divergence is deliberate — removing the rules
/// would break real routing to fix a difference that only shows up for names no
/// user sends bare. Product decision, not an oversight; flagged, not settled.
fn infer_provider_from_model_name(model_name: &str) -> &'static str {
    let model_name = model_name.to_lowercase();

    // Checked before the family prefixes, as in 9router's MODEL_PREFIX_PROVIDERS
    // (model.js:126-133): the Codex CLI sends this bare virtual model for
    // auto-review, and it must stay on OAuth Codex rather than falling through
    // to the openai fallback.
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
    } else if model_name.starts_with("mistral-")
        || model_name.starts_with("open-mistral-")
        || model_name.starts_with("mistralai-")
        || model_name.starts_with("codestral-")
        || model_name.starts_with("ministral-")
        || model_name.starts_with("mixtral-")
    {
        "mistral"
    } else if model_name.starts_with("command-") || model_name.starts_with("command-r") {
        "cohere"
    } else if model_name.starts_with("grok-") {
        "xai"
    } else if model_name.starts_with("jamba-") {
        "ai21"
    } else {
        // Unknown model prefixes route to "openai" as the generic fallback.
        // Common model families that land here: llama-*, codellama-*, phi-*,
        // nemotron-*, dbrx-*, qwen-*, yi-*, gemma-*.
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
    fn built_in_grok_build_alias_resolves_to_gcli() {
        let db = AppDb::default();
        let resolved = get_model_info("grok-build", &db);
        assert_eq!(resolved.provider.as_deref(), Some("gcli"));
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
