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
    #[serde(default)]
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

    pub fn alias_to_provider_id(&self) -> HashMap<String, String> {
        self.provider_id_to_alias
            .iter()
            .map(|(provider_id, alias)| (alias.clone(), provider_id.clone()))
            .collect()
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
