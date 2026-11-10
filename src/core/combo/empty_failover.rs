//! Port of `open-sse/services/emptyFailover.js` + the peek half of
//! `open-sse/services/combo.js` (`peekComboResponse`, `isZeroContentResponse`).
//!
//! A 2xx is necessary but NOT sufficient for combo success: Gemini aborts
//! (e.g. MALFORMED_FUNCTION_CALL) still return HTTP 200 and stream a terminal
//! frame with empty content, and a non-streaming member can answer 200 with
//! an empty assistant message. Both must fall through to the next combo
//! member instead of being delivered as a successful empty answer.
//!
//! The signal is STRUCTURAL, not an enum: "the stream ended and no content
//! frame was ever released". That covers MALFORMED_FUNCTION_CALL, any future
//! Gemini abort reason, and a stream that simply dies — with no reason
//! vocabulary to keep in sync. The finish-reason enum is used only in the
//! inverse: to NOT retry a legitimately-empty content-filter block.

use axum::body::Body;
use axum::http::header;
use axum::http::HeaderMap;
use axum::response::Response;
use bytes::Bytes;
use http_body::Frame;
use http_body_util::{BodyExt, StreamBody};
use once_cell::sync::Lazy;
use serde_json::Value;

use super::{ComboAttemptError, ComboOkVerdict};

/// Default peek deadline — `EMPTY_PEEK_DEADLINE_MS` env, 45s like upstream
/// `runtimeConfig.js:61`. The deadline is fail-open: a possibly-valid slow
/// stream is committed rather than held hostage.
const DEFAULT_EMPTY_PEEK_DEADLINE_MS: u64 = 45 * 1000;

static EMPTY_PEEK_DEADLINE_MS: Lazy<u64> = Lazy::new(|| {
    std::env::var("EMPTY_PEEK_DEADLINE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_EMPTY_PEEK_DEADLINE_MS)
});

/// Client-format (post-translation) finish_reasons whose empty output is
/// LEGITIMATE and would recur on every candidate — so an empty stream
/// ending in one of these must NOT be retried
/// (`NO_RETRY_EMPTY_FINISH`, translator/schema/finishReasons.js).
fn is_no_retry_finish_value(finish_reason: &str) -> bool {
    finish_reason.eq_ignore_ascii_case("content_filter")
}

/// `isNoRetryFinish` — true when a finished, content-less stream should NOT
/// be retried because the emptiness is legitimate (safety / recitation /
/// blocklist / prohibited-content). Everything else that finished empty is
/// retryable.
pub fn is_no_retry_finish(finish_reason: Option<&str>) -> bool {
    finish_reason.map(is_no_retry_finish_value).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// SSE line inspection (client format: OpenAI chat, Claude Messages,
// OpenAI Responses — the three formats the peek can read).
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct LineInfo {
    content: bool,
    finish_reason: Option<String>,
}

/// OpenAI chat content test (`deltaHasClientContent`). A chunk counts as
/// content only if it carries assistant output the caller can consume:
///   * delta.content (non-empty string, or non-empty array of parts)
///   * delta.tool_calls (a real tool call)
///   * delta.output_text (Responses-style non-chat deltas)
/// It deliberately does NOT count:
///   * delta.role — a `{role:"assistant"}` frame is emitted regardless of
///     content; treating it as content would defeat the whole check.
///   * reasoning_content / delta.reasoning — a think-then-abort produces
///     reasoning only, which is an unusable-but-non-empty turn; it must
///     still fall through.
fn delta_has_client_content(delta: &Value) -> bool {
    let Some(obj) = delta.as_object() else {
        return false;
    };
    match obj.get("content") {
        Some(Value::String(s)) if !s.is_empty() => return true,
        Some(Value::Array(a)) if !a.is_empty() => return true,
        _ => {}
    }
    if obj
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        return true;
    }
    if let Some(Value::String(s)) = obj.get("output_text") {
        if !s.is_empty() {
            return true;
        }
    }
    false
}

fn non_empty_string(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(s)) if !s.is_empty())
}

/// JS `a || b` truthiness for the `choice.delta || choice.message` /
/// `choice.message || choice.delta` picks (`inspectSSELine`,
/// `isZeroContentResponse`): null/undefined, `false`, `0` and `""` fall
/// through — but empty arrays/objects are TRUTHY in JS and must not.
fn is_js_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        _ => false,
    }
}

/// Claude stop_reason → hub (OpenAI) finish vocabulary — the `claude` arm of
/// `toOpenAIFinish` (translator/concerns/finishReason.js). Only
/// `refusal` maps to `content_filter`; every other reason maps to a
/// non-no-retry finish.
fn claude_stop_to_openai_finish(stop_reason: &str) -> String {
    match stop_reason {
        "max_tokens" => "length".to_string(),
        "tool_use" => "tool_calls".to_string(),
        "refusal" => "content_filter".to_string(),
        // end_turn / stop_sequence / unknown → stop
        _ => "stop".to_string(),
    }
}

/// Anthropic Messages event (`inspectClaudeEvent`).
fn inspect_claude_event(obj: &Value) -> LineInfo {
    let mut info = LineInfo::default();
    match obj.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => {
            let delta = obj.get("delta");
            match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                Some("text_delta") => {
                    info.content = non_empty_string(delta.and_then(|d| d.get("text")));
                }
                Some("input_json_delta") => {
                    info.content = non_empty_string(delta.and_then(|d| d.get("partial_json")));
                }
                _ => {}
            }
        }
        Some("content_block_start") => {
            let block = obj.get("content_block");
            match block.and_then(|b| b.get("type")).and_then(Value::as_str) {
                Some("tool_use") => info.content = true,
                Some("text") => {
                    info.content = non_empty_string(block.and_then(|b| b.get("text")));
                }
                _ => {}
            }
        }
        Some("message_delta") => {
            // refusal → content_filter (the translator renders content_filter
            // as refusal for Claude clients).
            if let Some(stop_reason) = obj
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str)
            {
                info.finish_reason = Some(claude_stop_to_openai_finish(stop_reason));
            }
        }
        _ => {}
    }
    info
}

/// OpenAI Responses event (`inspectResponsesEvent`). Tool calls are
/// function_call / custom_tool_call output items or their argument deltas.
/// Terminal events: completed → stop; incomplete → content_filter
/// (no-retry) or length by incomplete_details.reason; failed → "failed"
/// (retryable).
fn inspect_responses_event(obj: &Value) -> LineInfo {
    let mut info = LineInfo::default();
    match obj.get("type").and_then(Value::as_str) {
        Some(
            "response.output_text.delta"
            | "response.function_call_arguments.delta"
            | "response.custom_tool_call_input.delta",
        ) => {
            info.content = non_empty_string(obj.get("delta"));
        }
        Some("response.output_text.done") => {
            info.content = non_empty_string(obj.get("text"));
        }
        Some("response.output_item.added" | "response.output_item.done") => {
            info.content = matches!(
                obj.get("item")
                    .and_then(|i| i.get("type"))
                    .and_then(Value::as_str),
                Some("function_call" | "custom_tool_call")
            );
        }
        Some("response.completed") => {
            info.finish_reason = Some("stop".to_string());
        }
        Some("response.incomplete") => {
            let reason = obj
                .get("response")
                .and_then(|r| r.get("incomplete_details"))
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str);
            info.finish_reason = Some(match reason {
                Some("content_filter") => "content_filter".to_string(),
                Some("max_output_tokens") => "length".to_string(),
                _ => "incomplete".to_string(),
            });
        }
        Some("response.failed") => {
            info.finish_reason = Some("failed".to_string());
        }
        _ => {}
    }
    info
}

/// Inspect one complete client-format SSE line (`data: {...}`) and report
/// whether it carries content and/or a finish reason. Non-data lines and
/// "[DONE]" carry nothing (`inspectSSELine` / `parseSSELine`).
fn inspect_sse_line(line: &str) -> LineInfo {
    let line = line.trim_start();
    // parseSSELine: first char must be 'd'; "data:" prefix, then trim.
    let Some(data) = line.strip_prefix("data:") else {
        return LineInfo::default();
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return LineInfo::default();
    }
    let Ok(parsed) = serde_json::from_str::<Value>(data) else {
        return LineInfo::default();
    };

    let has_choices_array = parsed.get("choices").map(Value::is_array).unwrap_or(false);
    if let Some(ty) = parsed.get("type").and_then(Value::as_str) {
        if !has_choices_array {
            return if ty.starts_with("response.") {
                inspect_responses_event(&parsed)
            } else {
                inspect_claude_event(&parsed)
            };
        }
    }

    // OpenAI chat chunk: choices[].delta (or .message) + finish_reason.
    let mut info = LineInfo::default();
    if let Some(choices) = parsed.get("choices").and_then(Value::as_array) {
        for choice in choices {
            // `choice.delta || choice.message` — JS `||` falls through on
            // every falsy value (null/false/0/""), not just a missing key.
            let delta = choice
                .get("delta")
                .filter(|v| !is_js_falsy(v))
                .or_else(|| choice.get("message").filter(|v| !is_js_falsy(v)))
                .cloned()
                .unwrap_or(Value::Null);
            if delta_has_client_content(&delta) {
                info.content = true;
            }
            if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
                info.finish_reason = Some(finish.to_string());
            }
        }
    }
    info
}

/// Running decision while scanning a decoded SSE buffer
/// (`scanSSEBuffer` state).
#[derive(Default)]
struct ScanState {
    saw_content: bool,
    finish_reason: Option<String>,
}

fn inspect_line_into(line: &str, state: &mut ScanState) {
    let info = inspect_sse_line(line);
    if info.content {
        state.saw_content = true;
    }
    if info.finish_reason.is_some() {
        state.finish_reason = info.finish_reason;
    }
}

/// Scan the COMPLETE lines of a byte buffer and update the running
/// decision. The trailing fragment (a line still being received) stays in
/// `pending` for the caller to prepend to the next chunk.
fn scan_sse_buffer(pending: &mut Vec<u8>, state: &mut ScanState) {
    while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
        let line_bytes: Vec<u8> = pending.drain(..=pos).collect();
        let line = String::from_utf8_lossy(&line_bytes);
        inspect_line_into(line.trim_end(), state);
    }
}

/// Scan the final fragment at end-of-stream (upstream appends "\n" so the
/// last unterminated line is still parsed).
fn scan_final_line(pending: &mut Vec<u8>, state: &mut ScanState) {
    if pending.is_empty() {
        return;
    }
    let line = String::from_utf8_lossy(pending);
    inspect_line_into(line.trim_end(), state);
    pending.clear();
}

// ---------------------------------------------------------------------------
// Stream peek + lossless reconstruction (peekStreamForContent /
// reconstructStream).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeekOutcome {
    /// A content frame was seen (or the deadline fired — fail-open).
    Content,
    /// The stream ended and no content frame was ever released.
    Empty,
}

/// Result of peeking a streaming body. `head` holds the raw bytes already
/// consumed; `body` is the still-live remainder. Rebuild with
/// [`rebuild_body`] to get a lossless stream (`reconstructStream`).
pub struct PeekedStream {
    pub outcome: PeekOutcome,
    pub timed_out: bool,
    pub finish_reason: Option<String>,
    pub head: Vec<Bytes>,
    pub trailers: Option<HeaderMap>,
    /// A read error consumed during the peek (upstream `peeked.error`).
    /// Re-surfaced as the first error of the rebuilt stream so the client
    /// sees the same failure upstream's still-locked reader would re-throw.
    pub read_error: Option<axum::Error>,
    pub body: Body,
}

/// Peek a streaming body: read frames until the first content frame
/// (→ `Content`), the stream ends (→ `Empty`/`Content` by what was seen), a
/// trailers frame arrives (end of body), a read error surfaces (→ outcome
/// reflects content seen so far; the rebuilt stream re-surfaces it), or the
/// deadline elapses (→ `Content`, fail-open: never hold a possibly-valid
/// slow stream).
pub async fn peek_stream_for_content(body: Body) -> PeekedStream {
    let mut body = body;
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(*EMPTY_PEEK_DEADLINE_MS);
    let mut head: Vec<Bytes> = Vec::new();
    let mut trailers: Option<HeaderMap> = None;
    let mut pending: Vec<u8> = Vec::new();
    let mut state = ScanState::default();
    let mut read_error: Option<axum::Error> = None;

    let outcome = loop {
        let frame = match tokio::time::timeout_at(deadline, body.frame()).await {
            Err(_) => {
                // Deadline: fail-open, commit whatever we have. The dropped
                // frame() future consumed nothing, so no chunk is lost.
                return PeekedStream {
                    outcome: PeekOutcome::Content,
                    timed_out: true,
                    finish_reason: state.finish_reason,
                    head,
                    trailers,
                    read_error: None,
                    body,
                };
            }
            Ok(None) => {
                // Stream ended.
                scan_final_line(&mut pending, &mut state);
                break if state.saw_content {
                    PeekOutcome::Content
                } else {
                    PeekOutcome::Empty
                };
            }
            Ok(Some(Err(error))) => {
                // Upstream errored mid-peek. Hand the body back so the
                // caller's reconstruction surfaces the error on read; the
                // outcome reflects whether content was already seen
                // (upstream emptyFailover.js catch arm).
                read_error = Some(error);
                break if state.saw_content {
                    PeekOutcome::Content
                } else {
                    PeekOutcome::Empty
                };
            }
            Ok(Some(Ok(frame))) => frame,
        };

        match frame.into_data() {
            Ok(bytes) => {
                head.push(bytes.clone());
                pending.extend_from_slice(&bytes);
                scan_sse_buffer(&mut pending, &mut state);
                if state.saw_content {
                    return PeekedStream {
                        outcome: PeekOutcome::Content,
                        timed_out: false,
                        finish_reason: state.finish_reason,
                        head,
                        trailers,
                        read_error: None,
                        body,
                    };
                }
            }
            Err(frame) => {
                // Trailers frame — end of body.
                if let Ok(map) = frame.into_trailers() {
                    trailers = Some(map);
                }
                scan_final_line(&mut pending, &mut state);
                break if state.saw_content {
                    PeekOutcome::Content
                } else {
                    PeekOutcome::Empty
                };
            }
        }
    };

    PeekedStream {
        outcome,
        timed_out: false,
        finish_reason: state.finish_reason,
        head,
        trailers,
        read_error,
        body,
    }
}

/// Rebuild a lossless stream: buffered head bytes, then trailers (if the
/// peek consumed them), then whatever remains from the live body — the
/// `reconstructStream` equivalent.
fn rebuild_body(
    head: Vec<Bytes>,
    trailers: Option<HeaderMap>,
    read_error: Option<axum::Error>,
    body: Body,
) -> Body {
    let stream = async_stream::stream! {
        for chunk in head {
            yield Ok::<Frame<Bytes>, axum::Error>(Frame::data(chunk));
        }
        if let Some(map) = trailers {
            yield Ok(Frame::trailers(map));
        }
        // The peek consumed one failed read; replay it so the client sees
        // the same error upstream's still-locked reader would re-throw.
        if let Some(error) = read_error {
            yield Err(error);
            return;
        }
        let mut body = body;
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => yield Ok(frame),
                Err(error) => {
                    yield Err(error);
                    return;
                }
            }
        }
    };
    Body::new(StreamBody::new(stream))
}

fn rebuild_response(parts: axum::http::response::Parts, peeked: PeekedStream) -> Response {
    Response::from_parts(
        parts,
        rebuild_body(peeked.head, peeked.trailers, peeked.read_error, peeked.body),
    )
}

// ---------------------------------------------------------------------------
// Non-streaming JSON zero-content check (combo.js isZeroContentResponse).
// ---------------------------------------------------------------------------

/// Detect whether a 2xx JSON response contains zero usable content
/// (e.g. empty assistant message with in_progress/incomplete) so combo
/// fallback can continue to the next model.
pub fn is_zero_content_response(data: &Value) -> bool {
    if !data.is_object() {
        return false;
    }

    // OpenAI chat format
    if let Some(choices) = data.get("choices").and_then(Value::as_array) {
        if choices.is_empty() {
            return true;
        }
        let Some(choice) = choices.first() else {
            return true;
        };

        // `choice.message || choice.delta` — JS `||` falls through on every
        // falsy value, not just a missing key.
        let msg = choice
            .get("message")
            .filter(|v| !is_js_falsy(v))
            .or_else(|| choice.get("delta").filter(|v| !is_js_falsy(v)));
        let content = msg.and_then(|m| m.get("content"));
        let has_content = match content {
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            _ => false,
        };
        // Boolean(msg?.function_call) — JS truthiness: null/false/0/"" are
        // all falsy; objects, non-empty strings and non-zero numbers truthy.
        let has_tool_calls = msg.is_some_and(|m| {
            m.get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
                || m.get("function_call").is_some_and(|f| match f {
                    Value::Null | Value::Bool(false) => false,
                    Value::Number(n) => n.as_f64() != Some(0.0),
                    Value::String(s) => !s.is_empty(),
                    _ => true,
                })
        });
        let has_reasoning = msg
            .and_then(|m| m.get("reasoning_content"))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());

        let finish = choice.get("finish_reason").and_then(Value::as_str);
        let is_normal_finish = matches!(
            finish,
            Some("stop" | "length" | "tool_calls" | "function_call")
        );

        if !has_content
            && !has_tool_calls
            && !has_reasoning
            && (!is_normal_finish || finish == Some("in_progress") || finish == Some("incomplete"))
        {
            return true;
        }
    }

    // Anthropic messages format
    if let Some(content) = data.get("content").and_then(Value::as_array) {
        let has_content = content.iter().any(|c| {
            (c.get("type").and_then(Value::as_str) == Some("text")
                && c.get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|t| !t.trim().is_empty()))
                || c.get("type").and_then(Value::as_str) == Some("tool_use")
        });
        let stop_reason = data.get("stop_reason").and_then(Value::as_str);
        if !has_content
            && (stop_reason.is_none()
                || stop_reason == Some("in_progress")
                || stop_reason == Some("incomplete"))
        {
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Combo entry point — peekComboResponse + the zero-content JSON arm of
// handleComboChat's `if (result.ok)` block.
// ---------------------------------------------------------------------------

/// Inspect a 2xx combo member response for usable content
/// (`peekComboResponse` + the `!peeked` JSON arm, combo.js:482-549).
///
/// * `text/event-stream` → peek the stream for the first content frame.
///   Content → `Accept` the losslessly-rebuilt stream; empty → `Empty`
///   (the iterator returns it as-is on the last member / no-retry finish,
///   else fails over). A peek infrastructure error degrades to an empty
///   `data: [DONE]` SSE response, exactly like upstream.
/// * non-SSE `application/json` on a NON-LAST member → parse and run
///   [`is_zero_content_response`]; zero → `Empty` with the "Zero usable
///   content" synthetic failure (status 200, like upstream's `lastStatus`).
/// * anything else → `Accept` untouched.
pub async fn inspect_combo_response(response: Response, is_last: bool) -> ComboOkVerdict<Response> {
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    if content_type.contains("text/event-stream") {
        let (parts, body) = response.into_parts();
        let peeked = peek_stream_for_content(body).await;
        let outcome = peeked.outcome;
        let finish_reason = peeked.finish_reason.clone();
        let rebuilt = rebuild_response(parts, peeked);
        return match outcome {
            PeekOutcome::Content => ComboOkVerdict::Accept(rebuilt),
            PeekOutcome::Empty => ComboOkVerdict::Empty {
                result: rebuilt,
                failure: ComboAttemptError::new(
                    502,
                    format!(
                        "empty generation ({})",
                        finish_reason.as_deref().unwrap_or("no-content")
                    ),
                ),
                finish_reason,
            },
        };
    }

    // Non-streaming (or non-SSE) response — the zero-content JSON check
    // still applies on a non-final seat (combo.js:499-518).
    if !is_last && content_type.contains("application/json") {
        let (parts, body) = response.into_parts();
        return match body.collect().await {
            Ok(collected) => {
                let bytes = collected.to_bytes();
                let zero = serde_json::from_slice::<Value>(&bytes)
                    .map(|v| is_zero_content_response(&v))
                    .unwrap_or(false);
                let rebuilt = Response::from_parts(parts, Body::from(bytes));
                if zero {
                    ComboOkVerdict::Empty {
                        result: rebuilt,
                        failure: ComboAttemptError::new(200, "Zero usable content"),
                        finish_reason: None,
                    }
                } else {
                    ComboOkVerdict::Accept(rebuilt)
                }
            }
            Err(_) => {
                // The body was consumed by the failed collect; degrade to an
                // empty body rather than lose the response entirely.
                ComboOkVerdict::Accept(Response::from_parts(parts, Body::empty()))
            }
        };
    }

    ComboOkVerdict::Accept(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    fn sse_response(body: &'static str) -> Response {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(body))
            .unwrap()
    }

    async fn collect_string(body: Body) -> String {
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // --- isNoRetryFinish ---

    #[test]
    fn no_retry_finish_only_content_filter() {
        assert!(is_no_retry_finish(Some("content_filter")));
        assert!(is_no_retry_finish(Some("Content_Filter")));
        assert!(!is_no_retry_finish(Some("stop")));
        assert!(!is_no_retry_finish(Some("length")));
        assert!(!is_no_retry_finish(None));
    }

    // --- isZeroContentResponse ---

    #[test]
    fn zero_content_openai_empty_choices() {
        assert!(is_zero_content_response(
            &serde_json::json!({"choices": []})
        ));
    }

    #[test]
    fn zero_content_openai_empty_message_in_progress() {
        let data = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": ""},
                         "finish_reason": "in_progress"}]
        });
        assert!(is_zero_content_response(&data));
    }

    #[test]
    fn usable_openai_reasoning_only_is_not_zero() {
        // Upstream asymmetry: the non-stream JSON check DOES count
        // reasoning_content as usable content (`hasReasoning` in
        // isZeroContentResponse) — only the SSE peek excludes reasoning.
        // A reasoning-only JSON completion is accepted, not failed over.
        let data = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "",
                                     "reasoning_content": "thinking..."},
                         "finish_reason": "stop"}]
        });
        assert!(!is_zero_content_response(&data));
    }

    #[test]
    fn usable_openai_content_and_tool_calls() {
        let text = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "hi"},
                         "finish_reason": "stop"}]
        });
        assert!(!is_zero_content_response(&text));
        let tools = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "",
                                     "tool_calls": [{"id": "1"}]},
                         "finish_reason": "tool_calls"}]
        });
        assert!(!is_zero_content_response(&tools));
        // A normal finish with empty content is NOT zero-content either —
        // upstream only fails over on abnormal finishes here.
        let normal_empty = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": ""},
                         "finish_reason": "stop"}]
        });
        assert!(!is_zero_content_response(&normal_empty));
    }

    #[test]
    fn zero_content_claude_empty_blocks() {
        // Upstream isZeroContentResponse Anthropic arm: empty content only
        // fails over on an ABSENT or abnormal stop_reason — a normal
        // "end_turn" empty message is accepted, same as OpenAI "stop".
        let data = serde_json::json!({
            "content": [], "stop_reason": "end_turn"
        });
        assert!(!is_zero_content_response(&data));
        let data2 = serde_json::json!({
            "content": [{"type": "text", "text": "  "}], "stop_reason": "incomplete"
        });
        assert!(is_zero_content_response(&data2));
        let usable = serde_json::json!({
            "content": [{"type": "text", "text": "answer"}], "stop_reason": "end_turn"
        });
        assert!(!is_zero_content_response(&usable));
    }

    // --- SSE peek ---

    #[tokio::test]
    async fn sse_with_content_delta_is_accepted_and_lossless() {
        let wire = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let verdict = inspect_combo_response(sse_response(wire), false).await;
        let ComboOkVerdict::Accept(response) = verdict else {
            panic!("content stream must be accepted");
        };
        // Lossless: every byte the upstream sent is replayed.
        assert_eq!(collect_string(response.into_body()).await, wire);
    }

    #[tokio::test]
    async fn sse_empty_generation_fails_over() {
        // Role frame + finish, never any content — the Gemini-abort shape.
        let wire = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let verdict = inspect_combo_response(sse_response(wire), false).await;
        let ComboOkVerdict::Empty {
            failure,
            finish_reason,
            ..
        } = verdict
        else {
            panic!("content-less stream must be Empty");
        };
        assert_eq!(finish_reason.as_deref(), Some("stop"));
        assert_eq!(failure.status, 502);
        assert_eq!(failure.message, "empty generation (stop)");
    }

    #[tokio::test]
    async fn sse_reasoning_only_is_empty() {
        let wire = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let verdict = inspect_combo_response(sse_response(wire), false).await;
        assert!(matches!(verdict, ComboOkVerdict::Empty { .. }));
    }

    #[tokio::test]
    async fn sse_tool_calls_count_as_content() {
        let wire = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"x\"}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let verdict = inspect_combo_response(sse_response(wire), false).await;
        assert!(matches!(verdict, ComboOkVerdict::Accept(_)));
    }

    #[tokio::test]
    async fn sse_claude_and_responses_events() {
        // Claude text_delta is content.
        let claude = concat!(
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        );
        let verdict = inspect_combo_response(sse_response(claude), false).await;
        assert!(matches!(verdict, ComboOkVerdict::Accept(_)));

        // Claude refusal maps to content_filter finish — still Empty verdict,
        // the iterator decides no-retry vs last-member.
        let refusal = concat!(
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"}}\n\n",
        );
        let verdict = inspect_combo_response(sse_response(refusal), false).await;
        let ComboOkVerdict::Empty { finish_reason, .. } = verdict else {
            panic!("refusal stream is content-less");
        };
        assert_eq!(finish_reason.as_deref(), Some("content_filter"));

        // Responses output_text.delta is content; response.completed → stop.
        let responses = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "data: {\"type\":\"response.completed\"}\n\n",
        );
        let verdict = inspect_combo_response(sse_response(responses), false).await;
        assert!(matches!(verdict, ComboOkVerdict::Accept(_)));
    }

    #[tokio::test]
    async fn json_zero_content_fails_over_on_non_last() {
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({"choices": [{"message": {"role": "assistant", "content": ""},
                                                "finish_reason": "in_progress"}]})
                .to_string(),
            ))
            .unwrap();
        let verdict = inspect_combo_response(response, false).await;
        let ComboOkVerdict::Empty { failure, .. } = verdict else {
            panic!("zero-content JSON must be Empty on a non-last seat");
        };
        assert_eq!(failure.status, 200);
        assert_eq!(failure.message, "Zero usable content");
    }

    #[tokio::test]
    async fn json_zero_content_untouched_on_last_member() {
        let wire = serde_json::json!({"choices": []}).to_string();
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(wire.clone()))
            .unwrap();
        // is_last → upstream returns the result as-is without even parsing.
        let verdict = inspect_combo_response(response, true).await;
        let ComboOkVerdict::Accept(response) = verdict else {
            panic!("last member must be accepted as-is");
        };
        assert_eq!(collect_string(response.into_body()).await, wire);
    }

    #[tokio::test]
    async fn usable_json_is_accepted_losslessly() {
        let wire = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "answer"},
                         "finish_reason": "stop"}]
        })
        .to_string();
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(wire.clone()))
            .unwrap();
        let verdict = inspect_combo_response(response, false).await;
        let ComboOkVerdict::Accept(response) = verdict else {
            panic!("usable JSON must be accepted");
        };
        assert_eq!(collect_string(response.into_body()).await, wire);
    }
}
