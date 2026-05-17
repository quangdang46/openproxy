//! Black Forest Labs (FLUX) — async submit + polling_url.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{
    empty_normalized, now_secs, sleep, ImageAdapter, ImageRequest, ImageResponse, ParseContext,
    POLL_INTERVAL_MS, POLL_TIMEOUT_MS,
};

const BASE_URL: &str = "https://api.bfl.ai/v1";

pub struct BlackForestLabsAdapter;
pub static ADAPTER: BlackForestLabsAdapter = BlackForestLabsAdapter;

#[async_trait]
impl ImageAdapter for BlackForestLabsAdapter {
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        Ok(format!("{BASE_URL}/{}", request.model))
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
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(
            "x-key",
            HeaderValue::from_str(key).map_err(|e| format!("x-key header: {e}"))?,
        );
        Ok(h)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        let mut req = json!({"prompt": prompt});
        if let Some(size) = request.size() {
            let mut parts = size.split('x');
            if let (Some(w), Some(h)) = (parts.next(), parts.next()) {
                if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                    if let Some(obj) = req.as_object_mut() {
                        obj.insert("width".into(), json!(w));
                        obj.insert("height".into(), json!(h));
                    }
                }
            }
        }
        if let Some(image) = request.image().and_then(|v| v.as_str()) {
            if let Some(obj) = req.as_object_mut() {
                obj.insert("image_prompt".into(), json!(image));
            }
        }
        Ok(req)
    }

    async fn parse_response(
        &self,
        client: &Client,
        response: reqwest::Response,
        ctx: ParseContext<'_>,
    ) -> Result<ImageResponse, String> {
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("parse bfl submit: {e}"))?;
        let polling_url = body
            .get("polling_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "BFL: no polling_url".to_string())?
            .to_string();

        let mut poll_headers = HeaderMap::new();
        if let Some(key) = ctx.headers.get("x-key") {
            poll_headers.insert("x-key", key.clone());
        }
        poll_headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(POLL_TIMEOUT_MS);
        loop {
            if std::time::Instant::now() > deadline {
                return Err("BFL polling timeout".to_string());
            }
            sleep(POLL_INTERVAL_MS).await;
            let r = client
                .get(&polling_url)
                .headers(poll_headers.clone())
                .send()
                .await
                .map_err(|e| format!("bfl poll: {e}"))?;
            if !r.status().is_success() {
                return Err(format!("BFL poll HTTP {}", r.status()));
            }
            let s: Value = r.json().await.map_err(|e| format!("parse bfl poll: {e}"))?;
            match s.get("status").and_then(|v| v.as_str()) {
                Some("Ready") => return Ok(ImageResponse::Json(s)),
                Some("Error") | Some("Failed") => {
                    let msg = s
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("BFL generation failed");
                    return Err(msg.to_string());
                }
                _ => {}
            }
        }
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        if let Some(sample) = body.pointer("/result/sample").and_then(|v| v.as_str()) {
            return json!({"created": now_secs(), "data": [{"url": sample}]});
        }
        empty_normalized()
    }
}
