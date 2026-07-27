//! ComfyUI adapter — STUB.
//!
//! This adapter is a partial/placeholder implementation. It constructs a basic
//! JSON prompt payload targeting `http://localhost:8188`, but does **not**
//! implement ComfyUI's full graph-based workflow API (only a minimal
//! prompt-echo shim matching the upstream JS fallback).

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use super::base::{ImageAdapter, ImageRequest};

pub struct ComfyuiAdapter;
pub static ADAPTER: ComfyuiAdapter = ComfyuiAdapter;

#[async_trait]
impl ImageAdapter for ComfyuiAdapter {
    fn no_auth(&self) -> bool {
        true
    }
    fn build_url(&self, _: &ImageRequest<'_>) -> Result<String, String> {
        Ok("http://localhost:8188".to_string())
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
        Ok(json!({"prompt": prompt}))
    }
    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        body.clone()
    }
}
