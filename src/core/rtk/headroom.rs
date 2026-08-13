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

    let fields = body.as_object()?;

    if format.eq_ignore_ascii_case("claude") {
        return compress_claude_body(body, config, model, hooks).await;
    }

    if format.eq_ignore_ascii_case("openai-responses") {
        return compress_responses_body(body, config, model, hooks, diagnostics).await;
    }

    if format.eq_ignore_ascii_case("kiro") {
        return compress_kiro_body(body, config, model, hooks, diagnostics).await;
    }

    // OpenAI / Responses-API shape.
    let (key, messages) = extract_openai_messages(body)?;

    // Notify hook before compression.
    if let Some(h) = hooks {
        h.before_compress(&messages);
    }

    let original_size = serde_json::to_string(&messages)
        .map(|s| s.len())
        .unwrap_or(0);
    let data = call_compress(config, &messages, model).await?;
    let compressed_size = data
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|arr| serde_json::to_string(arr).ok())
        .map(|s| s.len())
        .unwrap_or(0);
    let stats = parse_stats(&data);
    write_compressed_messages(body, key, &data)?;

    if let Some(d) = diagnostics.as_deref_mut() {
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
/// JSON response on success. Returns `None` on any failure.
///
/// Ports `callCompress()` from upstream 9router.
async fn call_compress(config: &HeadroomConfig, messages: &[Value], model: &str) -> Option<Value> {
    let endpoint = format!("{}/v1/compress", config.url.trim_end_matches('/'));

    let mut payload = build_openai_body(messages, model);
    if config.compress_user_messages {
        payload["config"] = json!({ "compress_user_messages": true });
    }

    let timeout = Duration::from_millis(config.timeout_ms);

    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(_) => return None,
    };

    let response = match client
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return None,
    };

    if !response.status().is_success() {
        return None;
    }

    let data: Value = match response.json().await {
        Ok(v) => v,
        Err(_) => return None,
    };

    // The proxy must return a messages array.
    if data.get("messages").and_then(Value::as_array).is_none() {
        return None;
    }

    Some(data)
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
) -> Option<HeadroomStats> {
    let raw_messages = body.get("messages").and_then(Value::as_array)?.clone();

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
    let data = call_compress(config, &flat_messages, model).await?;

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
    let data = call_compress(config, &messages, model).await?;
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

    if let Some(d) = diagnostics.as_deref_mut() {
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

    let history = state.get("history").and_then(Value::as_array)?;
    for (idx, item) in history.iter().enumerate() {
        let user = item.get("userInputMessage");
        if let Some(user) = user {
            if let Some(text) = user.get("systemInstruction").and_then(Value::as_str) {
                messages.push(json!({ "role": "system", "content": text }));
                targets.push(format!(
                    "/conversationState/history/{idx}/userInputMessage/systemInstruction"
                ));
            }
            if let Some(text) = user.get("content").and_then(Value::as_str) {
                messages.push(json!({ "role": "user", "content": text }));
                targets.push(format!(
                    "/conversationState/history/{idx}/userInputMessage/content"
                ));
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
                            "/conversationState/history/{idx}/userInputMessage/userInputMessageContext/toolResults/{ri}/content/{pi}/text"
                        ));
                    }
                }
            }
            continue;
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
                                    serde_json::to_string(input)
                                        .unwrap_or_else(|_| "{}".to_string())
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
                continue;
            }
            messages.push(msg);
            targets.push(format!(
                "/conversationState/history/{idx}/assistantResponseMessage/content"
            ));
        }
    }
    if messages.is_empty() {
        return None;
    }
    Some((messages, targets))
}

/// Extract the text content from a headroom-compressed message
/// (9router `textFromHeadroomMessage`).
fn text_from_headroom_message(msg: &Value) -> Option<String> {
    msg.get("content")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
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
    let data = call_compress(config, &messages, model).await?;
    let stats = parse_stats(&data);
    let compressed = data.get("messages").and_then(Value::as_array).cloned()?;

    if !apply_kiro_headroom_messages(body, &messages, &targets, &compressed) {
        if let Some(d) = diagnostics.as_deref_mut() {
            d.set_reason("proxy response did not match Kiro message count/order/text");
        }
        return None;
    }

    if let Some(d) = diagnostics.as_deref_mut() {
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

/// Byte-size snapshot of a request body for the headroom size log.
/// 9router `captureSizeSnapshot`: body bytes, message/input array bytes,
/// tools (top-level `tools`) bytes, and tool-history bytes (messages with
/// role tool/function, tool_calls, or content blocks of type tool_use/tool_result).
pub fn capture_size_snapshot(body: &Value) -> SizeSnapshot {
    let body_bytes = json_bytes(body);
    let message_bytes = body
        .get("messages")
        .or_else(|| body.get("input"))
        .and_then(Value::as_array)
        .map(|arr| json_bytes(&Value::Array(arr.clone())))
        .unwrap_or(0);
    let tool_schema_bytes = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| json_bytes(&Value::Array(arr.clone())))
        .unwrap_or(0);
    let tool_history_bytes = body
        .get("messages")
        .or_else(|| body.get("input"))
        .and_then(Value::as_array)
        .map(|arr| {
            let filtered: Vec<&Value> = arr
                .iter()
                .filter(|msg| {
                    let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
                    if role == "tool" || role == "function" {
                        return true;
                    }
                    if msg.get("tool_calls").and_then(Value::as_array).is_some() {
                        return true;
                    }
                    msg.get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| {
                            blocks.iter().any(|b| {
                                matches!(
                                    b.get("type").and_then(Value::as_str),
                                    Some("tool_use" | "tool_result")
                                )
                            })
                        })
                })
                .collect();
            json_bytes(&Value::Array(filtered.into_iter().cloned().collect()))
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
pub fn build_compress_endpoint(base_url: &str) -> String {
    format!("{}/v1/compress", base_url.trim_end_matches('/'))
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

/// True when the reported savings are "phantom" — the body did not actually
/// shrink by the minimum ratio (9router `isHeadroomPhantomSavings`, default
/// 0.05 / 5%).
pub fn is_headroom_phantom_savings(
    stats: &HeadroomStats,
    diag: &HeadroomDiagnostics,
    min_shrink_ratio: f64,
) -> bool {
    if stats.tokens_saved == 0 {
        return false;
    }
    let before = diag.before.as_ref().map(|s| s.body_bytes).unwrap_or(0);
    let after = diag.after.as_ref().map(|s| s.body_bytes).unwrap_or(0);
    if before == 0 || after == 0 {
        return false;
    }
    after as f64 >= before as f64 * (1.0 - min_shrink_ratio)
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
    fn headroom_phantom_savings_detected() {
        // Acceptance: tokens_saved>0 with before/after where after >= before*0.95
        // → is_headroom_phantom_savings true.
        let stats = HeadroomStats {
            tokens_before: 100,
            tokens_after: 98,
            tokens_saved: 2,
        };
        let diag = HeadroomDiagnostics {
            before: Some(SizeSnapshot {
                body_bytes: 1000,
                ..Default::default()
            }),
            after: Some(SizeSnapshot {
                body_bytes: 990,
                ..Default::default()
            }),
            ..Default::default()
        };
        // 990 >= 1000 * 0.95 = 950 → phantom.
        assert!(is_headroom_phantom_savings(&stats, &diag, 0.05));

        // A real shrink (after 600 < 950) → not phantom.
        let real = HeadroomDiagnostics {
            before: Some(SizeSnapshot {
                body_bytes: 1000,
                ..Default::default()
            }),
            after: Some(SizeSnapshot {
                body_bytes: 600,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!is_headroom_phantom_savings(&stats, &real, 0.05));
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
}
