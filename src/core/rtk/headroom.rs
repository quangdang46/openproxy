use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

const DEFAULT_TIMEOUT_MS: u64 = 3000;

/// Byte-size snapshot of a request body for the headroom size log.
/// 9router `captureSizeSnapshot`.
#[derive(Debug, Clone, Default)]
pub struct SizeSnapshot {
    pub body_bytes: usize,
    pub message_bytes: usize,
    pub tool_schema_bytes: usize,
    pub tool_history_bytes: usize,
}

/// Diagnostics for one headroom compression pass (9router `diagnostics`).
#[derive(Debug, Clone, Default)]
pub struct HeadroomDiagnostics {
    pub reason: Option<String>,
    pub endpoint: Option<String>,
    pub before: Option<SizeSnapshot>,
    pub after: Option<SizeSnapshot>,
}

impl HeadroomDiagnostics {
    /// Set `reason` only if not already set (9router `setDiagnostic`).
    pub fn set_reason(&mut self, reason: impl Into<String>) {
        if self.reason.is_none() {
            self.reason = Some(reason.into());
        }
    }
}

/// Rough estimate: chars per token used for phantom savings prediction.
const PHANTOM_CHARS_PER_TOKEN: usize = 4;

/// Rough estimate: expected compression ratio for phantom savings (40% reduction).
const PHANTOM_ESTIMATED_RATIO: f64 = 0.6;

// ---------------------------------------------------------------------------
// Phantom savings estimation
// ---------------------------------------------------------------------------

/// Estimate token savings before actual compression.
///
/// Takes the full request body and estimates how many tokens would be saved by
/// compression, based on the text content character count and a conservative
/// expected compression ratio.
///
/// Returns the estimated number of tokens saved (u32). Returns 0 if the body
/// contains no text content.
pub fn estimate_phantom_savings(body: &Value) -> u32 {
    let char_count: usize = extract_text_from_body(body).chars().count();

    if char_count == 0 {
        return 0;
    }

    let tokens_before = char_count.div_ceil(PHANTOM_CHARS_PER_TOKEN);
    let tokens_before = tokens_before.max(1);
    let tokens_after = (tokens_before as f64 * PHANTOM_ESTIMATED_RATIO).round() as usize;
    let tokens_saved = tokens_before.saturating_sub(tokens_after);
    tokens_saved as u32
}

/// Extract all text content from a request body for token estimation.
///
/// Handles:
///   - "system" field (string or array of content blocks with "text" keys)
///   - "messages" array (string content or content blocks)
///   - "input" array (OpenAI Responses API)
fn extract_text_from_body(body: &Value) -> String {
    let mut text = String::new();
    let obj = match body.as_object() {
        Some(o) => o,
        None => return text,
    };

    // Extract system prompt
    if let Some(system) = obj.get("system") {
        match system {
            Value::String(s) => text.push_str(s),
            Value::Array(arr) => {
                for item in arr {
                    if let Some(t) = item.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                        text.push(' ');
                    }
                }
            }
            _ => {}
        }
        text.push('\n');
    }

    // Extract messages content
    if let Some(messages) = obj.get("messages").and_then(Value::as_array) {
        for msg in messages {
            if let Some(content) = msg.get("content") {
                match content {
                    Value::String(s) => text.push_str(s),
                    Value::Array(blocks) => {
                        for block in blocks {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                text.push_str(t);
                                text.push(' ');
                            }
                        }
                    }
                    _ => {}
                }
            }
            text.push('\n');
        }
    }

    // Extract OpenAI Responses API input
    if let Some(input) = obj.get("input").and_then(Value::as_array) {
        for msg in input {
            if let Some(content) = msg.get("content") {
                match content {
                    Value::String(s) => text.push_str(s),
                    Value::Array(blocks) => {
                        for block in blocks {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                text.push_str(t);
                                text.push(' ');
                            }
                        }
                    }
                    _ => {}
                }
            }
            text.push('\n');
        }
    }

    text
}

// ---------------------------------------------------------------------------
// Lifecycle hooks (trait-based)
// ---------------------------------------------------------------------------

/// Lifecycle hooks for the Headroom compression pipeline.
///
/// Implement this trait to observe or modify the compression flow.
///
/// * `before_compress` — called **before** the compression request is sent to
///   the Headroom proxy. Receives the flattened message array that will be
///   compressed. Return `Some(Value)` to replace the messages, or `None` to
///   keep them as-is. The default implementation is a no-op (returns `None`).
/// * `after_compress` — called **after** compression completes (or fails).
///   Provides the original body size, compressed body size, and the result
///   (`Ok(HeadroomStats)` on success, `Err(String)` on failure).
///   The default implementation is a no-op.
///
/// Both methods run synchronously inside the `compress_with_headroom` call and
/// block further pipeline progress while they execute, so keep them lightweight
/// (e.g., emit a trace, increment a counter, push to a log buffer).
pub trait HeadroomHooks: Send + Sync {
    /// Called before the compression request is sent.
    /// Return `Some(Value::Array(...))` with replacement messages, or `None` to
    /// keep the original messages unchanged (default).
    fn before_compress(&self, _messages: &[Value]) -> Option<Value> {
        None
    }

    /// Called after compression completes (or fails).
    /// `result` is `Ok(HeadroomStats)` on success, or `Err(String)` on failure.
    fn after_compress(
        &self,
        _original_size: usize,
        _compressed_size: usize,
        _result: &Result<HeadroomStats, String>,
    ) {
    }
}

// ---------------------------------------------------------------------------
// Configuration and stats types
// ---------------------------------------------------------------------------

/// Configuration for Headroom token compression.
///
/// Constructed from `Settings` fields by the caller and passed into
/// [`compress_with_headroom`]. All fields are plain data — no interior
/// mutability or shared state.
#[derive(Debug, Clone)]
pub struct HeadroomConfig {
    pub enabled: bool,
    pub url: String,
    pub timeout_ms: u64,
    pub compress_user_messages: bool,
}

impl Default for HeadroomConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            compress_user_messages: false,
        }
    }
}

/// Token-level statistics returned by the Headroom proxy after a successful
/// compression pass. All counters default to zero when the response omits them.
#[derive(Debug, Clone, Default)]
pub struct HeadroomStats {
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub tokens_saved: u64,
}

impl HeadroomStats {
    /// Format a human-readable one-liner suitable for request logs.
    ///
    /// Mirrors `formatHeadroomLog()` from upstream 9router
    /// (`open-sse/rtk/headroom.js`). Returns `None` when the stats are
    /// all-zero (no compression happened).
    pub fn format_headroom_log(&self) -> Option<String> {
        if self.tokens_before == 0 && self.tokens_after == 0 && self.tokens_saved == 0 {
            return None;
        }
        let pct = if self.tokens_before > 0 {
            (self.tokens_saved as f64 / self.tokens_before as f64) * 100.0
        } else {
            0.0
        };
        let after_part = if self.tokens_after > 0 {
            format!(" after={}", self.tokens_after)
        } else {
            String::new()
        };
        let tag = if self.tokens_saved == 0 && self.tokens_before > 0 {
            " [phantom]"
        } else {
            ""
        };
        Some(format!(
            "saved {} tokens / {} ({:.1}%){}{}",
            self.tokens_saved, self.tokens_before, pct, after_part, tag
        ))
    }

    /// Returns `true` if this is a phantom (estimated) stat, not an actual
    /// compression result.
    pub fn is_phantom(&self) -> bool {
        self.tokens_before > 0 && self.tokens_saved == 0
    }
}

// ---------------------------------------------------------------------------
// Main compression entry point
// ---------------------------------------------------------------------------

/// Compress the request body in-place via the Headroom `/v1/compress` proxy.
///
/// Fail-open: returns `None` on any error (network, timeout, bad response,
/// disabled config) so the caller can proceed with the original body.
///
/// # Format detection
///
/// * **Claude** — body has a `"system"` key. Messages are extracted, POSTed in
///   OpenAI shape, and the compressed result is written back to
///   `body["messages"]`.
/// * **OpenAI** — body has `"messages"` or `"input"`. The array is POSTed
///   directly and replaced in-place on success.
///
/// Ports `compressWithHeadroom()` from upstream 9router
/// (`open-sse/rtk/headroom.js`).
///
/// `format` should be `"claude"` when the body is in Anthropic's Messages API
/// shape (has `messages[]` with typed content blocks and a `system` field).
/// For OpenAI or Responses-API shapes, pass `"openai"`.
///
/// `hooks` provides optional lifecycle callbacks (before/after compress) for
/// observability. Pass `None` to skip hooks.
pub async fn compress_with_headroom(
    body: &mut Value,
    config: &HeadroomConfig,
    model: &str,
    format: &str,
    hooks: Option<&dyn HeadroomHooks>,
) -> Option<HeadroomStats> {
    compress_with_headroom_diag(body, config, model, format, hooks, None).await
}

/// Like [`compress_with_headroom`] but also fills `diagnostics` when provided.
pub async fn compress_with_headroom_diag(
    body: &mut Value,
    config: &HeadroomConfig,
    model: &str,
    format: &str,
    hooks: Option<&dyn HeadroomHooks>,
    mut diagnostics: Option<&mut HeadroomDiagnostics>,
) -> Option<HeadroomStats> {
    if !config.enabled || config.url.is_empty() {
        if let Some(h) = hooks {
            h.after_compress(0, 0, &Err("compression disabled".to_string()));
        }
        if let Some(d) = diagnostics {
            d.set_reason("headroom disabled");
        }
        return None;
    }

    if let Some(d) = diagnostics.as_deref_mut() {
        d.endpoint = Some(mask_endpoint(&build_compress_endpoint(&config.url)));
        d.before = Some(capture_size_snapshot(body));
    }

    let _fields = body.as_object()?;

    // Tool-output-only projection runs FIRST for the formats that have one
    // (9router headroom.js:417-432): unsafe openai-responses input and the
    // Gemini family. When it applies, ONLY the plain-text tool outputs are
    // compressed — protocol items and structured payloads stay native.
    if let Some(proj) = collect_tool_output_text(body, format) {
        return compress_tool_outputs_only(body, config, model, proj, hooks, diagnostics).await;
    }

    if format.eq_ignore_ascii_case("claude") {
        return compress_claude_body(body, config, model, hooks, diagnostics).await;
    }

    if format.eq_ignore_ascii_case("openai-responses") {
        return compress_responses_body(body, config, model, hooks, diagnostics).await;
    }

    if format.eq_ignore_ascii_case("kiro") {
        return compress_kiro_body(body, config, model, hooks, diagnostics).await;
    }

    if format.eq_ignore_ascii_case("commandcode") {
        return compress_commandcode_body(body, config, model, hooks, diagnostics).await;
    }

    // OpenAI / Responses-API shape.
    let Some((key, messages)) = extract_openai_messages(body) else {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason(format!("unsupported {} request shape", format));
        }
        return None;
    };

    // Notify hook before compression.
    if let Some(h) = hooks {
        h.before_compress(&messages);
    }

    let original_size = serde_json::to_string(&messages)
        .map(|s| s.len())
        .unwrap_or(0);
    let data = call_compress(config, &messages, model, diagnostics.as_deref_mut()).await?;
    let compressed_size = data
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|arr| serde_json::to_string(arr).ok())
        .map(|s| s.len())
        .unwrap_or(0);
    let stats = parse_stats(&data);
    write_compressed_messages(body, key, &data)?;

    if let Some(d) = diagnostics {
        d.after = Some(capture_size_snapshot(body));
    }
    if let Some(h) = hooks {
        h.after_compress(original_size, compressed_size, &Ok(stats.clone()));
    }
    Some(stats)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// POST messages to the Headroom `/v1/compress` endpoint and return the parsed
/// JSON response on success. Returns `None` on any failure, recording the
/// reason in `diagnostics` when provided (9router `callCompress` →
/// `setDiagnostic`, headroom.js:371-396).
async fn call_compress(
    config: &HeadroomConfig,
    messages: &[Value],
    model: &str,
    mut diagnostics: Option<&mut HeadroomDiagnostics>,
) -> Option<Value> {
    let endpoint = build_compress_endpoint(&config.url);

    let mut payload = build_openai_body(messages, model);
    if config.compress_user_messages {
        payload["config"] = json!({ "compress_user_messages": true });
    }

    let timeout = Duration::from_millis(config.timeout_ms);

    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => {
            if let Some(d) = diagnostics.as_deref_mut() {
                d.set_reason(format!("request failed: {}", describe_fetch_error(&e)));
            }
            return None;
        }
    };

    let response = match client
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            if let Some(d) = diagnostics.as_deref_mut() {
                d.set_reason(format!("request failed: {}", describe_fetch_error(&e)));
            }
            return None;
        }
    };

    if !response.status().is_success() {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason(format!("proxy returned HTTP {}", response.status()));
        }
        return None;
    }

    let data: Value = match response.json().await {
        Ok(v) => v,
        Err(_) => {
            if let Some(d) = diagnostics.as_deref_mut() {
                d.set_reason("proxy response missing messages[]");
            }
            return None;
        }
    };

    // The proxy must return a messages array.
    if data.get("messages").and_then(Value::as_array).is_none() {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason("proxy response missing messages[]");
        }
        return None;
    }

    Some(data)
}

/// Scrub credentials and query strings out of an error message before it is
/// recorded in diagnostics — 9router `scrubSensitiveUrlText` /
/// `describeFetchError` (headroom.js:56-63, 371-379). A proxy URL may carry a
/// tenant token in its query, and reqwest errors can echo the full URL.
fn describe_fetch_error(error: &reqwest::Error) -> String {
    let msg = error.to_string();
    let msg = regex::Regex::new(r"//[^/@\s]+@")
        .map(|re| re.replace_all(&msg, "//").into_owned())
        .unwrap_or(msg);
    regex::Regex::new(r"(https?://[^\s?#]+)[?#][^\s)]*")
        .map(|re| re.replace_all(&msg, "$1").into_owned())
        .unwrap_or(msg)
}

/// Detect which key holds the message array in an OpenAI-shaped body.
///
/// Returns `("messages", ...)` or `("input", ...)` depending on which key
/// contains an array value. Returns `None` when neither is present.
fn extract_openai_messages(body: &Value) -> Option<(&'static str, Vec<Value>)> {
    let fields = body.as_object()?;
    if let Some(arr) = fields.get("messages").and_then(Value::as_array) {
        return Some(("messages", arr.clone()));
    }
    if let Some(arr) = fields.get("input").and_then(Value::as_array) {
        return Some(("input", arr.clone()));
    }
    None
}

/// Build the `{ messages, model }` payload expected by `/v1/compress`.
fn build_openai_body(messages: &[Value], model: &str) -> Value {
    json!({
        "messages": messages,
        "model": model,
    })
}

/// Handle Claude-shaped bodies: flatten content blocks to simple
/// `{role, content}` strings before POSTing (the Headroom proxy expects
/// OpenAI-format text messages), then write compressed messages back.
async fn compress_claude_body(
    body: &mut Value,
    config: &HeadroomConfig,
    model: &str,
    hooks: Option<&dyn HeadroomHooks>,
    mut diagnostics: Option<&mut HeadroomDiagnostics>,
) -> Option<HeadroomStats> {
    let raw_messages = match body.get("messages").and_then(Value::as_array) {
        Some(m) => m.clone(),
        None => {
            if let Some(d) = diagnostics.as_deref_mut() {
                d.set_reason("Claude request has no eligible text content");
            }
            return None;
        }
    };

    // Flatten Claude's typed content blocks to plain text messages
    // so the Headroom proxy (which expects OpenAI format) can process them.
    let flat_messages: Vec<Value> = raw_messages
        .iter()
        .map(|msg| {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = match msg.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            json!({"role": role, "content": content})
        })
        .collect();

    // Notify hook before compression.
    if let Some(h) = hooks {
        h.before_compress(&flat_messages);
    }

    let original_size = serde_json::to_string(&flat_messages)
        .map(|s| s.len())
        .unwrap_or(0);
    let data = call_compress(config, &flat_messages, model, diagnostics.as_deref_mut()).await?;

    // Write compressed messages back into the Claude body.
    if let Some(compressed) = data.get("messages").and_then(Value::as_array) {
        body["messages"] = Value::Array(compressed.clone());
    } else {
        if let Some(h) = hooks {
            h.after_compress(
                original_size,
                0,
                &Err("no messages in response".to_string()),
            );
        }
        return None;
    }

    let compressed_size = data
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|arr| serde_json::to_string(arr).ok())
        .map(|s| s.len())
        .unwrap_or(0);
    let stats = parse_stats(&data);

    if let Some(h) = hooks {
        h.after_compress(original_size, compressed_size, &Ok(stats.clone()));
    }
    Some(stats)
}

/// True when the Responses-API `input` contains items whose `type` is a
/// non-"message" string (tool/reasoning/etc.) — not safe to compress.
/// 9router `hasUnsafeResponsesInputForCompression`.
fn has_unsafe_responses_input(body: &Value) -> bool {
    let Some(input) = body.get("input").and_then(Value::as_array) else {
        return false;
    };
    input
        .iter()
        .any(|item| matches!(item.get("type").and_then(Value::as_str), Some(t) if t != "message"))
}

/// Headroom pass for the OpenAI Responses-API format: translate `input` to
/// OpenAI messages, compress, translate back, write `body.input`.
/// 9router headroom.js `format === "openai-responses"`.
async fn compress_responses_body(
    body: &mut Value,
    config: &HeadroomConfig,
    model: &str,
    hooks: Option<&dyn HeadroomHooks>,
    mut diagnostics: Option<&mut HeadroomDiagnostics>,
) -> Option<HeadroomStats> {
    if has_unsafe_responses_input(body) {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason("skipped: openai-responses tool/reasoning input is not safe to compress");
        }
        return None;
    }

    let mut oai = body.clone();
    let translated =
        crate::core::translator::request::openai_responses::openai_responses_to_chat_request(
            model, &mut oai, false, None,
        );
    if !translated {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason("openai-responses request did not translate to messages[]");
        }
        return None;
    }
    let Some(messages) = oai.get("messages").and_then(Value::as_array).cloned() else {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason("openai-responses request did not translate to messages[]");
        }
        return None;
    };

    if let Some(h) = hooks {
        h.before_compress(&messages);
    }
    let data = call_compress(config, &messages, model, diagnostics.as_deref_mut()).await?;
    if let Some(d) = diagnostics.as_deref_mut() {
        d.set_reason("compressed");
    }
    let stats = parse_stats(&data);
    let compressed_messages = data.get("messages").and_then(Value::as_array).cloned()?;

    // input: undefined so the translator rebuilds input from the compressed
    // messages instead of echoing the original input (9router #1998).
    oai["input"] = Value::Null;
    oai["messages"] = Value::Array(compressed_messages.clone());
    let mut responses_body = oai;
    let translated_back =
        crate::core::translator::request::openai_responses::chat_to_openai_responses_request(
            model,
            &mut responses_body,
            false,
            None,
        );
    if translated_back {
        if let Some(input) = responses_body.get("input") {
            body["input"] = input.clone();
        }
    }

    if let Some(d) = diagnostics {
        d.after = Some(capture_size_snapshot(body));
    }
    if let Some(h) = hooks {
        h.after_compress(0, 0, &Ok(stats.clone()));
    }
    Some(stats)
}

/// Project a Kiro `conversationState` body into OpenAI messages + JSON-pointer
/// targets (paths into the body to write compressed text back).
/// 9router `collectKiroHeadroomMessages`.
fn collect_kiro_headroom_messages(body: &Value) -> Option<(Vec<Value>, Vec<String>)> {
    let state = body.get("conversationState")?;
    if !state.is_object() {
        return None;
    }
    let mut messages: Vec<Value> = Vec::new();
    let mut targets: Vec<String> = Vec::new();

    // `history` is optional and the newest turn also lives in `currentMessage`
    // — a single-turn request carries all of its payload there, and that is
    // exactly where the freshest tool-result bloat sits (9router headroom.js:164-166).
    if let Some(history) = state.get("history").and_then(Value::as_array) {
        for (idx, item) in history.iter().enumerate() {
            collect_kiro_item(
                item,
                &format!("/conversationState/history/{idx}"),
                &mut messages,
                &mut targets,
            );
        }
    }
    if let Some(current) = state.get("currentMessage") {
        collect_kiro_item(
            current,
            "/conversationState/currentMessage",
            &mut messages,
            &mut targets,
        );
    }

    if messages.is_empty() {
        return None;
    }
    Some((messages, targets))
}

/// Project one Kiro turn onto the flat message list. `base_ptr` is the JSON
/// pointer prefix of the turn, so history entries and `currentMessage` share
/// this code (9router headroom.js `visit`).
fn collect_kiro_item(
    item: &Value,
    base_ptr: &str,
    messages: &mut Vec<Value>,
    targets: &mut Vec<String>,
) {
    let user = item.get("userInputMessage");
    if let Some(user) = user {
        if let Some(text) = user.get("systemInstruction").and_then(Value::as_str) {
            messages.push(json!({ "role": "system", "content": text }));
            targets.push(format!("{base_ptr}/userInputMessage/systemInstruction"));
        }
        if let Some(text) = user.get("content").and_then(Value::as_str) {
            messages.push(json!({ "role": "user", "content": text }));
            targets.push(format!("{base_ptr}/userInputMessage/content"));
        }
        if let Some(tool_results) = user
            .get("userInputMessageContext")
            .and_then(|ctx| ctx.get("toolResults"))
            .and_then(Value::as_array)
        {
            for (ri, tool_result) in tool_results.iter().enumerate() {
                let Some(content) = tool_result.get("content").and_then(Value::as_array) else {
                    continue;
                };
                for (pi, part) in content.iter().enumerate() {
                    let Some(text) = part.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    let mut msg = json!({ "role": "tool", "content": text });
                    if let Some(id) = tool_result.get("toolUseId").and_then(Value::as_str) {
                        msg["tool_call_id"] = json!(id);
                    }
                    messages.push(msg);
                    targets.push(format!(
                        "{base_ptr}/userInputMessage/userInputMessageContext/toolResults/{ri}/content/{pi}/text"
                    ));
                }
            }
        }
        return;
    }

    let assistant = item.get("assistantResponseMessage");
    if let Some(assistant) = assistant {
        let mut msg = json!({ "role": "assistant", "content": "" });
        let mut has_tool_calls = false;
        let tool_calls: Vec<Value> = assistant
            .get("toolUses")
            .and_then(Value::as_array)
            .map(|uses| {
                uses.iter()
                    .map(|tu| {
                        let args = tu
                            .get("input")
                            .map(|input| {
                                serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string())
                            })
                            .unwrap_or_else(|| "{}".to_string());
                        json!({
                            "id": tu.get("toolUseId").and_then(Value::as_str).unwrap_or(""),
                            "type": "function",
                            "function": {
                                "name": tu.get("name").and_then(Value::as_str).unwrap_or(""),
                                "arguments": args
                            }
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !tool_calls.is_empty() {
            msg["tool_calls"] = Value::Array(tool_calls);
            has_tool_calls = true;
        }
        if let Some(text) = assistant.get("content").and_then(Value::as_str) {
            msg["content"] = json!(text);
        }
        if !has_tool_calls
            && msg
                .get("content")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return;
        }
        messages.push(msg);
        targets.push(format!("{base_ptr}/assistantResponseMessage/content"));
    }
}

/// Set a value at a JSON-pointer-ish path (`/a/b/0/c`) on a mutable body.
/// Returns `false` if any path segment is missing.
fn set_json_path(body: &mut Value, path: &str, value: Value) -> bool {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let mut cur = body;
    for (i, seg) in segments.iter().enumerate() {
        let is_last = i + 1 == segments.len();
        if let Ok(idx) = seg.parse::<usize>() {
            let arr = match cur.as_array_mut() {
                Some(arr) => arr,
                None => return false,
            };
            if idx >= arr.len() {
                return false;
            }
            if is_last {
                arr[idx] = value;
                return true;
            }
            cur = &mut arr[idx];
            continue;
        }
        if is_last {
            match cur.as_object_mut() {
                Some(obj) => {
                    obj.insert((*seg).to_string(), value);
                    return true;
                }
                None => return false,
            }
        }
        let obj = match cur.as_object_mut() {
            Some(obj) => obj,
            None => return false,
        };
        match obj.get_mut(*seg) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    false
}

/// Verify the compressed messages match the Kiro projection (count, role
/// order, non-null text) and write the compressed text back into the original
/// Kiro fields. 9router `applyKiroHeadroomMessages`.
fn apply_kiro_headroom_messages(
    body: &mut Value,
    messages: &[Value],
    targets: &[String],
    compressed: &[Value],
) -> bool {
    if compressed.len() != messages.len() {
        return false;
    }
    let mut updates: Vec<(String, String)> = Vec::new();
    for (expected, actual) in messages.iter().zip(compressed.iter()) {
        if actual.get("role").and_then(Value::as_str)
            != expected.get("role").and_then(Value::as_str)
        {
            return false;
        }
        let Some(text) = text_from_headroom_message(actual) else {
            return false;
        };
        updates.push((String::new(), text));
    }
    for (i, (target, (_, text))) in targets.iter().zip(updates.iter()).enumerate() {
        let _ = i;
        if !set_json_path(body, target, Value::String(text.clone())) {
            return false;
        }
    }
    true
}

/// Headroom pass for the Kiro format: project `conversationState` to OpenAI
/// messages, compress, verify, write back. 9router `format === "kiro"`.
async fn compress_kiro_body(
    body: &mut Value,
    config: &HeadroomConfig,
    model: &str,
    hooks: Option<&dyn HeadroomHooks>,
    mut diagnostics: Option<&mut HeadroomDiagnostics>,
) -> Option<HeadroomStats> {
    let (messages, targets) = collect_kiro_headroom_messages(body)?;
    if messages.is_empty() {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason("Kiro request did not project to messages[]");
        }
        return None;
    }

    if let Some(h) = hooks {
        h.before_compress(&messages);
    }
    let data = call_compress(config, &messages, model, diagnostics.as_deref_mut()).await?;
    let stats = parse_stats(&data);
    let compressed = data.get("messages").and_then(Value::as_array).cloned()?;

    if !apply_kiro_headroom_messages(body, &messages, &targets, &compressed) {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason("proxy response did not match Kiro message count/order/text");
        }
        return None;
    }

    if let Some(d) = diagnostics {
        d.after = Some(capture_size_snapshot(body));
    }
    if let Some(h) = hooks {
        h.after_compress(0, 0, &Ok(stats.clone()));
    }
    Some(stats)
}

// ---------------------------------------------------------------------------
// Tool-output-only projection (9router collectToolOutputText, headroom.js:109-147)
// ---------------------------------------------------------------------------

/// Bound on native tool-output traversal (9router
/// `HEADROOM_TOOL_OUTPUT_MAX_DEPTH`, rtk/constants.js:4).
const HEADROOM_TOOL_OUTPUT_MAX_DEPTH: usize = 8;

/// One step in a path from the body root to a projected text leaf.
#[derive(Debug, Clone, PartialEq)]
enum TargetSeg {
    Key(String),
    Index(usize),
}

/// A writable text leaf: path into the body plus the original text length
/// (compressed text is only written back when it is shorter).
#[derive(Debug, Clone)]
struct TextTarget {
    path: Vec<TargetSeg>,
    text_len: usize,
}

/// Projection of native tool outputs onto flat OpenAI `role:"tool"` messages
/// for the `/v1/compress` proxy, plus the paths to write compressed text back.
/// 9router `{ messages, targets }` from `collectToolOutputText` /
/// `collectCommandCodeHeadroomMessages`.
#[derive(Debug, Default)]
struct HeadroomProjection {
    messages: Vec<Value>,
    /// `None` for pseudo-messages that carry context only (e.g. CommandCode
    /// assistant tool_calls) and are never written back — upstream pushes
    /// `targets.push(null)` for those.
    targets: Vec<Option<TextTarget>>,
}

impl HeadroomProjection {
    fn push(&mut self, message: Value, target: Option<TextTarget>) {
        self.messages.push(message);
        self.targets.push(target);
    }
}

/// Recursive leaf visitor shared by the tool-output and CommandCode
/// projections. Mirrors the `visit` inside 9router `collectToolOutputText`:
/// strings that are empty or JSON-parseable are skipped (structured payloads
/// stay native); objects with a `type` other than `text`/`input_text` are
/// skipped entirely; only the `result`/`output`/`content`/`text` fields are
/// descended.
fn visit_projection_value(
    value: &Value,
    path: Vec<TargetSeg>,
    depth: usize,
    out: &mut HeadroomProjection,
) {
    if depth > HEADROOM_TOOL_OUTPUT_MAX_DEPTH {
        return;
    }
    match value {
        Value::String(s) => {
            if s.trim().is_empty() {
                return;
            }
            // JSON-parseable strings are structured payloads — keep them
            // native instead of round-tripping (upstream JSON.parse probe).
            if serde_json::from_str::<Value>(s).is_ok() {
                return;
            }
            let id = format!("headroom-tool-{}", out.messages.len());
            out.push(
                json!({"role": "tool", "tool_call_id": id, "content": s}),
                Some(TextTarget {
                    path,
                    text_len: s.chars().count(),
                }),
            );
        }
        Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                let mut p = path.clone();
                p.push(TargetSeg::Index(i));
                visit_projection_value(item, p, depth + 1, out);
            }
        }
        Value::Object(map) => {
            if let Some(t) = map.get("type").and_then(Value::as_str) {
                if t != "text" && t != "input_text" {
                    return;
                }
            }
            for field in ["result", "output", "content", "text"] {
                if let Some(v) = map.get(field) {
                    let mut p = path.clone();
                    p.push(TargetSeg::Key(field.to_string()));
                    visit_projection_value(v, p, depth + 1, out);
                }
            }
        }
        _ => {}
    }
}

/// Project only plain tool-output leaves for compression — never the native
/// blocks themselves (a Claude/OpenAI round-trip would drop documents, signed
/// thinking and tool-result metadata). Returns `None` for formats without a
/// tool-output projection. 9router `collectToolOutputText` (headroom.js:109-147).
fn collect_tool_output_text(body: &Value, format: &str) -> Option<HeadroomProjection> {
    let mut proj = HeadroomProjection::default();
    let f = format.to_ascii_lowercase();

    if f == "openai-responses" {
        if !has_unsafe_responses_input(body) {
            return None;
        }
        let input = body.get("input").and_then(Value::as_array)?;
        for (i, item) in input.iter().enumerate() {
            if item.get("type").and_then(Value::as_str) != Some("function_call_output") {
                continue;
            }
            let Some(output) = item.get("output") else {
                continue;
            };
            visit_projection_value(
                output,
                vec![
                    TargetSeg::Key("input".into()),
                    TargetSeg::Index(i),
                    TargetSeg::Key("output".into()),
                ],
                0,
                &mut proj,
            );
        }
    } else if matches!(
        f.as_str(),
        "gemini" | "gemini-cli" | "antigravity" | "vertex"
    ) {
        // `body.request?.contents || body.contents` — a present-but-null
        // request.contents falls through to body.contents (JS `||`).
        let contents = body
            .get("request")
            .and_then(|r| r.get("contents"))
            .filter(|v| !v.is_null())
            .or_else(|| body.get("contents"))?;
        let contents = contents.as_array()?;
        let (base, contents_ref) = if body
            .get("request")
            .and_then(|r| r.get("contents"))
            .filter(|v| !v.is_null())
            .is_some()
        {
            (vec![TargetSeg::Key("request".into())], contents)
        } else {
            (Vec::new(), contents)
        };
        for (ci, content) in contents_ref.iter().enumerate() {
            let Some(parts) = content.get("parts").and_then(Value::as_array) else {
                continue;
            };
            for (pi, part) in parts.iter().enumerate() {
                let Some(fr) = part.get("functionResponse") else {
                    continue;
                };
                let mut path = base.clone();
                path.push(TargetSeg::Key("contents".into()));
                path.push(TargetSeg::Index(ci));
                path.push(TargetSeg::Key("parts".into()));
                path.push(TargetSeg::Index(pi));
                path.push(TargetSeg::Key("functionResponse".into()));
                if let Some(response) = fr.get("response") {
                    let mut p = path;
                    p.push(TargetSeg::Key("response".into()));
                    visit_projection_value(response, p, 0, &mut proj);
                }
            }
        }
    } else {
        return None;
    }
    Some(proj)
}

/// Write a compressed string back at a target path. Returns false when any
/// segment is missing (fail-open: the whole apply is abandoned upstream).
fn set_target_path(body: &mut Value, path: &[TargetSeg], text: &str) -> bool {
    let mut cur = body;
    for (i, seg) in path.iter().enumerate() {
        let is_last = i + 1 == path.len();
        match seg {
            TargetSeg::Key(key) => {
                let obj = match cur.as_object_mut() {
                    Some(o) => o,
                    None => return false,
                };
                if is_last {
                    obj.insert(key.clone(), Value::String(text.to_string()));
                    return true;
                }
                match obj.get_mut(key) {
                    Some(next) => cur = next,
                    None => return false,
                }
            }
            TargetSeg::Index(idx) => {
                let arr = match cur.as_array_mut() {
                    Some(a) => a,
                    None => return false,
                };
                if *idx >= arr.len() {
                    return false;
                }
                if is_last {
                    arr[*idx] = Value::String(text.to_string());
                    return true;
                }
                cur = &mut arr[*idx];
            }
        }
    }
    false
}

/// Verify the compressed tool-output messages match the projection (count,
/// role, tool_call_id, string content) and write shorter non-empty texts back
/// into the native leaves. 9router `applyToolOutputText` (headroom.js:149-168).
fn apply_tool_output_text(
    body: &mut Value,
    proj: &HeadroomProjection,
    compressed: &[Value],
    diagnostics: Option<&mut HeadroomDiagnostics>,
) -> bool {
    let mut diagnostics = diagnostics;
    if compressed.len() != proj.messages.len() {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason("proxy response did not match tool-output count");
        }
        return false;
    }
    let mut updates: Vec<(Vec<TargetSeg>, String)> = Vec::new();
    for (i, actual) in compressed.iter().enumerate() {
        let expected = &proj.messages[i];
        let ok = actual.get("role").and_then(Value::as_str) == Some("tool")
            && actual.get("tool_call_id") == expected.get("tool_call_id")
            && actual.get("content").and_then(Value::as_str).is_some();
        if !ok {
            if let Some(d) = diagnostics.as_deref_mut() {
                d.set_reason(
                    "proxy response did not preserve tool-output IDs, order, roles or text",
                );
            }
            return false;
        }
        let actual_text = actual.get("content").and_then(Value::as_str).unwrap_or("");
        if let Some(target) = &proj.targets[i] {
            if !actual_text.trim().is_empty() && actual_text.chars().count() < target.text_len {
                updates.push((target.path.clone(), actual_text.to_string()));
            }
        }
    }
    for (path, text) in updates {
        if !set_target_path(body, &path, &text) {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// CommandCode projection (9router collectCommandCodeHeadroomMessages,
// headroom.js:287-324)
// ---------------------------------------------------------------------------

/// Project a CommandCode `params.messages` body onto flat OpenAI messages for
/// the proxy. Native `tool-call`/`tool-result` blocks stay in place; only
/// `text` parts and tool-result `output.value` strings are writable targets.
/// Returns `None` when `params.messages` is not an array.
fn collect_commandcode_headroom_messages(body: &Value) -> Option<HeadroomProjection> {
    let params = body.get("params")?;
    let messages = params.get("messages").and_then(Value::as_array)?;
    let mut proj = HeadroomProjection::default();

    // params.system is a plain string on the CommandCode shape.
    if let Some(system) = params.get("system").and_then(Value::as_str) {
        proj.push(
            json!({"role": "system", "content": system}),
            Some(TextTarget {
                path: vec![
                    TargetSeg::Key("params".into()),
                    TargetSeg::Key("system".into()),
                ],
                text_len: system.chars().count(),
            }),
        );
    }

    for (mi, message) in messages.iter().enumerate() {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        let msg_base = vec![
            TargetSeg::Key("params".into()),
            TargetSeg::Key("messages".into()),
            TargetSeg::Index(mi),
            TargetSeg::Key("content".into()),
        ];
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");

        // Assistant tool-call metadata is context for the proxy only —
        // upstream pushes a null target so it is never written back.
        let tool_calls: Vec<Value> = content
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool-call"))
            .map(|part| {
                let input = part.get("input").cloned().unwrap_or(Value::Null);
                let args = if input.is_null() {
                    "{}".to_string()
                } else {
                    serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string())
                };
                json!({
                    "id": part.get("toolCallId").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": part.get("toolName").cloned().unwrap_or(Value::Null),
                        "arguments": args
                    }
                })
            })
            .collect();
        if role == "assistant" && !tool_calls.is_empty() {
            proj.push(
                json!({"role": "assistant", "content": "", "tool_calls": tool_calls}),
                None,
            );
        }

        for (ci, part) in content.iter().enumerate() {
            let ptype = part.get("type").and_then(Value::as_str).unwrap_or("");
            if ptype == "text" {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    let mut path = msg_base.clone();
                    path.push(TargetSeg::Index(ci));
                    path.push(TargetSeg::Key("text".into()));
                    proj.push(
                        json!({"role": role, "content": text}),
                        Some(TextTarget {
                            path,
                            text_len: text.chars().count(),
                        }),
                    );
                }
            } else if ptype == "tool-result"
                && part
                    .get("output")
                    .and_then(|o| o.get("type"))
                    .and_then(Value::as_str)
                    == Some("text")
            {
                if let Some(value) = part
                    .get("output")
                    .and_then(|o| o.get("value"))
                    .and_then(Value::as_str)
                {
                    let mut path = msg_base.clone();
                    path.push(TargetSeg::Index(ci));
                    path.push(TargetSeg::Key("output".into()));
                    path.push(TargetSeg::Key("value".into()));
                    proj.push(
                        json!({
                            "role": "tool",
                            "content": value,
                            "tool_call_id": part.get("toolCallId").cloned().unwrap_or(Value::Null),
                            "name": part.get("toolName").cloned().unwrap_or(Value::Null),
                        }),
                        Some(TextTarget {
                            path,
                            text_len: value.chars().count(),
                        }),
                    );
                }
            }
        }
    }
    Some(proj)
}

/// Extract the text content from a headroom-compressed message
/// (9router `textFromHeadroomMessage`, headroom.js:326-338): a string
/// `content`, or an array of string / `{text}` parts joined with "\n".
fn text_from_headroom_message(msg: &Value) -> Option<String> {
    let content = msg.get("content")?;
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for part in parts {
                if let Some(s) = part.as_str() {
                    out.push(s.to_string());
                } else if let Some(t) = part.get("text").and_then(Value::as_str) {
                    out.push(t.to_string());
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out.join("\n"))
            }
        }
        _ => None,
    }
}

/// Verify compressed messages against a projection (count, role,
/// tool_call_id, non-null text) and write text back into the projected
/// targets. 9router `applyProjectedHeadroomMessages` (headroom.js:340-368).
fn apply_projected_headroom_messages(
    body: &mut Value,
    proj: &HeadroomProjection,
    compressed: &[Value],
    label: &str,
    diagnostics: Option<&mut HeadroomDiagnostics>,
) -> bool {
    let mut diagnostics = diagnostics;
    if compressed.len() != proj.messages.len() {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason(format!(
                "proxy response did not match {label} message count"
            ));
        }
        return false;
    }
    let mut updates: Vec<(Vec<TargetSeg>, String)> = Vec::new();
    for (i, (expected, actual)) in proj.messages.iter().zip(compressed.iter()).enumerate() {
        let role_ok = actual.get("role").and_then(Value::as_str)
            == expected.get("role").and_then(Value::as_str);
        let tc_ok = actual.get("tool_call_id") == expected.get("tool_call_id");
        if !role_ok || !tc_ok {
            if let Some(d) = diagnostics.as_deref_mut() {
                d.set_reason(format!(
                    "proxy response did not preserve {label} message order"
                ));
            }
            return false;
        }
        let Some(text) = text_from_headroom_message(actual) else {
            if let Some(d) = diagnostics.as_deref_mut() {
                d.set_reason(format!("proxy response missing {label} text content"));
            }
            return false;
        };
        let _ = i;
        if let Some(Some(target)) = proj.targets.get(i) {
            updates.push((target.path.clone(), text));
        }
    }
    for (path, text) in updates {
        if !set_target_path(body, &path, &text) {
            return false;
        }
    }
    true
}

/// Headroom pass for CommandCode bodies: project `params.messages` text
/// leaves, compress, verify, write back. 9router `format === "commandcode"`.
async fn compress_commandcode_body(
    body: &mut Value,
    config: &HeadroomConfig,
    model: &str,
    hooks: Option<&dyn HeadroomHooks>,
    mut diagnostics: Option<&mut HeadroomDiagnostics>,
) -> Option<HeadroomStats> {
    let proj = match collect_commandcode_headroom_messages(body) {
        Some(p) => p,
        None => {
            if let Some(d) = diagnostics.as_deref_mut() {
                d.set_reason("CommandCode request did not project to messages[]");
            }
            return None;
        }
    };

    if let Some(h) = hooks {
        h.before_compress(&proj.messages);
    }
    let data = call_compress(config, &proj.messages, model, diagnostics.as_deref_mut()).await?;
    let stats = parse_stats(&data);
    let compressed = data.get("messages").and_then(Value::as_array).cloned()?;

    if !apply_projected_headroom_messages(
        body,
        &proj,
        &compressed,
        "CommandCode",
        diagnostics.as_deref_mut(),
    ) {
        return None;
    }

    if let Some(d) = diagnostics {
        d.after = Some(capture_size_snapshot(body));
    }
    if let Some(h) = hooks {
        h.after_compress(0, 0, &Ok(stats.clone()));
    }
    Some(stats)
}

/// Compress only the projected plain-text tool outputs and write the shorter
/// results back into the native leaves (Responses `function_call_output`
/// items, Gemini `functionResponse.response`). 9router headroom.js:417-432.
async fn compress_tool_outputs_only(
    body: &mut Value,
    config: &HeadroomConfig,
    model: &str,
    proj: HeadroomProjection,
    hooks: Option<&dyn HeadroomHooks>,
    mut diagnostics: Option<&mut HeadroomDiagnostics>,
) -> Option<HeadroomStats> {
    if proj.messages.is_empty() {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason("no eligible plain-text tool output");
        }
        return None;
    }

    if let Some(h) = hooks {
        h.before_compress(&proj.messages);
    }
    let data = call_compress(config, &proj.messages, model, diagnostics.as_deref_mut()).await?;
    let stats = parse_stats(&data);
    let compressed = data.get("messages").and_then(Value::as_array).cloned()?;

    if !apply_tool_output_text(body, &proj, &compressed, diagnostics.as_deref_mut()) {
        return None;
    }

    if let Some(d) = diagnostics {
        d.after = Some(capture_size_snapshot(body));
    }
    if let Some(h) = hooks {
        h.after_compress(0, 0, &Ok(stats.clone()));
    }
    Some(stats)
}

/// Replace the message array in the body under the given key.
fn write_compressed_messages(body: &mut Value, key: &str, data: &Value) -> Option<()> {
    let compressed = data.get("messages").and_then(Value::as_array)?;
    body[key] = Value::Array(compressed.clone());
    Some(())
}

/// The message array a headroom pass should measure, across every supported
/// body shape (9router `messagePayload`, headroom.js:23-32):
/// `messages`, `input`, `params.messages` (CommandCode), `request.contents`
/// (Antigravity envelope), `contents` (Gemini/Vertex), then the Kiro
/// `conversationState` projection.
fn message_payload(body: &Value) -> Option<Vec<Value>> {
    let arr = |v: Option<&Value>| v.and_then(Value::as_array).cloned();
    if let Some(m) = arr(body.get("messages")) {
        return Some(m);
    }
    if let Some(m) = arr(body.get("input")) {
        return Some(m);
    }
    if let Some(m) = arr(body.pointer("/params/messages")) {
        return Some(m);
    }
    if let Some(m) = arr(body.pointer("/request/contents")) {
        return Some(m);
    }
    if let Some(m) = arr(body.get("contents")) {
        return Some(m);
    }
    collect_kiro_headroom_messages(body).map(|(m, _)| m)
}

/// First non-null of `tools` / `params.tools` / `request.tools`, defaulting to
/// an empty array — 9router `body?.tools || body?.params?.tools ||
/// body?.request?.tools || []` (headroom.js:37).
fn tool_schema_value(body: &Value) -> Value {
    body.get("tools")
        .filter(|v| !v.is_null())
        .or_else(|| body.pointer("/params/tools").filter(|v| !v.is_null()))
        .or_else(|| body.pointer("/request/tools").filter(|v| !v.is_null()))
        .cloned()
        .unwrap_or_else(|| json!([]))
}

/// True when a message carries tool-call/tool-result material — the
/// `toolHistory` filter from 9router `captureSizeSnapshot` (headroom.js:34-47):
/// role tool/function, a non-empty `tool_calls`, a Responses
/// `function_call_output` item, `parts[]` with functionResponse/functionCall,
/// or `content[]` blocks of type tool_use/tool_result/tool-call/tool-result.
fn is_tool_history_message(msg: &Value) -> bool {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
    if role == "tool" || role == "function" {
        return true;
    }
    if msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        return true;
    }
    if msg.get("type").and_then(Value::as_str) == Some("function_call_output") {
        return true;
    }
    if msg
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            parts
                .iter()
                .any(|p| p.get("functionResponse").is_some() || p.get("functionCall").is_some())
        })
    {
        return true;
    }
    msg.get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|b| {
                matches!(
                    b.get("type").and_then(Value::as_str),
                    Some("tool_use" | "tool_result" | "tool-call" | "tool-result")
                )
            })
        })
}

/// Byte-size snapshot of a request body for the headroom size log.
/// 9router `captureSizeSnapshot`: body bytes, message/input array bytes,
/// tools (`tools`/`params.tools`/`request.tools`) bytes, and tool-history
/// bytes across every supported message shape.
pub fn capture_size_snapshot(body: &Value) -> SizeSnapshot {
    let body_bytes = json_bytes(body);
    let message_bytes = message_payload(body)
        .map(|m| json_bytes(&Value::Array(m)))
        .unwrap_or(0);
    let tool_schema_bytes = json_bytes(&tool_schema_value(body));
    let tool_history_bytes = message_payload(body)
        .map(|msgs| {
            let filtered: Vec<Value> = msgs
                .into_iter()
                .filter(|msg| is_tool_history_message(msg))
                .collect();
            json_bytes(&Value::Array(filtered))
        })
        .unwrap_or(0);
    SizeSnapshot {
        body_bytes,
        message_bytes,
        tool_schema_bytes,
        tool_history_bytes,
    }
}

/// JSON byte length of a value.
fn json_bytes(v: &Value) -> usize {
    serde_json::to_string(v).map(|s| s.len()).unwrap_or(0)
}

/// Build the `/v1/compress` endpoint from a base URL, stripping a trailing
/// slash (9router `buildCompressEndpoint`).
/// Build the Headroom `/v1/compress` endpoint, preserving the base URL's query
/// string — a tenant selector or token carried there is part of the endpoint,
/// not decoration (9router `buildCompressEndpoint`, headroom.js:65-77).
pub fn build_compress_endpoint(base_url: &str) -> String {
    match url::Url::parse(base_url) {
        Ok(mut parsed) => {
            let base = parsed.path().trim_end_matches('/');
            parsed.set_path(&format!("{base}/v1/compress"));
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => {
            let raw = base_url.split('#').next().unwrap_or(base_url);
            let (base, query) = match raw.split_once('?') {
                Some((base, query)) => (base, Some(query)),
                None => (raw, None),
            };
            let endpoint = format!("{}/v1/compress", base.trim_end_matches('/'));
            match query {
                Some(query) => format!("{endpoint}?{query}"),
                None => endpoint,
            }
        }
    }
}

/// Mask credentials/query/fragment from a URL for diagnostics
/// (9router `maskEndpoint`).
pub fn mask_endpoint(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_username("").ok();
            parsed.set_password(None).ok();
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.to_string(),
    }
}

/// Human-readable one-liner of the size delta (9router `formatHeadroomSizeLog`).
pub fn format_headroom_size_log(diag: &HeadroomDiagnostics) -> String {
    let (Some(before), Some(after)) = (&diag.before, &diag.after) else {
        return String::new();
    };
    let effective = if before.body_bytes > 0 {
        format!(
            "{:.1}",
            ((before.body_bytes - after.body_bytes) as f64 / before.body_bytes as f64) * 100.0
        )
    } else {
        "0.0".to_string()
    };
    format!(
        "body={}B→{}B messages={}B→{}B tools={}B→{}B toolHistory={}B→{}B effective={}%",
        before.body_bytes,
        after.body_bytes,
        before.message_bytes,
        after.message_bytes,
        before.tool_schema_bytes,
        after.tool_schema_bytes,
        before.tool_history_bytes,
        after.tool_history_bytes,
        effective
    )
}

/// True when the reported savings are "phantom" — NEITHER the messages NOR
/// the outbound body actually shrank by the minimum ratio (9router
/// `isHeadroomPhantomSavings`, headroom.js:545-553, default 0.05 / 5%).
///
/// Both `bodyBytes` and `messageBytes` must independently fail the shrink
/// test (each with positive before/after) for the warning to fire. A large
/// unchanged tool schema can keep `bodyBytes` nearly flat while
/// `messageBytes` still shrank — that is real savings, not phantom.
pub fn is_headroom_phantom_savings(
    stats: &HeadroomStats,
    diag: &HeadroomDiagnostics,
    min_shrink_ratio: f64,
) -> bool {
    if stats.tokens_saved == 0 {
        return false;
    }
    let flat = |pick: fn(&SizeSnapshot) -> usize| -> bool {
        let before = diag.before.as_ref().map(pick).unwrap_or(0);
        let after = diag.after.as_ref().map(pick).unwrap_or(0);
        before > 0 && after > 0 && after as f64 >= before as f64 * (1.0 - min_shrink_ratio)
    };
    flat(|s| s.body_bytes) && flat(|s| s.message_bytes)
}

/// Extract token statistics from the Headroom response, defaulting missing
/// fields to zero.
fn parse_stats(data: &Value) -> HeadroomStats {
    HeadroomStats {
        tokens_before: data
            .get("tokens_before")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        tokens_after: data
            .get("tokens_after")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        tokens_saved: data
            .get("tokens_saved")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- phantom savings tests ----

    // ---- .101 guard tests ----

    #[test]
    fn headroom_responses_shape_rejected_when_unsafe() {
        // Acceptance: body.input with a "function_call" item → compress returns
        // None and diagnostics.reason starts with "skipped:".
        let mut body = json!({
            "input": [
                { "type": "message", "role": "user", "content": "hi" },
                { "type": "function_call", "name": "f", "arguments": "{}" }
            ]
        });
        let config = HeadroomConfig {
            enabled: true,
            url: "http://localhost:9999".into(),
            ..HeadroomConfig::default()
        };
        let mut diag = HeadroomDiagnostics::default();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(compress_with_headroom_diag(
            &mut body,
            &config,
            "gpt-4o",
            "openai-responses",
            None,
            Some(&mut diag),
        ));
        assert!(result.is_none());
        let reason = diag.reason.clone().unwrap_or_default();
        assert!(
            reason.starts_with("skipped:"),
            "expected skipped reason, got: {reason}"
        );
    }

    #[test]
    fn headroom_kiro_projection_roundtrips() {
        // Acceptance: a kiro body with one history tool result; compressed text
        // is written back into conversationState.history[0]....toolResults[0].content[0].text.
        let mut body = json!({
            "conversationState": {
                "history": [
                    {
                        "userInputMessage": {
                            "content": "what's the weather",
                            "userInputMessageContext": {
                                "toolResults": [
                                    {
                                        "toolUseId": "t1",
                                        "content": [ { "text": "old tool result text" } ]
                                    }
                                ]
                            }
                        }
                    }
                ]
            }
        });
        let (messages, targets) = collect_kiro_headroom_messages(&body).unwrap();
        assert_eq!(messages.len(), 2); // user content + tool result
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["content"], "old tool result text");

        // Simulate the proxy compressing to a shorter text.
        let compressed = json!([
            { "role": "user", "content": "what's the weather" },
            { "role": "tool", "content": "compressed" }
        ]);
        let compressed_arr = compressed.as_array().unwrap();
        assert!(apply_kiro_headroom_messages(
            &mut body,
            &messages,
            &targets,
            compressed_arr
        ));
        let written = &body["conversationState"]["history"][0]["userInputMessage"]
            ["userInputMessageContext"]["toolResults"][0]["content"][0]["text"];
        assert_eq!(written, "compressed");
    }

    #[test]
    fn headroom_kiro_projection_includes_current_message() {
        // The newest turn lives in `currentMessage`; skipping it drops exactly
        // the tool result Headroom is most able to compress.
        let body = json!({
            "conversationState": {
                "history": [
                    { "userInputMessage": { "content": "earlier" } }
                ],
                "currentMessage": {
                    "userInputMessage": {
                        "content": "newest",
                        "userInputMessageContext": {
                            "toolResults": [
                                { "toolUseId": "t1", "content": [ { "text": "fresh tool output" } ] }
                            ]
                        }
                    }
                }
            }
        });
        let (messages, targets) = collect_kiro_headroom_messages(&body).expect("projects a turn");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(
            targets[2],
            "/conversationState/currentMessage/userInputMessage/userInputMessageContext/toolResults/0/content/0/text"
        );
    }

    #[test]
    fn headroom_kiro_projection_without_history() {
        let body = json!({
            "conversationState": {
                "currentMessage": { "userInputMessage": { "content": "only turn" } }
            }
        });
        let (messages, targets) =
            collect_kiro_headroom_messages(&body).expect("history is optional");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            targets[0],
            "/conversationState/currentMessage/userInputMessage/content"
        );
    }

    #[test]
    fn headroom_kiro_projection_empty_state_returns_none() {
        assert!(collect_kiro_headroom_messages(&json!({ "conversationState": {} })).is_none());
    }

    #[test]
    fn headroom_phantom_savings_detected() {
        // Acceptance: tokens_saved>0 with before/after where BOTH bodyBytes
        // and messageBytes shrink < minShrinkRatio → phantom.
        let stats = HeadroomStats {
            tokens_before: 100,
            tokens_after: 98,
            tokens_saved: 2,
        };
        let diag = HeadroomDiagnostics {
            before: Some(SizeSnapshot {
                body_bytes: 1000,
                message_bytes: 800,
                ..Default::default()
            }),
            after: Some(SizeSnapshot {
                body_bytes: 990,
                message_bytes: 795,
                ..Default::default()
            }),
            ..Default::default()
        };
        // 990 >= 1000 * 0.95 = 950 and 795 >= 800 * 0.95 = 760 → phantom.
        assert!(is_headroom_phantom_savings(&stats, &diag, 0.05));

        // A real shrink on either metric (after 600 < 950) → not phantom.
        let real = HeadroomDiagnostics {
            before: Some(SizeSnapshot {
                body_bytes: 1000,
                message_bytes: 800,
                ..Default::default()
            }),
            after: Some(SizeSnapshot {
                body_bytes: 600,
                message_bytes: 400,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!is_headroom_phantom_savings(&stats, &real, 0.05));
    }

    #[test]
    fn headroom_phantom_savings_ignores_unchanged_tool_schema() {
        // A huge tool schema keeps bodyBytes nearly flat while the messages
        // themselves compressed well past 5% — upstream must NOT warn.
        let stats = HeadroomStats {
            tokens_before: 100,
            tokens_after: 60,
            tokens_saved: 40,
        };
        let diag = HeadroomDiagnostics {
            before: Some(SizeSnapshot {
                body_bytes: 100_000, // dominated by unchanged tools
                message_bytes: 10_000,
                tool_schema_bytes: 90_000,
                ..Default::default()
            }),
            after: Some(SizeSnapshot {
                body_bytes: 95_000,   // <5% body shrink
                message_bytes: 5_000, // 50% message shrink → real savings
                tool_schema_bytes: 90_000,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!is_headroom_phantom_savings(&stats, &diag, 0.05));
    }

    #[test]
    fn headroom_phantom_savings_warns_when_genuinely_flat() {
        // Both bodyBytes and messageBytes flat (<5%) with a reported token
        // delta → phantom warning.
        let stats = HeadroomStats {
            tokens_before: 100,
            tokens_after: 98,
            tokens_saved: 2,
        };
        let diag = HeadroomDiagnostics {
            before: Some(SizeSnapshot {
                body_bytes: 1000,
                message_bytes: 800,
                ..Default::default()
            }),
            after: Some(SizeSnapshot {
                body_bytes: 990,
                message_bytes: 795,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(is_headroom_phantom_savings(&stats, &diag, 0.05));
    }

    #[test]
    fn headroom_capture_size_snapshot_counts_components() {
        let body = json!({
            "messages": [ { "role": "user", "content": "hi" } ],
            "tools": [ { "type": "function" } ]
        });
        let snap = capture_size_snapshot(&body);
        assert!(snap.body_bytes > 0);
        assert!(snap.message_bytes > 0);
        assert!(snap.tool_schema_bytes > 0);
        // No tool/function/tool_calls messages → empty filtered array ("[]" = 2 bytes).
        assert!(snap.tool_history_bytes <= 2);

        // A message with a tool role counts toward tool history.
        let body2 = json!({
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "tool", "content": "result", "tool_call_id": "t1" }
            ]
        });
        let snap2 = capture_size_snapshot(&body2);
        assert!(snap2.tool_history_bytes > 2);
    }

    #[test]
    fn headroom_size_log_and_endpoint() {
        assert_eq!(
            build_compress_endpoint("http://localhost:4623"),
            "http://localhost:4623/v1/compress"
        );
        assert_eq!(
            build_compress_endpoint("http://localhost:4623/"),
            "http://localhost:4623/v1/compress"
        );
        assert_eq!(
            mask_endpoint("http://user:pass@host:1/x?q=1#f"),
            "http://host:1/x"
        );
    }

    #[test]
    fn build_compress_endpoint_preserves_query() {
        assert_eq!(
            build_compress_endpoint("https://hr.example.com/base?tenant=acme&token=xyz"),
            "https://hr.example.com/base/v1/compress?tenant=acme&token=xyz"
        );
    }

    #[test]
    fn build_compress_endpoint_strips_trailing_slash_and_fragment() {
        assert_eq!(
            build_compress_endpoint("https://hr.example.com/base/#frag"),
            "https://hr.example.com/base/v1/compress"
        );
    }

    #[test]
    fn build_compress_endpoint_fallback_reappends_query() {
        // Unparseable base: the string fallback still has to carry the query,
        // or a tenant selector silently vanishes.
        let endpoint = build_compress_endpoint("not a url?tenant=acme");
        assert!(endpoint.ends_with("?tenant=acme"), "got: {endpoint}");
        assert!(
            endpoint.starts_with("not a url/v1/compress"),
            "got: {endpoint}"
        );
    }

    #[test]
    fn estimate_phantom_savings_returns_reasonable_estimate() {
        let body = json!({
            "messages": [
                {"role": "user", "content": &"A".repeat(400)},
                {"role": "user", "content": &"A".repeat(400)},
                {"role": "user", "content": &"A".repeat(400)},
                {"role": "user", "content": &"A".repeat(400)},
                {"role": "user", "content": &"A".repeat(400)},
            ]
        });
        // 5 * 400 = 2000 chars -> ~500 tokens before -> ~300 tokens after -> ~200 saved
        let saved = estimate_phantom_savings(&body);
        assert!(saved > 0, "should estimate savings");
    }

    #[test]
    fn estimate_phantom_savings_with_empty_body() {
        let body = json!({"messages": []});
        let saved = estimate_phantom_savings(&body);
        assert_eq!(saved, 0, "empty body should give 0 savings");
    }

    #[test]
    fn estimate_phantom_savings_handles_non_text_content() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": "https://example.com/img.png"}}
                ]}
            ]
        });
        let saved = estimate_phantom_savings(&body);
        assert_eq!(saved, 0, "no text content should give 0 savings");
    }

    #[test]
    fn estimate_phantom_savings_includes_system_field() {
        let body = json!({
            "system": "You are a helpful assistant.",
            "messages": [
                {"role": "user", "content": "Hello!"}
            ]
        });
        let saved = estimate_phantom_savings(&body);
        assert!(saved > 0, "system field text should be counted");
    }

    #[test]
    fn estimate_phantom_savings_includes_input_field() {
        let body = json!({
            "input": [
                {"role": "user", "content": "Hello from Responses API"}
            ]
        });
        let saved = estimate_phantom_savings(&body);
        assert!(saved > 0, "input field should be counted");
    }

    #[test]
    fn estimate_phantom_savings_with_system_array_blocks() {
        let body = json!({
            "system": [
                {"type": "text", "text": "You are Claude."},
                {"type": "text", "text": "Be concise."}
            ],
            "messages": [
                {"role": "user", "content": "Hi"}
            ]
        });
        let saved = estimate_phantom_savings(&body);
        assert!(saved > 0, "system array blocks should be counted");
    }

    // ---- HeadroomHooks trait tests ----

    #[test]
    fn headroom_hooks_trait_default_does_nothing() {
        struct NoopHooks;
        impl HeadroomHooks for NoopHooks {}

        let hooks = NoopHooks;
        hooks.before_compress(&[]);
        hooks.after_compress(0, 0, &Err("test".to_string()));
    }

    #[test]
    fn headroom_hooks_trait_invokes_before() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct TestHooks {
            before_called: Arc<AtomicBool>,
        }
        impl HeadroomHooks for TestHooks {
            fn before_compress(&self, _messages: &[Value]) -> Option<Value> {
                self.before_called.store(true, Ordering::SeqCst);
                None
            }
        }

        let called = Arc::new(AtomicBool::new(false));
        let hooks = TestHooks {
            before_called: called.clone(),
        };

        hooks.before_compress(&[]);
        assert!(called.load(Ordering::SeqCst));
    }

    #[test]
    fn headroom_hooks_trait_invokes_after() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct TestHooks {
            after_called: Arc<AtomicBool>,
        }
        impl HeadroomHooks for TestHooks {
            fn after_compress(
                &self,
                _orig: usize,
                _comp: usize,
                _result: &Result<HeadroomStats, String>,
            ) {
                self.after_called.store(true, Ordering::SeqCst);
            }
        }

        let called = Arc::new(AtomicBool::new(false));
        let hooks = TestHooks {
            after_called: called.clone(),
        };

        hooks.after_compress(0, 0, &Err("test".to_string()));
        assert!(called.load(Ordering::SeqCst));
    }

    #[test]
    fn headroom_hooks_trait_tracks_sizes_on_success() {
        use std::sync::Mutex;

        struct SizeHooks {
            seen: Arc<Mutex<Option<(usize, usize)>>>,
        }
        impl HeadroomHooks for SizeHooks {
            fn after_compress(
                &self,
                original_size: usize,
                compressed_size: usize,
                result: &Result<HeadroomStats, String>,
            ) {
                if result.is_ok() {
                    let mut s = self.seen.lock().unwrap();
                    *s = Some((original_size, compressed_size));
                }
            }
        }

        let seen = Arc::new(Mutex::new(None));
        let hooks = SizeHooks { seen: seen.clone() };

        let stats = HeadroomStats {
            tokens_before: 100,
            tokens_after: 60,
            tokens_saved: 40,
        };
        hooks.after_compress(500, 300, &Ok(stats));
        let recorded = seen.lock().unwrap().expect("sizes should be recorded");
        assert_eq!(recorded, (500, 300));
    }

    #[test]
    fn headroom_hooks_trait_tracks_failure() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct FailHooks {
            had_failure: Arc<AtomicBool>,
        }
        impl HeadroomHooks for FailHooks {
            fn after_compress(
                &self,
                _orig: usize,
                _comp: usize,
                result: &Result<HeadroomStats, String>,
            ) {
                if result.is_err() {
                    self.had_failure.store(true, Ordering::SeqCst);
                }
            }
        }

        let had_failure = Arc::new(AtomicBool::new(false));
        let hooks = FailHooks {
            had_failure: had_failure.clone(),
        };

        hooks.after_compress(0, 0, &Err("network error".to_string()));
        assert!(had_failure.load(Ordering::SeqCst));
    }

    // ---- HeadroomStats tests ----

    #[test]
    fn headroom_config_default_is_disabled() {
        let cfg = HeadroomConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.url.is_empty());
        assert_eq!(cfg.timeout_ms, 3000);
        assert!(!cfg.compress_user_messages);
    }

    #[test]
    fn headroom_stats_format_log_with_savings() {
        let stats = HeadroomStats {
            tokens_before: 1000,
            tokens_after: 600,
            tokens_saved: 400,
        };
        let log = stats.format_headroom_log().expect("should format");
        assert!(log.contains("saved 400 tokens / 1000"));
        assert!(log.contains("40.0%"));
        assert!(log.contains("after=600"));
        assert!(!log.contains("[phantom]"));
    }

    #[test]
    fn headroom_stats_format_log_zero_returns_none() {
        let stats = HeadroomStats::default();
        assert!(stats.format_headroom_log().is_none());
    }

    #[test]
    fn headroom_stats_format_log_no_after_when_zero() {
        let stats = HeadroomStats {
            tokens_before: 500,
            tokens_after: 0,
            tokens_saved: 500,
        };
        let log = stats.format_headroom_log().expect("should format");
        assert!(!log.contains("after="));
        assert!(log.contains("100.0%"));
    }

    #[test]
    fn headroom_stats_is_phantom() {
        let phantom = HeadroomStats {
            tokens_before: 1000,
            tokens_after: 1000,
            tokens_saved: 0,
        };
        assert!(phantom.is_phantom());

        let actual = HeadroomStats {
            tokens_before: 1000,
            tokens_after: 600,
            tokens_saved: 400,
        };
        assert!(!actual.is_phantom());

        let zero = HeadroomStats::default();
        assert!(!zero.is_phantom());
    }

    #[test]
    fn phantom_savings_format_tag() {
        let phantom = HeadroomStats {
            tokens_before: 1000,
            tokens_after: 1000,
            tokens_saved: 0,
        };
        let log = phantom.format_headroom_log().expect("should format");
        assert!(log.contains("[phantom]"));

        let actual = HeadroomStats {
            tokens_before: 1000,
            tokens_after: 600,
            tokens_saved: 400,
        };
        let log2 = actual.format_headroom_log().expect("should format");
        assert!(!log2.contains("[phantom]"));
    }

    // ---- compress helpers tests ----

    #[test]
    fn extract_openai_messages_finds_messages_key() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "user", "content": "hello" }
            ]
        });
        let (key, msgs) = extract_openai_messages(&body).expect("should find");
        assert_eq!(key, "messages");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn extract_openai_messages_finds_input_key() {
        let body = json!({
            "model": "gpt-4o",
            "input": [
                { "role": "user", "content": "hello" }
            ]
        });
        let (key, msgs) = extract_openai_messages(&body).expect("should find");
        assert_eq!(key, "input");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn extract_openai_messages_returns_none_for_unknown_shape() {
        let body = json!({ "model": "gpt-4o" });
        assert!(extract_openai_messages(&body).is_none());
    }

    #[test]
    fn build_openai_body_includes_model_and_messages() {
        let msgs = vec![json!({ "role": "user", "content": "hi" })];
        let body = build_openai_body(&msgs, "gpt-4o");
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["messages"].as_array().expect("arr").len(), 1);
    }

    #[test]
    fn parse_stats_handles_missing_fields() {
        let data = json!({ "messages": [] });
        let stats = parse_stats(&data);
        assert_eq!(stats.tokens_before, 0);
        assert_eq!(stats.tokens_after, 0);
        assert_eq!(stats.tokens_saved, 0);
    }

    #[test]
    fn parse_stats_extracts_all_fields() {
        let data = json!({
            "messages": [],
            "tokens_before": 1000,
            "tokens_after": 700,
            "tokens_saved": 300,
        });
        let stats = parse_stats(&data);
        assert_eq!(stats.tokens_before, 1000);
        assert_eq!(stats.tokens_after, 700);
        assert_eq!(stats.tokens_saved, 300);
    }

    #[test]
    fn write_compressed_messages_replaces_array() {
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "original long text" }
            ]
        });
        let data = json!({
            "messages": [
                { "role": "user", "content": "short" }
            ]
        });
        assert!(write_compressed_messages(&mut body, "messages", &data).is_some());
        assert_eq!(body["messages"][0]["content"], "short");
    }

    #[test]
    fn write_compressed_messages_returns_none_when_no_array() {
        let mut body = json!({ "messages": [] });
        let data = json!({ "error": "bad" });
        assert!(write_compressed_messages(&mut body, "messages", &data).is_none());
    }

    // ---- compress_with_headroom integration tests ----

    #[tokio::test]
    async fn compress_with_headroom_returns_none_when_disabled() {
        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let config = HeadroomConfig::default();
        assert!(
            compress_with_headroom(&mut body, &config, "gpt-4o", "openai", None)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn compress_with_headroom_returns_none_when_url_empty() {
        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let config = HeadroomConfig {
            enabled: true,
            url: String::new(),
            ..HeadroomConfig::default()
        };
        assert!(
            compress_with_headroom(&mut body, &config, "gpt-4o", "openai", None)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn compress_with_headroom_returns_none_for_unknown_body_shape() {
        let mut body = json!({ "model": "gpt-4o" });
        let config = HeadroomConfig {
            enabled: true,
            url: "http://localhost:9999".into(),
            ..HeadroomConfig::default()
        };
        assert!(
            compress_with_headroom(&mut body, &config, "gpt-4o", "openai", None)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn compress_with_headroom_returns_none_on_network_error() {
        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let config = HeadroomConfig {
            enabled: true,
            url: "http://127.0.0.1:1".into(),
            timeout_ms: 100,
            ..HeadroomConfig::default()
        };
        assert!(
            compress_with_headroom(&mut body, &config, "gpt-4o", "openai", None)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn compress_with_headroom_claude_shape_returns_none_on_network_error() {
        let mut body = json!({
            "system": "You are helpful.",
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let config = HeadroomConfig {
            enabled: true,
            url: "http://127.0.0.1:1".into(),
            timeout_ms: 100,
            ..HeadroomConfig::default()
        };
        assert!(compress_with_headroom(
            &mut body,
            &config,
            "claude-sonnet-4-20250514",
            "claude",
            None,
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn compress_with_headroom_invokes_hooks_on_disabled() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct TestHooks {
            after_called: Arc<AtomicBool>,
        }
        impl HeadroomHooks for TestHooks {
            fn after_compress(
                &self,
                _orig: usize,
                _comp: usize,
                result: &Result<HeadroomStats, String>,
            ) {
                assert!(result.is_err());
                self.after_called.store(true, Ordering::SeqCst);
            }
        }

        let after_called = Arc::new(AtomicBool::new(false));
        let hooks = TestHooks {
            after_called: after_called.clone(),
        };

        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let config = HeadroomConfig::default();
        assert!(compress_with_headroom(
            &mut body,
            &config,
            "gpt-4o",
            "openai",
            Some(&hooks as &dyn HeadroomHooks),
        )
        .await
        .is_none());
        assert!(after_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn compress_with_headroom_invokes_before_hook() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct TestHooks {
            before_called: Arc<AtomicBool>,
        }
        impl HeadroomHooks for TestHooks {
            fn before_compress(&self, messages: &[Value]) -> Option<Value> {
                assert!(!messages.is_empty(), "messages should not be empty");
                self.before_called.store(true, Ordering::SeqCst);
                None
            }
        }

        let before_called = Arc::new(AtomicBool::new(false));
        let hooks = TestHooks {
            before_called: before_called.clone(),
        };

        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });
        // Use a dead port so it fails gracefully, but before_hook should fire
        let config = HeadroomConfig {
            enabled: true,
            url: "http://127.0.0.1:1".into(),
            timeout_ms: 100,
            ..HeadroomConfig::default()
        };
        let result = compress_with_headroom(
            &mut body,
            &config,
            "gpt-4o",
            "openai",
            Some(&hooks as &dyn HeadroomHooks),
        )
        .await;
        assert!(result.is_none());
        assert!(before_called.load(Ordering::SeqCst));
    }

    // ---- openproxy-4d5h.22: CommandCode / tool-output / Gemini projections ----

    #[test]
    fn message_payload_covers_commandcode_and_gemini_shapes() {
        let cc = json!({"params": {"messages": [{"role": "user", "content": []}]}});
        assert_eq!(message_payload(&cc).unwrap().len(), 1);

        let gem = json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]});
        assert_eq!(message_payload(&gem).unwrap().len(), 1);

        let ag = json!({"request": {"contents": [{"role": "user", "parts": []}]}});
        assert_eq!(message_payload(&ag).unwrap().len(), 1);

        // Snapshot picks up params.messages for message_bytes.
        let snap = capture_size_snapshot(&cc);
        assert!(snap.message_bytes > 0);
        assert!(snap.tool_schema_bytes >= 2); // "[]" floor
    }

    #[test]
    fn tool_output_projection_gemini_function_response() {
        let body = json!({
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "user", "parts": [{
                    "functionResponse": {
                        "name": "read",
                        "response": {
                            "result": "a very long tool output that should compress",
                            "meta": {"keep": "structured stays native"}
                        }
                    }
                }]}
            ]
        });
        let proj = collect_tool_output_text(&body, "gemini").expect("gemini projection");
        assert_eq!(proj.messages.len(), 1);
        assert_eq!(proj.messages[0]["role"], "tool");
        assert_eq!(
            proj.messages[0]["content"],
            "a very long tool output that should compress"
        );

        // Simulate the proxy returning a shorter text → written back.
        let mut body2 = body.clone();
        let compressed = json!([{"role": "tool", "tool_call_id": proj.messages[0]["tool_call_id"], "content": "short"}]);
        assert!(apply_tool_output_text(
            &mut body2,
            &proj,
            compressed.as_array().unwrap(),
            None
        ));
        assert_eq!(
            body2["contents"][1]["parts"][0]["functionResponse"]["response"]["result"],
            "short"
        );
        // Non-leaf fields are untouched.
        assert_eq!(
            body2["contents"][1]["parts"][0]["functionResponse"]["response"]["meta"]["keep"],
            "structured stays native"
        );
    }

    #[test]
    fn tool_output_projection_skips_json_strings_and_empty() {
        let body = json!({
            "contents": [{"role": "user", "parts": [{
                "functionResponse": {"response": {
                    "result": "{\"structured\": true}",
                    "content": "   ",
                    "text": "plain text output"
                }}
            }]}]
        });
        let proj = collect_tool_output_text(&body, "vertex").expect("vertex projection");
        // Only "plain text output" is eligible — JSON and blank strings stay native.
        assert_eq!(proj.messages.len(), 1);
        assert_eq!(proj.messages[0]["content"], "plain text output");
    }

    #[test]
    fn tool_output_projection_antigravity_request_envelope() {
        let body = json!({
            "request": {"contents": [{"role": "user", "parts": [{
                "functionResponse": {"response": {"result": "tool text here"}}
            }]}]}
        });
        let proj = collect_tool_output_text(&body, "antigravity").expect("antigravity projection");
        assert_eq!(proj.messages.len(), 1);

        let mut body2 = body.clone();
        let compressed = json!([{"role": "tool", "tool_call_id": proj.messages[0]["tool_call_id"], "content": "compressed"}]);
        assert!(apply_tool_output_text(
            &mut body2,
            &proj,
            compressed.as_array().unwrap(),
            None
        ));
        assert_eq!(
            body2["request"]["contents"][0]["parts"][0]["functionResponse"]["response"]["result"],
            "compressed"
        );
    }

    #[test]
    fn tool_output_projection_responses_unsafe_input() {
        let body = json!({
            "input": [
                {"type": "message", "role": "user", "content": "hi"},
                {"type": "function_call_output", "output": "a long plain tool result"}
            ]
        });
        let proj = collect_tool_output_text(&body, "openai-responses")
            .expect("unsafe responses projection");
        assert_eq!(proj.messages.len(), 1);
        assert_eq!(proj.messages[0]["content"], "a long plain tool result");

        // Safe responses input → no tool-output projection.
        let safe = json!({"input": [{"type": "message", "role": "user", "content": "hi"}]});
        assert!(collect_tool_output_text(&safe, "openai-responses").is_none());
        // Unrelated formats → no projection.
        assert!(collect_tool_output_text(&body, "openai").is_none());
        assert!(collect_tool_output_text(&body, "claude").is_none());
    }

    #[test]
    fn tool_output_apply_rejects_mismatched_proxy_response() {
        let body = json!({"contents": [{"role": "user", "parts": [{
            "functionResponse": {"response": {"result": "original text"}}
        }]}]});
        let proj = collect_tool_output_text(&body, "gemini").unwrap();
        let mut diag = HeadroomDiagnostics::default();
        // Wrong count.
        assert!(!apply_tool_output_text(
            &mut body.clone(),
            &proj,
            &[],
            Some(&mut diag)
        ));
        assert!(diag.reason.is_some());
        // Wrong role.
        let bad = json!([{"role": "user", "content": "x"}]);
        let mut diag2 = HeadroomDiagnostics::default();
        assert!(!apply_tool_output_text(
            &mut body.clone(),
            &proj,
            bad.as_array().unwrap(),
            Some(&mut diag2)
        ));
        assert!(diag2.reason.is_some());
        // Longer text is NOT written back (only shorter non-empty).
        let longer = json!([{"role": "tool", "tool_call_id": proj.messages[0]["tool_call_id"], "content": "this is much longer than the original text was"}]);
        let mut body3 = body.clone();
        assert!(apply_tool_output_text(
            &mut body3,
            &proj,
            longer.as_array().unwrap(),
            None
        ));
        assert_eq!(
            body3["contents"][0]["parts"][0]["functionResponse"]["response"]["result"],
            "original text"
        );
    }

    #[test]
    fn commandcode_projection_collects_and_writes_back() {
        let body = json!({
            "params": {
                "system": "you are helpful",
                "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "hello there"}]},
                    {"role": "assistant", "content": [
                        {"type": "text", "text": "calling tool"},
                        {"type": "tool-call", "toolCallId": "tc1", "toolName": "read", "input": {"path": "/x"}}
                    ]},
                    {"role": "tool", "content": [
                        {"type": "tool-result", "toolCallId": "tc1", "toolName": "read",
                         "output": {"type": "text", "value": "a very long tool output text"}}
                    ]}
                ]
            }
        });
        let proj = collect_commandcode_headroom_messages(&body).expect("commandcode projection");
        // system + user text + assistant tool_calls pseudo + assistant text + tool output
        assert_eq!(proj.messages.len(), 5);
        assert_eq!(proj.messages[0]["role"], "system");
        assert_eq!(proj.messages[2]["role"], "assistant");
        assert_eq!(
            proj.messages[2]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert_eq!(proj.messages[4]["role"], "tool");
        assert_eq!(proj.messages[4]["tool_call_id"], "tc1");
        // The pseudo-message has no writable target.
        assert!(proj.targets[2].is_none());

        // Simulate proxy compression: shorter texts for the writable targets.
        let compressed: Vec<Value> = proj
            .messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                if proj.targets[i].is_some() {
                    json!({"role": m["role"], "content": "c", "tool_call_id": m.get("tool_call_id").cloned().unwrap_or(Value::Null)})
                } else {
                    m.clone()
                }
            })
            .collect();
        let mut body2 = body.clone();
        assert!(apply_projected_headroom_messages(
            &mut body2,
            &proj,
            &compressed,
            "CommandCode",
            None
        ));
        assert_eq!(body2["params"]["system"], "c");
        assert_eq!(body2["params"]["messages"][0]["content"][0]["text"], "c");
        assert_eq!(
            body2["params"]["messages"][2]["content"][0]["output"]["value"],
            "c"
        );
        // tool-call input object untouched.
        assert_eq!(
            body2["params"]["messages"][1]["content"][1]["input"]["path"],
            "/x"
        );
    }

    #[test]
    fn commandcode_projection_none_without_params_messages() {
        let body = json!({"params": {"system": "x"}});
        assert!(collect_commandcode_headroom_messages(&body).is_none());
        let body2 = json!({"messages": []});
        assert!(collect_commandcode_headroom_messages(&body2).is_none());
    }

    #[test]
    fn commandcode_format_dispatches_in_headroom() {
        // Format string routing: "commandcode" must not fall through to the
        // OpenAI messages/input extractor.
        let body = json!({"params": {"messages": []}});
        // No network: a dead port proves the dispatch reached call_compress
        // (reason is a request failure, not "unsupported ... request shape").
        let config = HeadroomConfig {
            enabled: true,
            url: "http://127.0.0.1:1".into(),
            timeout_ms: 100,
            ..HeadroomConfig::default()
        };
        let mut diag = HeadroomDiagnostics::default();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(compress_with_headroom_diag(
            &mut body.clone(),
            &config,
            "m",
            "commandcode",
            None,
            Some(&mut diag),
        ));
        assert!(result.is_none());
        let reason = diag.reason.clone().unwrap_or_default();
        assert!(
            reason.starts_with("request failed:") || reason.contains("HTTP"),
            "expected transport failure reason, got: {reason}"
        );
    }
}
