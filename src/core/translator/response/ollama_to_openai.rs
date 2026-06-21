use serde_json::Value;
use std::collections::HashMap;

pub fn ollama_to_openai_response(
    chunk: &Value,
    state: &mut HashMap<String, Value>,
) -> Option<Value> {
    if !state.contains_key("id") {
        let id = format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis());
        let model = chunk
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("ollama")
            .to_string();
        state.insert("id".to_string(), Value::String(id));
        state.insert("model".to_string(), Value::String(model));
        state.insert(
            "created".to_string(),
            Value::Number(chrono::Utc::now().timestamp().into()),
        );
    }

    let id = state
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("ollama")
        .to_string();
    let created = state.get("created").and_then(|v| v.as_i64()).unwrap_or(0);

    if chunk.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
        let usage = serde_json::json!({
            "prompt_tokens": chunk.get("prompt_eval_count").and_then(|v| v.as_u64()).unwrap_or(0),
            "completion_tokens": chunk.get("eval_count").and_then(|v| v.as_u64()).unwrap_or(0),
            "total_tokens": chunk.get("prompt_eval_count").and_then(|v| v.as_u64()).unwrap_or(0)
                + chunk.get("eval_count").and_then(|v| v.as_u64()).unwrap_or(0)
        });

        let mut finish_reason = "stop";
        if chunk.get("done_reason").and_then(|v| v.as_str()) == Some("tool_calls")
            || state
                .get("hadToolCalls")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            finish_reason = "tool_calls";
        }

        // Extract tool_calls from the final chunk's message, if present.
        // Ollama may emit tool_calls only in the done=true chunk.
        let mut delta = serde_json::Map::new();
        if let Some(message) = chunk.get("message") {
            if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                if !tool_calls.is_empty() {
                    state.insert("hadToolCalls".to_string(), Value::Bool(true));
                    let converted: Vec<Value> = tool_calls.iter().enumerate().map(|(i, tc)| {
                        let args = tc.get("function").and_then(|f| f.get("arguments"))
                            .map(|a| {
                                if a.is_string() {
                                    a.as_str().unwrap_or("{}").to_string()
                                } else {
                                    serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string())
                                }
                            })
                            .unwrap_or_else(|| "{}".to_string());
                        serde_json::json!({
                            "index": tc.get("function").and_then(|f| f.get("index")).and_then(|v| v.as_u64()).unwrap_or(i as u64),
                            "id": tc.get("id").and_then(|v| v.as_str()).unwrap_or(&format!("call_{}_{}", i, chrono::Utc::now().timestamp_millis())),
                            "type": "function",
                            "function": {
                                "name": tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
                                "arguments": args
                            }
                        })
                    }).collect();
                    delta.insert("tool_calls".to_string(), Value::Array(converted));
                }
            }
        }

        return Some(serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
            }],
            "usage": usage
        }));
    }

    let Some(message) = chunk.get("message") else {
        return None;
    };

    let content = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let thinking = message
        .get("thinking")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_calls = message.get("tool_calls").and_then(|v| v.as_array());

    if content.is_empty() && thinking.is_empty() && tool_calls.is_none() {
        return None;
    }

    let mut delta = serde_json::Map::new();
    if !content.is_empty() {
        delta.insert("content".to_string(), Value::String(content.to_string()));
    }
    if !thinking.is_empty() {
        delta.insert(
            "reasoning_content".to_string(),
            Value::String(thinking.to_string()),
        );
    }
    if let Some(tool_calls_arr) = tool_calls {
        state.insert("hadToolCalls".to_string(), Value::Bool(true));
        let converted: Vec<Value> = tool_calls_arr.iter().enumerate().map(|(i, tc)| {
            let args = tc.get("function").and_then(|f| f.get("arguments"))
                .map(|a| {
                    if a.is_string() {
                        a.as_str().unwrap_or("{}").to_string()
                    } else {
                        serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string())
                    }
                })
                .unwrap_or_else(|| "{}".to_string());
            serde_json::json!({
                "index": tc.get("function").and_then(|f| f.get("index")).and_then(|v| v.as_u64()).unwrap_or(i as u64),
                "id": tc.get("id").and_then(|v| v.as_str()).unwrap_or(&format!("call_{}_{}", i, chrono::Utc::now().timestamp_millis())),
                "type": "function",
                "function": {
                    "name": tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
                    "arguments": args
                }
            })
        }).collect();
        delta.insert("tool_calls".to_string(), Value::Array(converted));
    }

    Some(serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": null
        }]
    }))
}
