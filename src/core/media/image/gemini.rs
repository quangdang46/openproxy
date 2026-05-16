//! Google Gemini image adapter (Nano Banana models).

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use super::base::{empty_normalized, now_secs, ImageAdapter, ImageRequest};

const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiAdapter;
pub static ADAPTER: GeminiAdapter = GeminiAdapter;

#[async_trait]
impl ImageAdapter for GeminiAdapter {
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        let model = request.model.strip_prefix("models/").unwrap_or(request.model);
        Ok(format!(
            "{BASE_URL}/{model}:generateContent?key={}",
            urlencoding::encode(key)
        ))
    }

    fn build_headers(
        &self,
        _request: &ImageRequest<'_>,
        _body: &Value,
    ) -> Result<HeaderMap, String> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        Ok(json!({
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {"responseModalities": ["TEXT", "IMAGE"]}
        }))
    }

    fn normalize(&self, body: &Value, prompt: &str) -> Value {
        let parts = body
            .pointer("/candidates/0/content/parts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let images: Vec<Value> = parts
            .iter()
            .filter_map(|p| {
                p.pointer("/inlineData/data")
                    .and_then(|v| v.as_str())
                    .map(|s| json!({"b64_json": s}))
            })
            .collect();
        if images.is_empty() {
            return json!({
                "created": now_secs(),
                "data": [{"b64_json": "", "revised_prompt": prompt}],
            });
        }
        let _ = empty_normalized();
        json!({"created": now_secs(), "data": images})
    }
}
