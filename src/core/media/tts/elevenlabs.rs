//! ElevenLabs TTS — voice id with optional model_id prefix.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use reqwest::Client;
use serde_json::json;

use super::base::{upstream_error, TtsAdapter, TtsError, TtsRequest, TtsResult};

pub struct ElevenlabsAdapter;
pub static ADAPTER: ElevenlabsAdapter = ElevenlabsAdapter;

#[async_trait]
impl TtsAdapter for ElevenlabsAdapter {
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
            .ok_or_else(|| TtsError::MissingCredentials("elevenlabs".to_string()))?;

        let (model_id, voice_id) = if let Some(idx) = request.model.find('/') {
            (
                request.model[..idx].to_string(),
                request.model[idx + 1..].to_string(),
            )
        } else {
            ("eleven_flash_v2_5".to_string(), request.model.to_string())
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "xi-api-key",
            HeaderValue::from_str(api_key).map_err(|e| TtsError::Parse(e.to_string()))?,
        );

        let body = json!({
            "text": request.text,
            "model_id": model_id,
            "voice_settings": {"stability": 0.5, "similarity_boost": 0.75},
        });

        let res = client
            .post(format!(
                "https://api.elevenlabs.io/v1/text-to-speech/{voice_id}"
            ))
            .headers(headers)
            .json(&body)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(upstream_error(res).await);
        }
        let bytes = res.bytes().await?;
        if bytes.len() < 1024 {
            return Err(TtsError::Parse("ElevenLabs returned empty audio".into()));
        }
        Ok(TtsResult {
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            format: "mp3".to_string(),
        })
    }
}
