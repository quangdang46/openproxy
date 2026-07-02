use serde_json::Value;
use std::collections::HashMap;

pub fn kiro_to_openai_response(chunk: &Value, state: &mut HashMap<String, Value>) -> Option<Value> {
    if chunk.get("object").and_then(|v| v.as_str()) == Some("chat.completion.chunk")
        && chunk.get("choices").is_some()
    {
        return Some(chunk.clone());
    }

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
        state.insert("chunkIndex".to_string(), Value::Number(0usize.into()));
    }

    let response_id = state
        .get("responseId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let created = state.get("created").and_then(|v| v.as_i64()).unwrap_or(0);
    let chunk_idx = state
        .get("chunkIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("kiro")
        .to_string();

    let event_type = chunk
        .get("_eventType")
        .or_else(|| chunk.get("event"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if event_type == "assistantResponseEvent" || chunk.get("assistantResponseEvent").is_some() {
        let content = chunk
            .get("assistantResponseEvent")
            .and_then(|v| v.get("content"))
            .or_else(|| chunk.get("content"))
            .and_then(|v| v.as_str());
        let Some(content) = content else {
            return None;
        };

        let mut delta = serde_json::Map::new();
        if chunk_idx == 0 {
            delta.insert("role".to_string(), Value::String("assistant".to_string()));
        }
        delta.insert("content".to_string(), Value::String(content.to_string()));

        state.insert(
            "chunkIndex".to_string(),
            Value::Number((chunk_idx + 1).into()),
        );
        return Some(serde_json::json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": null
            }]
        }));
    }

    if event_type == "reasoningContentEvent" || chunk.get("reasoningContentEvent").is_some() {
        let reasoning = chunk.get("reasoningContentEvent").unwrap_or(chunk);
        let content = reasoning
            .get("text")
            .or_else(|| reasoning.get("content"))
            .and_then(|v| v.as_str())
            .or_else(|| chunk.get("content").and_then(|v| v.as_str()));
        let Some(content) = content else {
            return None;
        };

        let mut delta = serde_json::Map::new();
        if chunk_idx == 0 {
            delta.insert("role".to_string(), Value::String("assistant".to_string()));
        }
        delta.insert(
            "reasoning_content".to_string(),
            Value::String(content.to_string()),
        );

        state.insert(
            "chunkIndex".to_string(),
            Value::Number((chunk_idx + 1).into()),
        );
        return Some(serde_json::json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": null
            }]
        }));
    }

    if event_type == "toolUseEvent" || chunk.get("toolUseEvent").is_some() {
        let tool_use = chunk.get("toolUseEvent").unwrap_or(chunk);
        let tool_call_id = tool_use
            .get("toolUseId")
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("call_{}", chrono::Utc::now().timestamp_millis()))
            .to_string();
        let tool_name = tool_use
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let tool_input = tool_use.get("input").cloned().unwrap_or(Value::Null);

        let mut delta = serde_json::Map::new();
        if chunk_idx == 0 {
            delta.insert("role".to_string(), Value::String("assistant".to_string()));
        }
        delta.insert("tool_calls".to_string(), serde_json::json!([{
            "index": 0,
            "id": tool_call_id,
            "type": "function",
            "function": {
                "name": tool_name,
                "arguments": serde_json::to_string(&tool_input).unwrap_or_else(|_| "{}".to_string())
            }
        }]));

        state.insert(
            "chunkIndex".to_string(),
            Value::Number((chunk_idx + 1).into()),
        );
        return Some(serde_json::json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": null
            }]
        }));
    }

    if event_type == "messageStopEvent"
        || event_type == "done"
        || chunk.get("messageStopEvent").is_some()
    {
        state.insert(
            "finishReason".to_string(),
            Value::String("stop".to_string()),
        );
        let mut final_chunk = serde_json::json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        if let Some(usage) = state.get("usage") {
            final_chunk["usage"] = usage.clone();
        }
        return Some(final_chunk);
    }

    if event_type == "usageEvent" || chunk.get("usageEvent").is_some() {
        let usage = chunk.get("usageEvent").unwrap_or(chunk);
        if let Some(usage_obj) = usage.as_object() {
            let input_tokens = usage_obj
                .get("inputTokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output_tokens = usage_obj
                .get("outputTokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            state.insert(
                "usage".to_string(),
                serde_json::json!({
                    "prompt_tokens": input_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": input_tokens + output_tokens
                }),
            );
        }
        return None;
    }

    None
}

use crate::core::translator::registry::ResponseTransformState;

/// Registry-compatible streaming wrapper.
/// Signature matches `registry::ResponseTransformFn`.
pub fn kiro_to_openai_streaming(chunk: &[u8], state: &mut ResponseTransformState) -> Vec<String> {
    let val: serde_json::Value = match serde_json::from_slice(chunk) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let inner = &mut state.kiro.state;
    match kiro_to_openai_response(&val, inner) {
        Some(v) => vec![format!(
            "data: {}\n\n",
            serde_json::to_string(&v).unwrap_or_default()
        )],
        None => vec![],
    }
}
