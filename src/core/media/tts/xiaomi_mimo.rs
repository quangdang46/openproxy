//! Xiaomi MiMo TTS — via OpenAI-compatible chat completions (non-streaming).
//!
//! Message contract (9router `open-sse/handlers/ttsProviders/xiaomi-mimo.js`):
//! target text goes in a `role: assistant` message; style/voice instructions
//! go in a prepended `role: user` message. The voice is selected via the
//! top-level `audio.voice` field — NOT embedded in the model name.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{TtsAdapter, TtsError, TtsRequest, TtsResult};

const DEFAULT_MODEL: &str = "mimo-v2.5-tts";
const DEFAULT_VOICE: &str = "mimo_default";
const KNOWN_MODELS: &[&str] = &["mimo-v2.5-tts"];
const ENDPOINT: &str = "https://api.xiaomimimo.com/v1/chat/completions";

pub struct XiaomiMimoAdapter;
pub static ADAPTER: XiaomiMimoAdapter = XiaomiMimoAdapter;

#[async_trait]
impl TtsAdapter for XiaomiMimoAdapter {
    async fn synthesize(
        &self,
        client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError> {
        let provider = request.credentials.provider.as_str();
        let api_key = request
            .credentials
            .api_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| TtsError::MissingCredentials(provider.to_string()))?;

        let (model_id, voice_id) = super::base::parse_model_voice(
            request.model,
            DEFAULT_MODEL,
            DEFAULT_VOICE,
            KNOWN_MODELS,
        );

        // Language and style are soft instructions → prepend as a role:user
        // message (JS `instructions` = [`Speak in ${language}.`, style]).
        let mut instructions: Vec<String> = Vec::new();
        if let Some(language) = request.language {
            instructions.push(format!("Speak in {language}."));
        }
        if let Some(style) = request.style {
            if !style.trim().is_empty() {
                instructions.push(style.to_string());
            }
        }

        let mut messages = vec![json!({ "role": "assistant", "content": request.text })];
        if !instructions.is_empty() {
            messages.insert(0, json!({ "role": "user", "content": instructions.join(" ") }));
        }

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|e| TtsError::Parse(e.to_string()))?,
        );

        let body = json!({
            "model": model_id,
            "stream": false,
            "messages": messages,
            "audio": {
                "format": "wav",
                "voice": if voice_id.is_empty() { DEFAULT_VOICE } else { voice_id.as_str() },
            },
        });

        let response = client
            .post(ENDPOINT)
            .headers(headers)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let raw_text = response.text().await.unwrap_or_default();
        let data: Value = serde_json::from_str(&raw_text).unwrap_or_else(|_| json!({}));

        if !status.is_success() {
            let msg = data
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or(&raw_text);
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                message: msg.to_string(),
            });
        }

        let audio = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("audio"))
            .and_then(|a| a.get("data"))
            .and_then(|v| v.as_str());
        let audio = audio.ok_or_else(|| {
            let msg = data
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("MiMo TTS returned no audio");
            TtsError::Parse(msg.to_string())
        })?;

        let format = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("audio"))
            .and_then(|a| a.get("format"))
            .and_then(|v| v.as_str())
            .unwrap_or("wav")
            .to_string();

        Ok(TtsResult {
            base64: audio.to_string(),
            format,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderConnection;

    /// Build the request body exactly as the adapter would, to lock the
    /// message/audio contract without a live HTTP call.
    fn build_body(text: &str, language: Option<&str>, style: Option<&str>, model: &str) -> Value {
        let (model_id, voice_id) =
            crate::core::media::tts::base::parse_model_voice(model, DEFAULT_MODEL, DEFAULT_VOICE, KNOWN_MODELS);
        let mut instructions: Vec<String> = Vec::new();
        if let Some(language) = language {
            instructions.push(format!("Speak in {language}."));
        }
        if let Some(style) = style {
            if !style.trim().is_empty() {
                instructions.push(style.to_string());
            }
        }
        let mut messages = vec![json!({ "role": "assistant", "content": text })];
        if !instructions.is_empty() {
            messages.insert(0, json!({ "role": "user", "content": instructions.join(" ") }));
        }
        json!({
            "model": model_id,
            "stream": false,
            "messages": messages,
            "audio": {
                "format": "wav",
                "voice": if voice_id.is_empty() {
                    DEFAULT_VOICE.to_string()
                } else {
                    voice_id
                },
            },
        })
    }

    #[test]
    fn xiaomi_mimo_messages_contract() {
        let body = build_body("hi", Some("en"), Some("whisper softly"), "mimo-v2.5-tts");
        let messages = body["messages"].as_array().unwrap();
        // Instructions (role:user) unshifted BEFORE the assistant message.
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Speak in en. whisper softly");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "hi");
        // Voice is top-level audio.voice, NOT embedded in the model name.
        assert_eq!(body["audio"]["voice"], DEFAULT_VOICE);
        assert_eq!(body["audio"]["format"], "wav");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn xiaomi_mimo_language_only_instruction() {
        let body = build_body("hi", Some("zh"), None, "mimo-v2.5-tts");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Speak in zh.");
    }

    #[test]
    fn xiaomi_mimo_no_instructions_single_message() {
        let body = build_body("hi", None, None, "mimo-v2.5-tts");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "assistant");
    }

    #[test]
    fn xiaomi_mimo_is_tts_provider() {
        assert!(super::super::is_tts_provider("xiaomi-mimo"));
    }
}
