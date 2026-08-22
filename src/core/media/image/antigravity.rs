//! Antigravity image adapter (9router imageProviders/antigravity.js parity).
//!
//! Delegates to the proven antigravity executor envelope shape:
//! - Cloud Code endpoint `{ANTIGRAVITY_BASE_URL}/v1internal:generateContent`
//!   with `Authorization: Bearer <accessToken>` + Client-Metadata headers.
//! - Body is Gemini-shaped `{contents:[{role:"user",parts:[…]}]}`; an input
//!   image (data-URI or raw base64) becomes a leading `inlineData` part so
//!   img2img editing works.
//! - Response normalize maps `candidates[0].content.parts[*].inlineData.data`
//!   → `{created, data:[{b64_json}…]}`.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::base::{empty_normalized, now_secs, ImageAdapter, ImageRequest};
use crate::core::executor::antigravity::ANTIGRAVITY_BASE_URL;

pub struct AntigravityAdapter;
pub static ADAPTER: AntigravityAdapter = AntigravityAdapter;

/// JS resolveImageInput: data-URI or raw base64 → inlineData part.
/// HTTP(S) URLs are not supported by the executor envelope.
fn resolve_image_input(input: &str) -> Option<Value> {
    if let Some(rest) = input.strip_prefix("data:") {
        let (mime, data) = rest.split_once(";base64,")?;
        return Some(json!({"inlineData": {"mimeType": mime, "data": data}}));
    }
    // Raw base64 heuristic (JS: /^[A-Za-z0-9+/]/ && len > 100 && !http).
    if input.len() > 100
        && !input.starts_with("http")
        && input
            .chars()
            .take_while(|c| !c.is_whitespace())
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
        && !input.is_empty()
    {
        return Some(json!({"inlineData": {"mimeType": "image/png", "data": input}}));
    }
    None
}

#[async_trait]
impl ImageAdapter for AntigravityAdapter {
    fn no_auth(&self) -> bool {
        false
    }

    fn build_url(&self, _request: &ImageRequest<'_>) -> Result<String, String> {
        Ok(format!("{ANTIGRAVITY_BASE_URL}/v1internal:generateContent"))
    }

    fn build_headers(
        &self,
        request: &ImageRequest<'_>,
        _body: &Value,
    ) -> Result<HeaderMap, String> {
        let token = request
            .credentials
            .access_token
            .as_deref()
            .or(request.credentials.api_key.as_deref())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Antigravity image requires an access token".to_string())?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| format!("invalid token header: {e}"))?,
        );
        // Executor-parity client metadata (antigravity.js buildHeaders).
        headers.insert(
            "User-Agent",
            HeaderValue::from_static("antigravity/1.0.0 windows/amd64"),
        );
        Ok(headers)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;

        // parts: [inlineData?, text] — the image goes FIRST (JS unshift).
        let mut parts: Vec<Value> = Vec::new();
        let image_input = request
            .body
            .get("image")
            .and_then(Value::as_str)
            .or_else(|| {
                request
                    .body
                    .get("images")
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.first())
                    .and_then(Value::as_str)
            });
        if let Some(input) = image_input.and_then(resolve_image_input) {
            parts.push(input);
        }
        parts.push(json!({"text": prompt}));

        Ok(json!({
            "contents": [{"role": "user", "parts": parts}],
        }))
    }

    fn normalize(&self, body: &Value, prompt: &str) -> Value {
        let candidates = body
            .get("candidates")
            .or_else(|| body.pointer("/response/candidates"))
            .and_then(Value::as_array);
        let parts = candidates
            .and_then(|c| c.first())
            .and_then(|c| c.pointer("/content/parts"))
            .and_then(Value::as_array)
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
            let _ = empty_normalized();
            return json!({
                "created": now_secs(),
                "data": [{"b64_json": "", "revised_prompt": prompt}],
            });
        }
        json!({"created": now_secs(), "data": images})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_uri_becomes_inline_data() {
        let part = resolve_image_input("data:image/png;base64,AAAA").unwrap();
        assert_eq!(part["inlineData"]["mimeType"], "image/png");
        assert_eq!(part["inlineData"]["data"], "AAAA");
    }

    #[test]
    fn raw_base64_assumes_png_http_rejected() {
        let long_b64 = "A".repeat(120);
        let part = resolve_image_input(&long_b64).unwrap();
        assert_eq!(part["inlineData"]["mimeType"], "image/png");
        assert!(resolve_image_input("https://example.com/x.png").is_none());
        assert!(resolve_image_input("short").is_none());
    }

    #[tokio::test]
    async fn body_puts_image_before_text() {
        let creds = crate::types::ProviderConnection {
            access_token: Some("tok".into()),
            ..Default::default()
        };
        let request = ImageRequest {
            body: &json!({"prompt": "edit this", "image": "data:image/jpeg;base64,BBBB"}),
            model: "gemini-3-pro-image",
            credentials: &creds,
        };
        let adapter = &ADAPTER;
        let body = adapter.build_body(&request).await.unwrap();
        let parts = body.pointer("/contents/0/parts").unwrap().as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["inlineData"]["mimeType"], "image/jpeg");
        assert_eq!(parts[1]["text"], "edit this");
    }

    #[tokio::test]
    async fn missing_prompt_rejected() {
        let creds = crate::types::ProviderConnection::default();
        let request = ImageRequest {
            body: &json!({}),
            model: "gemini-3-pro-image",
            credentials: &creds,
        };
        assert!(ADAPTER.build_body(&request).await.is_err());
    }

    #[test]
    fn normalize_maps_candidates_to_b64() {
        let upstream = json!({
            "candidates": [{"content": {"parts": [
                {"text": "here"},
                {"inlineData": {"mimeType": "image/png", "data": "XYZ"}}
            ]}}]
        });
        let out = ADAPTER.normalize(&upstream, "p");
        assert_eq!(out["data"][0]["b64_json"], "XYZ");
        // Nested response.candidates shape too.
        let nested = json!({"response": {"candidates": [{"content": {"parts": [
            {"inlineData": {"data": "Q"}}]}}]}});
        assert_eq!(ADAPTER.normalize(&nested, "p")["data"][0]["b64_json"], "Q");
    }
}
