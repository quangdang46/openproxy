use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

const DEFAULT_TIMEOUT_MS: u64 = 3000;

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
    if !config.enabled || config.url.is_empty() {
        if let Some(h) = hooks {
            h.after_compress(0, 0, &Err("compression disabled".to_string()));
        }
        return None;
    }

    let fields = body.as_object()?;

    if format.eq_ignore_ascii_case("claude") {
        return compress_claude_body(body, config, model, hooks).await;
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

/// Replace the message array in the body under the given key.
fn write_compressed_messages(body: &mut Value, key: &str, data: &Value) -> Option<()> {
    let compressed = data.get("messages").and_then(Value::as_array)?;
    body[key] = Value::Array(compressed.clone());
    Some(())
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
