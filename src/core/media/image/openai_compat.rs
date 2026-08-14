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
    /// Whitelist of request-body fields to forward (9router `bodyFields`).
    /// Empty slice = no whitelist → forward the full body.
    pub body_fields: &'static [&'static str],
}

pub static OPENAI: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "openai",
    endpoint: "https://api.openai.com/v1/images/generations",
    include_referer: false,
    body_fields: &[],
};

pub static MINIMAX: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "minimax",
    endpoint: "https://api.minimaxi.com/v1/images/generations",
    include_referer: false,
    body_fields: &[],
};

pub static OPENROUTER: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "openrouter",
    endpoint: "https://openrouter.ai/api/v1/images/generations",
    include_referer: true,
    body_fields: &[],
};

pub static RECRAFT: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "recraft",
    endpoint: "https://external.api.recraft.ai/v1/images/generations",
    include_referer: false,
    body_fields: &[],
};

/// xAI accepts only model/prompt/n/response_format — quality/style/size are
/// dropped. 9router parity: `open-sse/providers/registry/xai.js:38`
/// imageConfig.bodyFields = ["model","prompt","n","response_format"].
pub static XAI: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "xai",
    endpoint: "https://api.x.ai/v1/images/generations",
    include_referer: false,
    body_fields: &["model", "prompt", "n", "response_format"],
};

/// Vercel AI Gateway image adapter. 9router parity:
/// `open-sse/providers/registry/vercel-ai-gateway.js:33` imageConfig.baseUrl =
/// `https://ai-gateway.vercel.sh/v1/images/generations`.
pub static VERCEL_AI_GATEWAY: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "vercel-ai-gateway",
    endpoint: "https://ai-gateway.vercel.sh/v1/images/generations",
    include_referer: false,
    body_fields: &[],
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
            // 9router parity (registry/openrouter.js:50-58): media endpoints
            // use the endpoint-proxy.local referer, matching the chat path.
            headers.insert(
                "HTTP-Referer",
                HeaderValue::from_static("https://endpoint-proxy.local"),
            );
            headers.insert("X-Title", HeaderValue::from_static("Endpoint Proxy"));
        }
        Ok(headers)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        let n = request.n();
        let size = request.size().unwrap_or("1024x1024");

        let mut full = json!({
            "model": request.model,
            "prompt": prompt,
            "n": n,
            "size": size,
        });
        for key in ["quality", "style", "response_format"] {
            if let Some(v) = request.body.get(key) {
                if let Some(obj) = full.as_object_mut() {
                    obj.insert(key.to_string(), v.clone());
                }
            }
        }

        // bodyFields whitelist (9router imageProviders/openai.js:23-29):
        // when non-empty, forward only the listed keys (JS checks
        // `full[f] !== undefined`, so e.g. `size` is dropped for xAI).
        if !self.body_fields.is_empty() {
            let mut filtered = serde_json::Map::new();
            for f in self.body_fields {
                if let Some(v) = full.get(*f) {
                    filtered.insert((*f).to_string(), v.clone());
                }
            }
            return Ok(Value::Object(filtered));
        }

        Ok(full)
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        body.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::media::image::base::ImageRequest;
    use crate::types::ProviderConnection;
    use serde_json::json;

    fn xai_request() -> ImageRequest<'static> {
        let body = json!({
            "model": "grok-2-image-1212",
            "prompt": "A cat",
            "n": 1,
            "size": "1024x1024",
            "quality": "high",
            "style": "vivid"
        });
        ImageRequest {
            body: Box::leak(Box::new(body)),
            model: "grok-2-image-1212",
            credentials: Box::leak(Box::new(ProviderConnection::default())),
        }
    }

    #[tokio::test]
    async fn xai_body_drops_disallowed_fields() {
        let req = xai_request();
        let body = XAI.build_body(&req).await.unwrap();
        let obj = body.as_object().unwrap();
        assert!(obj.contains_key("model"));
        assert!(obj.contains_key("prompt"));
        assert!(obj.contains_key("n"));
        // xAI whitelist = [model, prompt, n, response_format] — size/quality/style dropped.
        assert!(!obj.contains_key("size"), "size must be dropped for xai");
        assert!(
            !obj.contains_key("quality"),
            "quality must be dropped for xai"
        );
        assert!(!obj.contains_key("style"), "style must be dropped for xai");
    }

    #[tokio::test]
    async fn xai_forwards_response_format_when_present() {
        let mut req = xai_request();
        let body = json!({
            "model": "grok-2-image-1212",
            "prompt": "A cat",
            "n": 1,
            "size": "1024x1024",
            "response_format": "b64_json"
        });
        req.body = Box::leak(Box::new(body));
        let out = XAI.build_body(&req).await.unwrap();
        assert_eq!(
            out.get("response_format").and_then(|v| v.as_str()),
            Some("b64_json")
        );
    }

    #[tokio::test]
    async fn openai_keeps_full_body() {
        // Empty body_fields → full body (including size/quality/style) forwarded.
        let mut req = xai_request();
        let body = json!({
            "model": "dall-e-3",
            "prompt": "A cat",
            "n": 1,
            "size": "1024x1024",
            "quality": "high"
        });
        req.body = Box::leak(Box::new(body));
        let out = OPENAI.build_body(&req).await.unwrap();
        let obj = out.as_object().unwrap();
        assert!(obj.contains_key("size"));
        assert!(obj.contains_key("quality"));
    }

    #[test]
    fn xai_image_adapter_registered() {
        let adapter = super::super::get_image_adapter("xai").expect("xai adapter");
        // Verify via the static directly (adapter is &dyn, no endpoint accessor).
        assert_eq!(XAI.endpoint, "https://api.x.ai/v1/images/generations");
        assert!(!XAI.include_referer);
        // get_image_adapter returns Some for xai.
        let _ = adapter;
    }

    #[test]
    fn vercel_gateway_image_registered() {
        // get_image_adapter returns Some for vercel-ai-gateway.
        let adapter = super::super::get_image_adapter("vercel-ai-gateway").expect("vercel adapter");
        let _ = adapter;
        assert_eq!(
            VERCEL_AI_GATEWAY.endpoint,
            "https://ai-gateway.vercel.sh/v1/images/generations"
        );
        assert!(!VERCEL_AI_GATEWAY.include_referer);
        assert!(VERCEL_AI_GATEWAY.body_fields.is_empty());
    }

    #[test]
    fn openrouter_image_referer_matches_registry() {
        // 9router registry/openrouter.js:56-59 — endpoint-proxy.local referer.
        let body = json!({"prompt": "cat", "model": "dall-e-3"});
        let creds = ProviderConnection::default();
        let req = ImageRequest {
            body: &body,
            model: "dall-e-3",
            credentials: &creds,
        };
        let headers = OPENROUTER.build_headers(&req, &body).unwrap();
        assert_eq!(
            headers.get("HTTP-Referer").and_then(|v| v.to_str().ok()),
            Some("https://endpoint-proxy.local")
        );
        assert_eq!(
            headers.get("X-Title").and_then(|v| v.to_str().ok()),
            Some("Endpoint Proxy")
        );
    }
}
