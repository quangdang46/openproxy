use std::collections::HashMap;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderCatalogFile {
    provider_id_to_alias: HashMap<String, String>,
    provider_models: Vec<ProviderModelsEntry>,
    providers: Vec<ProviderCatalogProvider>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub quota_family: Option<String>,
    #[serde(default)]
    pub strip: Option<String>,
    #[serde(default)]
    pub target_format: Option<String>,
    #[serde(default)]
    pub upstream_model_id: Option<String>,
    #[serde(default, alias = "contextLength")]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsEntry {
    pub alias: String,
    pub models: Vec<ProviderCatalogModel>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogProvider {
    pub id: String,
    pub alias: String,
    pub service_kinds: Vec<String>,
    pub tts_models: Vec<String>,
    pub embedding_models: Vec<String>,
    pub has_search: bool,
    pub has_fetch: bool,
    #[serde(default)]
    pub vision: Option<bool>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output: Option<u32>,
    #[serde(default)]
    pub tools: Option<bool>,
}

#[derive(Debug)]
pub struct ProviderCatalog {
    provider_id_to_alias: HashMap<String, String>,
    provider_models: Vec<ProviderModelsEntry>,
    provider_models_by_alias: HashMap<String, Vec<ProviderCatalogModel>>,
    providers_by_id: HashMap<String, ProviderCatalogProvider>,
}

impl ProviderCatalog {
    pub fn provider_info(&self, provider_id: &str) -> Option<&ProviderCatalogProvider> {
        self.providers_by_id.get(provider_id)
    }

    pub fn static_alias_for_provider(&self, provider_id: &str) -> Option<&str> {
        self.provider_id_to_alias
            .get(provider_id)
            .map(String::as_str)
    }

    pub fn provider_ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.provider_id_to_alias.keys().map(|s| s.as_str())
    }

    pub fn iter_provider_models(&self) -> impl Iterator<Item = &ProviderModelsEntry> {
        self.provider_models.iter()
    }

    pub fn models_for_alias(&self, alias: &str) -> Option<&[ProviderCatalogModel]> {
        self.provider_models_by_alias.get(alias).map(Vec::as_slice)
    }

    pub fn find_model(&self, provider_id: &str, model_id: &str) -> Option<&ProviderCatalogModel> {
        let alias = self.static_alias_for_provider(provider_id)?;
        self.models_for_alias(alias)?
            .iter()
            .find(|m| m.id == model_id)
    }

    /// Build reverse map: alias → provider_id.
    ///
    /// Self-referencing entries (where provider_id == alias) are inserted
    /// **only** when no non-self-referencing entry already claimed that alias.
    /// This prevents `qianfan → qianfan` from overwriting the correct
    /// `qianfan → baidu` mapping produced by the forward entry `baidu → qianfan`.
    pub fn alias_to_provider_id(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        // Pass 1: non-self-referencing entries (baidu → qianfan).
        for (provider_id, alias) in &self.provider_id_to_alias {
            if provider_id != alias {
                map.insert(alias.clone(), provider_id.clone());
            }
        }
        // Pass 2: self-referencing entries (openai → openai) only if not taken.
        for (provider_id, alias) in &self.provider_id_to_alias {
            if provider_id == alias && !map.contains_key(alias) {
                map.insert(alias.clone(), provider_id.clone());
            }
        }
        map
    }
}

static PROVIDER_CATALOG: Lazy<ProviderCatalog> = Lazy::new(|| {
    let raw = include_str!("provider_catalog.json");
    let parsed: ProviderCatalogFile =
        serde_json::from_str(raw).expect("provider_catalog.json should be valid");

    let provider_models_by_alias = parsed
        .provider_models
        .iter()
        .map(|entry| (entry.alias.clone(), entry.models.clone()))
        .collect();

    let providers_by_id = parsed
        .providers
        .iter()
        .map(|provider| (provider.id.clone(), provider.clone()))
        .collect();

    ProviderCatalog {
        provider_id_to_alias: parsed.provider_id_to_alias,
        provider_models: parsed.provider_models,
        provider_models_by_alias,
        providers_by_id,
    }
});

pub fn provider_catalog() -> &'static ProviderCatalog {
    &PROVIDER_CATALOG
}

#[cfg(test)]
mod tests {
    use super::*;

    // Guards against catalog regeneration silently dropping the opencode-zen
    // registration (this happened once: merged in 0895cac, then lost when the
    // catalog was regenerated, which broke the /dashboard/combos model picker).
    #[test]
    fn opencode_zen_registered_in_static_catalog() {
        let catalog = provider_catalog();

        let provider = catalog
            .provider_info("opencode-zen")
            .expect("opencode-zen should have a provider entry in provider_catalog.json");
        assert_eq!(provider.alias, "opencode-zen");

        let models = catalog
            .models_for_alias("opencode-zen")
            .expect("opencode-zen should have models in provider_catalog.json");
        assert!(
            models.len() >= 40,
            "expected the zen model list, got {}",
            models.len()
        );
        assert!(models.iter().any(|m| m.id == "gpt-5.4"));
        assert!(models.iter().any(|m| m.id == "kimi-k2.6"));
    }

    // Bead .46: all 17 parity providers must be registered in the static
    // catalog (providerIdToAlias + providerModels + providers[]), keyed by the
    // same aliases the 9router v0.5.50 registry files declare.
    #[test]
    fn all_17_parity_providers_registered() {
        let catalog = provider_catalog();

        // (provider id, js alias) — mirror of the `alias` fields in
        // open-sse/providers/registry/*.js for the 17 parity providers.
        let expected: &[(&str, &str)] = &[
            ("api-airforce", "af"),
            ("baidu", "qianfan"),
            ("bluesminds", "bm"),
            ("clinepass", "clinepass"),
            ("codebuddy-intl", "cbai"),
            ("featherless", "featherless"),
            ("kilo-gateway", "kgw"),
            ("perplexity-agent", "perplexity-agent"),
            ("poolside", "poolside"),
            ("selfhosted-embedding", "selfhosted-embedding"),
            ("selfhosted-stt", "selfhosted-stt"),
            ("selfhosted-tts", "selfhosted-tts"),
            ("tencent", "hunyuan"),
            ("tokenrouter", "tokenrouter"),
            ("venice", "venice"),
            ("zed", "zd"),
            ("alims-intl", "alims-intl"),
        ];

        for (provider_id, alias) in expected {
            assert_eq!(
                catalog.static_alias_for_provider(provider_id),
                Some(*alias),
                "providerIdToAlias[{}] should resolve to {}",
                provider_id,
                alias
            );
            assert!(
                catalog.models_for_alias(alias).is_some(),
                "providerModels should contain an entry for alias {}",
                alias
            );
            let provider = catalog
                .provider_info(provider_id)
                .unwrap_or_else(|| panic!("providers[] should contain {}", provider_id));
            assert_eq!(provider.alias, *alias);
        }
    }

    // The catalog models carry the JS `contextLength` through into
    // context_window (bead .46: it was previously dropped by deserialization).
    #[test]
    fn catalog_model_context_length_survives_deserialization() {
        let catalog = provider_catalog();

        let m = catalog
            .find_model("baidu", "deepseek-v4-pro")
            .expect("baidu/deepseek-v4-pro should be in provider_catalog.json");
        assert_eq!(m.context_window, Some(1_048_576));
        assert_eq!(m.kind, "llm");

        // api-airforce and kilo-gateway carry explicit context lengths too.
        let m = catalog
            .find_model("api-airforce", "google/gemini-2.5-flash")
            .expect("api-airforce/google/gemini-2.5-flash should be in the catalog");
        assert_eq!(m.context_window, Some(1_048_576));
        let m = catalog
            .find_model("kilo-gateway", "nvidia/nemotron-3-ultra-550b-a55b:free")
            .expect("kilo-gateway nemotron should be in the catalog");
        assert_eq!(m.context_window, Some(1_000_000));
    }

    // Media kinds and service metadata for the parity providers must match the
    // JS registry files (bead .46).
    #[test]
    fn catalog_media_kinds_match_registry() {
        let catalog = provider_catalog();

        // venice: embedding models present, image models carry kind image.
        let venice = catalog.models_for_alias("venice").expect("venice models");
        let emb: Vec<&str> = venice
            .iter()
            .filter(|m| m.kind == "embedding")
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(
            emb,
            vec![
                "text-embedding-3-large",
                "text-embedding-bge-m3",
                "text-embedding-qwen3-8b"
            ]
        );
        assert!(venice
            .iter()
            .any(|m| m.id == "venice-sd35" && m.kind == "image"));

        // tokenrouter: video/image/audio kinds preserved from the seed snapshot.
        let tr = catalog
            .models_for_alias("tokenrouter")
            .expect("tokenrouter models");
        assert!(tr
            .iter()
            .any(|m| m.id == "MiniMax-Hailuo-2.3" && m.kind == "video"));
        assert!(tr
            .iter()
            .any(|m| m.id == "bytedance-seed/seedream-5.0-pro" && m.kind == "image"));
        assert!(tr
            .iter()
            .any(|m| m.id == "openai/gpt-audio" && m.kind == "audio"));
        assert!(tr
            .iter()
            .any(|m| m.id == "openai/gpt-5.4" && m.kind == "llm"));
        // The embedding service model is declared on the provider entry, not as
        // a kind on the model (JS leaves its kind implicit).
        let tr_provider = catalog
            .provider_info("tokenrouter")
            .expect("tokenrouter provider");
        assert_eq!(
            tr_provider.embedding_models,
            vec!["google/gemini-embedding-2"]
        );
        assert_eq!(tr_provider.service_kinds, vec!["llm", "embedding", "image"]);

        // perplexity-agent: webSearch kind + hasSearch.
        let pa = catalog
            .provider_info("perplexity-agent")
            .expect("perplexity-agent provider");
        assert!(pa.has_search);
        assert_eq!(pa.service_kinds, vec!["llm", "webSearch"]);

        // selfhosted-*: single placeholder models, correct kinds and lists.
        let se = catalog
            .provider_info("selfhosted-embedding")
            .expect("selfhosted-embedding");
        assert_eq!(se.service_kinds, vec!["embedding"]);
        assert_eq!(se.embedding_models, vec!["embedding"]);
        assert!(catalog
            .models_for_alias("selfhosted-embedding")
            .is_some_and(|ms| {
                ms.len() == 1 && ms[0].id == "embedding" && ms[0].kind == "embedding"
            }));
        let stt = catalog
            .provider_info("selfhosted-stt")
            .expect("selfhosted-stt");
        assert_eq!(stt.service_kinds, vec!["stt"]);
        assert!(catalog
            .models_for_alias("selfhosted-stt")
            .is_some_and(|ms| { ms.len() == 1 && ms[0].id == "whisper-1" && ms[0].kind == "stt" }));
        let tts = catalog
            .provider_info("selfhosted-tts")
            .expect("selfhosted-tts");
        assert_eq!(tts.service_kinds, vec!["tts"]);
        assert_eq!(tts.tts_models, vec!["kokoro"]);
        assert!(catalog
            .models_for_alias("selfhosted-tts")
            .is_some_and(|ms| { ms.len() == 1 && ms[0].id == "kokoro" && ms[0].kind == "tts" }));
    }

    // providerIdToAlias must resolve provider ids to the JS aliases, and
    // find_model must reach models through them (bead .46).
    #[test]
    fn parity_provider_aliases_resolve() {
        let catalog = provider_catalog();

        for (provider_id, alias) in [
            ("venice", "venice"),
            ("tencent", "hunyuan"),
            ("baidu", "qianfan"),
            ("zed", "zd"),
            ("codebuddy-intl", "cbai"),
            ("kilo-gateway", "kgw"),
            ("api-airforce", "af"),
            ("bluesminds", "bm"),
            ("tokenrouter", "tokenrouter"),
            ("perplexity-agent", "perplexity-agent"),
            ("alitp-intl", "alitp-intl"),
        ] {
            assert_eq!(
                catalog.static_alias_for_provider(provider_id),
                Some(alias),
                "{} should map to {}",
                provider_id,
                alias
            );
        }

        // Reverse resolution: alias -> provider id.
        let reverse = catalog.alias_to_provider_id();
        for (provider_id, alias) in [
            ("venice", "venice"),
            ("tencent", "hunyuan"),
            ("baidu", "qianfan"),
        ] {
            assert_eq!(reverse.get(alias).map(String::as_str), Some(provider_id));
        }

        let m = catalog
            .find_model("baidu", "deepseek-v4-pro")
            .expect("baidu/deepseek-v4-pro should resolve through qianfan");
        assert_eq!(m.name.as_deref(), Some("DeepSeek V4 Pro"));

        // alitp-intl (Alibaba Token Plan) — new in v0.5.55.
        let m = catalog
            .find_model("alitp-intl", "qwen3.7-max")
            .expect("alitp-intl/qwen3.7-max should resolve");
        assert_eq!(m.name.as_deref(), Some("Qwen3.7 Max"));
        let m = catalog
            .find_model("alitp-intl", "deepseek-v4-pro")
            .expect("alitp-intl/deepseek-v4-pro should resolve");
        assert_eq!(m.name.as_deref(), Some("DeepSeek V4 Pro"));

        // GLM 5.3 — new in v0.5.55.
        let m = catalog
            .find_model("glm", "glm-5.3")
            .expect("glm/glm-5.3 should resolve");
        assert_eq!(m.name.as_deref(), Some("GLM 5.3"));
        let m = catalog
            .find_model("glm-cn", "glm-5.3")
            .expect("glm-cn/glm-5.3 should resolve");
        assert_eq!(m.name.as_deref(), Some("GLM 5.3"));

        // Gemini 3.7 Flash tiered — new in v0.5.55.
        for (id, name) in [
            ("gemini-3.7-flash-high", "Gemini 3.7 Flash (High)"),
            ("gemini-3.7-flash-medium", "Gemini 3.7 Flash (Medium)"),
            ("gemini-3.7-flash-low", "Gemini 3.7 Flash (Low)"),
        ] {
            let m = catalog
                .find_model("antigravity", id)
                .unwrap_or_else(|| panic!("antigravity/{id} should resolve"));
            assert_eq!(m.name.as_deref(), Some(name));
            // Each tier maps to an upstreamModelId.
            let upstream = m.upstream_model_id.as_deref().unwrap_or("");
            assert!(
                upstream.contains("gemini-3.7-flash-tiered"),
                "{id} should have upstreamModelId containing tiered, got: {upstream}"
            );
        }
    }
}
