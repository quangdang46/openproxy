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
/// Native Images API (9router `PROVIDER_MEDIA.codex.imageConfig.baseUrl`,
/// registry/codex.js:94). Used for dedicated `gpt-image-*` tool models when
/// the request carries no output controls the native endpoint would ignore.
const CODEX_IMAGES_GENERATIONS_URL: &str =
    "https://chatgpt.com/backend-api/codex/images/generations";
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
    "gpt-image-2.6-flare",
    "gpt-image-2.6-sunburst",
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

/// JS truthiness for the `body.image` check (`body?.image || …`): null,
/// false, "", 0 and empty arrays/objects are falsy upstream.
fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `hasImageRefs` (imageProviders/codex.js): any `image` or non-empty
/// `images[]` input forces the Responses+tool path — the native endpoint
/// has no edit/reference support.
fn has_image_refs(body: &Value) -> bool {
    if body.get("image").is_some_and(js_truthy) {
        return true;
    }
    body.get("images")
        .and_then(Value::as_array)
        .is_some_and(|arr| !arr.is_empty())
}

/// `shouldUseNativeGeneration` (imageProviders/codex.js): dedicated
/// `gpt-image-*` tool models go to the native Images endpoint only when
/// the request carries no refs and no output controls native would
/// silently drop (n>1, explicit size/format/background/quality/compression).
fn should_use_native_generation(model: &str, body: &Value) -> bool {
    if codex_tool_image_model(model).is_none() || has_image_refs(body) {
        return false;
    }
    if body.get("n").and_then(Value::as_u64) != Some(1)
        && body.get("n").is_some()
    {
        return false;
    }
    if body
        .get("size")
        .and_then(Value::as_str)
        .is_some_and(|s| s != "auto")
    {
        return false;
    }
    if body
        .get("output_format")
        .and_then(Value::as_str)
        .is_some_and(|f| !f.eq_ignore_ascii_case("png"))
    {
        return false;
    }
    if body
        .get("background")
        .and_then(Value::as_str)
        .is_some_and(|b| !matches!(b, "auto" | "opaque"))
    {
        return false;
    }
    if body
        .get("quality")
        .and_then(Value::as_str)
        .is_some_and(|q| !matches!(q, "auto" | "low" | "medium" | "high"))
    {
        return false;
    }
    body.get("output_compression").is_none()
}

/// `buildNativeGenerationBody` — the native endpoint ignores n/size and
/// only accepts prompt + the auto-defaulted output knobs.
fn build_native_generation_body(model: &str, body: &Value) -> Value {
    json!({
        "model": model,
        "prompt": body.get("prompt").cloned().unwrap_or(Value::Null),
        "n": 1,
        "size": "auto",
        "quality": body.get("quality").and_then(Value::as_str).unwrap_or("auto"),
        "background": body.get("background").and_then(Value::as_str).unwrap_or("auto"),
        "output_format": "png",
    })
}

/// True when the built upstream body is the native Images shape
/// (`{prompt, …}` with no `input`) — upstream keys the accept header and
/// the JSON parse branch on exactly this (`requestBody?.prompt &&
/// !requestBody?.input`).
fn is_native_body(body: &Value) -> bool {
    body.get("prompt").is_some() && body.get("input").is_none()
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
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        // Upstream `buildUrl`: native Images endpoint for tool models on
        // plain generate requests; Responses SSE for edits and controls.
        if should_use_native_generation(request.model, request.body) {
            return Ok(CODEX_IMAGES_GENERATIONS_URL.to_string());
        }
        Ok(CODEX_RESPONSES_URL.to_string())
    }

    fn build_headers(
        &self,
        request: &ImageRequest<'_>,
        body: &Value,
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
        // Upstream `buildHeaders`: the native Images endpoint answers JSON;
        // the Responses path is SSE (JSON tolerated).
        h.insert(
            "accept",
            if is_native_body(body) {
                HeaderValue::from_static("application/json")
            } else {
                HeaderValue::from_static("text/event-stream, application/json")
            },
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

        // Native Images API path (upstream `buildBody` first branch).
        if should_use_native_generation(request.model, request.body) {
            return Ok(build_native_generation_body(request.model, request.body));
        }

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
            // Upstream sends `reasoning: null` unconditionally — the Codex
            // backend rejects explicit reasoning/complexity levels.
            "reasoning": Value::Null,
        }))
    }

    async fn parse_response(
        &self,
        _client: &Client,
        response: reqwest::Response,
        ctx: ParseContext<'_>,
    ) -> Result<ImageResponse, String> {
        // Native Images API answers plain JSON (upstream parseResponse
        // `requestBody?.prompt && !requestBody?.input` branch); the
        // Responses path is SSE. Content-Type is the reliable discriminator
        // here — the built request body is not visible at this layer.
        let is_native = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"));
        if is_native {
            return parse_native_image_response(response, ctx.stream_to_client).await;
        }
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

/// Native Images API response handling (upstream parseResponse first
/// branch): `{created, data:[{b64_json|url}]}`. `streamToClient` callers
/// get the result wrapped in a single `event: done` SSE frame
/// (`nativeImageSseResponse`); everyone else gets the JSON envelope.
async fn parse_native_image_response(
    response: reqwest::Response,
    stream_to_client: bool,
) -> Result<ImageResponse, String> {
    let payload: Value = response
        .json()
        .await
        .map_err(|_| "Codex Images API returned invalid JSON.".to_string())?;
    let data = payload.get("data").and_then(Value::as_array);
    let has_image = data.is_some_and(|items| {
        items.iter().any(|item| {
            item.get("b64_json").and_then(Value::as_str).is_some()
                || item.get("url").and_then(Value::as_str).is_some()
        })
    });
    if !has_image {
        let error = payload.get("error").cloned().unwrap_or(Value::Null);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Codex Images API returned no image.");
        return Err(message.to_string());
    }
    let result = json!({
        "created": payload.get("created").cloned().unwrap_or_else(|| json!(now_secs())),
        "data": data.cloned().unwrap_or_default(),
    });
    if stream_to_client {
        use axum::response::IntoResponse;
        let frame = format!("event: done\ndata: {result}\n\n");
        let mut resp = (axum::http::StatusCode::OK, frame).into_response();
        let headers = resp.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, no-transform"),
        );
        headers.insert(
            axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        return Ok(ImageResponse::Sse(resp));
    }
    Ok(ImageResponse::Json(result))
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
        // An explicit size keeps the request off the native Images endpoint
        // (native would silently drop it) → Responses + image_generation tool.
        let body = serde_json::json!({"prompt": "a cat", "size": "1024x1024"});
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
        // Upstream sends reasoning:null unconditionally (backend rejects
        // explicit reasoning/complexity levels).
        assert!(out["reasoning"].is_null());
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

    // --- openproxy-4d5h.47: native Images endpoint + 2.6 tool models ---

    #[test]
    fn tool_image_models_include_2_6_variants() {
        for m in ["gpt-image-2.6-flare", "gpt-image-2.6-sunburst"] {
            assert_eq!(codex_tool_image_model(m), Some(m));
        }
    }

    #[test]
    fn native_generation_gate_matches_upstream() {
        let plain = json!({"prompt": "a cat"});
        // Dedicated tool model + no refs + default controls → native.
        assert!(should_use_native_generation("gpt-image-2.5", &plain));
        assert!(should_use_native_generation("gpt-image-2.6-flare", &plain));
        // Non-tool models never take the native path.
        assert!(!should_use_native_generation("gpt-6-sol-image", &plain));
        assert!(!should_use_native_generation("dall-e-3", &plain));
        // Image refs force the Responses+tool path.
        let with_ref = json!({"prompt": "edit", "image": "aGVsbG8="});
        assert!(!should_use_native_generation("gpt-image-2", &with_ref));
        let with_refs = json!({"prompt": "edit", "images": ["aGVsbG8="]});
        assert!(!should_use_native_generation("gpt-image-2", &with_refs));
        // Output controls native would drop force Responses.
        for key in ["size", "output_format", "background", "quality", "output_compression"] {
            let mut b = plain.clone();
            b[key] = json!("explicit");
            assert!(
                !should_use_native_generation("gpt-image-2", &b),
                "{key} must force the Responses path"
            );
        }
        // Permitted auto-values still route native.
        let auto = json!({
            "prompt": "a cat", "n": 1, "size": "auto",
            "output_format": "png", "background": "auto", "quality": "high",
        });
        assert!(should_use_native_generation("gpt-image-2", &auto));
        // n != 1 forces Responses.
        let two = json!({"prompt": "a cat", "n": 2});
        assert!(!should_use_native_generation("gpt-image-2", &two));
    }

    #[tokio::test]
    async fn native_path_builds_generations_url_and_body() {
        let body = json!({"prompt": "a cat"});
        let req = ImageRequest {
            body: &body,
            model: "gpt-image-2.6-sunburst",
            credentials: &crate::types::ProviderConnection::default(),
        };
        assert_eq!(
            ADAPTER.build_url(&req).unwrap(),
            CODEX_IMAGES_GENERATIONS_URL
        );
        let out = ADAPTER.build_body(&req).await.unwrap();
        assert_eq!(out["model"], "gpt-image-2.6-sunburst");
        assert_eq!(out["prompt"], "a cat");
        assert_eq!(out["n"], 1);
        assert_eq!(out["size"], "auto");
        assert_eq!(out["output_format"], "png");
        assert!(out.get("input").is_none());
        // Native body → JSON accept header.
        let headers = ADAPTER.build_headers(&req, &out).unwrap();
        assert_eq!(
            headers.get("accept").and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
    }

    #[tokio::test]
    async fn tool_model_with_refs_stays_on_responses() {
        let body = json!({"prompt": "edit this", "image": "aGVsbG8="});
        let req = ImageRequest {
            body: &body,
            model: "gpt-image-2.6-flare",
            credentials: &crate::types::ProviderConnection::default(),
        };
        assert_eq!(ADAPTER.build_url(&req).unwrap(), CODEX_RESPONSES_URL);
        let out = ADAPTER.build_body(&req).await.unwrap();
        assert_eq!(out["model"], "gpt-6-sol");
        assert_eq!(out["tools"][0]["model"], "gpt-image-2.6-flare");
        assert_eq!(out["tools"][0]["action"], "edit");
    }
}
