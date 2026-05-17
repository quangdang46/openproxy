//! CommandCode to OpenAI response translator.
//!
//! Converts CommandCode NDJSON AI SDK v5 stream events to OpenAI SSE chunks.

use serde_json::Value;

fn map_finish_reason(reason: &str) -> &str {
    match reason {
        "stop" => "stop",
        "length" => "length",
        "tool-calls" | "tool_use" => "tool_calls",
        "content-filter" => "content_filter",
        "error" => "stop",
        _ => "stop",
    }
}

pub fn commandcode_to_openai_response(
    chunk: &Value,
    state: &mut serde_json::Map<String, Value>,
) -> Vec<Value> {
    if chunk.get("object").and_then(|v| v.as_str()) == Some("chat.completion.chunk") {
        return vec![chunk.clone()];
    }

    let event = if let Some(s) = chunk.as_str() {
        let line = s.trim();
        if line.is_empty() || line == "[DONE]" {
            return vec![];
        }
        let json = line.strip_prefix("data:").map(|s| s.trim()).unwrap_or(line);
        if json.is_empty() {
            return vec![];
        }
        match serde_json::from_str::<Value>(json) {
            Ok(v) => v,
            Err(_) => return vec![],
        }
    } else {
        chunk.clone()
    };

    let event_type = event.get("type").and_then(|v| v.as_str());
    if event_type.is_none() {
        return vec![];
    }
    let event_type = event_type.unwrap();

    if !state.contains_key("responseId") {
        state.insert(
            "responseId".to_string(),
            Value::String(format!(
                "chatcmpl-{}",
                chrono::Utc::now().timestamp_millis()
            )),
        );
        state.insert(
            "created".to_string(),
            Value::Number(chrono::Utc::now().timestamp().into()),
        );
        state.insert(
            "model".to_string(),
            event
                .get("model")
                .cloned()
                .unwrap_or(Value::String("commandcode".to_string())),
        );
        state.insert("chunkIndex".to_string(), Value::Number(0.into()));
        state.insert("toolIndex".to_string(), Value::Number(0.into()));
        state.insert("toolIndexById".to_string(), serde_json::json!({}));
    }

    let response_id = state
        .get("responseId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let created = state.get("created").and_then(|v| v.as_i64()).unwrap_or(0);
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("commandcode")
        .to_string();
    let chunk_index = state
        .get("chunkIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let tool_index = state.get("toolIndex").and_then(|v| v.as_u64()).unwrap_or(0);
    let tool_index_by_id = state
        .get("toolIndexById")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let make_chunk = |delta: Value, finish_reason: Option<&str>| -> Value {
        serde_json::json!({
            "id": &response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": &model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
            }]
        })
    };

    let mut out = Vec::new();

    match event_type {
        "text-delta" => {
            let text = event
                .get("text")
                .or_else(|| event.get("delta"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if text.is_empty() {
                return vec![];
            }
            let delta = if chunk_index == 0 {
                serde_json::json!({"role": "assistant", "content": text})
            } else {
                serde_json::json!({"content": text})
            };
            out.push(make_chunk(delta, None));
            state.insert(
                "chunkIndex".to_string(),
                Value::Number((chunk_index + 1).into()),
            );
        }
        "reasoning-delta" => {
            let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() {
                return vec![];
            }
            let delta = if chunk_index == 0 {
                serde_json::json!({"role": "assistant", "reasoning_content": text})
            } else {
                serde_json::json!({"reasoning_content": text})
            };
            out.push(make_chunk(delta, None));
            state.insert(
                "chunkIndex".to_string(),
                Value::Number((chunk_index + 1).into()),
            );
        }
        "tool-input-start" => {
            let id = event
                .get("id")
                .or_else(|| event.get("toolCallId"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mut tool_idx_by_id = tool_index_by_id.clone();
            let idx = if let Some(existing) = tool_idx_by_id.get(id) {
                existing.as_u64().unwrap_or(tool_index)
            } else {
                tool_idx_by_id[id] = Value::Number(tool_index.into());
                state.insert(
                    "toolIndex".to_string(),
                    Value::Number((tool_index + 1).into()),
                );
                tool_index
            };
            state.insert("toolIndexById".to_string(), tool_idx_by_id);
            let delta = if chunk_index == 0 {
                serde_json::json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "index": idx,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": event.get("toolName").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": ""
                        }
                    }]
                })
            } else {
                serde_json::json!({
                    "tool_calls": [{
                        "index": idx,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": event.get("toolName").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": ""
                        }
                    }]
                })
            };
            out.push(make_chunk(delta, None));
            state.insert(
                "chunkIndex".to_string(),
                Value::Number((chunk_index + 1).into()),
            );
        }
        "tool-input-delta" => {
            let id = event
                .get("id")
                .or_else(|| event.get("toolCallId"))
                .and_then(|v| v.as_str());
            if id.is_none() {
                return vec![];
            }
            let id = id.unwrap();
            let tool_idx_by_id = state
                .get("toolIndexById")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            if let Some(idx_val) = tool_idx_by_id.get(id) {
                let idx = idx_val.as_u64().unwrap_or(0);
                let delta = serde_json::json!({
                    "tool_calls": [{
                        "index": idx,
                        "function": {
                            "arguments": event.get("delta").or_else(|| event.get("inputTextDelta")).and_then(|v| v.as_str()).unwrap_or("")
                        }
                    }]
                });
                out.push(make_chunk(delta, None));
            }
        }
        "tool-call" => {
            let id = event.get("toolCallId").and_then(|v| v.as_str());
            if id.is_none() {
                return vec![];
            }
            let id = id.unwrap();
            let tool_idx_by_id = state
                .get("toolIndexById")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            if tool_idx_by_id.get(id).is_some() {
                return vec![];
            }

            let mut new_idx_by_id = tool_idx_by_id.clone();
            new_idx_by_id[id] = Value::Number(tool_index.into());
            state.insert(
                "toolIndex".to_string(),
                Value::Number((tool_index + 1).into()),
            );
            state.insert("toolIndexById".to_string(), new_idx_by_id);

            let args_str = if let Some(s) = event.get("input").and_then(|v| v.as_str()) {
                s.to_string()
            } else {
                serde_json::to_string(
                    &event
                        .get("input")
                        .cloned()
                        .unwrap_or(Value::Object(serde_json::Map::new())),
                )
                .unwrap_or_else(|_| "{}".to_string())
            };
            let delta = if chunk_index == 0 {
                serde_json::json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "index": tool_index,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": event.get("toolName").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": args_str
                        }
                    }]
                })
            } else {
                serde_json::json!({
                    "tool_calls": [{
                        "index": tool_index,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": event.get("toolName").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": args_str
                        }
                    }]
                })
            };
            out.push(make_chunk(delta, None));
            state.insert(
                "chunkIndex".to_string(),
                Value::Number((chunk_index + 1).into()),
            );
        }
        "finish-step" => {
            if let Some(reason) = event.get("finishReason").and_then(|v| v.as_str()) {
                state.insert(
                    "finishReason".to_string(),
                    Value::String(map_finish_reason(reason).to_string()),
                );
            }
            if let Some(usage) = event.get("usage") {
                state.insert("usage".to_string(), usage.clone());
            }
        }
        "finish" => {
            let finish_reason = event
                .get("finishReason")
                .and_then(|v| v.as_str())
                .map(map_finish_reason)
                .or_else(|| state.get("finishReason").and_then(|v| v.as_str()))
                .unwrap_or("stop");
            let mut final_chunk = make_chunk(serde_json::json!({}), Some(finish_reason));

            let total_usage = event.get("totalUsage").or_else(|| state.get("usage"));
            if let Some(usage) = total_usage {
                let input_tokens = usage
                    .get("inputTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("outputTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let total = usage
                    .get("totalTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(input_tokens + output_tokens);
                final_chunk["usage"] = serde_json::json!({
                    "prompt_tokens": input_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": total
                });
            }
            out.push(final_chunk);
        }
        "error" => {
            let err_val = event.get("error").or_else(|| event.get("message"));
            let err_str = err_val
                .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "unknown".to_string()))
                .unwrap_or_else(|| "unknown".to_string());
            out.push(make_chunk(
                serde_json::json!({"content": format!("\n\n[CommandCode error: {}]", err_str)}),
                None,
            ));
            out.push(make_chunk(serde_json::json!({}), Some("stop")));
            state.insert(
                "finishReason".to_string(),
                Value::String("stop".to_string()),
            );
        }
        _ => {}
    }

    out
}
