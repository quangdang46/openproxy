//! OpenAI-compatible image adapter.
//!
//! Used by openai, minimax, openrouter, recraft. Each variant differs
//! only by base URL and a couple of optional headers; the request shape
//! is identical so we share an implementation parametrized over those.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::base::{ImageAdapter, ImageRequest};

pub struct OpenAiCompatAdapter {
    pub provider_id: &'static str,
    pub endpoint: &'static str,
    pub include_referer: bool,
}

pub static OPENAI: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "openai",
    endpoint: "https://api.openai.com/v1/images/generations",
    include_referer: false,
};

pub static MINIMAX: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "minimax",
    endpoint: "https://api.minimaxi.com/v1/images/generations",
    include_referer: false,
};

pub static OPENROUTER: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "openrouter",
    endpoint: "https://openrouter.ai/api/v1/images/generations",
    include_referer: true,
};

pub static RECRAFT: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "recraft",
    endpoint: "https://external.api.recraft.ai/v1/images/generations",
    include_referer: false,
};

#[async_trait]
impl ImageAdapter for OpenAiCompatAdapter {
    fn build_url(&self, _: &ImageRequest<'_>) -> Result<String, String> {
        Ok(self.endpoint.to_string())
    }

    fn build_headers(
        &self,
        request: &ImageRequest<'_>,
        _body: &Value,
    ) -> Result<HeaderMap, String> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        if !key.is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|e| format!("auth header: {e}"))?,
            );
        }
        if self.include_referer {
            headers.insert(
                "HTTP-Referer",
                HeaderValue::from_static("https://openproxy.local"),
            );
            headers.insert("X-Title", HeaderValue::from_static("OpenProxy"));
        }
        Ok(headers)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        let n = request.n();
        let size = request.size().unwrap_or("1024x1024");

        let mut req = json!({
            "model": request.model,
            "prompt": prompt,
            "n": n,
            "size": size,
        });
        for key in ["quality", "style", "response_format"] {
            if let Some(v) = request.body.get(key) {
                if let Some(obj) = req.as_object_mut() {
                    obj.insert(key.to_string(), v.clone());
                }
            }
        }
        Ok(req)
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        body.clone()
    }
}
