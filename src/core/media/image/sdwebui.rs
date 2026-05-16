//! Stable Diffusion WebUI (AUTOMATIC1111) — local, no auth.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use super::base::{now_secs, ImageAdapter, ImageRequest};

pub struct SdWebuiAdapter;
pub static ADAPTER: SdWebuiAdapter = SdWebuiAdapter;

#[async_trait]
impl ImageAdapter for SdWebuiAdapter {
    fn no_auth(&self) -> bool {
        true
    }

    fn build_url(&self, _: &ImageRequest<'_>) -> Result<String, String> {
        Ok("http://localhost:7860/sdapi/v1/txt2img".to_string())
    }

    fn build_headers(
        &self,
        _request: &ImageRequest<'_>,
        _body: &Value,
    ) -> Result<HeaderMap, String> {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(h)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        let n = request.n();
        let size = request.size().unwrap_or("1024x1024");
        let mut parts = size.split('x');
        let width: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(512);
        let height: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(512);
        Ok(json!({
            "prompt": prompt,
            "width": if width == 0 { 512 } else { width },
            "height": if height == 0 { 512 } else { height },
            "steps": 20,
            "batch_size": n,
        }))
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        let images = body
            .get("images")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| json!({"b64_json": s}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        json!({"created": now_secs(), "data": images})
    }
}
