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
                            provider: Some(node.id.clone()),
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

    if let Some(resolved) = resolve_model_alias_from_map(&alias_name, &db.model_aliases) {
        return ResolvedModel {
            provider: Some(resolved.provider),
            model: resolved.model,
            route_kind: ModelRouteKind::Direct,
        };
    }

    ResolvedModel {
        provider: Some(infer_provider_from_model_name(&alias_name).to_string()),
        model: alias_name,
        route_kind: ModelRouteKind::Direct,
    }
}

fn infer_provider_from_model_name(model_name: &str) -> &'static str {
    let model_name = model_name.to_lowercase();

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
        "openai"
    }
}
