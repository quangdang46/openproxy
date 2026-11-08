//! Google Gemini image adapter (Nano Banana models).

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use super::base::{empty_normalized, now_secs, ImageAdapter, ImageRequest};

const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiAdapter;
pub static ADAPTER: GeminiAdapter = GeminiAdapter;

/// JS `resolveImageInput`: data-URI or raw base64 → `inlineData` part.
/// HTTP(S) URLs and short/non-base64 strings are rejected.
fn resolve_image_input(input: &str) -> Option<Value> {
    if let Some(rest) = input.strip_prefix("data:") {
        let (mime, data) = rest.split_once(";base64,")?;
        if mime.starts_with("image/") && !data.is_empty() {
            return Some(json!({"inlineData": {"mimeType": mime, "data": data}}));
        }
        return None;
    }
    // Raw base64 heuristic (JS: /^[A-Za-z0-9+/]/ && len > 100 && !http).
    if input.len() > 100
        && !input.starts_with("http")
        && input
            .chars()
            .take_while(|c| !c.is_whitespace())
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
    {
        return Some(json!({"inlineData": {"mimeType": "image/png", "data": input}}));
    }
    None
}

/// JS buildBody: `body.image` or `body.images[0]` (string only), unshifted
/// before the text part.
fn reference_image_part(body: &Value) -> Option<Value> {
    let input = body.get("image").and_then(Value::as_str).or_else(|| {
        body.get("images")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(Value::as_str)
    })?;
    resolve_image_input(input)
}

#[async_trait]
impl ImageAdapter for GeminiAdapter {
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        let model = request
            .model
            .strip_prefix("models/")
            .unwrap_or(request.model);
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
        // parts: [inlineData?, text] — the reference image goes FIRST
        // (JS `parts.unshift(inlineData)`).
        let mut parts: Vec<Value> = Vec::new();
        if let Some(part) = reference_image_part(request.body) {
            parts.push(part);
        }
        parts.push(json!({"text": prompt}));
        Ok(json!({
            "contents": [{"parts": parts}],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_uri_and_raw_base64_resolve() {
        let part = resolve_image_input("data:image/jpeg;base64,QUJD").unwrap();
        assert_eq!(part["inlineData"]["mimeType"], "image/jpeg");
        assert_eq!(part["inlineData"]["data"], "QUJD");

        let long_b64 = "A".repeat(120);
        let part = resolve_image_input(&long_b64).unwrap();
        assert_eq!(part["inlineData"]["mimeType"], "image/png");

        assert!(resolve_image_input("https://example.com/x.png").is_none());
        assert!(resolve_image_input("data:text/plain;base64,QUJD").is_none());
        assert!(resolve_image_input("short").is_none());
    }

    #[test]
    fn reference_image_reads_image_then_images_first() {
        let body = json!({"image": "data:image/png;base64,AA"});
        assert!(reference_image_part(&body).is_some());
        let body = json!({"images": ["data:image/png;base64,BB", "data:image/png;base64,CC"]});
        let part = reference_image_part(&body).unwrap();
        assert_eq!(part["inlineData"]["data"], "BB");
        // Object entries are not accepted on the gemini adapter (JS parity).
        let body = json!({"image": {"b64_json": "data:image/png;base64,DD"}});
        assert!(reference_image_part(&body).is_none());
    }

    #[tokio::test]
    async fn body_puts_reference_image_before_text() {
        let creds = crate::types::ProviderConnection {
            api_key: Some("k".into()),
            ..Default::default()
        };
        let request = ImageRequest {
            body: &json!({"prompt": "make it pop", "image": "data:image/png;base64,QUJD"}),
            model: "gemini-2.5-flash-image",
            credentials: &creds,
        };
        let body = ADAPTER.build_body(&request).await.unwrap();
        let parts = body
            .pointer("/contents/0/parts")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["text"], "make it pop");
    }
}
