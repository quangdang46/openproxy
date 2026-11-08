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
use crate::core::config::app_constants::ANTIGRAVITY_IDE_USER_AGENT;
use crate::core::executor::antigravity::ANTIGRAVITY_BASE_URL;

pub struct AntigravityAdapter;
pub static ADAPTER: AntigravityAdapter = AntigravityAdapter;

/// Antigravity accepts up to 10 reference images per request (upstream
/// `MAX_REFERENCE_IMAGES`).
const MAX_REFERENCE_IMAGES: usize = 10;

/// JS `resolveImageInput`: accepts a string or an object carrying
/// `image_url.url` / `url` / `data` / `b64_json`. Data-URI or raw base64 →
/// inlineData part. HTTP(S) URLs are not supported by the executor envelope.
fn resolve_image_input(input: &Value) -> Option<Value> {
    let input = if input.is_object() {
        input
            .pointer("/image_url/url")
            .or_else(|| input.get("url"))
            .or_else(|| input.get("data"))
            .or_else(|| input.get("b64_json"))
            .and_then(Value::as_str)?
    } else {
        input.as_str()?
    };
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

/// JS `collectImageInputs`: accepted request field shapes
/// (`reference_images`, `images`, `reference_image`, `image`), arrays
/// flattened, capped at [`MAX_REFERENCE_IMAGES`].
fn collect_image_inputs<'a>(body: &'a Value) -> Vec<&'a Value> {
    ["reference_images", "images", "reference_image", "image"]
        .iter()
        .filter_map(|key| body.get(*key))
        .flat_map(|v| {
            if let Some(arr) = v.as_array() {
                arr.iter().collect::<Vec<_>>()
            } else {
                vec![v]
            }
        })
        .take(MAX_REFERENCE_IMAGES)
        .collect()
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
        // Executor-parity client fingerprint: upstream routes image requests
        // through the antigravity executor, whose UA is
        // `ANTIGRAVITY_IDE_USER_AGENT` (Antigravity Hub 2.24.0).
        headers.insert(
            "User-Agent",
            HeaderValue::from_static(ANTIGRAVITY_IDE_USER_AGENT),
        );
        Ok(headers)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        // Gemini image models have no negative prompt param — fold it into
        // the prompt text (upstream `executeViaExecutor`).
        let mut prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?
            .to_string();
        if let Some(negative) = request
            .body
            .get("negative_prompt")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            prompt.push_str(&format!("\n\nNegative prompt: {negative}"));
        }

        // parts: [inlineData…, text] — reference images go FIRST (JS pushes
        // them before the text part).
        let mut parts: Vec<Value> = Vec::new();
        for image_input in collect_image_inputs(request.body) {
            if let Some(part) = resolve_image_input(image_input) {
                parts.push(part);
            }
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
        let part = resolve_image_input(&json!("data:image/png;base64,AAAA")).unwrap();
        assert_eq!(part["inlineData"]["mimeType"], "image/png");
        assert_eq!(part["inlineData"]["data"], "AAAA");
    }

    #[test]
    fn raw_base64_assumes_png_http_rejected() {
        let long_b64 = "A".repeat(120);
        let part = resolve_image_input(&json!(long_b64)).unwrap();
        assert_eq!(part["inlineData"]["mimeType"], "image/png");
        assert!(resolve_image_input(&json!("https://example.com/x.png")).is_none());
        assert!(resolve_image_input(&json!("short")).is_none());
    }

    #[test]
    fn object_shapes_resolve() {
        for input in [
            json!({"image_url": {"url": "data:image/webp;base64,QUJD"}}),
            json!({"url": "data:image/png;base64,QUJD"}),
            json!({"data": "data:image/png;base64,QUJD"}),
            json!({"b64_json": "data:image/png;base64,QUJD"}),
        ] {
            let part = resolve_image_input(&input).unwrap();
            assert_eq!(part["inlineData"]["data"], "QUJD");
        }
    }

    #[test]
    fn collect_image_inputs_covers_all_field_shapes_and_caps_at_ten() {
        let body = json!({
            "reference_images": ["data:image/png;base64,AA", "data:image/png;base64,BB"],
            "images": "data:image/png;base64,CC",
            "reference_image": {"b64_json": "data:image/png;base64,DD"},
            "image": "data:image/png;base64,EE",
        });
        let inputs = collect_image_inputs(&body);
        assert_eq!(inputs.len(), 5);

        let many = json!({
            "images": (0..15).map(|i| format!("data:image/png;base64,{i}")).collect::<Vec<_>>()
        });
        assert_eq!(collect_image_inputs(&many).len(), MAX_REFERENCE_IMAGES);
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
        let parts = body
            .pointer("/contents/0/parts")
            .unwrap()
            .as_array()
            .unwrap();
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
