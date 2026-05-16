//! HuggingFace Inference API — returns raw image bytes.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{now_secs, ImageAdapter, ImageRequest, ImageResponse, ParseContext};

pub struct HuggingfaceAdapter;
pub static ADAPTER: HuggingfaceAdapter = HuggingfaceAdapter;

const BASE_URL: &str = "https://api-inference.huggingface.co/models";

#[async_trait]
impl ImageAdapter for HuggingfaceAdapter {
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        Ok(format!("{BASE_URL}/{}", request.model))
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
        Ok(headers)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        Ok(json!({"inputs": prompt}))
    }

    async fn parse_response(
        &self,
        _client: &Client,
        response: reqwest::Response,
        _ctx: ParseContext<'_>,
    ) -> Result<ImageResponse, String> {
        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("read hf bytes: {e}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(ImageResponse::Json(json!({
            "created": now_secs(),
            "data": [{"b64_json": b64}],
        })))
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        body.clone()
    }
}
