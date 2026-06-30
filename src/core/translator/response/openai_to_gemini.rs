//! OpenAI to Gemini response translator.
//!
//! Converts OpenAI SSE chunks (`chat.completion.chunk`) to Gemini-style
//! response structures:
//!   `{candidates:[{content:{role:"model", parts:[{text:...}]}}]}`
//!
//! Matches the registry's `ResponseTransformFn` signature:
//!   fn(chunk: &[u8], state: &mut ResponseTransformState) -> Vec<String>

use serde_json::Value;

use crate::core::translator::registry::ResponseTransformState;
use crate::core::translator::request::openai_to_gemini::DEFAULT_THINKING_AG_SIGNATURE;

pub fn openai_to_gemini_response(chunk: &[u8], state: &mut ResponseTransformState) -> Vec<String> {
    let text = String::from_utf8_lossy(chunk);
    let line = text.trim();

    if line.is_empty() {
        return vec![];
    }

    let chunk_val: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let gs = &mut state.gemini;

    // Extract identity from the first chunk
    if gs.response_id.is_empty() {
        gs.response_id = chunk_val
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        gs.model = chunk_val
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("gemini")
            .to_string();
    }

    let choice = chunk_val
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());

    let Some(choice) = choice else {
        // Usage-only chunk (no choice)
        if chunk_val.get("usage").is_some() && !gs.finish_emitted && gs.current_part_index > 0 {
            gs.finish_emitted = true;
            return emit_finish(chunk_val, gs, "STOP", false);
        }
        return vec![];
    };

    let delta = choice
        .get("delta")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));
    let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

    let mut parts: Vec<Value> = Vec::new();

    // reasoning_content -> thought block
    if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
        if !reasoning.is_empty() {
            parts.push(serde_json::json!({"thought": true, "text": reasoning}));
        }
    }

    // content -> text block
    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
            parts.push(serde_json::json!({"text": content}));
        }
    }

    // Accumulate tool_calls
    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let idx_str = idx.to_string();

            if !gs.tool_calls_accum.contains_key(&idx_str) {
                gs.tool_calls_accum.insert(
                    idx_str.clone(),
                    serde_json::json!({"id": "", "name": "", "arguments": ""}),
                );
            }

            let accum = gs.tool_calls_accum.get_mut(&idx_str).unwrap();

            if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    accum["id"] = Value::String(id.to_string());
                }
            }

            if let Some(name) = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
            {
                let existing = accum["name"].as_str().unwrap_or("");
                accum["name"] = Value::String(format!("{}{}", existing, name));
            }

            if let Some(args) = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
            {
                let existing = accum["arguments"].as_str().unwrap_or("");
                accum["arguments"] = Value::String(format!("{}{}", existing, args));
            }
        }
    }

    // Emit parts as Gemini response chunk
    if !parts.is_empty() {
        gs.current_part_index += 1;

        let mut response = serde_json::json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": parts
                }
            }],
            "modelVersion": gs.model,
            "responseId": gs.response_id
        });

        let json_str = serde_json::to_string(&response).unwrap_or_default();
        return vec![json_str];
    }

    // Handle finish_reason
    if let Some(fr) = finish_reason {
        if gs.finish_emitted {
            return vec![];
        }
        gs.finish_emitted = true;

        // Emit accumulated tool calls as functionCall parts
        let mut finish_parts: Vec<Value> = Vec::new();
        if !gs.tool_calls_accum.is_empty() {
            for idx in gs.tool_calls_accum.keys() {
                if let Some(accum) = gs.tool_calls_accum.get(idx) {
                    let name = accum.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let args_str = accum
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    let args: Value = serde_json::from_str(args_str)
                        .unwrap_or(Value::Object(serde_json::Map::new()));

                    finish_parts.push(serde_json::json!({
                        "thoughtSignature": DEFAULT_THINKING_AG_SIGNATURE,
                        "functionCall": {
                            "name": name,
                            "args": args
                        }
                    }));
                }
            }
        }

        if finish_parts.is_empty() && parts.is_empty() {
            finish_parts.push(serde_json::json!({"text": ""}));
        }

        let gemini_fr = map_finish_reason(fr);
        let mut response = serde_json::json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": finish_parts
                },
                "finishReason": gemini_fr
            }],
            "modelVersion": gs.model,
            "responseId": gs.response_id
        });

        // Attach usage metadata
        if let Some(usage) = chunk_val.get("usage") {
            attach_usage(&mut response, usage);
        }

        let json_str = serde_json::to_string(&response).unwrap_or_default();
        return vec![json_str];
    }

    // If delta has tool_calls but no content/reasoning/finish yet, hold
    // to avoid empty-message Gemini responses
    if delta.get("tool_calls").is_some() && parts.is_empty() && finish_reason.is_none() {
        return vec![];
    }

    vec![]
}

/// Map OpenAI finish_reason to Gemini finishReason constant.
fn map_finish_reason(fr: &str) -> &str {
    match fr {
        "stop" => "STOP",
        "length" | "max_tokens" => "MAX_TOKENS",
        "tool_calls" | "function_call" => "STOP",
        "content_filter" => "SAFETY",
        _ => "STOP",
    }
}

/// Attach usage metadata from an OpenAI usage object to a Gemini response.
fn attach_usage(response: &mut Value, usage: &Value) {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut usage_meta = serde_json::json!({
        "promptTokenCount": prompt_tokens,
        "candidatesTokenCount": completion_tokens,
        "totalTokenCount": total_tokens,
    });

    if let Some(details) = usage.get("completion_tokens_details") {
        if let Some(reasoning) = details.get("reasoning_tokens").and_then(|v| v.as_u64()) {
            usage_meta["thoughtsTokenCount"] = Value::Number(reasoning.into());
        }
    }
    if let Some(details) = usage.get("prompt_tokens_details") {
        if let Some(cached) = details.get("cached_tokens").and_then(|v| v.as_u64()) {
            usage_meta["cachedContentTokenCount"] = Value::Number(cached.into());
        }
    }

    response["usageMetadata"] = usage_meta;
}

/// Emit finish chunk for usage-only final chunk (when finish was already
/// emitted as a separate chunk before usage).
#[allow(dead_code)]
fn emit_finish(
    chunk_val: Value,
    gs: &mut crate::core::translator::registry::GeminiResponseState,
    gemini_fr: &str,
    _include_parts: bool,
) -> Vec<String> {
    let mut response = serde_json::json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{"text": ""}]
            },
            "finishReason": gemini_fr
        }],
        "modelVersion": gs.model,
        "responseId": gs.response_id
    });

    if let Some(usage) = chunk_val.get("usage") {
        attach_usage(&mut response, usage);
    }

    let json_str = serde_json::to_string(&response).unwrap_or_default();
    vec![json_str]
}
