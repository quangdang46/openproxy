//! OpenAI SSE → Claude SSE response translator.
//!
//! Port of `open-sse/translator/response/openai-to-claude.js`. Walks the
//! OpenAI `chat.completion.chunk` stream and emits Anthropic Messages
//! API events (`message_start`, `content_block_*`, `message_delta`,
//! `message_stop`).

use serde_json::{json, Map, Value};

/// Tool-name prefix the request side uses when masking proxy-injected
/// tools to keep them distinct from caller tool names. Stripped on the
/// way back so the response surfaces the caller's original name.
const CLAUDE_OAUTH_TOOL_PREFIX: &str = "proxy_";

// ── tool-argument sanitization ─────────────────────────────────────────

/// Sanitize tool call arguments to fix bad params from non-Anthropic models.
/// Mirrors the upstream JS `sanitizeToolArgs`.
fn sanitize_tool_args(tool_name: &str, args_json: &str) -> String {
    let Ok(mut args) = serde_json::from_str::<Value>(args_json) else {
        return args_json.to_string();
    };
    let name = tool_name
        .strip_prefix(CLAUDE_OAUTH_TOOL_PREFIX)
        .unwrap_or(tool_name);
    if name == "Read" {
        sanitize_read_args(&mut args);
    }
    serde_json::to_string(&args).unwrap_or_else(|_| args_json.to_string())
}

/// Coerce and clamp `Read` tool arguments so that string-typed numeric
/// fields become numbers and out-of-range values are corrected.
fn sanitize_read_args(args: &mut Value) {
    // Coerce string limit → number
    if let Some(limit) = args.get("limit") {
        if limit.is_string() {
            if let Some(s) = limit.as_str() {
                if s.parse::<u64>().is_ok() {
                    if let Ok(n) = serde_json::from_str::<Value>(s) {
                        args["limit"] = n;
                    }
                }
            }
        }
    }
    // Coerce string offset → number (allow negative sign in pattern)
    if let Some(offset) = args.get("offset") {
        if offset.is_string() {
            if let Some(s) = offset.as_str() {
                if s.parse::<i64>().is_ok() {
                    if let Ok(n) = serde_json::from_str::<Value>(s) {
                        args["offset"] = n;
                    }
                }
            }
        }
    }

    // Clamp limit to 1..=2000
    if let Some(limit) = args.get("limit").and_then(|v| v.as_i64()) {
        if limit > 2000 {
            args["limit"] = Value::from(2000);
        } else if limit < 1 {
            if let Some(obj) = args.as_object_mut() {
                obj.remove("limit");
            }
        }
    }

    // Clamp offset >= 0
    if let Some(offset) = args.get("offset").and_then(|v| v.as_i64()) {
        if offset < 0 {
            args["offset"] = Value::from(0);
        }
    }

    // Remove `pages` if not a valid PDF pages arg
    if args.get("pages").is_some() {
        let file_path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
        let pages = args.get("pages").and_then(|v| v.as_str()).unwrap_or("");
        if !is_valid_pdf_pages_arg(file_path, pages) {
            if let Some(obj) = args.as_object_mut() {
                obj.remove("pages");
            }
        }
    }
}

/// Returns `true` when `file_path` ends with `.pdf` (case-insensitive) and
/// `pages` matches the pattern `\d+(-\d+)?`.
fn is_valid_pdf_pages_arg(file_path: &str, pages: &str) -> bool {
    if !file_path.to_lowercase().ends_with(".pdf") {
        return false;
    }
    if pages.is_empty() {
        return false;
    }
    // Validate pages format: digits optionally followed by -digits
    let mut parts = pages.splitn(2, '-');
    let start_valid = parts
        .next()
        .map(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false);
    if !start_valid {
        return false;
    }
    match parts.next() {
        None => true,
        Some(end) => !end.is_empty() && end.chars().all(|c| c.is_ascii_digit()),
    }
}

/// Convert one OpenAI chat-completion chunk into zero or more Claude SSE
/// events. `state` is the per-stream scratch space.
pub fn openai_to_claude_response(chunk: &Value, state: &mut Map<String, Value>) -> Vec<Value> {
    let Some(choice) = chunk.pointer("/choices/0") else {
        return vec![];
    };

    let mut results: Vec<Value> = Vec::new();

    // ── usage tracking ─────────────────────────────────────────────────
    if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
        let prompt_tokens = usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output_tokens = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read = usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_create = usage
            .pointer("/prompt_tokens_details/cache_creation_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let input_tokens = prompt_tokens
            .saturating_sub(cache_read)
            .saturating_sub(cache_create);

        let mut tracked = json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        });
        if cache_read > 0 {
            tracked["cache_read_input_tokens"] = Value::from(cache_read);
        }
        if cache_create > 0 {
            tracked["cache_creation_input_tokens"] = Value::from(cache_create);
        }
        state.insert("usage".into(), tracked);
    }

    // ── first chunk → emit message_start ───────────────────────────────
    let already_started = state
        .get("messageStartSent")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !already_started {
        state.insert("messageStartSent".into(), Value::Bool(true));

        let raw_id = chunk.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let stripped = raw_id.strip_prefix("chatcmpl-").unwrap_or(raw_id);
        let mut message_id = if !stripped.is_empty() {
            stripped.to_string()
        } else {
            format!("msg_{}", chrono::Utc::now().timestamp_millis())
        };
        // Replace placeholder ids with a derived value.
        if message_id == "chat" || message_id.len() < 8 {
            message_id = chunk
                .pointer("/extend_fields/requestId")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    chunk
                        .pointer("/extend_fields/traceId")
                        .and_then(|v| v.as_str())
                })
                .map(str::to_string)
                .unwrap_or_else(|| format!("msg_{}", chrono::Utc::now().timestamp_millis()));
        }
        state.insert("messageId".into(), Value::String(message_id.clone()));

        let model = chunk
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        state.insert("model".into(), Value::String(model.clone()));
        state.insert("nextBlockIndex".into(), Value::from(0u64));
        // toolCalls map: Claude block index keyed by OpenAI's tc.index.
        state
            .entry("toolCalls".to_string())
            .or_insert_with(|| Value::Object(Map::new()));

        results.push(json!({
            "type": "message_start",
            "message": {
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }
        }));
    }

    let delta = choice.get("delta");

    // ── reasoning_content → thinking block ─────────────────────────────
    let reasoning = delta
        .and_then(|d| d.get("reasoning_content"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            delta
                .and_then(|d| d.get("reasoning"))
                .and_then(|v| v.as_str())
        });
    if let Some(reasoning) = reasoning {
        if !reasoning.is_empty() {
            stop_text_block(state, &mut results);

            let already = state
                .get("thinkingBlockStarted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !already {
                let block_idx = next_block_index(state);
                state.insert("thinkingBlockIndex".into(), Value::from(block_idx));
                state.insert("thinkingBlockStarted".into(), Value::Bool(true));
                results.push(json!({
                    "type": "content_block_start",
                    "index": block_idx,
                    "content_block": {"type": "thinking", "thinking": ""}
                }));
            }

            let block_idx = state
                .get("thinkingBlockIndex")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            results.push(json!({
                "type": "content_block_delta",
                "index": block_idx,
                "delta": {"type": "thinking_delta", "thinking": reasoning}
            }));
        }
    }

    // ── content → text block ───────────────────────────────────────────
    if let Some(content) = delta
        .and_then(|d| d.get("content"))
        .and_then(|v| v.as_str())
    {
        if !content.is_empty() {
            stop_thinking_block(state, &mut results);

            let already = state
                .get("textBlockStarted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !already {
                let block_idx = next_block_index(state);
                state.insert("textBlockIndex".into(), Value::from(block_idx));
                state.insert("textBlockStarted".into(), Value::Bool(true));
                state.insert("textBlockClosed".into(), Value::Bool(false));
                results.push(json!({
                    "type": "content_block_start",
                    "index": block_idx,
                    "content_block": {"type": "text", "text": ""}
                }));
            }

            let block_idx = state
                .get("textBlockIndex")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            results.push(json!({
                "type": "content_block_delta",
                "index": block_idx,
                "delta": {"type": "text_delta", "text": content}
            }));
        }
    }

    // ── tool_calls → tool_use blocks ───────────────────────────────────
    if let Some(tool_calls) = delta
        .and_then(|d| d.get("tool_calls"))
        .and_then(|v| v.as_array())
    {
        for tc in tool_calls {
            let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

            // First chunk for this tool: emit content_block_start.
            if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                stop_thinking_block(state, &mut results);
                stop_text_block(state, &mut results);

                let block_idx = next_block_index(state);
                let raw_name = tc
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut tool_name = raw_name.to_string();
                if let Some(rest) = tool_name.strip_prefix(CLAUDE_OAUTH_TOOL_PREFIX) {
                    tool_name = rest.to_string();
                }
                if let Some(map) = state.get_mut("toolCalls").and_then(|v| v.as_object_mut()) {
                    map.insert(
                        idx.to_string(),
                        json!({
                            "id": id,
                            "name": raw_name,
                            "blockIndex": block_idx
                        }),
                    );
                }
                results.push(json!({
                    "type": "content_block_start",
                    "index": block_idx,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": tool_name,
                        "input": {}
                    }
                }));
            }

            // Subsequent argument chunks — buffer instead of streaming so
            // we can sanitize the full args at finish time.
            if let Some(args) = tc.pointer("/function/arguments").and_then(|v| v.as_str()) {
                if !args.is_empty() {
                    let tool_info = state
                        .get("toolCalls")
                        .and_then(|v| v.as_object())
                        .and_then(|m| m.get(&idx.to_string()));
                    if tool_info.is_some() {
                        let key = format!("argBuf_{idx}");
                        let buffered = state
                            .get(&key)
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        state.insert(key, Value::String(format!("{buffered}{args}")));
                    }
                }
            }
        }
    }

    // ── finish_reason → close out blocks + message_delta + message_stop ─
    if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
        stop_thinking_block(state, &mut results);
        stop_text_block(state, &mut results);

        // Emit buffered + sanitized tool args, then close every open tool block.
        let tool_entries: Vec<(u64, String, u64)> = state
            .get("toolCalls")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        let block_idx = v.get("blockIndex").and_then(|n| n.as_u64())?;
                        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let idx: u64 = k.parse().ok()?;
                        Some((idx, name.to_string(), block_idx))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (idx, name, block_idx) in tool_entries {
            let key = format!("argBuf_{idx}");
            if let Some(buffered) = state.get(&key).and_then(|v| v.as_str()) {
                if !buffered.is_empty() {
                    let sanitized = sanitize_tool_args(&name, buffered);
                    results.push(json!({
                        "type": "content_block_delta",
                        "index": block_idx,
                        "delta": {"type": "input_json_delta", "partial_json": sanitized}
                    }));
                }
            }
            results.push(json!({"type": "content_block_stop", "index": block_idx}));
        }

        state.insert(
            "finishReason".into(),
            Value::String(finish_reason.to_string()),
        );
        let usage = state
            .get("usage")
            .cloned()
            .unwrap_or_else(|| json!({"input_tokens": 0, "output_tokens": 0}));
        results.push(json!({
            "type": "message_delta",
            "delta": {"stop_reason": convert_finish_reason(finish_reason)},
            "usage": usage
        }));
        results.push(json!({"type": "message_stop"}));
    }

    results
}

fn next_block_index(state: &mut Map<String, Value>) -> u64 {
    let next = state
        .get("nextBlockIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    state.insert("nextBlockIndex".into(), Value::from(next + 1));
    next
}

fn stop_text_block(state: &mut Map<String, Value>, results: &mut Vec<Value>) {
    let started = state
        .get("textBlockStarted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let closed = state
        .get("textBlockClosed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !started || closed {
        return;
    }
    state.insert("textBlockClosed".into(), Value::Bool(true));
    let block_idx = state
        .get("textBlockIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    results.push(json!({"type": "content_block_stop", "index": block_idx}));
    state.insert("textBlockStarted".into(), Value::Bool(false));
}

fn stop_thinking_block(state: &mut Map<String, Value>, results: &mut Vec<Value>) {
    let started = state
        .get("thinkingBlockStarted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !started {
        return;
    }
    let block_idx = state
        .get("thinkingBlockIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    results.push(json!({"type": "content_block_stop", "index": block_idx}));
    state.insert("thinkingBlockStarted".into(), Value::Bool(false));
}

fn convert_finish_reason(reason: &str) -> &'static str {
    match reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        _ => "end_turn",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: &[Value]) -> Vec<Value> {
        let mut state = Map::new();
        let mut out = Vec::new();
        for ev in events {
            out.extend(openai_to_claude_response(ev, &mut state));
        }
        out
    }

    #[test]
    fn first_chunk_emits_message_start() {
        let events = [json!({
            "id": "chatcmpl-abc12345",
            "model": "gpt-4o",
            "choices": [{"index": 0, "delta": {"role": "assistant"}}]
        })];
        let out = run(&events);
        assert_eq!(out[0]["type"], "message_start");
        assert_eq!(out[0]["message"]["id"], "abc12345");
        assert_eq!(out[0]["message"]["model"], "gpt-4o");
    }

    #[test]
    fn text_content_creates_text_block_with_deltas() {
        let events = [
            json!({"id": "chatcmpl-x12345678", "model": "gpt-4o", "choices": [{"index": 0, "delta": {"role": "assistant"}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": "Hello"}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": " world"}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ];
        let out = run(&events);
        let types: Vec<&str> = out
            .iter()
            .map(|v| v["type"].as_str().unwrap_or(""))
            .collect();
        assert!(types.contains(&"message_start"));
        assert!(types.contains(&"content_block_start"));
        assert!(types.contains(&"content_block_delta"));
        assert!(types.contains(&"content_block_stop"));
        assert!(types.contains(&"message_delta"));
        assert!(types.contains(&"message_stop"));

        let text_deltas: Vec<&str> = out
            .iter()
            .filter(|v| v["type"] == "content_block_delta")
            .filter_map(|v| v["delta"]["text"].as_str())
            .collect();
        assert_eq!(text_deltas, vec!["Hello", " world"]);
    }

    #[test]
    fn reasoning_content_creates_thinking_block() {
        let events = [
            json!({"id": "chatcmpl-a", "model": "gpt-5", "choices": [{"index": 0, "delta": {"reasoning_content": "thinking..."}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": "answer"}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ];
        let out = run(&events);
        // Find the thinking block start.
        let thinking_start = out
            .iter()
            .find(|v| {
                v["type"] == "content_block_start" && v["content_block"]["type"] == "thinking"
            })
            .expect("thinking block_start");
        assert_eq!(thinking_start["index"], 0);

        // Then the thinking_delta with the reasoning text.
        let thinking_delta = out
            .iter()
            .find(|v| v["type"] == "content_block_delta" && v["delta"]["type"] == "thinking_delta")
            .expect("thinking_delta");
        assert_eq!(thinking_delta["delta"]["thinking"], "thinking...");

        // Text block should come AFTER thinking block (separate index).
        let text_start = out
            .iter()
            .find(|v| v["type"] == "content_block_start" && v["content_block"]["type"] == "text")
            .expect("text block_start");
        assert_eq!(text_start["index"], 1);
    }

    #[test]
    fn tool_calls_emit_tool_use_blocks() {
        let events = [
            json!({"id": "chatcmpl-a", "model": "gpt-5", "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_xyz",
                        "type": "function",
                        "function": {"name": "WebSearch", "arguments": ""}
                    }]
                }
            }]}),
            json!({"choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 0, "function": {"arguments": "{\"q\":\"hi\"}"}}]
            }}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        ];
        let out = run(&events);

        let tool_start = out
            .iter()
            .find(|v| {
                v["type"] == "content_block_start" && v["content_block"]["type"] == "tool_use"
            })
            .expect("tool_use start");
        assert_eq!(tool_start["content_block"]["id"], "call_xyz");
        assert_eq!(tool_start["content_block"]["name"], "WebSearch");

        let json_delta = out
            .iter()
            .find(|v| {
                v["type"] == "content_block_delta" && v["delta"]["type"] == "input_json_delta"
            })
            .expect("input_json_delta");
        assert_eq!(json_delta["delta"]["partial_json"], "{\"q\":\"hi\"}");

        let final_delta = out
            .iter()
            .find(|v| v["type"] == "message_delta")
            .expect("message_delta");
        assert_eq!(final_delta["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn proxy_prefix_is_stripped_from_tool_name() {
        let events = [json!({
            "id": "chatcmpl-a", "model": "gpt-5",
            "choices": [{"index": 0, "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_y",
                    "function": {"name": "proxy_my_tool", "arguments": ""}
                }]
            }}]
        })];
        let out = run(&events);
        let tool_start = out
            .iter()
            .find(|v| {
                v["type"] == "content_block_start" && v["content_block"]["type"] == "tool_use"
            })
            .expect("tool start");
        assert_eq!(tool_start["content_block"]["name"], "my_tool");
    }

    // ── sanitize_tool_args tests ────────────────────────────────────────

    #[test]
    fn sanitize_returns_original_on_invalid_json() {
        let result = sanitize_tool_args("Read", "not json");
        assert_eq!(result, "not json");
    }

    #[test]
    fn sanitize_read_coerces_string_limit_to_number() {
        let result = sanitize_tool_args("Read", r#"{"limit":"50","file_path":"/tmp/f.txt"}"#);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["limit"], 50);
    }

    #[test]
    fn sanitize_read_coerces_string_offset_to_number() {
        let result = sanitize_tool_args("Read", r#"{"offset":"100","file_path":"/tmp/f.txt"}"#);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["offset"], 100);
    }

    #[test]
    fn sanitize_read_clamps_limit_over_2000() {
        let result = sanitize_tool_args("Read", r#"{"limit":5000,"file_path":"/tmp/f.txt"}"#);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["limit"], 2000);
    }

    #[test]
    fn sanitize_read_removes_limit_below_1() {
        let result = sanitize_tool_args("Read", r#"{"limit":0,"file_path":"/tmp/f.txt"}"#);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("limit").is_none());
    }

    #[test]
    fn sanitize_read_clamps_negative_offset_to_zero() {
        let result = sanitize_tool_args("Read", r#"{"offset":-5,"file_path":"/tmp/f.txt"}"#);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["offset"], 0);
    }

    #[test]
    fn sanitize_read_removes_pages_for_non_pdf() {
        let result = sanitize_tool_args(
            "Read",
            r#"{"file_path":"/tmp/f.txt","pages":"1-3"}"#,
        );
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("pages").is_none());
    }

    #[test]
    fn sanitize_read_keeps_valid_pdf_pages() {
        let result = sanitize_tool_args(
            "Read",
            r#"{"file_path":"/tmp/f.pdf","pages":"1-3"}"#,
        );
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["pages"], "1-3");
    }

    #[test]
    fn sanitize_read_keeps_single_page_number() {
        let result = sanitize_tool_args(
            "Read",
            r#"{"file_path":"/tmp/doc.PDF","pages":"5"}"#,
        );
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["pages"], "5");
    }

    #[test]
    fn sanitize_read_removes_pages_with_invalid_format() {
        let result = sanitize_tool_args(
            "Read",
            r#"{"file_path":"/tmp/f.pdf","pages":"abc"}"#,
        );
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("pages").is_none());
    }

    #[test]
    fn sanitize_read_removes_pages_when_no_file_path() {
        let result = sanitize_tool_args("Read", r#"{"pages":"1"}"#);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("pages").is_none());
    }

    #[test]
    fn sanitize_non_read_tool_passes_through() {
        let input = r#"{"limit":"50","offset":"-3"}"#;
        let result = sanitize_tool_args("WebSearch", input);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        // Should not coerce — WebSearch is not Read
        assert_eq!(parsed["limit"], "50");
        assert_eq!(parsed["offset"], "-3");
    }

    #[test]
    fn sanitize_read_with_proxy_prefix() {
        let result = sanitize_tool_args(
            "proxy_Read",
            r#"{"limit":"50","file_path":"/tmp/f.txt"}"#,
        );
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["limit"], 50);
    }

    // ── is_valid_pdf_pages_arg tests ────────────────────────────────────

    #[test]
    fn valid_pdf_pages_single_page() {
        assert!(is_valid_pdf_pages_arg("/tmp/doc.pdf", "1"));
    }

    #[test]
    fn valid_pdf_pages_range() {
        assert!(is_valid_pdf_pages_arg("/tmp/doc.pdf", "1-5"));
    }

    #[test]
    fn valid_pdf_pages_case_insensitive_ext() {
        assert!(is_valid_pdf_pages_arg("/tmp/doc.PDF", "3"));
    }

    #[test]
    fn invalid_pages_non_pdf_extension() {
        assert!(!is_valid_pdf_pages_arg("/tmp/doc.txt", "1"));
    }

    #[test]
    fn invalid_pages_empty_string() {
        assert!(!is_valid_pdf_pages_arg("/tmp/doc.pdf", ""));
    }

    #[test]
    fn invalid_pages_letters() {
        assert!(!is_valid_pdf_pages_arg("/tmp/doc.pdf", "abc"));
    }

    #[test]
    fn invalid_pages_no_file_path() {
        assert!(!is_valid_pdf_pages_arg("", "1"));
    }

    // ── integration: tool args are buffered and sanitized at finish ─────

    #[test]
    fn tool_call_read_limit_sanitized_at_finish() {
        let events = [
            json!({"id": "chatcmpl-a", "model": "gpt-5", "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_r",
                        "type": "function",
                        "function": {"name": "Read", "arguments": ""}
                    }]
                }
            }]}),
            json!({"choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 0, "function": {"arguments": r#"{"limit":"9999","file_path":"/tmp/f.txt"}"#}}]
            }}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        ];
        let out = run(&events);

        let json_delta = out
            .iter()
            .find(|v| {
                v["type"] == "content_block_delta" && v["delta"]["type"] == "input_json_delta"
            })
            .expect("input_json_delta");
        let partial: Value =
            serde_json::from_str(json_delta["delta"]["partial_json"].as_str().unwrap()).unwrap();
        assert_eq!(partial["limit"], 2000);
    }

    #[test]
    fn tool_call_read_pages_removed_for_non_pdf() {
        let events = [
            json!({"id": "chatcmpl-a", "model": "gpt-5", "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_p",
                        "type": "function",
                        "function": {"name": "Read", "arguments": ""}
                    }]
                }
            }]}),
            json!({"choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 0, "function": {"arguments": r#"{"file_path":"/tmp/f.txt","pages":"1-3"}"#}}]
            }}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        ];
        let out = run(&events);

        let json_delta = out
            .iter()
            .find(|v| {
                v["type"] == "content_block_delta" && v["delta"]["type"] == "input_json_delta"
            })
            .expect("input_json_delta");
        let partial: Value =
            serde_json::from_str(json_delta["delta"]["partial_json"].as_str().unwrap()).unwrap();
        assert!(partial.get("pages").is_none());
    }
}
