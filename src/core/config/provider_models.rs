//! Port of `open-sse/config/providerModels.js`.
//!
//! Single source of truth for provider model catalogues. The data lives in
//! `data/9router_models.json` (auto-extracted from the upstream JS via the
//! one-shot dump script) and is parsed once at startup into a
//! `BTreeMap<provider_alias, Vec<ProviderModel>>`.
//!
//! Why a JSON file instead of a Rust literal? The upstream catalog is ~5000
//! lines and changes frequently; keeping it as data lets us re-dump from
//! 9router without touching code, and it lets the dashboard JSON-stringify
//! the same blob for the front-end without re-encoding.

use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

const RAW_DATA: &str = include_str!("data/9router_models.json");

#[derive(Debug, Clone, Deserialize)]
struct RawDump {
    #[serde(rename = "PROVIDER_MODELS")]
    provider_models: BTreeMap<String, Vec<ProviderModel>>,
    #[serde(rename = "TTS_MODELS_CONFIG")]
    #[allow(dead_code)]
    tts_models_config: Value,
    #[serde(rename = "GOOGLE_TTS_LANGUAGES")]
    #[allow(dead_code)]
    google_tts_languages: Vec<Value>,
}

/// One catalogue entry for a provider's model list. Free-form by design —
/// the upstream catalogue mixes chat, image, embedding, TTS, and STT
/// entries in the same arrays, each with its own optional fields.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    /// `chat` (default) | `image` | `embedding` | `tts` | `stt` | `llm`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Models that should never be advertised (used by Antigravity to
    /// strip `image`/`audio` from inbound requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip: Option<Vec<String>>,
    /// Image/STT/TTS-specific tunable parameters surfaced to the dashboard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<String>>,
    /// Image-only: which capabilities the upstream supports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// When set, the request is translated to this format before being
    /// forwarded upstream (e.g. `claude` to wrap an OpenAI-shaped chat
    /// request in Anthropic's Messages format).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_format: Option<String>,
    /// Antigravity: skip thinking-mode signature for this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    /// Codex: original upstream id when this entry is a synthetic alias
    /// (e.g. `gpt-5-codex-review` aliases `gpt-5-codex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model_id: Option<String>,
    /// Codex: quota family the synthetic alias counts against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_family: Option<String>,
    /// Catch-all for any fields we have not explicitly modelled.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

static DUMP: Lazy<RawDump> = Lazy::new(|| {
    serde_json::from_str(RAW_DATA).expect("9router_models.json is malformed; re-run the dump script")
});

/// Provider catalog keyed by 9router alias (`cc`, `cx`, `kr`, `openai`, …).
pub fn provider_models() -> &'static BTreeMap<String, Vec<ProviderModel>> {
    &DUMP.provider_models
}

/// Lookup the model list for a single provider alias.
pub fn models_for(alias: &str) -> Option<&'static [ProviderModel]> {
    DUMP.provider_models.get(alias).map(Vec::as_slice)
}

/// All known provider aliases.
pub fn aliases() -> impl Iterator<Item = &'static str> {
    DUMP.provider_models.keys().map(String::as_str)
}

/// Raw JSON Value of `TTS_MODELS_CONFIG` (consumed by `tts_models.rs`).
pub fn tts_models_config() -> &'static Value {
    &DUMP.tts_models_config
}

/// Raw JSON Value of `GOOGLE_TTS_LANGUAGES` (consumed by
/// `google_tts_languages.rs`).
pub fn google_tts_languages() -> &'static [Value] {
    &DUMP.google_tts_languages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_known_aliases() {
        let known = ["cc", "cx", "gc", "qw", "if", "ag", "gh", "kr", "openai", "anthropic", "gemini"];
        for alias in known {
            assert!(
                models_for(alias).is_some(),
                "expected provider alias {alias} in provider_models.json"
            );
        }
    }

    #[test]
    fn kiro_has_thinking_variants() {
        let kr = models_for("kr").expect("kr alias");
        assert!(kr.iter().any(|m| m.id == "claude-sonnet-4.5-thinking"));
        assert!(kr
            .iter()
            .any(|m| m.id == "claude-sonnet-4.5-thinking-agentic"));
    }

    #[test]
    fn openai_has_image_models_with_params() {
        let oai = models_for("openai").expect("openai alias");
        let dalle = oai.iter().find(|m| m.id == "dall-e-3").expect("dall-e-3");
        assert_eq!(dalle.r#type.as_deref(), Some("image"));
        let params = dalle.params.as_ref().expect("params");
        assert!(params.contains(&"size".to_string()));
    }
}
