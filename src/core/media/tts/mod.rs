//! TTS (text-to-speech) provider adapters ported from
//! `open-sse/handlers/ttsProviders/`.
//!
//! Each adapter implements the [`TtsAdapter`] trait. The
//! [`get_tts_adapter`] registry returns a special-case adapter for
//! providers with custom request shapes (OpenAI, OpenRouter, Gemini,
//! ElevenLabs, MiniMax, Edge-TTS, Google-TTS, local-device). For
//! everything else, [`synthesize_via_format`] dispatches by the upstream
//! `ttsConfig.format` (hyperbolic, deepgram, nvidia, huggingface,
//! inworld, cartesia, playht, coqui, tortoise, openai-compat).

pub mod base;
mod edge_tts;
mod elevenlabs;
mod gemini;
mod generic_formats;
mod google_tts;
pub mod handler;
mod local_device;
mod minimax;
mod openai;
mod openrouter;

pub use base::{TtsAdapter, TtsRequest, TtsResult};
pub use generic_formats::{synthesize_via_format, GenericFormat, GenericTtsRequest};
pub use handler::{handle_tts, TtsHandlerError};

/// Synthesize speech for `provider` via either a dedicated adapter or
/// the generic format-driven path. Returns the OpenAI-shape
/// `{audio, format}` body, or `None` if no TTS path is registered for
/// this provider (the caller falls through to its generic forwarder).
pub async fn dispatch(
    client: &reqwest::Client,
    credentials: &crate::types::ProviderConnection,
    provider: &str,
    model: &str,
    body: &serde_json::Value,
) -> Option<Result<serde_json::Value, super::MediaError>> {
    if !is_tts_provider(provider) {
        return None;
    }
    let text = body.get("input").and_then(|v| v.as_str()).unwrap_or("");
    if text.trim().is_empty() {
        return Some(Err(super::MediaError::Validation(
            "Missing required field: input".into(),
        )));
    }
    if let Some(adapter) = get_tts_adapter(provider) {
        let request = TtsRequest {
            text,
            model,
            credentials,
            language: body.get("language").and_then(|v| v.as_str()),
        };
        return Some(
            adapter
                .synthesize(client, &request)
                .await
                .map(|r| serde_json::json!({"audio": r.base64, "format": r.format}))
                .map_err(Into::into),
        );
    }
    let format = provider_generic_format(provider)?;
    let base_url = generic_base_url(credentials, provider);
    let api_key = credentials
        .api_key
        .as_deref()
        .or(credentials.access_token.as_deref())
        .filter(|s| !s.is_empty());
    let (model_id, voice_id) = split_model_voice(model);
    let request = GenericTtsRequest {
        format,
        base_url: &base_url,
        api_key,
        text,
        model_id,
        voice_id,
    };
    Some(
        synthesize_via_format(client, request)
            .await
            .map(|r| serde_json::json!({"audio": r.base64, "format": r.format}))
            .map_err(Into::into),
    )
}

/// Look up the TTS adapter for a provider id.
pub fn get_tts_adapter(provider: &str) -> Option<&'static dyn TtsAdapter> {
    match provider {
        "openai" => Some(&openai::ADAPTER),
        "openrouter" => Some(&openrouter::ADAPTER),
        "gemini" => Some(&gemini::ADAPTER),
        "elevenlabs" => Some(&elevenlabs::ADAPTER),
        "minimax" => Some(&minimax::ADAPTER),
        "google-tts" => Some(&google_tts::ADAPTER),
        "edge-tts" => Some(&edge_tts::ADAPTER),
        "local-device" => Some(&local_device::ADAPTER),
        _ => None,
    }
}

/// Map a provider id to its generic `ttsConfig.format` for the
/// config-driven path in [`synthesize_via_format`].
pub fn provider_generic_format(provider: &str) -> Option<GenericFormat> {
    Some(match provider {
        "hyperbolic" => GenericFormat::Hyperbolic,
        "deepgram" => GenericFormat::Deepgram,
        "nvidia" => GenericFormat::NvidiaTts,
        "huggingface" => GenericFormat::HuggingfaceTts,
        "inworld" => GenericFormat::Inworld,
        "cartesia" => GenericFormat::Cartesia,
        "playht" => GenericFormat::Playht,
        "coqui" => GenericFormat::Coqui,
        "tortoise" => GenericFormat::Tortoise,
        _ => return None,
    })
}

/// Returns true if `provider` exposes a TTS endpoint via
/// [`get_tts_adapter`] or via [`synthesize_via_format`]'s generic path.
pub fn is_tts_provider(provider: &str) -> bool {
    get_tts_adapter(provider).is_some() || provider_generic_format(provider).is_some()
}

/// Resolve the upstream URL for a generic-format TTS provider, preferring
/// the per-connection override in `provider_specific_data.baseUrl`.
fn generic_base_url(credentials: &crate::types::ProviderConnection, provider: &str) -> String {
    if let Some(url) = credentials
        .provider_specific_data
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return url.to_string();
    }
    default_generic_base_url(provider).to_string()
}

/// Upstream defaults that mirror 9router's `providers.js` ttsConfig.
fn default_generic_base_url(provider: &str) -> &'static str {
    match provider {
        "hyperbolic" => "https://api.hyperbolic.xyz/v1/audio/generation",
        "deepgram" => "https://api.deepgram.com/v1/speak",
        "nvidia" => "https://integrate.api.nvidia.com/v1/audio/synthesis",
        "huggingface" => "https://api-inference.huggingface.co/models",
        "inworld" => "https://api.inworld.ai/tts/v1/voice",
        "cartesia" => "https://api.cartesia.ai/tts/bytes",
        "playht" => "https://api.play.ht/api/v2/tts/stream",
        "coqui" => "http://localhost:5002/api/tts",
        "tortoise" => "http://localhost:8000/tts",
        _ => "",
    }
}

/// Split a `model/voice` string into `(model_id, voice_id)`. Returns
/// `(model, "")` when there's no slash.
fn split_model_voice(model: &str) -> (&str, &str) {
    if let Some(idx) = model.find('/') {
        (&model[..idx], &model[idx + 1..])
    } else {
        (model, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_classifies_known_providers() {
        for p in [
            "openai",
            "openrouter",
            "gemini",
            "elevenlabs",
            "minimax",
            "google-tts",
            "edge-tts",
            "local-device",
            "coqui",
            "inworld",
            "tortoise",
            "cartesia",
            "playht",
            "hyperbolic",
            "deepgram",
            "nvidia",
            "huggingface",
        ] {
            assert!(is_tts_provider(p), "{p} should be a TTS provider");
        }
        assert!(!is_tts_provider("not-a-real-provider"));
    }

    #[test]
    fn generic_format_maps_coqui_inworld_tortoise() {
        assert_eq!(provider_generic_format("coqui"), Some(GenericFormat::Coqui));
        assert_eq!(
            provider_generic_format("inworld"),
            Some(GenericFormat::Inworld)
        );
        assert_eq!(
            provider_generic_format("tortoise"),
            Some(GenericFormat::Tortoise)
        );
        assert_eq!(provider_generic_format("openai"), None);
        assert_eq!(provider_generic_format("not-real"), None);
    }

    #[test]
    fn split_model_voice_works() {
        assert_eq!(split_model_voice("tts-1/alloy"), ("tts-1", "alloy"));
        assert_eq!(split_model_voice("solo"), ("solo", ""));
        assert_eq!(split_model_voice(""), ("", ""));
    }

    #[test]
    fn default_generic_base_urls_are_populated() {
        for p in [
            "coqui",
            "tortoise",
            "inworld",
            "cartesia",
            "playht",
            "hyperbolic",
            "deepgram",
            "nvidia",
            "huggingface",
        ] {
            assert!(
                !default_generic_base_url(p).is_empty(),
                "default URL missing for {p}"
            );
        }
    }
}
