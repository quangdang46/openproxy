//! OpenAI TTS — `tts-model/voice` form.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::json;

use super::base::{upstream_error, TtsAdapter, TtsError, TtsRequest, TtsResult};

pub struct OpenaiAdapter;
pub static ADAPTER: OpenaiAdapter = OpenaiAdapter;

#[async_trait]
impl TtsAdapter for OpenaiAdapter {
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
            .ok_or_else(|| TtsError::MissingCredentials("openai".to_string()))?;

        let mut tts_model = "gpt-4o-mini-tts".to_string();
        let mut voice = "alloy".to_string();
        if request.model.contains('/') {
            let parts: Vec<&str> = request.model.splitn(2, '/').collect();
            if parts.len() == 2 {
                tts_model = parts[0].to_string();
                voice = parts[1].to_string();
            }
        } else if !request.model.is_empty() {
            voice = request.model.to_string();
        }

        let base = request
            .credentials
            .provider_specific_data
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.trim_end_matches('/'))
            .unwrap_or("https://api.openai.com");

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|e| TtsError::Parse(e.to_string()))?,
        );

        let body = json!({
            "model": tts_model,
            "voice": voice,
            "input": request.text,
        });

        let res = client
            .post(format!("{base}/v1/audio/speech"))
            .headers(headers)
            .json(&body)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(upstream_error(res).await);
        }
        let bytes = res.bytes().await?;
        Ok(TtsResult {
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            format: "mp3".to_string(),
        })
    }
}
