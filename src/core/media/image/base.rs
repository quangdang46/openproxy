//! Common building blocks for image-provider adapters.

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

use crate::types::ProviderConnection;

/// Default polling interval for async providers (Fal, BFL, NanoBanana, …).
pub const POLL_INTERVAL_MS: u64 = 1500;
/// Wall-clock cap on how long an async provider may take to finish.
pub const POLL_TIMEOUT_MS: u64 = 120_000;

/// Sleep for `ms` milliseconds. Wrapper kept here so adapters don't have
/// to import tokio directly.
pub async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// Map an OpenAI `size` string (e.g. `"1024x1792"`) to the closest
/// upstream aspect ratio (e.g. `"9:16"`). Falls back to `"1:1"`.
pub fn size_to_aspect_ratio(size: &str) -> &'static str {
    match size {
        "1024x1024" => "1:1",
        "1024x1792" => "9:16",
        "1792x1024" => "16:9",
        "1024x1536" => "2:3",
        "1536x1024" => "3:2",
        _ => "1:1",
    }
}

/// Fetch a remote URL and return its bytes encoded as a base64 string.
/// Used by adapters whose upstream returns image URLs that the caller
/// expects as inline `b64_json`.
pub async fn url_to_base64(client: &Client, url: &str) -> Result<String, String> {
    let res = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("fetch {url}: HTTP {}", res.status()));
    }
    let bytes = res.bytes().await.map_err(|e| format!("read {url}: {e}"))?;
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Wall-clock now() in seconds since epoch. Convenience for normalisers
/// emitting OpenAI-shaped responses with a `created` field.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Inbound image-generation request as parsed from the HTTP body.
#[derive(Debug, Clone)]
pub struct ImageRequest<'a> {
    /// OpenAI-shaped request body, kept as-is so adapters can pick out
    /// the fields they care about.
    pub body: &'a Value,
    /// Resolved upstream model id.
    pub model: &'a str,
    /// Resolved provider connection (api key, oauth token, etc.).
    pub credentials: &'a ProviderConnection,
}

impl<'a> ImageRequest<'a> {
    pub fn prompt(&self) -> Option<&'a str> {
        self.body.get("prompt").and_then(|v| v.as_str())
    }

    pub fn size(&self) -> Option<&'a str> {
        self.body.get("size").and_then(|v| v.as_str())
    }

    pub fn n(&self) -> u32 {
        self.body
            .get("n")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
            .unwrap_or(1)
    }

    pub fn image(&self) -> Option<&'a Value> {
        self.body.get("image")
    }
}

/// Outcome returned by [`ImageAdapter::parse_response`].
#[derive(Debug)]
pub enum ImageResponse {
    /// JSON response body, OpenAI-shape or provider-shape (caller will
    /// pass through `normalize`).
    Json(Value),
    /// Server-Sent Events response, already framed with the appropriate
    /// `event: …\ndata: …\n\n` envelopes. Used by Codex.
    Sse(axum::response::Response),
}

/// Per-request context exposed to [`ImageAdapter::parse_response`].
pub struct ParseContext<'a> {
    pub headers: &'a HeaderMap,
    pub stream_to_client: bool,
}

/// Trait implemented by every image-provider adapter.
///
/// Methods are intentionally minimal — only the URL/headers/body
/// builders, an optional response parser (for async / streaming
/// providers), and a normaliser to OpenAI shape are required.
#[async_trait]
pub trait ImageAdapter: Send + Sync {
    /// Whether the upstream is a noAuth (local / unauthenticated) target.
    fn no_auth(&self) -> bool {
        false
    }

    /// Build the upstream URL.
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String>;

    /// Build the request headers.
    fn build_headers(&self, request: &ImageRequest<'_>, body: &Value) -> Result<HeaderMap, String>;

    /// Build the request body (upstream-shaped JSON).
    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String>;

    /// Optional: custom response parsing (async polling, SSE streaming, …).
    /// Default implementation reads the response as JSON.
    async fn parse_response(
        &self,
        client: &Client,
        response: reqwest::Response,
        _ctx: ParseContext<'_>,
    ) -> Result<ImageResponse, String> {
        let _ = client;
        let value: Value = response
            .json()
            .await
            .map_err(|e| format!("parse json: {e}"))?;
        Ok(ImageResponse::Json(value))
    }

    /// Convert a parsed response body into an OpenAI-shape
    /// `{ created, data: [{url|b64_json}] }` envelope. If the input is
    /// already OpenAI-shaped, return it unchanged.
    fn normalize(&self, body: &Value, prompt: &str) -> Value {
        let _ = prompt;
        body.clone()
    }
}

/// Convenience helper used by adapters that emit a placeholder
/// "no-image-returned" body.
pub(crate) fn empty_normalized() -> Value {
    json!({
        "created": now_secs(),
        "data": [],
    })
}
