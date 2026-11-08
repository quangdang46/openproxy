//! Ollama Local TTS — Phonon family.
//!
//! Port of 9router `open-sse/handlers/ttsProviders/ollamaLocal.js`
//! (v0.5.111+, commit 31a9d262). The local Ollama daemon serves
//! `phonon*` checkpoints through an OpenAI-shaped
//! `POST {host}/v1/audio/speech`, but only accepts
//! `response_format: "wav" | "pcm"` — every other client format is
//! normalised to `wav` upstream (raw-PCM → WAV-header wrapping for
//! browser preview is a client-side concern upstream, in
//! `TtsExampleCard.js`, so the server returns the bytes as-is).
//!
//! Request model grammar (`model` here is the provider-stripped id):
//!   - `phonon:en/Zoey`        → model `phonon:en`, voice `Zoey`
//!   - `phonon`                → model `phonon`, voice `default`
//!   - `Zoey` (no `phonon`…)   → model `phonon:multilingual`, voice `Zoey`
//!
//! Fallbacks (both upstream):
//!   - 404 on a `phonon*` model → discover another installed tag via
//!     `GET {host}/api/tags`, else `phonon`→`phonon:multilingual`,
//!     `phonon:multilingual`→`phonon:latest`; retry once.
//!   - 400 whose body mentions `voice` + `not found` → retry once with
//!     the `default` voice (language-specific checkpoints only ship it).

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::json;

use super::base::{
    response_to_base64, upstream_error, upstream_error_parts, TtsAdapter, TtsError, TtsRequest,
    TtsResult,
};
use crate::core::media::ollama_local::resolve_host;

const DEFAULT_MODEL: &str = "phonon:multilingual";
const DEFAULT_VOICE: &str = "default";

pub struct OllamaLocalAdapter;
pub static ADAPTER: OllamaLocalAdapter = OllamaLocalAdapter;

/// Strip a saved endpoint suffix so `{host}/v1/audio/speech` never
/// doubles up — JS ollamaLocal.js:12-14 removes trailing slashes, then
/// `/v1/audio/speech`, then `/v1`.
fn normalize_host(raw: &str) -> String {
    let mut host = raw.trim_end_matches('/').to_string();
    if let Some(stripped) = host.strip_suffix("/v1/audio/speech") {
        host = stripped.trim_end_matches('/').to_string();
    }
    if let Some(stripped) = host.strip_suffix("/v1") {
        host = stripped.trim_end_matches('/').to_string();
    }
    host
}

/// Split the request model into `(tts_model, voice)` exactly like the JS
/// adapter: `>=2` `/`-separated parts → first is the model, the rest join
/// back into the voice; a single part starting with `phonon` is the
/// model; any other single part is a voice on [`DEFAULT_MODEL`].
/// Empty input yields the defaults.
fn split_model_voice(model: &str) -> (String, String) {
    let parts: Vec<&str> = model.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 {
        return (parts[0].to_string(), parts[1..].join("/"));
    }
    match parts.first() {
        None => (DEFAULT_MODEL.to_string(), DEFAULT_VOICE.to_string()),
        Some(val) if val.starts_with("phonon") => (val.to_string(), DEFAULT_VOICE.to_string()),
        Some(voice) => (DEFAULT_MODEL.to_string(), voice.to_string()),
    }
}

/// Ollama natively accepts only `wav`/`pcm`; everything else (mp3, json,
/// unspecified) is requested as `wav` upstream (ollamaLocal.js:37-39).
fn upstream_format(response_format: Option<&str>) -> &'static str {
    match response_format {
        Some("pcm") => "pcm",
        _ => "wav",
    }
}

/// Fallback tag when the requested phonon checkpoint 404s and `/api/tags`
/// discovery finds nothing — ollamaLocal.js:77.
fn fallback_tag(model: &str) -> Option<&'static str> {
    match model {
        "phonon" => Some("phonon:multilingual"),
        "phonon:multilingual" => Some("phonon:latest"),
        _ => None,
    }
}

/// `GET {host}/api/tags` → first installed model whose name starts with
/// `phonon` and differs from `current` (ollamaLocal.js:66-74). Returns
/// `None` on any transport/parse failure — the caller then tries the
/// static tag fallback.
async fn discover_installed_phonon_tag(
    client: &Client,
    host: &str,
    current: &str,
) -> Option<String> {
    let res = client.get(format!("{host}/api/tags")).send().await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    let data: serde_json::Value = res.json().await.ok()?;
    data.get("models")?
        .as_array()?
        .iter()
        .filter_map(|m| {
            m.get("name")
                .or_else(|| m.get("model"))
                .and_then(|v| v.as_str())
        })
        .find(|name| name.starts_with("phonon") && *name != current)
        .map(str::to_string)
}

fn build_headers(credentials: &crate::types::ProviderConnection) -> Result<HeaderMap, TtsError> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    // Bearer only when an API key is actually configured — a local daemon
    // needs no auth (ollamaLocal.js:51-53).
    if let Some(key) = credentials.api_key.as_deref().filter(|k| !k.is_empty()) {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|e| TtsError::Parse(e.to_string()))?,
        );
    }
    Ok(headers)
}

fn speech_payload<'a>(
    model: &'a str,
    voice: &'a str,
    text: &'a str,
    format: &'static str,
) -> serde_json::Value {
    json!({
        "model": model,
        "input": text,
        "voice": voice,
        "response_format": format,
    })
}

async fn post_speech(
    client: &Client,
    url: &str,
    headers: &HeaderMap,
    payload: &serde_json::Value,
) -> Result<reqwest::Response, TtsError> {
    client
        .post(url)
        .headers(headers.clone())
        .json(payload)
        .send()
        .await
        .map_err(|e| {
            // JS ollamaLocal.js:110-113 turns ECONNREFUSED / "fetch failed"
            // into an actionable "is Ollama running?" message.
            let msg = e.to_string();
            if e.is_connect()
                || msg.contains("ECONNREFUSED")
                || msg.contains("fetch failed")
                || msg.contains("Connection refused")
            {
                TtsError::Network(format!(
                    "Cannot connect to Ollama. Please ensure Ollama is running (e.g. 'ollama serve'). ({msg})"
                ))
            } else {
                TtsError::Network(msg)
            }
        })
}

#[async_trait]
impl TtsAdapter for OllamaLocalAdapter {
    async fn synthesize(
        &self,
        client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError> {
        let host = normalize_host(&resolve_host(request.credentials));
        let url = format!("{host}/v1/audio/speech");
        let headers = build_headers(request.credentials)?;

        let (mut tts_model, voice) = split_model_voice(request.model);

        let format = upstream_format(request.response_format);
        let payload = speech_payload(&tts_model, &voice, request.text, format);

        let mut res = post_speech(client, &url, &headers, &payload).await?;
        let mut status = res.status().as_u16();

        // 404 on a phonon tag → try another installed tag, then the static
        // fallback chain (ollamaLocal.js:63-93).
        if status == 404 && tts_model.starts_with("phonon") {
            let alt = discover_installed_phonon_tag(client, &host, &tts_model)
                .await
                .or_else(|| fallback_tag(&tts_model).map(str::to_string));

            if let Some(alt_model) = alt {
                let retry_payload = speech_payload(&alt_model, &voice, request.text, format);
                let retry = post_speech(client, &url, &headers, &retry_payload).await?;
                if retry.status().is_success() {
                    return response_to_base64(retry, request.response_format.unwrap_or("wav"))
                        .await;
                }
                // A 400 here (e.g. voice mismatch) flows into the voice
                // fallback below with the alt model, like upstream.
                res = retry;
                status = res.status().as_u16();
                tts_model = alt_model;
            }
        }

        // 400 "voice … not found" → retry once with the `default` voice
        // (ollamaLocal.js:95-108). The original response is what gets
        // reported when the retry also fails.
        if status == 400 && voice != DEFAULT_VOICE {
            let err_text = res.text().await.unwrap_or_default();
            if err_text.contains("voice") && err_text.contains("not found") {
                let retry_payload = speech_payload(&tts_model, DEFAULT_VOICE, request.text, format);
                if let Ok(retry) = post_speech(client, &url, &headers, &retry_payload).await {
                    if retry.status().is_success() {
                        return response_to_base64(retry, request.response_format.unwrap_or("wav"))
                            .await;
                    }
                }
            }
            return Err(upstream_error_parts(status, err_text));
        }

        if !res.status().is_success() {
            return Err(upstream_error(res).await);
        }
        response_to_base64(res, request.response_format.unwrap_or("wav")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_voice_split_matches_js() {
        // `phonon:en/Zoey` → tag + voice.
        assert_eq!(
            split_model_voice("phonon:en/Zoey"),
            ("phonon:en".to_string(), "Zoey".to_string())
        );
        // Bare phonon tag → default voice.
        assert_eq!(
            split_model_voice("phonon"),
            ("phonon".to_string(), DEFAULT_VOICE.to_string())
        );
        assert_eq!(
            split_model_voice("phonon:multilingual"),
            ("phonon:multilingual".to_string(), DEFAULT_VOICE.to_string())
        );
        // Voice-only value → default model + that voice.
        assert_eq!(
            split_model_voice("Zoey"),
            (DEFAULT_MODEL.to_string(), "Zoey".to_string())
        );
        // Empty → defaults.
        assert_eq!(
            split_model_voice(""),
            (DEFAULT_MODEL.to_string(), DEFAULT_VOICE.to_string())
        );
        // Voices containing slashes rejoin (JS parts.slice(1).join("/")).
        assert_eq!(
            split_model_voice("phonon/a/b"),
            ("phonon".to_string(), "a/b".to_string())
        );
    }

    #[test]
    fn upstream_format_normalizes_to_wav_or_pcm() {
        assert_eq!(upstream_format(Some("pcm")), "pcm");
        assert_eq!(upstream_format(Some("wav")), "wav");
        assert_eq!(upstream_format(Some("mp3")), "wav");
        assert_eq!(upstream_format(Some("json")), "wav");
        assert_eq!(upstream_format(None), "wav");
    }

    #[test]
    fn host_normalization_strips_endpoint_suffixes() {
        assert_eq!(
            normalize_host("http://localhost:11434"),
            "http://localhost:11434"
        );
        assert_eq!(normalize_host("http://host:11434/"), "http://host:11434");
        assert_eq!(normalize_host("http://host:11434/v1"), "http://host:11434");
        assert_eq!(
            normalize_host("http://host:11434/v1/audio/speech"),
            "http://host:11434"
        );
        assert_eq!(
            normalize_host("http://host:11434/v1/audio/speech/"),
            "http://host:11434"
        );
    }

    #[test]
    fn fallback_tag_chain() {
        assert_eq!(fallback_tag("phonon"), Some("phonon:multilingual"));
        assert_eq!(fallback_tag("phonon:multilingual"), Some("phonon:latest"));
        assert_eq!(fallback_tag("phonon:en"), None);
        assert_eq!(fallback_tag("phonon:40m-en"), None);
    }
}
