//! Gemini TTS — generateContent with AUDIO modality returns PCM L16,
//! wrapped as a WAV.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{upstream_error, TtsAdapter, TtsError, TtsRequest, TtsResult};

pub struct GeminiAdapter;
pub static ADAPTER: GeminiAdapter = GeminiAdapter;

const DEFAULT_MODEL: &str = "gemini-3.1-flash-tts-preview";
const DEFAULT_VOICE: &str = "Kore";
const KNOWN_MODELS: &[&str] = &[
    "gemini-3.1-flash-tts-preview",
    "gemini-2.5-flash-preview-tts",
    "gemini-2.5-pro-preview-tts",
];
const SAMPLE_RATE: u32 = 24_000;
const CHANNELS: u16 = 1;
const BITS_PER_SAMPLE: u16 = 16;

/// The 30 Gemini prebuilt voices, ported verbatim from 9router
/// `open-sse/handlers/ttsProviders/gemini.js` PREBUILT_VOICES (lines 93-124).
/// Each maps to `{voice_id, name, labels:{language, gender}}` per
/// `fetchGeminiVoices` (lines 126-128).
pub fn gemini_voices() -> Vec<Value> {
    const VOICES: &[(&str, &str)] = &[
        ("Zephyr", "Female"),
        ("Puck", "Male"),
        ("Charon", "Male"),
        ("Kore", "Female"),
        ("Fenrir", "Male"),
        ("Leda", "Female"),
        ("Orus", "Male"),
        ("Aoede", "Female"),
        ("Callirrhoe", "Female"),
        ("Autonoe", "Female"),
        ("Enceladus", "Male"),
        ("Iapetus", "Male"),
        ("Umbriel", "Male"),
        ("Algieba", "Male"),
        ("Despina", "Female"),
        ("Erinome", "Female"),
        ("Algenib", "Male"),
        ("Rasalgethi", "Male"),
        ("Laomedeia", "Female"),
        ("Achernar", "Female"),
        ("Alnilam", "Male"),
        ("Schedar", "Male"),
        ("Gacrux", "Female"),
        ("Pulcherrima", "Female"),
        ("Achird", "Male"),
        ("Zubenelgenubi", "Male"),
        ("Vindemiatrix", "Female"),
        ("Sadachbia", "Male"),
        ("Sadaltager", "Male"),
        ("Sulafat", "Female"),
    ];
    VOICES
        .iter()
        .map(|(id, gender)| {
            json!({
                "voice_id": id,
                "name": id,
                "labels": { "language": "en", "gender": gender },
            })
        })
        .collect()
}

fn parse_model_voice(input: &str) -> (String, String) {
    if input.is_empty() {
        return (DEFAULT_MODEL.to_string(), DEFAULT_VOICE.to_string());
    }
    for &id in KNOWN_MODELS {
        if input == id {
            return (id.to_string(), DEFAULT_VOICE.to_string());
        }
        let prefix = format!("{id}/");
        if let Some(rest) = input.strip_prefix(&prefix) {
            return (id.to_string(), rest.to_string());
        }
    }
    (DEFAULT_MODEL.to_string(), input.to_string())
}

fn pcm_to_wav(pcm: &[u8]) -> Vec<u8> {
    let data_size = pcm.len() as u32;
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * BITS_PER_SAMPLE as u32 / 8;
    let block_align = CHANNELS * BITS_PER_SAMPLE / 8;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

fn build_prompt(text: &str, language: Option<&str>) -> String {
    if text.contains(": ") {
        return text.to_string();
    }
    match language {
        Some(lang) => format!("Say in {lang}: {text}"),
        None => format!("Say: {text}"),
    }
}

#[async_trait]
impl TtsAdapter for GeminiAdapter {
    async fn synthesize(
        &self,
        client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError> {
        let api_key = request
            .credentials
            .api_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| TtsError::MissingCredentials("gemini".to_string()))?;

        let (model_id, voice_id) = parse_model_voice(request.model);
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model_id}:generateContent?key={}",
            urlencoding::encode(api_key)
        );

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let body = json!({
            "contents": [{"parts": [{"text": build_prompt(request.text, request.language)}]}],
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "speechConfig": {
                    "voiceConfig": {
                        "prebuiltVoiceConfig": {"voiceName": voice_id}
                    }
                }
            }
        });

        let res = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(upstream_error(res).await);
        }
        let parsed: Value = res
            .json()
            .await
            .map_err(|e| TtsError::Parse(format!("parse gemini: {e}")))?;

        let parts = parsed
            .pointer("/candidates/0/content/parts")
            .and_then(|v| v.as_array())
            .ok_or_else(|| TtsError::Parse("Gemini TTS: no parts".into()))?;
        let b64 = parts
            .iter()
            .find_map(|p| p.pointer("/inlineData/data").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                let reason = parsed
                    .pointer("/candidates/0/finishReason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                TtsError::Parse(format!(
                    "Gemini TTS returned no audio (finishReason: {reason}, voice: {voice_id}, model: {model_id})"
                ))
            })?;

        let pcm = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| TtsError::Parse(format!("decode pcm: {e}")))?;
        let wav = pcm_to_wav(&pcm);
        Ok(TtsResult {
            base64: base64::engine::general_purpose::STANDARD.encode(wav),
            format: "wav".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_tts_default_is_3_1_flash() {
        let (model, voice) = parse_model_voice("");
        assert_eq!(model, "gemini-3.1-flash-tts-preview");
        assert_eq!(voice, DEFAULT_VOICE);

        let (model, voice) = parse_model_voice("gemini-3.1-flash-tts-preview/Kore");
        assert_eq!(model, "gemini-3.1-flash-tts-preview");
        assert_eq!(voice, "Kore");
    }

    #[test]
    fn gemini_tts_known_models_keep_older_ids() {
        // 2.5 models must still resolve after the 3.1 addition.
        for id in ["gemini-2.5-flash-preview-tts", "gemini-2.5-pro-preview-tts"] {
            let (model, voice) = parse_model_voice(&format!("{id}/Kore"));
            assert_eq!(model, id);
            assert_eq!(voice, "Kore");
        }
        // A bare voice maps to the default model (now 3.1).
        let (model, voice) = parse_model_voice("Kore");
        assert_eq!(model, DEFAULT_MODEL);
        assert_eq!(voice, "Kore");
    }

    #[test]
    fn gemini_voices_returns_30_prebuilt() {
        let voices = gemini_voices();
        assert_eq!(voices.len(), 30, "exactly 30 prebuilt voices");

        let first = &voices[0];
        assert_eq!(first["voice_id"], "Zephyr");
        assert_eq!(first["name"], "Zephyr");
        assert_eq!(first["labels"]["language"], "en");
        assert_eq!(first["labels"]["gender"], "Female");

        // Spot-check genders per the JS table.
        let by_id: std::collections::HashMap<&str, &Value> = voices
            .iter()
            .map(|v| (v["voice_id"].as_str().unwrap(), v))
            .collect();
        assert_eq!(by_id["Kore"]["labels"]["gender"], "Female");
        assert_eq!(by_id["Puck"]["labels"]["gender"], "Male");
        assert_eq!(by_id["Sulafat"]["labels"]["gender"], "Female");
        assert_eq!(by_id["Enceladus"]["labels"]["gender"], "Male");
        for v in &voices {
            assert_eq!(v["labels"]["language"], "en");
            let g = v["labels"]["gender"].as_str().unwrap();
            assert!(g == "Female" || g == "Male");
        }
    }
}
