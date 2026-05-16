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

/// Convert one OpenAI chat-completion chunk into zero or more Claude SSE
/// events. `state` is the per-stream scratch space.
pub fn openai_to_claude_response(
    chunk: &Value,
    state: &mut Map<String, Value>,
) -> Vec<Value> {
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
        .or_else(|| delta.and_then(|d| d.get("reasoning")).and_then(|v| v.as_str()));
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
    if let Some(content) = delta.and_then(|d| d.get("content")).and_then(|v| v.as_str()) {
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
    if let Some(tool_calls) = delta.and_then(|d| d.get("tool_calls")).and_then(|v| v.as_array()) {
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
                if let Some(map) = state
                    .get_mut("toolCalls")
                    .and_then(|v| v.as_object_mut())
                {
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

            // Subsequent argument chunks.
            if let Some(args) = tc.pointer("/function/arguments").and_then(|v| v.as_str()) {
                if !args.is_empty() {
                    let block_idx = state
                        .get("toolCalls")
                        .and_then(|v| v.as_object())
                        .and_then(|m| m.get(&idx.to_string()))
                        .and_then(|v| v.get("blockIndex"))
                        .and_then(|v| v.as_u64());
                    if let Some(block_idx) = block_idx {
                        results.push(json!({
                            "type": "content_block_delta",
                            "index": block_idx,
                            "delta": {"type": "input_json_delta", "partial_json": args}
                        }));
                    }
                }
            }
        }
    }

    // ── finish_reason → close out blocks + message_delta + message_stop ─
    if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
        stop_thinking_block(state, &mut results);
        stop_text_block(state, &mut results);

        // Close every open tool block.
        let tool_blocks: Vec<u64> = state
            .get("toolCalls")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.values()
                    .filter_map(|v| v.get("blockIndex").and_then(|n| n.as_u64()))
                    .collect()
            })
            .unwrap_or_default();
        for bi in tool_blocks {
            results.push(json!({"type": "content_block_stop", "index": bi}));
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
                v["type"] == "content_block_start"
                    && v["content_block"]["type"] == "thinking"
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
            .find(|v| {
                v["type"] == "content_block_start" && v["content_block"]["type"] == "text"
            })
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
}
