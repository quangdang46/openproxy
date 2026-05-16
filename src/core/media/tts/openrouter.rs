//! OpenRouter TTS via chat completions + audio modality SSE.

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{upstream_error, TtsAdapter, TtsError, TtsRequest, TtsResult};

pub struct OpenrouterAdapter;
pub static ADAPTER: OpenrouterAdapter = OpenrouterAdapter;

#[async_trait]
impl TtsAdapter for OpenrouterAdapter {
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
            .ok_or_else(|| TtsError::MissingCredentials("openrouter".to_string()))?;

        // Parse model: "provider/model/voice" or "voice".
        let mut tts_model = "openai/gpt-4o-mini-tts".to_string();
        let mut voice = "alloy".to_string();
        if request.model.contains('/') {
            let last = request.model.rfind('/').unwrap();
            let head = &request.model[..last];
            let tail = &request.model[last + 1..];
            if head.contains('/') {
                tts_model = head.to_string();
                voice = tail.to_string();
            } else {
                voice = request.model.to_string();
            }
        } else if !request.model.is_empty() {
            voice = request.model.to_string();
        }

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|e| TtsError::Parse(e.to_string()))?,
        );
        headers.insert(
            "HTTP-Referer",
            HeaderValue::from_static("https://openproxy.local"),
        );
        headers.insert("X-Title", HeaderValue::from_static("OpenProxy"));

        let body = json!({
            "model": tts_model,
            "modalities": ["text", "audio"],
            "audio": {"voice": voice, "format": "wav"},
            "stream": true,
            "messages": [{"role": "user", "content": request.text}],
        });

        let res = client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .headers(headers)
            .json(&body)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(upstream_error(res).await);
        }

        let mut stream = res.bytes_stream();
        let mut buffer = String::new();
        let mut chunks: Vec<String> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| TtsError::Network(e.to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = buffer.find('\n') {
                let line = buffer[..idx].to_string();
                buffer.drain(..idx + 1);
                if !line.starts_with("data: ") || line == "data: [DONE]" {
                    continue;
                }
                let payload = &line[6..];
                if let Ok(parsed) = serde_json::from_str::<Value>(payload) {
                    if let Some(audio) = parsed
                        .pointer("/choices/0/delta/audio/data")
                        .and_then(|v| v.as_str())
                    {
                        chunks.push(audio.to_string());
                    }
                }
            }
        }

        if chunks.is_empty() {
            return Err(TtsError::Parse(
                "OpenRouter TTS returned no audio data".into(),
            ));
        }
        Ok(TtsResult {
            base64: chunks.concat(),
            format: "wav".to_string(),
        })
    }
}
