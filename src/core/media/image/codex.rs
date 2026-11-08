//! Codex (ChatGPT Plus/Pro) image generation via Responses API + SSE.
//!
//! Forwards an `input/instructions/tools` request to
//! `https://chatgpt.com/backend-api/codex/responses`, parses the SSE
//! stream, and returns the final base64-encoded image. Streaming to the
//! caller is supported via [`ImageResponse::Sse`].

use async_trait::async_trait;
use base64::Engine as _;
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use uuid::Uuid;

use super::base::{now_secs, ImageAdapter, ImageRequest, ImageResponse, ParseContext};

const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_USER_AGENT: &str = "codex_cli_rs/0.159.0";
const CODEX_VERSION: &str = "0.159.0";
const CODEX_ORIGINATOR: &str = "codex_cli_rs";
const CODEX_MODEL_SUFFIX: &str = "-image";
const CODEX_REF_DETAIL: &str = "high";
/// Chat model that hosts the image_generation tool call; any chat model on
/// the plan works as host (9router v0.5.111 default).
const CODEX_IMAGES_MAIN_MODEL: &str = "gpt-6-sol";
/// Dedicated gpt-image-* models route through the tool with an explicit
/// `model` + `action` instead of the plain `-image` suffix strip.
const CODEX_TOOL_IMAGE_MODELS: &[&str] = &[
    "gpt-image-1.5",
    "gpt-image-2",
    "gpt-image-2.5",
    "gpt-image-2.5-flare",
    "gpt-image-2.5-sunburst",
];

pub struct CodexAdapter;
pub static ADAPTER: CodexAdapter = CodexAdapter;

fn decode_account_id(id_token: &str) -> Option<String> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let raw = parts[1].replace('-', "+").replace('_', "/");
    let pad = (4 - (raw.len() % 4)) % 4;
    let padded = format!("{raw}{}", "=".repeat(pad));
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(padded)
        .ok()?;
    let payload: Value = serde_json::from_slice(&bytes).ok()?;
    payload
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn strip_image_suffix(model: &str) -> &str {
    model.strip_suffix(CODEX_MODEL_SUFFIX).unwrap_or(model)
}

/// If `model` is a dedicated `gpt-image-*` tool model, return it as `Some`.
/// Those models are hosted by [`CODEX_IMAGES_MAIN_MODEL`] on the Responses
/// API and passed through on the `image_generation` tool; every other model
/// is sent verbatim minus a trailing `-image` suffix.
fn codex_tool_image_model(model: &str) -> Option<&'static str> {
    CODEX_TOOL_IMAGE_MODELS
        .iter()
        .copied()
        .find(|m| *m == model)
}

fn to_data_url(input: &str) -> Option<String> {
    if input.is_empty() {
        return None;
    }
    if input.starts_with("data:image/")
        || input.starts_with("http://")
        || input.starts_with("https://")
    {
        return Some(input.to_string());
    }
    Some(format!("data:image/png;base64,{input}"))
}

fn build_content(prompt: &str, refs: &[String], detail: &str) -> Value {
    let mut content: Vec<Value> = Vec::new();
    for (i, url) in refs.iter().enumerate() {
        content.push(json!({"type": "input_text", "text": format!("<image name=image{}>", i + 1)}));
        content.push(json!({
            "type": "input_image",
            "image_url": url,
            "detail": detail,
        }));
        content.push(json!({"type": "input_text", "text": "</image>"}));
    }
    content.push(json!({"type": "input_text", "text": prompt}));
    Value::Array(content)
}

/// Drain a Codex SSE stream and return the final base64 image (if any).
async fn parse_codex_stream(response: reqwest::Response) -> Result<Option<String>, String> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::<u8>::new();
    let mut image_b64: Option<String> = None;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("codex stream: {e}"))?;
        buffer.extend_from_slice(&chunk);

        while let Some(idx) = find_blank_line(&buffer) {
            // Frames are delimited by \n\n.
            let frame = buffer.drain(..idx + 2).collect::<Vec<u8>>();
            let frame_str = String::from_utf8_lossy(&frame[..idx]);
            let mut event_name: Option<String> = None;
            let mut data = String::new();
            for line in frame_str.lines() {
                if let Some(rest) = line.strip_prefix("event:") {
                    event_name = Some(rest.trim().to_string());
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data.push_str(rest.trim());
                }
            }
            let Some(event) = event_name else {
                continue;
            };
            if event == "response.output_item.done" && !data.is_empty() {
                if let Ok(parsed) = serde_json::from_str::<Value>(&data) {
                    if let Some(item) = parsed.get("item") {
                        if item.get("type").and_then(|v| v.as_str())
                            == Some("image_generation_call")
                        {
                            if let Some(result) = item.get("result").and_then(|v| v.as_str()) {
                                image_b64 = Some(result.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(image_b64)
}

fn find_blank_line(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Build an SSE response that pipes Codex progress events to the caller.
/// Stream parsing is bounded by a 30-second timeout.
/// `streamToClient = true` callers fall back to non-streaming behaviour.
async fn build_sse_response(
    response: reqwest::Response,
) -> Result<axum::response::Response, String> {
    let b64 = tokio::time::timeout(Duration::from_secs(30), parse_codex_stream(response))
        .await
        .map_err(|_| "codex stream parsing timed out after 30s".to_string())??;
    let body = match b64 {
        Some(b) => json!({"created": now_secs(), "data": [{"b64_json": b}]}),
        None => json!({
            "error": {
                "message": "Codex did not return an image. Account may not be entitled (Plus/Pro required)."
            }
        }),
    };

    use axum::response::IntoResponse;
    let mut resp = (axum::http::StatusCode::OK, axum::Json(body)).into_response();
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(resp)
}

#[async_trait]
impl ImageAdapter for CodexAdapter {
    fn build_url(&self, _: &ImageRequest<'_>) -> Result<String, String> {
        Ok(CODEX_RESPONSES_URL.to_string())
    }

    fn build_headers(
        &self,
        request: &ImageRequest<'_>,
        _body: &Value,
    ) -> Result<HeaderMap, String> {
        let access_token = request.credentials.access_token.as_deref().unwrap_or("");
        let account_id = request
            .credentials
            .provider_specific_data
            .get("chatgptAccountId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                request
                    .credentials
                    .id_token
                    .as_deref()
                    .and_then(decode_account_id)
            })
            .unwrap_or_default();

        let mut h = HeaderMap::new();
        h.insert(
            "accept",
            HeaderValue::from_static("text/event-stream, application/json"),
        );
        h.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {access_token}"))
                .map_err(|e| format!("auth header: {e}"))?,
        );
        h.insert(
            "chatgpt-account-id",
            HeaderValue::from_str(&account_id).map_err(|e| format!("account id header: {e}"))?,
        );
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert("originator", HeaderValue::from_static(CODEX_ORIGINATOR));
        h.insert(
            "session_id",
            HeaderValue::from_str(&Uuid::new_v4().to_string())
                .map_err(|e| format!("session_id header: {e}"))?,
        );
        h.insert("user-agent", HeaderValue::from_static(CODEX_USER_AGENT));
        h.insert("version", HeaderValue::from_static(CODEX_VERSION));
        h.insert(
            "x-client-request-id",
            HeaderValue::from_str(&Uuid::new_v4().to_string())
                .map_err(|e| format!("request id header: {e}"))?,
        );
        Ok(h)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;

        let mut refs: Vec<String> = Vec::new();
        if let Some(arr) = request.body.get("images").and_then(|v| v.as_array()) {
            for img in arr.iter().filter_map(|v| v.as_str()) {
                if let Some(u) = to_data_url(img) {
                    refs.push(u);
                }
            }
        }
        if let Some(img) = request.image().and_then(|v| v.as_str()) {
            if let Some(u) = to_data_url(img) {
                refs.push(u);
            }
        }

        let detail = request
            .body
            .get("image_detail")
            .and_then(|v| v.as_str())
            .unwrap_or(CODEX_REF_DETAIL);

        let tool_model = codex_tool_image_model(request.model);
        let responses_model: String = match tool_model {
            Some(_) => CODEX_IMAGES_MAIN_MODEL.to_string(),
            None => strip_image_suffix(request.model).to_string(),
        };

        let mut img_tool = json!({
            "type": "image_generation",
            "output_format": request
                .body
                .get("output_format")
                .and_then(|v| v.as_str())
                .map(str::to_lowercase)
                .unwrap_or_else(|| "png".to_string()),
        });
        if let Some(tool) = tool_model {
            // Dedicated tool model: explicit action + model on the tool call.
            img_tool["action"] = json!(if refs.is_empty() { "generate" } else { "edit" });
            img_tool["model"] = json!(tool);
        }
        for key in ["size", "quality", "background"] {
            if let Some(v) = request.body.get(key).and_then(|v| v.as_str()) {
                if !v.is_empty() {
                    if let Some(obj) = img_tool.as_object_mut() {
                        obj.insert(key.to_string(), json!(v));
                    }
                }
            }
        }

        Ok(json!({
            "model": responses_model,
            "instructions": "",
            "input": [{
                "type": "message",
                "role": "user",
                "content": build_content(prompt, &refs, detail),
            }],
            "tools": [img_tool],
            "tool_choice": if tool_model.is_some() {
                json!({ "type": "image_generation" })
            } else {
                json!("auto")
            },
            "parallel_tool_calls": false,
            "prompt_cache_key": Uuid::new_v4().to_string(),
            "stream": true,
            "store": false,
            "reasoning": if tool_model.is_some() {
                json!({ "effort": "medium", "summary": "auto" })
            } else {
                Value::Null
            },
        }))
    }

    async fn parse_response(
        &self,
        _client: &Client,
        response: reqwest::Response,
        ctx: ParseContext<'_>,
    ) -> Result<ImageResponse, String> {
        if ctx.stream_to_client {
            let resp = build_sse_response(response).await?;
            return Ok(ImageResponse::Sse(resp));
        }
        let b64 = parse_codex_stream(response).await?;
        match b64 {
            Some(b) => Ok(ImageResponse::Json(json!({
                "created": now_secs(),
                "data": [{"b64_json": b}],
            }))),
            None => Err(
                "Codex did not return an image. Account may not be entitled (Plus/Pro required)."
                    .to_string(),
            ),
        }
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        let bytes = b"";
        let _placeholder = base64::engine::general_purpose::STANDARD.encode(bytes);
        body.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parity guard: headers are built from these constants, so the values
    /// must byte-match 9router `imageProviders/codex.js` (lines 7-9).
    #[test]
    fn codex_version_constants_match_js() {
        assert_eq!(CODEX_VERSION, "0.159.0");
        assert_eq!(CODEX_USER_AGENT, "codex_cli_rs/0.159.0");
    }

    #[test]
    fn tool_image_models_host_on_gpt_6_sol() {
        // v0.5.111: dedicated gpt-image-* models are hosted by gpt-6-sol and
        // passed through on the image_generation tool.
        for m in CODEX_TOOL_IMAGE_MODELS {
            assert_eq!(codex_tool_image_model(m), Some(*m));
        }
        assert_eq!(codex_tool_image_model("gpt-6-sol-image"), None);
        assert_eq!(codex_tool_image_model("gpt-5.6-sol-image"), None);
        assert_eq!(strip_image_suffix("gpt-6-sol-image"), "gpt-6-sol");
        assert_eq!(CODEX_IMAGES_MAIN_MODEL, "gpt-6-sol");
    }

    #[tokio::test]
    async fn build_body_uses_tool_model_path() {
        let body = serde_json::json!({"prompt": "a cat"});
        let req = ImageRequest {
            body: &body,
            model: "gpt-image-2",
            credentials: &crate::types::ProviderConnection::default(),
        };
        let out = ADAPTER.build_body(&req).await.unwrap();
        assert_eq!(out["model"], "gpt-6-sol");
        assert_eq!(
            out["tool_choice"],
            serde_json::json!({"type": "image_generation"})
        );
        assert_eq!(out["tools"][0]["model"], "gpt-image-2");
        assert_eq!(out["tools"][0]["action"], "generate");
        assert_eq!(out["reasoning"]["effort"], "medium");
    }

    #[tokio::test]
    async fn build_body_plain_image_suffix() {
        let body = serde_json::json!({"prompt": "a cat"});
        let req = ImageRequest {
            body: &body,
            model: "gpt-6-sol-image",
            credentials: &crate::types::ProviderConnection::default(),
        };
        let out = ADAPTER.build_body(&req).await.unwrap();
        assert_eq!(out["model"], "gpt-6-sol");
        assert_eq!(out["tool_choice"], "auto");
        assert!(out["tools"][0].get("model").is_none());
        assert!(out["reasoning"].is_null());
    }
}
