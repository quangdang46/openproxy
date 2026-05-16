//! Claude SSE → OpenAI SSE response translator.
//!
//! Port of `open-sse/translator/response/claude-to-openai.js`. Walks the
//! Anthropic Messages API streaming events (`message_start`,
//! `content_block_*`, `message_delta`, `message_stop`) and emits OpenAI
//! `chat.completion.chunk` deltas.

use serde_json::{json, Map, Value};

/// Convert one Claude SSE event into zero or more OpenAI chunks.
///
/// `state` is the per-stream scratch space; the same map is threaded
/// through every call for a given stream.
pub fn claude_to_openai_response(
    chunk: &Value,
    state: &mut Map<String, Value>,
) -> Vec<Value> {
    let Some(event) = chunk.get("type").and_then(|v| v.as_str()) else {
        return vec![];
    };
    let mut results: Vec<Value> = Vec::new();

    match event {
        "message_start" => {
            let message_id = chunk
                .pointer("/message/id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("msg_{}", chrono::Utc::now().timestamp_millis()));
            state.insert("messageId".into(), Value::String(message_id));
            if let Some(model) = chunk.pointer("/message/model").and_then(|v| v.as_str()) {
                state.insert("model".into(), Value::String(model.to_string()));
            }
            state.insert("toolCallIndex".into(), Value::from(0u64));
            // Pre-allocate the toolCalls map.
            state
                .entry("toolCalls".to_string())
                .or_insert_with(|| Value::Object(Map::new()));

            results.push(make_chunk(
                state,
                json!({"role": "assistant"}),
                Value::Null,
            ));
        }

        "content_block_start" => {
            let index = chunk.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let block_type = chunk
                .pointer("/content_block/type")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match block_type {
                "server_tool_use" => {
                    state.insert("serverToolBlockIndex".into(), Value::from(index));
                }
                "text" => {
                    state.insert("textBlockStarted".into(), Value::Bool(true));
                }
                "thinking" => {
                    state.insert("inThinkingBlock".into(), Value::Bool(true));
                    state.insert("currentBlockIndex".into(), Value::from(index));
                    results.push(make_chunk(state, json!({"content": "<think>"}), Value::Null));
                }
                "tool_use" => {
                    let tool_call_index = state
                        .get("toolCallIndex")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    state.insert(
                        "toolCallIndex".into(),
                        Value::from(tool_call_index + 1),
                    );

                    let raw_name = chunk
                        .pointer("/content_block/name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // Honour the optional tool-name remap stored by
                    // claude_cloaking on the request side.
                    let mapped = state
                        .get("toolNameMap")
                        .and_then(|m| m.get(raw_name))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| raw_name.to_string());
                    let tool_id = chunk
                        .pointer("/content_block/id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let tool_call = json!({
                        "index": tool_call_index,
                        "id": tool_id,
                        "type": "function",
                        "function": {"name": mapped, "arguments": ""}
                    });

                    // Stash the in-progress tool call by Claude block index.
                    if let Some(map) = state
                        .get_mut("toolCalls")
                        .and_then(|v| v.as_object_mut())
                    {
                        map.insert(index.to_string(), tool_call.clone());
                    }

                    results.push(make_chunk(
                        state,
                        json!({"tool_calls": [tool_call]}),
                        Value::Null,
                    ));
                }
                _ => {}
            }
        }

        "content_block_delta" => {
            let index = chunk.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let server_idx = state
                .get("serverToolBlockIndex")
                .and_then(|v| v.as_u64());
            if server_idx == Some(index) {
                return results;
            }

            let delta = chunk.get("delta");
            let dtype = delta.and_then(|d| d.get("type")).and_then(|v| v.as_str());
            match dtype {
                Some("text_delta") => {
                    if let Some(text) = delta.and_then(|d| d.get("text")).and_then(|v| v.as_str())
                    {
                        if !text.is_empty() {
                            results.push(make_chunk(
                                state,
                                json!({"content": text}),
                                Value::Null,
                            ));
                        }
                    }
                }
                Some("thinking_delta") => {
                    if let Some(thinking) = delta
                        .and_then(|d| d.get("thinking"))
                        .and_then(|v| v.as_str())
                    {
                        if !thinking.is_empty() {
                            results.push(make_chunk(
                                state,
                                json!({"reasoning_content": thinking}),
                                Value::Null,
                            ));
                        }
                    }
                }
                Some("input_json_delta") => {
                    let partial = delta
                        .and_then(|d| d.get("partial_json"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if partial.is_empty() {
                        return results;
                    }
                    // Look up the in-progress tool call, append args, and
                    // emit a delta chunk.
                    let key = index.to_string();
                    let tool_call_clone = state
                        .get_mut("toolCalls")
                        .and_then(|v| v.as_object_mut())
                        .and_then(|m| m.get_mut(&key))
                        .map(|tool_call| {
                            // Accumulate args in the cached tool call.
                            if let Some(args) = tool_call
                                .pointer_mut("/function/arguments")
                                .and_then(|v| v.as_str().map(str::to_string))
                            {
                                let next = format!("{args}{partial}");
                                if let Some(slot) = tool_call
                                    .pointer_mut("/function/arguments")
                                {
                                    *slot = Value::String(next);
                                }
                            }
                            tool_call.clone()
                        });
                    if let Some(tc) = tool_call_clone {
                        let idx_num = tc.get("index").cloned().unwrap_or(Value::from(0u64));
                        let id = tc.get("id").cloned().unwrap_or(Value::String(String::new()));
                        results.push(make_chunk(
                            state,
                            json!({
                                "tool_calls": [{
                                    "index": idx_num,
                                    "id": id,
                                    "function": {"arguments": partial}
                                }]
                            }),
                            Value::Null,
                        ));
                    }
                }
                _ => {}
            }
        }

        "content_block_stop" => {
            let index = chunk.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let server_idx = state
                .get("serverToolBlockIndex")
                .and_then(|v| v.as_u64());
            if server_idx == Some(index) {
                state.insert("serverToolBlockIndex".into(), Value::from(u64::MAX));
                return results;
            }

            let in_thinking = state
                .get("inThinkingBlock")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let current_block = state
                .get("currentBlockIndex")
                .and_then(|v| v.as_u64());
            if in_thinking && current_block == Some(index) {
                results.push(make_chunk(
                    state,
                    json!({"content": "</think>"}),
                    Value::Null,
                ));
                state.insert("inThinkingBlock".into(), Value::Bool(false));
            }
            state.insert("textBlockStarted".into(), Value::Bool(false));
            state.insert("thinkingBlockStarted".into(), Value::Bool(false));
        }

        "message_delta" => {
            // Update tracked usage if present.
            if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let cache_read = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let cache_creation = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let prompt_tokens = input_tokens + cache_read + cache_creation;

                let mut tracked = json!({
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": prompt_tokens + output_tokens,
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens
                });
                if cache_read > 0 {
                    tracked["cache_read_input_tokens"] = Value::from(cache_read);
                }
                if cache_creation > 0 {
                    tracked["cache_creation_input_tokens"] = Value::from(cache_creation);
                }
                state.insert("usage".into(), tracked);
            }

            if let Some(stop_reason) = chunk
                .pointer("/delta/stop_reason")
                .and_then(|v| v.as_str())
            {
                let finish = convert_stop_reason(stop_reason);
                state.insert("finishReason".into(), Value::String(finish.to_string()));
                let mut final_chunk = make_chunk(state, json!({}), Value::String(finish.to_string()));

                if let Some(usage) = state.get("usage").cloned() {
                    let mut openai_usage = json!({
                        "prompt_tokens": usage.get("prompt_tokens").cloned().unwrap_or(Value::from(0u64)),
                        "completion_tokens": usage.get("completion_tokens").cloned().unwrap_or(Value::from(0u64)),
                        "total_tokens": usage.get("total_tokens").cloned().unwrap_or(Value::from(0u64)),
                    });
                    let cache_read = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cache_create = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if cache_read > 0 || cache_create > 0 {
                        let mut details = Map::new();
                        if cache_read > 0 {
                            details.insert(
                                "cached_tokens".into(),
                                Value::from(cache_read),
                            );
                        }
                        if cache_create > 0 {
                            details.insert(
                                "cache_creation_tokens".into(),
                                Value::from(cache_create),
                            );
                        }
                        openai_usage["prompt_tokens_details"] = Value::Object(details);
                    }
                    if let Some(obj) = final_chunk.as_object_mut() {
                        obj.insert("usage".into(), openai_usage);
                    }
                }

                results.push(final_chunk);
                state.insert("finishReasonSent".into(), Value::Bool(true));
            }
        }

        "message_stop" => {
            let already = state
                .get("finishReasonSent")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !already {
                let finish = state
                    .get("finishReason")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        let has_tools = state
                            .get("toolCalls")
                            .and_then(|v| v.as_object())
                            .map(|m| !m.is_empty())
                            .unwrap_or(false);
                        if has_tools { "tool_calls".to_string() } else { "stop".to_string() }
                    });

                let mut final_chunk = make_chunk(state, json!({}), Value::String(finish));
                if let Some(usage) = state.get("usage").cloned() {
                    let input_tokens = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let output_tokens = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if let Some(obj) = final_chunk.as_object_mut() {
                        obj.insert(
                            "usage".into(),
                            json!({
                                "prompt_tokens": input_tokens,
                                "completion_tokens": output_tokens,
                                "total_tokens": input_tokens + output_tokens,
                            }),
                        );
                    }
                }
                results.push(final_chunk);
                state.insert("finishReasonSent".into(), Value::Bool(true));
            }
        }

        _ => {}
    }

    results
}

/// Build an OpenAI `chat.completion.chunk`. `delta` is the per-event
/// delta object; `finish_reason` is `Value::Null` for non-terminal chunks.
fn make_chunk(state: &Map<String, Value>, delta: Value, finish_reason: Value) -> Value {
    let message_id = state
        .get("messageId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    json!({
        "id": format!("chatcmpl-{message_id}"),
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason
        }]
    })
}

fn convert_stop_reason(reason: &str) -> &'static str {
    match reason {
        "end_turn" => "stop",
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "stop_sequence" => "stop",
        _ => "stop",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: &[Value]) -> Vec<Value> {
        let mut state = Map::new();
        let mut out = Vec::new();
        for ev in events {
            out.extend(claude_to_openai_response(ev, &mut state));
        }
        out
    }

    #[test]
    fn message_start_emits_role_chunk() {
        let events = [json!({
            "type": "message_start",
            "message": {"id": "msg_abc12345", "model": "claude-sonnet-4.5"}
        })];
        let out = run(&events);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(out[0]["id"], "chatcmpl-msg_abc12345");
        assert_eq!(out[0]["model"], "claude-sonnet-4.5");
    }

    #[test]
    fn text_block_emits_content_deltas() {
        let events = [
            json!({"type": "message_start", "message": {"id": "m", "model": "claude"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": " world"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"input_tokens": 5, "output_tokens": 10}}),
            json!({"type": "message_stop"}),
        ];
        let out = run(&events);
        let texts: Vec<&str> = out
            .iter()
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(texts, vec!["Hello", " world"]);
        // Final chunk has finish_reason: stop and usage block.
        let final_chunk = out.last().unwrap();
        assert_eq!(final_chunk["choices"][0]["finish_reason"], "stop");
        assert_eq!(final_chunk["usage"]["prompt_tokens"], 5);
        assert_eq!(final_chunk["usage"]["completion_tokens"], 10);
    }

    #[test]
    fn thinking_block_wraps_in_think_tags() {
        let events = [
            json!({"type": "message_start", "message": {"id": "m", "model": "claude"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "reasoning..."}}),
            json!({"type": "content_block_stop", "index": 0}),
        ];
        let out = run(&events);
        assert_eq!(out[1]["choices"][0]["delta"]["content"], "<think>");
        assert_eq!(out[2]["choices"][0]["delta"]["reasoning_content"], "reasoning...");
        assert_eq!(out[3]["choices"][0]["delta"]["content"], "</think>");
    }

    #[test]
    fn tool_use_block_streams_tool_call() {
        let events = [
            json!({"type": "message_start", "message": {"id": "m", "model": "claude"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {
                "type": "tool_use", "id": "tu_1", "name": "WebSearch"
            }}),
            json!({"type": "content_block_delta", "index": 0, "delta": {
                "type": "input_json_delta", "partial_json": "{\"q\":"
            }}),
            json!({"type": "content_block_delta", "index": 0, "delta": {
                "type": "input_json_delta", "partial_json": "\"hi\"}"
            }}),
            json!({"type": "content_block_stop", "index": 0}),
        ];
        let out = run(&events);
        let first_tool = out
            .iter()
            .find(|c| c["choices"][0]["delta"]["tool_calls"].is_array())
            .unwrap();
        let call = &first_tool["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(call["id"], "tu_1");
        assert_eq!(call["function"]["name"], "WebSearch");

        // Subsequent deltas should carry the partial json.
        let arg_deltas: Vec<&str> = out
            .iter()
            .filter_map(|c| {
                c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
            })
            .collect();
        // first tool call has empty arguments + 2 partial deltas
        assert!(arg_deltas.contains(&"{\"q\":"));
        assert!(arg_deltas.contains(&"\"hi\"}"));
    }

    #[test]
    fn tool_name_map_remaps_response_name() {
        let mut state = Map::new();
        let mut tool_map = Map::new();
        tool_map.insert("WebSearch_ide".to_string(), Value::String("WebSearch".to_string()));
        state.insert("toolNameMap".to_string(), Value::Object(tool_map));

        let events = [
            json!({"type": "message_start", "message": {"id": "m", "model": "claude"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {
                "type": "tool_use", "id": "tu_1", "name": "WebSearch_ide"
            }}),
        ];
        let mut out = Vec::new();
        for ev in &events {
            out.extend(claude_to_openai_response(ev, &mut state));
        }
        let call = &out[1]["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(call["function"]["name"], "WebSearch");
    }

    #[test]
    fn server_tool_use_block_is_ignored() {
        let events = [
            json!({"type": "message_start", "message": {"id": "m", "model": "claude"}}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "server_tool_use"}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "ignored"}}),
            json!({"type": "content_block_stop", "index": 1}),
        ];
        let out = run(&events);
        // Only the message_start chunk should be produced; server tool deltas suppressed.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["choices"][0]["delta"]["role"], "assistant");
    }
}
