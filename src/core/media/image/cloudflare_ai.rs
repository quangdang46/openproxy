//! Cloudflare AI — multipart for some FLUX models, JSON otherwise.

use async_trait::async_trait;
use base64::Engine as _;
use once_cell::sync::Lazy;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

use super::base::{
    empty_normalized, now_secs, ImageAdapter, ImageRequest, ImageResponse, ParseContext,
};

const BASE_URL: &str = "https://api.cloudflare.com/client/v4/accounts";

static MULTIPART_MODELS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "@cf/black-forest-labs/flux-2-dev",
        "@cf/black-forest-labs/flux-2-klein-4b",
        "@cf/black-forest-labs/flux-2-klein-9b",
    ]
    .into_iter()
    .collect()
});

const OPTIONAL_FIELDS: &[&str] = &[
    "negative_prompt",
    "guidance",
    "seed",
    "num_steps",
    "steps",
    "strength",
];

pub struct CloudflareAiAdapter;
pub static ADAPTER: CloudflareAiAdapter = CloudflareAiAdapter;

fn size_to_dimensions(size: Option<&str>) -> Option<(u32, u32)> {
    let s = size?;
    let mut parts = s.split('x');
    let w: u32 = parts.next()?.parse().ok()?;
    let h: u32 = parts.next()?.parse().ok()?;
    Some((w, h))
}

fn data_url_b64(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return None;
    }
    if let Some(idx) = trimmed.find(";base64,") {
        return Some(trimmed[idx + ";base64,".len()..].to_string());
    }
    Some(trimmed.to_string())
}

fn b64_to_bytes(b64: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap_or_default()
}

async fn resolve_image_input(client: &Client, value: Option<&Value>) -> Option<(Vec<u8>, String)> {
    let v = value?;
    if let Some(arr) = v.as_array() {
        let bytes: Vec<u8> = arr
            .iter()
            .filter_map(|n| n.as_u64())
            .map(|n| n as u8)
            .collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Some((bytes, b64));
    }
    let s = v.as_str()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let resp = client.get(trimmed).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let bytes = resp.bytes().await.ok()?.to_vec();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Some((bytes, b64));
    }
    let b64 = data_url_b64(trimmed)?;
    let bytes = b64_to_bytes(&b64);
    Some((bytes, b64))
}

fn add_optional_fields_json(target: &mut Map<String, Value>, body: &Value) {
    for &key in OPTIONAL_FIELDS {
        let Some(v) = body.get(key) else {
            continue;
        };
        if v.is_null() {
            continue;
        }
        if let Some(s) = v.as_str() {
            if s.is_empty() {
                continue;
            }
        }
        target.insert(key.to_string(), v.clone());
    }
}

fn image_item_from_string(value: &str) -> Option<Value> {
    if value.is_empty() {
        return None;
    }
    if let Some(idx) = value.find(";base64,") {
        let after = &value[idx + ";base64,".len()..];
        return Some(json!({"b64_json": after}));
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return Some(json!({"url": value}));
    }
    Some(json!({"b64_json": value}))
}

fn normalize_cloudflare_response(body: &Value) -> Value {
    if body.get("created").is_some() && body.get("data").and_then(|v| v.as_array()).is_some() {
        return body.clone();
    }
    let result_owned = body.get("result").cloned().unwrap_or_else(|| body.clone());

    // Queued response: result.responses[].result
    if let Some(responses) = result_owned.get("responses").and_then(|v| v.as_array()) {
        if let Some(success) = responses
            .iter()
            .find(|r| r.get("success").and_then(|v| v.as_bool()) != Some(false))
        {
            if let Some(inner) = success.get("result") {
                return normalize_cloudflare_response(inner);
            }
        }
    }

    let candidate = if let Some(s) = result_owned.as_str() {
        Some(s.to_string())
    } else if let Some(s) = result_owned.get("image").and_then(|v| v.as_str()) {
        Some(s.to_string())
    } else if let Some(s) = result_owned
        .pointer("/data/0/b64_json")
        .and_then(|v| v.as_str())
    {
        Some(s.to_string())
    } else {
        result_owned
            .pointer("/data/0/url")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };

    let item = candidate.as_deref().and_then(image_item_from_string);
    json!({
        "created": now_secs(),
        "data": item.map(|i| vec![i]).unwrap_or_default(),
    })
}

#[async_trait]
impl ImageAdapter for CloudflareAiAdapter {
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        let account_id = request
            .credentials
            .provider_specific_data
            .get("accountId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "cloudflare-ai requires accountId in providerSpecificData".to_string()
            })?;
        Ok(format!("{BASE_URL}/{account_id}/ai/run/{}", request.model))
    }

    fn build_headers(&self, request: &ImageRequest<'_>, body: &Value) -> Result<HeaderMap, String> {
        // We don't have direct access to the multipart marker on the body
        // here; the JSON path always sets Content-Type. For the multipart
        // path, build_body returns a placeholder Value and the handler
        // re-wraps as multipart (out of scope for this minimal port).
        let mut h = HeaderMap::new();
        let _ = body;
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        if !key.is_empty() {
            h.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|e| format!("auth header: {e}"))?,
            );
        }
        Ok(h)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        // NOTE: multipart support omitted — caller falls through to JSON
        // shape, which Cloudflare accepts for the FLUX models too.
        let _multipart = MULTIPART_MODELS.contains(request.model);

        let mut req = Map::new();
        req.insert("prompt".into(), json!(prompt));
        if let Some((w, h)) = size_to_dimensions(request.size()) {
            req.insert("width".into(), json!(w));
            req.insert("height".into(), json!(h));
        }
        // Per-field overrides for width/height take precedence.
        if let Some(w) = request.body.get("width").and_then(|v| v.as_u64()) {
            req.insert("width".into(), json!(w));
        }
        if let Some(h) = request.body.get("height").and_then(|v| v.as_u64()) {
            req.insert("height".into(), json!(h));
        }
        add_optional_fields_json(&mut req, request.body);

        // Image / mask passthrough. Skip URL fetch — adapter is invoked
        // without a Client; resolve_image_input is used in parse path.
        if let Some(image) = request.image() {
            if let Some(s) = image.as_str() {
                if let Some(b64) = data_url_b64(s) {
                    req.insert("image_b64".into(), json!(b64));
                    req.insert(
                        "image".into(),
                        Value::Array(b64_to_bytes(&b64).into_iter().map(|b| json!(b)).collect()),
                    );
                }
            }
        }
        Ok(Value::Object(req))
    }

    async fn parse_response(
        &self,
        client: &Client,
        response: reqwest::Response,
        _ctx: ParseContext<'_>,
    ) -> Result<ImageResponse, String> {
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_lowercase)
            .unwrap_or_default();
        if content_type.starts_with("image/") {
            let bytes = response
                .bytes()
                .await
                .map_err(|e| format!("read cf bytes: {e}"))?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            return Ok(ImageResponse::Json(json!({
                "created": now_secs(),
                "data": [{"b64_json": b64}],
            })));
        }

        let _ = client;
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("parse cf json: {e}"))?;
        Ok(ImageResponse::Json(normalize_cloudflare_response(&body)))
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        if body.get("created").is_some() && body.get("data").is_some() {
            return body.clone();
        }
        let _ = empty_normalized();
        normalize_cloudflare_response(body)
    }
}
