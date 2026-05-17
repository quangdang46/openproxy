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

const DEFAULT_MODEL: &str = "gemini-2.5-flash-preview-tts";
const DEFAULT_VOICE: &str = "Kore";
const KNOWN_MODELS: &[&str] = &["gemini-2.5-flash-preview-tts", "gemini-2.5-pro-preview-tts"];
const SAMPLE_RATE: u32 = 24_000;
const CHANNELS: u16 = 1;
const BITS_PER_SAMPLE: u16 = 16;

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
