use serde_json::Value;
use std::collections::HashMap;

pub fn gemini_to_openai_response(chunk: &Value, state: &mut HashMap<String, Value>) -> Vec<Value> {
    let mut results = Vec::new();

    let response = chunk.get("response").unwrap_or(chunk);
    let Some(candidates) = response.get("candidates").and_then(|v| v.as_array()) else {
        return results;
    };
    let Some(candidate) = candidates.first() else {
        return results;
    };

    if !state.contains_key("messageId") {
        let msg_id = response
            .get("responseId")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let model = response
            .get("modelVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("gemini")
            .to_string();
        state.insert("messageId".to_string(), Value::String(msg_id.clone()));
        state.insert("model".to_string(), Value::String(model.clone()));
        state.insert("functionIndex".to_string(), Value::Number(0.into()));

        results.push(serde_json::json!({
            "id": format!("chatcmpl-{}", msg_id),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant" },
                "finish_reason": null
            }]
        }));
    }

    let msg_id = state
        .get("messageId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gemini")
        .to_string();
    let mut func_idx = state
        .get("functionIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    if let Some(content) = candidate.get("content") {
        if let Some(parts) = content.get("parts").and_then(|v| v.as_array()) {
            for part in parts {
                let has_thought_sig = part
                    .get("thoughtSignature")
                    .or_else(|| part.get("thought_signature"))
                    .is_some();
                let is_thought = part
                    .get("thought")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if has_thought_sig {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            let delta_key = if is_thought {
                                "reasoning_content"
                            } else {
                                "content"
                            };
                            let mut delta = serde_json::Map::new();
                            delta.insert(delta_key.to_string(), Value::String(text.to_string()));
                            results.push(serde_json::json!({
                                "id": format!("chatcmpl-{}", msg_id),
                                "object": "chat.completion.chunk",
                                "created": chrono::Utc::now().timestamp(),
                                "model": model,
                                "choices": [{
                                    "index": 0,
                                    "delta": delta,
                                    "finish_reason": null
                                }]
                            }));
                        }
                    }

                    if let Some(func_call) = part.get("functionCall") {
                        let raw_name = func_call.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let fc_args = func_call.get("args").cloned().unwrap_or(Value::Null);
                        let tool_call_id = format!(
                            "{}-{}-{}",
                            raw_name,
                            chrono::Utc::now().timestamp_millis(),
                            func_idx
                        );
                        let tool_call = serde_json::json!({
                            "id": tool_call_id,
                            "index": func_idx,
                            "type": "function",
                            "function": {
                                "name": raw_name,
                                "arguments": serde_json::to_string(&fc_args).unwrap_or_else(|_| "{}".to_string())
                            }
                        });
                        func_idx += 1;
                        results.push(serde_json::json!({
                            "id": format!("chatcmpl-{}", msg_id),
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": { "tool_calls": [tool_call] },
                                "finish_reason": null
                            }]
                        }));
                    }
                    continue;
                }

                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        results.push(serde_json::json!({
                            "id": format!("chatcmpl-{}", msg_id),
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": { "content": text },
                                "finish_reason": null
                            }]
                        }));
                    }
                }

                if let Some(func_call) = part.get("functionCall") {
                    let raw_name = func_call.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let fc_args = func_call.get("args").cloned().unwrap_or(Value::Null);
                    let tool_call_id = format!(
                        "{}-{}-{}",
                        raw_name,
                        chrono::Utc::now().timestamp_millis(),
                        func_idx
                    );
                    let tool_call = serde_json::json!({
                        "id": tool_call_id,
                        "index": func_idx,
                        "type": "function",
                        "function": {
                            "name": raw_name,
                            "arguments": serde_json::to_string(&fc_args).unwrap_or_else(|_| "{}".to_string())
                        }
                    });
                    func_idx += 1;
                    results.push(serde_json::json!({
                        "id": format!("chatcmpl-{}", msg_id),
                        "object": "chat.completion.chunk",
                        "created": chrono::Utc::now().timestamp(),
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": { "tool_calls": [tool_call] },
                            "finish_reason": null
                        }]
                    }));
                }
            }
        }
    }

    state.insert("functionIndex".to_string(), Value::Number(func_idx.into()));

    if let Some(finish_reason) = candidate.get("finishReason").and_then(|v| v.as_str()) {
        let mut fr = finish_reason.to_lowercase();
        if fr == "stop" && func_idx > 0 {
            fr = "tool_calls".to_string();
        }
        let mut final_chunk = serde_json::json!({
            "id": format!("chatcmpl-{}", msg_id),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": fr
            }]
        });

        if let Some(usage_meta) = response
            .get("usageMetadata")
            .or_else(|| chunk.get("usageMetadata"))
        {
            if let Some(usage_obj) = usage_meta.as_object() {
                let prompt_tokens = usage_obj
                    .get("promptTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let thoughts_tokens = usage_obj
                    .get("thoughtsTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let mut candidates_tokens = usage_obj
                    .get("candidatesTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let total_tokens = usage_obj
                    .get("totalTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                if candidates_tokens == 0 && total_tokens > 0 {
                    candidates_tokens = total_tokens
                        .saturating_sub(prompt_tokens)
                        .saturating_sub(thoughts_tokens);
                }
                let completion_tokens = candidates_tokens + thoughts_tokens;

                let mut usage = serde_json::json!({
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": total_tokens
                });

                if let Some(cached) = usage_obj
                    .get("cachedContentTokenCount")
                    .and_then(|v| v.as_u64())
                {
                    if cached > 0 {
                        usage["prompt_tokens_details"] =
                            serde_json::json!({ "cached_tokens": cached });
                    }
                }
                if thoughts_tokens > 0 {
                    usage["completion_tokens_details"] =
                        serde_json::json!({ "reasoning_tokens": thoughts_tokens });
                }
                final_chunk["usage"] = usage;
            }
        }

        results.push(final_chunk);
    }

    results
}
