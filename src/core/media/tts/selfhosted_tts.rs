//! Self-hosted TTS adapter.
//!
//! Port of 9router `open-sse/handlers/ttsProviders/selfhostedTts.js`:
//! a local OpenAI `/v1/audio/speech`-shaped endpoint. Base URL comes from
//! `providerSpecificData.baseUrl` (or `http://localhost:8880`). The model
//! field splits on `/`: `model/voice` → model + voice; a bare value is the
//! **model** (NOT the voice — a deliberate divergence from the OpenAI
//! adapter, matching the JS comment).

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::json;

use super::base::{response_to_base64, TtsAdapter, TtsError, TtsRequest, TtsResult};

const DEFAULT_BASE_URL: &str = "http://localhost:8880";
const DEFAULT_MODEL: &str = "kokoro";
const DEFAULT_VOICE: &str = "af_heart";

pub struct SelfhostedTtsAdapter;
pub static ADAPTER: SelfhostedTtsAdapter = SelfhostedTtsAdapter;

/// Normalize the base URL: trim trailing `/`, strip a `/v1/audio/speech`
/// suffix, then a `/v1` suffix (JS selfhostedTts.js:22-25).
fn normalize_base_url(raw: &str) -> String {
    let mut base = raw.trim_end_matches('/').to_string();
    if let Some(stripped) = base.strip_suffix("/v1/audio/speech") {
        base = stripped.trim_end_matches('/').to_string();
    }
    if let Some(stripped) = base.strip_suffix("/v1") {
        base = stripped.trim_end_matches('/').to_string();
    }
    base
}

/// Split a `model/voice` identifier: `>=2` parts → (model, rest joined by
/// `/`); `==1` → (model, DEFAULT_VOICE).
fn split_model_voice(input: &str) -> (String, String) {
    let parts: Vec<&str> = input.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 {
        (parts[0].to_string(), parts[1..].join("/"))
    } else {
        (
            parts.first().unwrap_or(&DEFAULT_MODEL).to_string(),
            DEFAULT_VOICE.to_string(),
        )
    }
}

#[async_trait]
impl TtsAdapter for SelfhostedTtsAdapter {
    fn no_auth(&self) -> bool {
        false
    }

    async fn synthesize(
        &self,
        client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError> {
        // Base URL: providerSpecificData.baseUrl, else default.
        let base = request
            .credentials
            .provider_specific_data
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_BASE_URL);
        let base = normalize_base_url(base);
        let url = format!("{base}/v1/audio/speech");

        let (model, voice) = split_model_voice(request.model);
        let body = json!({
            "model": model,
            "voice": voice,
            "input": request.text,
            "response_format": "mp3",
        });

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(key) = request
            .credentials
            .api_key
            .as_deref()
            .filter(|k| !k.is_empty())
        {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|e| TtsError::Parse(e.to_string()))?,
            );
        }

        let resp = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| TtsError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_default();
            return Err(TtsError::Upstream { status, message });
        }
        response_to_base64(resp, "mp3").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_normalization() {
        assert_eq!(normalize_base_url("http://host:8880/"), "http://host:8880");
        assert_eq!(
            normalize_base_url("http://host:8880/v1/audio/speech"),
            "http://host:8880"
        );
        assert_eq!(
            normalize_base_url("http://host:8880/v1"),
            "http://host:8880"
        );
        assert_eq!(
            normalize_base_url("http://host:8880/v1/audio/speech/"),
            "http://host:8880"
        );
    }

    #[test]
    fn model_voice_split() {
        // Bare value is the MODEL (not voice) — selfhosted divergence.
        assert_eq!(
            split_model_voice("kokoro"),
            ("kokoro".to_string(), DEFAULT_VOICE.to_string())
        );
        assert_eq!(
            split_model_voice("kokoro/af_heart"),
            ("kokoro".to_string(), "af_heart".to_string())
        );
        assert_eq!(
            split_model_voice("kokoro/af_heart/extra"),
            ("kokoro".to_string(), "af_heart/extra".to_string())
        );
    }
}
