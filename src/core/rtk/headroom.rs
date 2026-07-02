use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

const DEFAULT_TIMEOUT_MS: u64 = 3000;

/// Rough estimate: chars per token used for phantom savings prediction.
const PHANTOM_CHARS_PER_TOKEN: usize = 4;

/// Rough estimate: expected compression ratio for phantom savings (40% reduction).
const PHANTOM_ESTIMATED_RATIO: f64 = 0.6;

// ---------------------------------------------------------------------------
// Lifecycle hooks
// ---------------------------------------------------------------------------

/// Observer callbacks invoked during the Headroom compression pipeline.
///
/// Both hooks are optional — implement only the ones you need. Hook functions
/// run inside the `compress_with_headroom` call and block further pipeline
/// progress while they execute, so keep them lightweight (e.g., emit a trace,
/// increment a counter, push to a log buffer).
pub struct HeadroomHooks {
    /// Called **before** the compression request is sent to the Headroom proxy.
    /// `messages` is the flattened message array that will be POSTed.
    pub before_compress: Option<Arc<dyn Fn(&[Value]) + Send + Sync>>,
    /// Called **after** compression completes (or fails). `result` carries
    /// the optional stats (None when compression was skipped or errored).
    pub after_compress: Option<Arc<dyn Fn(Option<&HeadroomStats>) + Send + Sync>>,
}

impl Default for HeadroomHooks {
    fn default() -> Self {
        Self {
            before_compress: None,
            after_compress: None,
        }
    }
}

impl HeadroomHooks {
    /// Create a new `HeadroomHooks` with no hooks registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Convenience: register both hooks at once.
    pub fn with_hooks(
        before: Option<Arc<dyn Fn(&[Value]) + Send + Sync>>,
        after: Option<Arc<dyn Fn(Option<&HeadroomStats>) + Send + Sync>>,
    ) -> Self {
        Self {
            before_compress: before,
            after_compress: after,
        }
    }

    fn invoke_before(&self, messages: &[Value]) {
        if let Some(ref hook) = self.before_compress {
            hook(messages);
        }
    }

    fn invoke_after(&self, stats: Option<&HeadroomStats>) {
        if let Some(ref hook) = self.after_compress {
            hook(stats);
        }
    }
}

// ---------------------------------------------------------------------------
// Phantom savings estimation
// ---------------------------------------------------------------------------

/// Pre-report estimated compression savings before actually compressing.
///
/// Phantom savings provide an early signal of expected token reduction to
/// dashboards, request logs, or feedback loops that need to know (before the
/// Headroom proxy responds) roughly how much compression the system *would*
/// achieve. The estimate is intentionally conservative rather than optimstic
/// so callers don't over-promise savings.
///
/// The estimate is based on the text content of all messages, counting
/// characters and dividing by a typical tokens-per-character ratio, then
/// applying the expected compression ratio.
///
/// `tokens_before` is the estimated token count of the input. `tokens_after`
/// is estimated at ~60% of `tokens_before`. The estimate is returned as a
/// [`HeadroomStats`] so it can be logged or displayed identically to real
/// compression results.
pub fn estimate_phantom_savings(messages: &[Value]) -> HeadroomStats {
    let char_count: usize = messages
        .iter()
        .filter_map(|msg| {
            msg.get("content")
                .and_then(|c| c.as_str())
                .map(|s| s.len())
        })
        .sum();

    let tokens_before = (char_count + PHANTOM_CHARS_PER_TOKEN - 1) / PHANTOM_CHARS_PER_TOKEN;
    let tokens_before = tokens_before.max(1) as u64;
    let tokens_after = (tokens_before as f64 * PHANTOM_ESTIMATED_RATIO).round() as u64;
    let tokens_saved = tokens_before.saturating_sub(tokens_after);

    HeadroomStats {
        tokens_before,
        tokens_after,
        tokens_saved,
    }
}

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
/// When `format` is `"claude"`, the messages are sent as-is to the proxy
/// (the proxy must handle Claude-native content-block messages).
///
/// `hooks` provides optional lifecycle callbacks (before/after compress) for
/// observability. Pass `None` to skip hooks.
pub async fn compress_with_headroom(
    body: &mut Value,
    config: &HeadroomConfig,
    model: &str,
    format: &str,
    hooks: Option<&HeadroomHooks>,
) -> Option<HeadroomStats> {
    if !config.enabled || config.url.is_empty() {
        hooks.invoke_after_inner(None);
        return None;
    }

    let fields = body.as_object()?;

    if format.eq_ignore_ascii_case("claude") {
        return compress_claude_body(body, config, model, hooks).await;
    }

    // OpenAI / Responses-API shape.
    let (key, messages) = extract_openai_messages(body)?;
    hooks.invoke_before_inner(&messages);
    let data = call_compress(config, &messages, model).await?;
    let stats = parse_stats(&data);
    write_compressed_messages(body, key, &data)?;
    hooks.invoke_after_inner(Some(&stats));
    Some(stats)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Hook helper: invoke before_compress if hooks are present.
fn invoke_hooks_before(hooks: Option<&HeadroomHooks>, messages: &[Value]) {
    if let Some(h) = hooks {
        h.invoke_before(messages);
    }
}

/// Hook helper: invoke after_compress if hooks are present.
fn invoke_hooks_after(hooks: Option<&HeadroomHooks>, stats: Option<&HeadroomStats>) {
    if let Some(h) = hooks {
        h.invoke_after(stats);
    }
}

/// Extension trait to allow calling hook methods on `Option<&HeadroomHooks>`.
trait HeadroomHooksOption {
    fn invoke_before_inner(&self, messages: &[Value]);
    fn invoke_after_inner(&self, stats: Option<&HeadroomStats>);
}

impl HeadroomHooksOption for Option<&HeadroomHooks> {
    fn invoke_before_inner(&self, messages: &[Value]) {
        invoke_hooks_before(*self, messages);
    }

    fn invoke_after_inner(&self, stats: Option<&HeadroomStats>) {
        invoke_hooks_after(*self, stats);
    }
}

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
    hooks: Option<&HeadroomHooks>,
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

    hooks.invoke_before_inner(&flat_messages);
    let data = call_compress(config, &flat_messages, model).await?;

    // Write compressed messages back into the Claude body.
    if let Some(compressed) = data.get("messages").and_then(Value::as_array) {
        body["messages"] = Value::Array(compressed.clone());
    } else {
        hooks.invoke_after_inner(None);
        return None;
    }

    let stats = parse_stats(&data);
    hooks.invoke_after_inner(Some(&stats));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        // tokens_before > 0, tokens_saved == 0 → phantom
        let phantom = HeadroomStats {
            tokens_before: 1000,
            tokens_after: 1000,
            tokens_saved: 0,
        };
        assert!(phantom.is_phantom());

        // Actual stats
        let actual = HeadroomStats {
            tokens_before: 1000,
            tokens_after: 600,
            tokens_saved: 400,
        };
        assert!(!actual.is_phantom());

        // All-zero
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

    #[test]
    fn estimate_phantom_savings_returns_reasonable_estimate() {
        let msg = json!({"role": "user", "content": "A".repeat(400)});
        let messages = vec![msg; 5]; // 5 * 400 = 2000 chars → ~500 tokens before → ~300 tokens after

        let stats = estimate_phantom_savings(&messages);
        assert!(stats.tokens_before > 0, "should estimate tokens_before");
        assert!(stats.tokens_after > 0, "should estimate tokens_after");
        assert!(stats.tokens_saved > 0, "should estimate savings");
        assert!(stats.tokens_before >= stats.tokens_after);
    }

    #[test]
    fn estimate_phantom_savings_with_empty_messages() {
        let stats = estimate_phantom_savings(&[]);
        // tokens_before = max(1) = 1; tokens_after = (1 * 0.6).round() = 1
        assert_eq!(stats.tokens_before, 1);
        assert_eq!(stats.tokens_after, 1);
        // tokens_saved = 1 - 1 = 0 (a phantom stat — pre-report, no actual savings)
        assert_eq!(stats.tokens_saved, 0);
        assert!(stats.is_phantom());
    }

    #[test]
    fn estimate_phantom_savings_handles_non_text_content() {
        // Messages with no text content should still produce a token estimate
        let messages = vec![
            json!({"role": "user", "content": [{"type": "image_url", "image_url": {"url": "https://example.com/img.png"}}]}),
        ];
        let stats = estimate_phantom_savings(&messages);
        // The content is an array, not a string, so char_count = 0
        assert_eq!(stats.tokens_before, 1);
    }

    #[test]
    fn headroom_hooks_default_does_nothing() {
        let hooks = HeadroomHooks::default();
        // Just ensure we don't panic
        hooks.invoke_before(&[]);
        hooks.invoke_after(None);
    }

    #[test]
    fn headroom_hooks_with_hooks_creates_both() {
        let before_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let after_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let before = before_called.clone();
        let after = after_called.clone();

        let hooks = HeadroomHooks::with_hooks(
            Some(Arc::new(move |_: &[Value]| {
                before.store(true, std::sync::atomic::Ordering::SeqCst);
            })),
            Some(Arc::new(move |_: Option<&HeadroomStats>| {
                after.store(true, std::sync::atomic::Ordering::SeqCst);
            })),
        );

        hooks.invoke_before(&[]);
        assert!(before_called.load(std::sync::atomic::Ordering::SeqCst));

        hooks.invoke_after(None);
        assert!(after_called.load(std::sync::atomic::Ordering::SeqCst));
    }

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
        // Use a URL that will fail to connect (no server on this port).
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
        assert!(
            compress_with_headroom(
                &mut body,
                &config,
                "claude-sonnet-4-20250514",
                "claude",
                None,
            )
            .await
            .is_none()
        );
    }

    #[tokio::test]
    async fn compress_with_headroom_invokes_hooks_on_disabled() {
        // When config is disabled, invoke_after should be called with None
        let after_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let after = after_called.clone();
        let hooks = HeadroomHooks::with_hooks(
            None,
            Some(Arc::new(move |stats: Option<&HeadroomStats>| {
                assert!(stats.is_none());
                after.store(true, std::sync::atomic::Ordering::SeqCst);
            })),
        );

        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let config = HeadroomConfig::default();
        assert!(
            compress_with_headroom(&mut body, &config, "gpt-4o", "openai", Some(&hooks))
                .await
                .is_none()
        );
        assert!(after_called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
