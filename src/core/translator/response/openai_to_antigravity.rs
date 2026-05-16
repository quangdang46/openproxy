//! OpenAI to Antigravity response translator.
//!
//! Converts OpenAI SSE chunks to Antigravity Gemini-style format.

use serde_json::Value;
use std::collections::HashMap;

pub fn openai_to_antigravity_response(chunk: &Value, state: &mut serde_json::Map<String, Value>) -> Vec<Value> {
    let choice = chunk.get("choices").and_then(|v| v.as_array()).and_then(|a| a.first());
    if choice.is_none() {
        if chunk.get("usage").is_some() {
            state.insert("_usage".to_string(), chunk["usage"].clone());
        }
        return vec![];
    }
    let choice = choice.unwrap();
    let delta = choice.get("delta").cloned().unwrap_or(Value::Object(serde_json::Map::new()));
    let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

    if !state.contains_key("_toolCallAccum") {
        state.insert("_toolCallAccum".to_string(), serde_json::json!({}));
    }
    if !state.contains_key("_responseId") {
        let rid = chunk.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        state.insert("_responseId".to_string(), Value::String(rid.to_string()));
    }
    if !state.contains_key("_modelVersion") {
        let mv = chunk.get("model").and_then(|v| v.as_str()).unwrap_or("");
        state.insert("_modelVersion".to_string(), Value::String(mv.to_string()));
    }

    let mut parts: Vec<Value> = Vec::new();

    if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
        if !reasoning.is_empty() {
            parts.push(serde_json::json!({"thought": true, "text": reasoning}));
        }
    }

    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
            parts.push(serde_json::json!({"text": content}));
        }
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let accum_key = idx.to_string();
            if !state["_toolCallAccum"].get(&accum_key).is_some() {
                state["_toolCallAccum"][&accum_key] = serde_json::json!({"id": "", "name": "", "arguments": ""});
            }
            let accum = &mut state["_toolCallAccum"][&accum_key];
            if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                accum["id"] = Value::String(id.to_string());
            }
            if let Some(name) = tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()) {
                if let Some(existing) = accum["name"].as_str() {
                    accum["name"] = Value::String(format!("{}{}", existing, name));
                } else {
                    accum["name"] = Value::String(name.to_string());
                }
            }
            if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")).and_then(|v| v.as_str()) {
                if let Some(existing) = accum["arguments"].as_str() {
                    accum["arguments"] = Value::String(format!("{}{}", existing, args));
                } else {
                    accum["arguments"] = Value::String(args.to_string());
                }
            }
        }
        if parts.is_empty() && finish_reason.is_none() {
            return vec![];
        }
    }

    if finish_reason.is_some() {
        if let Some(accum_obj) = state["_toolCallAccum"].as_object() {
            let indices: Vec<String> = accum_obj.keys().cloned().collect();
            for idx in indices {
                let accum = &state["_toolCallAccum"][&idx];
                let args_str = accum.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                let args: Value = serde_json::from_str(args_str).unwrap_or(Value::Object(serde_json::Map::new()));
                let name = accum.get("name").and_then(|v| v.as_str()).unwrap_or("");

                let tool_name_map = state.get("toolNameMap");
                let original_name = if let Some(map) = tool_name_map.and_then(|v| v.as_object()) {
                    map.get(name).and_then(|v| v.as_str()).unwrap_or(name)
                } else {
                    name
                };

                parts.push(serde_json::json!({
                    "functionCall": {
                        "name": original_name,
                        "args": args
                    }
                }));
            }
        }
    }

    if parts.is_empty() && finish_reason.is_none() {
        return vec![];
    }

    if parts.is_empty() && finish_reason.is_some() {
        parts.push(serde_json::json!({"text": ""}));
    }

    let fr_map: HashMap<&str, &str> = [
        ("stop", "STOP"),
        ("length", "MAX_TOKENS"),
        ("tool_calls", "STOP"),
        ("content_filter", "SAFETY"),
    ].into_iter().collect();

    let candidate = {
        let mut c = serde_json::json!({
            "content": {"role": "model", "parts": parts}
        });
        if let Some(fr) = finish_reason {
            c["finishReason"] = Value::String(fr_map.get(fr).unwrap_or(&"STOP").to_string());
        }
        c
    };

    let mut response = serde_json::json!({
        "candidates": [candidate],
        "modelVersion": state.get("_modelVersion").and_then(|v| v.as_str()).unwrap_or(""),
        "responseId": state.get("_responseId").and_then(|v| v.as_str()).unwrap_or("")
    });

    let usage = chunk.get("usage").or_else(|| state.get("_usage"));
    if let Some(u) = usage {
        let prompt_tokens = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let completion_tokens = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let total_tokens = u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let mut usage_meta = serde_json::json!({
            "promptTokenCount": prompt_tokens,
            "candidatesTokenCount": completion_tokens,
            "totalTokenCount": total_tokens
        });
        if let Some(details) = u.get("completion_tokens_details") {
            if let Some(reasoning) = details.get("reasoning_tokens").and_then(|v| v.as_u64()) {
                usage_meta["thoughtsTokenCount"] = Value::Number(reasoning.into());
            }
        }
        if let Some(details) = u.get("prompt_tokens_details") {
            if let Some(cached) = details.get("cached_tokens").and_then(|v| v.as_u64()) {
                usage_meta["cachedContentTokenCount"] = Value::Number(cached.into());
            }
        }
        response["usageMetadata"] = usage_meta;
    }

    vec![response]
}
