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

/// Synthesize speech for `provider` if a matching adapter exists.
/// Returns the OpenAI-shape `{audio, format}` body, or `None` to fall
/// through to a generic flow.
pub async fn dispatch(
    client: &reqwest::Client,
    credentials: &crate::types::ProviderConnection,
    provider: &str,
    model: &str,
    body: &serde_json::Value,
) -> Option<Result<serde_json::Value, super::MediaError>> {
    let adapter = get_tts_adapter(provider)?;
    let text = body.get("input").and_then(|v| v.as_str()).unwrap_or("");
    if text.trim().is_empty() {
        return Some(Err(super::MediaError::Validation(
            "Missing required field: input".into(),
        )));
    }
    let request = TtsRequest {
        text,
        model,
        credentials,
        language: body.get("language").and_then(|v| v.as_str()),
    };
    Some(
        adapter
            .synthesize(client, &request)
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

/// Returns true if `provider` exposes a TTS endpoint via
/// [`get_tts_adapter`] or via [`synthesize_via_format`]'s generic path.
pub fn is_tts_provider(provider: &str) -> bool {
    get_tts_adapter(provider).is_some()
}
