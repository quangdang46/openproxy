//! Port of `open-sse/config/ttsModels.js`. The full TTS_MODELS_CONFIG
//! object is exposed as a `serde_json::Value` so callers can json-relay
//! it to the dashboard or pick fields by path.

use crate::core::config::provider_models::tts_models_config as raw_config;
use serde_json::Value;

/// Full TTS configuration as a JSON object keyed by provider name.
pub fn config() -> &'static Value {
    raw_config()
}

/// Subsection for a single provider (e.g. `"openai"`, `"elevenlabs"`,
/// `"edge-tts"`, `"google-tts"`, `"minimax"`).
pub fn provider(name: &str) -> Option<&'static Value> {
    raw_config().get(name)
}

/// Models advertised by `provider`, or `None` if the provider is not
/// known.
pub fn models_for(provider_name: &str) -> Option<&'static Value> {
    provider(provider_name).and_then(|p| p.get("models"))
}

/// Voices advertised for `model_id` under `provider_name`.
pub fn voices_for(provider_name: &str, model_id: &str) -> Option<&'static Value> {
    provider(provider_name)
        .and_then(|p| p.get("voices"))
        .and_then(|v| v.get(model_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_are_present() {
        for name in ["openai", "openrouter", "elevenlabs", "edge-tts"] {
            assert!(provider(name).is_some(), "missing TTS provider {name}");
        }
    }

    #[test]
    fn openai_has_voices_for_tts_1() {
        let voices = voices_for("openai", "tts-1").expect("openai/tts-1 voices");
        let arr = voices.as_array().expect("voice list");
        assert!(!arr.is_empty());
    }
}
