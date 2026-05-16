//! Stability AI v2 (sync, returns `{ image: <b64> }`).

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::base::{empty_normalized, now_secs, size_to_aspect_ratio, ImageAdapter, ImageRequest};

const BASE_URL: &str = "https://api.stability.ai/v2beta/stable-image/generate";

pub struct StabilityAiAdapter;
pub static ADAPTER: StabilityAiAdapter = StabilityAiAdapter;

fn model_to_endpoint(model: &str) -> &'static str {
    if model.contains("ultra") {
        "ultra"
    } else if model.contains("sd3") {
        "sd3"
    } else {
        "core"
    }
}

#[async_trait]
impl ImageAdapter for StabilityAiAdapter {
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        Ok(format!("{BASE_URL}/{}", model_to_endpoint(request.model)))
    }

    fn build_headers(
        &self,
        request: &ImageRequest<'_>,
        _body: &Value,
    ) -> Result<HeaderMap, String> {
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|e| format!("auth header: {e}"))?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        let mut req = json!({
            "prompt": prompt,
            "output_format": request
                .body
                .get("output_format")
                .and_then(|v| v.as_str())
                .map(str::to_lowercase)
                .unwrap_or_else(|| "png".to_string()),
        });
        if let Some(size) = request.size() {
            if let Some(obj) = req.as_object_mut() {
                obj.insert("aspect_ratio".into(), json!(size_to_aspect_ratio(size)));
            }
        }
        if let Some(style) = request.body.get("style").and_then(|v| v.as_str()) {
            if let Some(obj) = req.as_object_mut() {
                obj.insert("style_preset".into(), json!(style));
            }
        }
        if request.model.contains("sd3") {
            if let Some(obj) = req.as_object_mut() {
                obj.insert("model".into(), json!(request.model));
            }
        }
        Ok(req)
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        if let Some(image) = body.get("image").and_then(|v| v.as_str()) {
            return json!({
                "created": now_secs(),
                "data": [{"b64_json": image}],
            });
        }
        empty_normalized()
    }
}
