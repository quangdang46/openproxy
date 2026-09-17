//! OpenAI Responses API ↔ Chat Completions request translator.

use serde_json::Value;

fn normalize_tool_parameters(params: Option<&Value>) -> Value {
    match params {
        None => serde_json::json!({"type": "object", "properties": {}}),
        Some(p) => {
            if p.get("type").and_then(|v| v.as_str()) == Some("object")
                && p.get("properties").is_none()
            {
                let mut clone = p.clone();
                clone["properties"] = serde_json::json!({});
                clone
            } else {
                p.clone()
            }
        }
    }
}

/// Strict Responses upstreams reject overlong call_ids with
/// InputValidationError (#393). Mirrors `clampResponsesCallId` in
/// `open-sse/translator/formats/responsesApi.js:27-37`: non-string/empty ids
/// get a unique `call_<ms>_<seq>` fallback; overlong ids are truncated.
fn clamp_call_id(id: Option<&str>) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    match id {
        Some(s) if !s.is_empty() => {
            if s.len() > 64 {
                s[..64].to_string()
            } else {
                s.to_string()
            }
        }
        _ => {
            let n = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
            format!("call_{}_{}", chrono::Utc::now().timestamp_millis(), n)
        }
    }
}

/// Single-stringify: objects → JSON once; valid JSON strings pass through
/// untouched; anything else falls back to "{}". Mirrors
/// `coerceResponsesArguments` in responsesApi.js:42-57.
fn coerce_arguments(value: Option<&Value>) -> String {
    match value {
        None => "{}".to_string(),
        Some(Value::Null) => "{}".to_string(),
        Some(Value::String(s)) => {
            if s.is_empty() {
                return "{}".to_string();
            }
            match serde_json::from_str::<Value>(s) {
                Ok(_) => s.clone(),
                Err(_) => "{}".to_string(),
            }
        }
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
    }
}

/// function_call_output.output must be a string — never null/object.
/// Mirrors `coerceResponsesOutput` in responsesApi.js:60-77.
fn coerce_output(content: Option<&Value>) -> String {
    match content {
        None => String::new(),
        Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|c| {
                if let Some(t) = c.get("text").and_then(Value::as_str) {
                    t.to_string()
                } else {
                    serde_json::to_string(c).unwrap_or_else(|_| c.to_string())
                }
            })
            .collect::<String>(),
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| v.to_string()),
    }
}

/// JS parity (responsesApi.js coerceResponsesArguments): objects → JSON once;
/// valid JSON strings pass through; anything else falls back to "{}".
pub fn coerce_responses_arguments(value: &Value) -> String {
    match value {
        Value::Null => "{}".to_string(),
        Value::String(s) => {
            if s.is_empty() {
                return "{}".to_string();
            }
            match serde_json::from_str::<Value>(s) {
                Ok(_) => s.clone(),
                Err(_) => "{}".to_string(),
            }
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
    }
}

/// JS parity (responsesApi.js coerceResponsesOutput): output must be a string;
/// arrays join c.text (fallback: stringify each element).
pub fn coerce_responses_output(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Array(arr) => arr
            .iter()
            .map(|c| {
                if let Some(t) = c.get("text").and_then(Value::as_str) {
                    t.to_string()
                } else {
                    serde_json::to_string(c).unwrap_or_else(|_| c.to_string())
                }
            })
            .collect::<String>(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// JS parity (openai-responses.js:117-118): custom tools carry freeform
/// `input`; the Chat arguments payload is `JSON.stringify({ input })` where
/// a non-string input is first JSON-stringified.
fn custom_tool_arguments(item: &Value) -> Value {
    let raw = item
        .get("input")
        .cloned()
        .unwrap_or(Value::String(String::new()));
    let inner = match &raw {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "\"\"".to_string()),
    };
    let tool_input = serde_json::json!({ "input": inner });
    Value::String(serde_json::to_string(&tool_input).unwrap_or_else(|_| "{}".to_string()))
}

/// JS parity (openai-responses.js:196-217): expose a Responses `custom` tool
/// declaration as a Chat function with one raw `input` string. Returns None
/// for nameless tools. Records the name in `custom_names`.
fn convert_custom_tool_declaration(tool: &Value, custom_names: &mut Vec<String>) -> Option<Value> {
    let name = tool.get("name").and_then(Value::as_str)?;
    if name.trim().is_empty() {
        return None;
    }
    if !custom_names.iter().any(|n| n == name) {
        custom_names.push(name.to_string());
    }
    let hint = [
        tool.pointer("/format/syntax").and_then(Value::as_str),
        tool.pointer("/format/definition").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n");
    let mut description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !hint.is_empty() {
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description.push_str(&hint);
    }
    Some(serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Raw freeform input for this custom tool"
                    }
                },
                "required": ["input"],
                "additionalProperties": false
            }
        }
    }))
}

/// Extract reasoning text from a Responses reasoning item, mirroring the
/// upstream JS `extractReasoningText` (openai-responses.js:42-52): join
/// `item.summary[].text` with "\n"; fall back to `item.content[].text`
/// (same join); return "" when neither exists.
fn extract_reasoning_text(item: &Value) -> String {
    if let Some(summaries) = item.get("summary").and_then(Value::as_array) {
        let joined = summaries
            .iter()
            .filter_map(|s| s.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !joined.is_empty() {
            return joined;
        }
    }
    if let Some(contents) = item.get("content").and_then(Value::as_array) {
        return contents
            .iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

/// Build a Responses `{type:"reasoning", ...}` input item for a chat-format
/// assistant message, mirroring the upstream JS `buildReasoningInputItem`
/// (openai-responses.js:266-296). Returns None when neither reasoning text
/// nor encrypted content is present. summaryText priority:
/// `reasoning_content` (trimmed) > `reasoning` > `reasoning_details` joined
/// with "\n". encrypted = `encrypted_content` || `reasoning_encrypted_content`
/// || `reasoning.encrypted_content`.
fn build_reasoning_input_item(msg: &Value) -> Option<Value> {
    let summary_text = msg
        .get("reasoning_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            msg.get("reasoning")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            msg.get("reasoning_details")
                .and_then(Value::as_array)
                .map(|details| {
                    details
                        .iter()
                        .filter_map(|d| {
                            d.get("text")
                                .and_then(Value::as_str)
                                .or_else(|| d.get("content").and_then(Value::as_str))
                                .filter(|s| !s.is_empty())
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|s| !s.is_empty())
        });

    let encrypted = msg
        .get("encrypted_content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            msg.get("reasoning_encrypted_content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            msg.pointer("/reasoning/encrypted_content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });

    if summary_text.is_none() && encrypted.is_none() {
        return None;
    }

    let mut item = serde_json::json!({
        "type": "reasoning",
        "summary": [{
            "type": "summary_text",
            "text": summary_text.unwrap_or_default()
        }]
    });
    if let Some(e) = encrypted {
        item["encrypted_content"] = Value::String(e);
    }
    Some(item)
}

pub fn openai_responses_to_chat_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let input = body.get("input");
    if input.is_none() {
        return true;
    }

    let mut result = body.clone();
    result["messages"] = Value::Array(Vec::new());

    if let Some(instructions) = body.get("instructions") {
        if let Some(s) = instructions.as_str() {
            result["messages"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "role": "system", "content": s
                }));
        }
    }

    // JS parity (responsesApi.js normalizeResponsesInput): a string input
    // becomes a single user message (empty/blank → "..." placeholder), and
    // an empty array gets the same placeholder so providers never see an
    // empty messages[] (#389).
    let input_items = match input {
        Some(Value::String(text)) => {
            let text = if text.trim().is_empty() {
                "...".to_string()
            } else {
                text.clone()
            };
            vec![serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            })]
        }
        Some(Value::Array(arr)) if arr.is_empty() => {
            vec![serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "..." }]
            })]
        }
        Some(Value::Array(arr)) => arr.clone(),
        _ => return true,
    };

    let mut current_assistant_msg: Option<Value> = None;
    // Reasoning continuity: buffer reasoning text/encrypted_content across
    // input items and attach to the next assistant message (JS 33-34, 149-160).
    let mut pending_reasoning = String::new();
    let mut pending_reasoning_encrypted = String::new();
    // JS parity (openai-responses.js:38-39, 149-151): additional_tools items
    // contribute tool declarations; custom tool names are tracked in metadata.
    let mut additional_tools: Vec<Value> = Vec::new();
    let mut custom_tool_names: Vec<String> = Vec::new();

    let default_msg_type = Value::String("message".to_string());
    for item in &input_items {
        let item_type = item
            .get("type")
            .or_else(|| {
                if item.get("role").is_some() {
                    Some(&default_msg_type)
                } else {
                    None
                }
            })
            .and_then(|v| v.as_str());

        match item_type {
            Some("message") => {
                if let Some(msg) = current_assistant_msg.take() {
                    result["messages"].as_array_mut().unwrap().push(msg);
                }

                let content = if let Some(arr) = item.get("content").and_then(|v| v.as_array()) {
                    let converted: Vec<Value> = arr.iter().map(|c| {
                        match c.get("type").and_then(|v| v.as_str()) {
                            Some("input_text") | Some("output_text") => {
                                serde_json::json!({"type": "text", "text": c.get("text").and_then(|v| v.as_str()).unwrap_or("")})
                            }
                            Some("input_image") => {
                                let url = c.get("image_url").or_else(|| c.get("file_id")).and_then(|v| v.as_str()).unwrap_or("");
                                let detail = c.get("detail").and_then(|v| v.as_str()).unwrap_or("auto");
                                serde_json::json!({"type": "image_url", "image_url": {"url": url, "detail": detail}})
                            }
                            _ => c.clone()
                        }
                    }).collect();
                    Value::Array(converted)
                } else {
                    item.get("content").cloned().unwrap_or(Value::Null)
                };

                if let Some(role) = item.get("role").and_then(|v| v.as_str()) {
                    let mut msg = serde_json::json!({
                        "role": role, "content": content
                    });
                    if role == "assistant" {
                        if !pending_reasoning.is_empty() {
                            msg["reasoning_content"] = Value::String(pending_reasoning.clone());
                        }
                        if !pending_reasoning_encrypted.is_empty() {
                            msg["encrypted_content"] =
                                Value::String(pending_reasoning_encrypted.clone());
                        }
                    } else {
                        // Non-assistant messages clear the pending buffers (JS 95-98).
                        pending_reasoning.clear();
                        pending_reasoning_encrypted.clear();
                    }
                    result["messages"].as_array_mut().unwrap().push(msg);
                }
            }
            // JS parity (openai-responses.js:104-128): function_call and
            // custom_tool_call accumulate into one assistant message; custom
            // tools carry freeform `input` instead of `arguments`.
            Some("function_call") | Some("custom_tool_call") => {
                let name = item.get("name").and_then(|v| v.as_str());
                if name.is_none() || name.map(|s| s.trim().is_empty()).unwrap_or(true) {
                    continue;
                }
                if item_type == Some("custom_tool_call") {
                    if let Some(n) = item.get("name").and_then(Value::as_str) {
                        if !n.trim().is_empty() && !custom_tool_names.iter().any(|x| x == n) {
                            custom_tool_names.push(n.to_string());
                        }
                    }
                }
                if current_assistant_msg.is_none() {
                    let mut msg = serde_json::json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": []
                    });
                    if !pending_reasoning.is_empty() {
                        msg["reasoning_content"] = Value::String(pending_reasoning.clone());
                    }
                    if !pending_reasoning_encrypted.is_empty() {
                        msg["encrypted_content"] =
                            Value::String(pending_reasoning_encrypted.clone());
                    }
                    current_assistant_msg = Some(msg);
                }
                // custom_tool_call carries `input` (string or freeform) —
                // wrap as {"input": ...} JSON so chat providers accept it
                // (JS openai-responses.js:117-119).
                let arguments = if item_type == Some("custom_tool_call") {
                    if item.get("input").is_some() {
                        custom_tool_arguments(item)
                    } else {
                        Value::String(coerce_responses_arguments(
                            item.get("arguments").unwrap_or(&Value::Null),
                        ))
                    }
                } else {
                    Value::String(coerce_responses_arguments(
                        item.get("arguments").unwrap_or(&Value::Null),
                    ))
                };
                if let Some(ref mut msg) = current_assistant_msg {
                    msg["tool_calls"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "id": clamp_call_id(item.get("call_id").and_then(|v| v.as_str())),
                            "type": "function",
                            "function": {
                                "name": name.unwrap_or(""),
                                "arguments": arguments
                            }
                        }));
                }
            }
            // JS parity (openai-responses.js:129-148): both output variants
            // flush the assistant message, then any pending tool results.
            Some("function_call_output") | Some("custom_tool_call_output") => {
                if let Some(msg) = current_assistant_msg.take() {
                    result["messages"].as_array_mut().unwrap().push(msg);
                }
                // Non-assistant items clear the pending reasoning buffers (JS 95-98).
                pending_reasoning.clear();
                pending_reasoning_encrypted.clear();
                let output = coerce_responses_output(item.get("output").unwrap_or(&Value::Null));
                result["messages"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": clamp_call_id(item.get("call_id").and_then(|v| v.as_str())),
                        "content": output
                    }));
            }
            Some("additional_tools") => {
                if let Some(tools) = item.get("tools").and_then(Value::as_array) {
                    additional_tools.extend(tools.iter().cloned());
                }
                continue;
            }
            Some("reasoning") => {
                let txt = extract_reasoning_text(item);
                if !txt.is_empty() {
                    if pending_reasoning.is_empty() {
                        pending_reasoning = txt;
                    } else {
                        pending_reasoning.push('\n');
                        pending_reasoning.push_str(&txt);
                    }
                }
                if let Some(e) = item.get("encrypted_content").and_then(Value::as_str) {
                    if !e.is_empty() {
                        pending_reasoning_encrypted = e.to_string();
                    }
                }
                continue;
            }
            _ => {}
        }
    }

    if let Some(msg) = current_assistant_msg.take() {
        result["messages"].as_array_mut().unwrap().push(msg);
    }

    // JS parity (openai-responses.js:181-232): body.tools plus items-level
    // additional_tools[].tools, exposed as Chat functions; `custom` tools
    // get a single raw `input` string declaration and their names recorded
    // in translator-only `_customToolNames` metadata.
    let mut response_tools: Vec<Value> = body
        .get("tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    response_tools.extend(additional_tools);
    if !response_tools.is_empty() {
        let converted: Vec<Value> = response_tools
            .iter()
            .filter_map(|tool| {
                if tool.get("function").is_some() {
                    return Some(tool.clone());
                }
                let name = tool.get("name").and_then(|v| v.as_str());
                if name.is_none() || name.map(|s| s.trim().is_empty()).unwrap_or(true) {
                    return None;
                }
                // Hosted tools carry no name and cannot be Chat functions — skip.
                let tool_type = tool.get("type").and_then(|v| v.as_str());
                if tool_type == Some("custom") {
                    return convert_custom_tool_declaration(tool, &mut custom_tool_names);
                }
                if name.is_some() {
                    return Some(serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": name.unwrap_or(""),
                            "description": tool.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                            "parameters": normalize_tool_parameters(tool.get("parameters")),
                            "strict": tool.get("strict").cloned()
                        }
                    }));
                }
                None
            })
            .collect();
        result["tools"] = Value::Array(converted);
    }
    if !custom_tool_names.is_empty() {
        result["_customToolNames"] =
            Value::Array(custom_tool_names.into_iter().map(Value::String).collect());
    }

    let obj = result.as_object_mut().unwrap();
    obj.remove("input");
    obj.remove("instructions");
    obj.remove("include");
    obj.remove("prompt_cache_key");
    obj.remove("store");

    // responses→chat: map reasoning.effort → reasoning_effort, then drop
    // reasoning + client_metadata (9router openai-responses.js:243-247).
    if let Some(r) = obj.get("reasoning") {
        if let Some(e) = r.get("effort").and_then(Value::as_str) {
            obj.insert("reasoning_effort".into(), Value::String(e.to_string()));
        }
    }
    obj.remove("reasoning");
    obj.remove("client_metadata");

    // responses→chat: max_output_tokens → max_tokens when absent.
    if obj.get("max_tokens").is_none() {
        if let Some(v) = obj.get("max_output_tokens").cloned() {
            obj.insert("max_tokens".into(), v);
        }
    }
    obj.remove("max_output_tokens");

    *body = result;
    let _ = stream;
    true
}

pub fn chat_to_openai_responses_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    if body.get("input").is_some() {
        body["model"] = Value::String(model.to_string());
        body["stream"] = Value::Bool(true);
        return true;
    }

    let mut result = serde_json::json!({
        "model": model,
        "input": [],
        "stream": true,
        "store": false
    });

    let mut has_system = false;
    let messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for msg in &messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        if role == "system" || role == "developer" {
            if !has_system {
                result["instructions"] = msg
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
                    .into();
                has_system = true;
            }
            continue;
        }

        if role == "user" || role == "assistant" {
            // Build the reasoning input item (if any) for assistant messages
            // (JS 332-335); it is pushed immediately before the message item.
            let reasoning_item = if role == "assistant" {
                build_reasoning_input_item(msg)
            } else {
                None
            };
            let content_type = if role == "user" {
                "input_text"
            } else {
                "output_text"
            };
            let content = if let Some(s) = msg.get("content").and_then(|v| v.as_str()) {
                vec![serde_json::json!({"type": content_type, "text": s})]
            } else if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
                arr.iter().filter_map(|c| {
                    match c.get("type").and_then(|v| v.as_str()) {
                        Some("text") => Some(serde_json::json!({"type": content_type, "text": c.get("text").and_then(|v| v.as_str()).unwrap_or("")})),
                        Some("image_url") => {
                            let url = if let Some(s) = c.get("image_url").and_then(|v| v.as_str()) {
                                s.to_string()
                            } else {
                                c.get("image_url").and_then(|u| u.get("url")).and_then(|v| v.as_str()).unwrap_or("").to_string()
                            };
                            let detail = c.get("image_url").and_then(|u| u.get("detail")).and_then(|v| v.as_str()).unwrap_or("auto");
                            Some(serde_json::json!({"type": "input_image", "image_url": url, "detail": detail}))
                        }
                        Some("input_image") => Some(c.clone()),
                        _ => {
                            let text = c.get("text").or_else(|| c.get("content")).map(|v| serde_json::to_string(v).unwrap_or_else(|_| v.to_string())).unwrap_or_else(|| serde_json::to_string(c).unwrap_or_default());
                            Some(serde_json::json!({"type": content_type, "text": text}))
                        }
                    }
                }).collect()
            } else {
                vec![]
            };

            // Push the reasoning item (if any) immediately before the message
            // item; emit the message even when content is empty if a reasoning
            // item precedes it, so the pairing survives (JS 332-335).
            if !content.is_empty() || reasoning_item.is_some() {
                if let Some(ri) = reasoning_item {
                    result["input"].as_array_mut().unwrap().push(ri);
                }
                result["input"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({
                        "type": "message",
                        "role": role,
                        "content": content
                    }));
            }
        }

        if role == "assistant" {
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    // Skip nameless calls — strict Responses upstreams reject
                    // them (#444). Names are clamped to 128 chars (JS 406-408).
                    let raw_name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim();
                    if raw_name.is_empty() {
                        continue;
                    }
                    let name: String = raw_name.chars().take(128).collect();
                    result["input"].as_array_mut().unwrap().push(serde_json::json!({
                        "type": "function_call",
                        "call_id": clamp_call_id(tc.get("id").and_then(|v| v.as_str())),
                        "name": name,
                        "arguments": coerce_arguments(tc.get("function").and_then(|f| f.get("arguments")))
                    }));
                }
            }
        }

        if role == "tool" {
            let output = coerce_output(msg.get("content"));
            result["input"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": clamp_call_id(msg.get("tool_call_id").and_then(|v| v.as_str())),
                    "output": output
                }));
        }
    }

    if !has_system {
        result["instructions"] = Value::String(String::new());
    }

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        // Strict upstreams reject nameless/overlong tool declarations
        // (JS e74db4d0 openaiToOpenAIResponsesRequest tools[] mapping +
        // `.filter(Boolean)`; mirrors the tool_calls path above, #444).
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                if tool.get("type").and_then(|v| v.as_str()) == Some("function") {
                    if let Some(fn_obj) = tool.get("function") {
                        let raw_name = fn_obj
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim();
                        if raw_name.is_empty() {
                            return None;
                        }
                        let name: String = raw_name.chars().take(128).collect();
                        return Some(serde_json::json!({
                            "type": "function",
                            "name": name,
                            "description": fn_obj.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                            "parameters": normalize_tool_parameters(fn_obj.get("parameters")),
                            "strict": fn_obj.get("strict").cloned()
                        }));
                    }
                }
                Some(tool.clone())
            })
            .collect();
        result["tools"] = Value::Array(converted);
    }

    if let Some(t) = body.get("temperature") {
        result["temperature"] = t.clone();
    }
    if let Some(m) = body.get("max_tokens") {
        result["max_tokens"] = m.clone();
    }
    if let Some(m) = body.get("max_completion_tokens") {
        result["max_completion_tokens"] = m.clone();
    }
    if let Some(t) = body.get("top_p") {
        result["top_p"] = t.clone();
    }

    // Passthrough service_tier (ported from 9router v0.5.40 fix(translator):
    // pass service_tier through OpenAI→Responses conversion).
    if let Some(tier) = body.get("service_tier") {
        result["service_tier"] = tier.clone();
    }

    // reasoning / reasoning_effort → result.reasoning (9router openai-responses.js:417-423).
    // body.reasoning is copied first, then reasoning_effort OVERWRITES it with
    // { effort, summary: "auto" } — reasoning_effort wins (JS order).
    if let Some(r) = body.get("reasoning") {
        result["reasoning"] = r.clone();
    }
    if let Some(e) = body.get("reasoning_effort") {
        result["reasoning"] = serde_json::json!({ "effort": e, "summary": "auto" });
    }

    // Passthrough prompt_cache_key (9router 70ba0024 fix(translator):
    // preserve prompt_cache_key when converting chat to responses).
    if let Some(k) = body.get("prompt_cache_key") {
        result["prompt_cache_key"] = k.clone();
    }

    *body = result;
    let _ = stream;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_to_responses_maps_reasoning_effort() {
        // reasoning_effort → result.reasoning = { effort, summary: "auto" }
        let mut body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "reasoning_effort": "high"
        });
        chat_to_openai_responses_request("gpt-4", &mut body, false, None);
        let reasoning = body.get("reasoning").unwrap();
        assert_eq!(reasoning["effort"], "high");
        assert_eq!(reasoning["summary"], "auto");
    }

    #[test]
    fn test_chat_to_responses_reasoning_effort_wins_over_reasoning() {
        // JS order: body.reasoning set first, then reasoning_effort OVERWRITES.
        let mut body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "reasoning": {"effort": "low", "summary": "auto"},
            "reasoning_effort": "high"
        });
        chat_to_openai_responses_request("gpt-4", &mut body, false, None);
        let reasoning = body.get("reasoning").unwrap();
        assert_eq!(reasoning["effort"], "high", "reasoning_effort must win");
        assert_eq!(reasoning["summary"], "auto");
    }

    #[test]
    fn test_chat_to_responses_preserves_prompt_cache_key() {
        // 9router 70ba0024: prompt_cache_key must survive chat→responses.
        let mut body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "prompt_cache_key": "session-abc"
        });
        chat_to_openai_responses_request("gpt-4", &mut body, false, None);
        assert_eq!(
            body.get("prompt_cache_key").unwrap().as_str().unwrap(),
            "session-abc"
        );
    }

    #[test]
    fn test_responses_to_chat_maps_reasoning_effort_and_strips_client_metadata() {
        let mut body: Value = serde_json::json!({
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "Hi"}]}],
            "model": "gpt-4",
            "reasoning": {"effort": "medium", "summary": "auto"},
            "client_metadata": {"x": 1}
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        // reasoning.effort → reasoning_effort, reasoning removed.
        assert_eq!(
            body.get("reasoning_effort").unwrap().as_str().unwrap(),
            "medium"
        );
        assert!(body.get("reasoning").is_none());
        // client_metadata removed.
        assert!(body.get("client_metadata").is_none());
    }

    #[test]
    fn test_responses_to_chat_maps_max_output_tokens_to_max_tokens() {
        let mut body: Value = serde_json::json!({
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "Hi"}]}],
            "model": "gpt-4",
            "max_output_tokens": 4096
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        assert_eq!(body.get("max_tokens").unwrap().as_i64().unwrap(), 4096);
        assert!(body.get("max_output_tokens").is_none());
    }

    #[test]
    fn responses_reasoning_item_buffers_onto_next_assistant() {
        // JS: reasoning item summary text buffers across input items and
        // attaches to the next assistant message as reasoning_content.
        let mut body: Value = serde_json::json!({
            "input": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "hmm"}]},
                {"type": "message", "role": "assistant", "content": []}
            ],
            "model": "gpt-4"
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["reasoning_content"], "hmm");
    }

    #[test]
    fn responses_reasoning_item_buffers_across_items_with_newline_join() {
        // Multiple reasoning items join with "\n"; summary[] takes priority
        // over content[] fallback; encrypted_content is stashed.
        let mut body: Value = serde_json::json!({
            "input": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "first"}]},
                {"type": "reasoning", "content": [{"type": "summary_text", "text": "second"}]},
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "third"}], "encrypted_content": "blob"},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hi"}]}
            ],
            "model": "gpt-4"
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["reasoning_content"], "first\nsecond\nthird");
        assert_eq!(messages[0]["encrypted_content"], "blob");
    }

    #[test]
    fn responses_non_assistant_message_clears_pending_reasoning() {
        // A non-assistant message between reasoning items and the assistant
        // message clears the pending buffers (JS 95-98).
        let mut body: Value = serde_json::json!({
            "input": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "hmm"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hi"}]}
            ],
            "model": "gpt-4"
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages[1].get("reasoning_content").is_none());
        assert!(messages[1].get("encrypted_content").is_none());
    }

    #[test]
    fn responses_reasoning_attaches_to_function_call_assistant() {
        // Reasoning buffers onto the assistant message built from a
        // function_call item (JS attachPendingReasoning at function_call).
        let mut body: Value = serde_json::json!({
            "input": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "hmm"}], "encrypted_content": "blob"},
                {"type": "function_call", "call_id": "call_1", "name": "f", "arguments": "{}"}
            ],
            "model": "gpt-4"
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["reasoning_content"], "hmm");
        assert_eq!(messages[0]["encrypted_content"], "blob");
        assert_eq!(messages[0]["tool_calls"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn chat_assistant_reemits_reasoning_item() {
        // JS: buildReasoningInputItem pushes a reasoning item immediately
        // before the assistant message item.
        let mut body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": [], "reasoning_content": "hmm", "encrypted_content": "blob"}
            ]
        });
        chat_to_openai_responses_request("gpt-4", &mut body, false, None);
        let input = body.get("input").unwrap().as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["summary"][0]["type"], "summary_text");
        assert_eq!(input[0]["summary"][0]["text"], "hmm");
        assert_eq!(input[0]["encrypted_content"], "blob");
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["role"], "assistant");
    }

    #[test]
    fn chat_reasoning_priorities_and_fallbacks() {
        // summaryText priority: reasoning_content (trim) > reasoning >
        // reasoning_details joined "\n"; encrypted: encrypted_content >
        // reasoning_encrypted_content > reasoning.encrypted_content.
        let mut body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "hi"}],
                    "reasoning_content": "  top  ",
                    "reasoning": "plain",
                    "reasoning_details": [{"text": "a"}, {"content": "b"}],
                    "encrypted_content": "e1",
                    "reasoning_encrypted_content": "e2",
                    "reasoning": {"encrypted_content": "e3"}
                }
            ]
        });
        chat_to_openai_responses_request("gpt-4", &mut body, false, None);
        let input = body.get("input").unwrap().as_array().unwrap();
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(
            input[0]["summary"][0]["text"], "top",
            "reasoning_content wins and is trimmed"
        );
        assert_eq!(
            input[0]["encrypted_content"], "e1",
            "encrypted_content wins"
        );

        // Fallback: only reasoning_details + reasoning.encrypted_content.
        let mut body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "hi"}],
                    "reasoning_details": [{"text": "a"}, {"content": "b"}],
                    "reasoning": {"encrypted_content": "e3"}
                }
            ]
        });
        chat_to_openai_responses_request("gpt-4", &mut body, false, None);
        let input = body.get("input").unwrap().as_array().unwrap();
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["summary"][0]["text"], "a\nb");
        assert_eq!(input[0]["encrypted_content"], "e3");

        // No reasoning → no reasoning item before the assistant message.
        let mut body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": [{"type": "text", "text": "hi"}]}
            ]
        });
        chat_to_openai_responses_request("gpt-4", &mut body, false, None);
        let input = body.get("input").unwrap().as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
    }

    #[test]
    fn responses_string_and_empty_input_normalized() {
        // JS parity (responsesApi.js normalizeResponsesInput): string input →
        // one user message; blank string and empty array → "..." placeholder.
        let mut body: Value = serde_json::json!({"input": "hello", "model": "gpt-4"});
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");

        let mut body: Value = serde_json::json!({"input": "   ", "model": "gpt-4"});
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages[0]["role"], "user");

        let mut body: Value = serde_json::json!({"input": [], "model": "gpt-4"});
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn responses_custom_tool_items_convert() {
        // JS parity (openai-responses.js:117-127, confirmed by
        // tests/unit/openai-responses-custom-tools.test.js:63):
        // custom_tool_call `input` is wrapped as JSON {"input": ...} in
        // Chat arguments; custom_tool_call_output → tool message.
        let mut body: Value = serde_json::json!({
            "input": [
                {"type": "custom_tool_call", "call_id": "call_1", "name": "shell", "input": "ls"},
                {"type": "custom_tool_call_output", "call_id": "call_1", "output": "ok"}
            ],
            "model": "gpt-4"
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "assistant");
        // (batch2's responses_coerces_call_ids_and_outputs assertions live on
        // in the dedicated clamp/coerce tests below; this test covers custom input.)
        let args: Value = serde_json::from_str(
            messages[0]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(args, serde_json::json!({"input": "ls"}));
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["content"], "ok");
    }

    #[test]
    fn responses_custom_tool_object_input_stringified_inside_wrapper() {
        // Non-string `input` is first JSON-stringified, then wrapped:
        // input {"cmd":"ls"} → '{"input":"{\"cmd\":\"ls\"}"}'.
        let mut body: Value = serde_json::json!({
            "input": [
                {"type": "custom_tool_call", "call_id": "call_1", "name": "shell", "input": {"cmd": "ls"}}
            ],
            "model": "gpt-4"
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        let args: Value = serde_json::from_str(
            messages[0]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(args, serde_json::json!({"input": "{\"cmd\":\"ls\"}"}));
    }

    #[test]
    fn responses_additional_tools_promoted_to_chat_tools() {
        // JS parity (openai-responses.js:181-232, confirmed by
        // openai-responses-custom-tools.test.js:21-44): additional_tools
        // custom declarations merge with body.tools as Chat functions.
        let mut body: Value = serde_json::json!({
            "input": [
                {"type": "additional_tools", "role": "developer", "tools": [
                    {"type": "custom", "name": "exec", "description": "Run code",
                     "format": {"syntax": "lark", "definition": "start: /.+/"}}
                ]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Run pwd"}]}
            ],
            "tools": [{"type": "function", "name": "search", "parameters": {"type": "object", "properties": {}}}],
            "model": "gpt-4"
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let tools = body.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["function"]["name"], "search");
        assert_eq!(tools[1]["function"]["name"], "exec");
        assert_eq!(
            tools[1]["function"]["parameters"]["required"],
            serde_json::json!(["input"])
        );
        let names = body.get("_customToolNames").unwrap().as_array().unwrap();
        assert!(names.iter().any(|n| n == "exec"));
        // additional_tools items are declarations, not messages.
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert!(
            !messages
                .iter()
                .any(|m| m.get("role").and_then(Value::as_str) == Some("developer")),
            "additional_tools must not leak a developer message"
        );
    }

    #[test]
    fn coerce_helpers_match_js() {
        // coerceResponsesArguments: object → JSON once; invalid-JSON string → "{}".
        assert_eq!(
            coerce_responses_arguments(&serde_json::json!({"q": "x"})),
            "{\"q\":\"x\"}"
        );
        assert_eq!(
            coerce_responses_arguments(&Value::String("{\"q\":\"x\"}".to_string())),
            "{\"q\":\"x\"}"
        );
        assert_eq!(
            coerce_responses_arguments(&Value::String("{bad".to_string())),
            "{}"
        );
        // coerceResponsesOutput: array joins c.text.
        assert_eq!(
            coerce_responses_output(&serde_json::json!([{"text": "a"}, {"text": "b"}])),
            "ab"
        );
        assert_eq!(coerce_responses_output(&Value::Null), "");
    }

    #[test]
    fn responses_coerces_call_ids_and_outputs() {
        // clampResponsesCallId: overlong ids truncated to 64; missing ids
        // get a call_ fallback. coerceResponsesArguments: valid JSON passes
        // through, objects stringify once, garbage → "{}".
        // coerceResponsesOutput: objects stringify, null → "".
        let long_id = "x".repeat(100);
        let mut body: Value = serde_json::json!({
            "input": [
                {"type": "function_call", "call_id": long_id, "name": "f", "arguments": {"a": 1}},
                {"type": "function_call_output", "call_id": long_id, "output": {"ok": true}},
                {"type": "function_call", "name": "g", "arguments": "not-json{{{"
                },
                {"type": "function_call_output", "output": null}
            ],
            "model": "gpt-4"
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(
            messages[0]["tool_calls"][0]["id"].as_str().unwrap().len(),
            64
        );
        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["arguments"],
            "{\"a\":1}"
        );
        assert_eq!(messages[1]["content"], "{\"ok\":true}");
        assert_eq!(messages[2]["tool_calls"][0]["function"]["arguments"], "{}");
        let fallback = messages[2]["tool_calls"][0]["id"].as_str().unwrap();
        assert!(fallback.starts_with("call_"), "fallback id: {fallback}");
        assert_eq!(messages[3]["content"], "");
    }

    #[test]
    fn responses_custom_tools_become_input_functions_with_names() {
        // {type:"custom"} tools → functions with one raw `input` param and
        // names retained in _customToolNames (JS 196-198, 232); hosted tools
        // without names are dropped (JS 179-180).
        let mut body: Value = serde_json::json!({
            "input": [
                {"type": "custom_tool_call", "call_id": "c1", "name": "exec", "input": "ls"}
            ],
            "tools": [
                {"type": "custom", "name": "exec", "description": "run"},
                {"type": "request_user_input"},
                {"type": "function", "name": "f", "description": "", "parameters": {"type": "object"}}
            ],
            "model": "gpt-4"
        });
        openai_responses_to_chat_request("gpt-4", &mut body, false, None);
        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["arguments"],
            "{\"input\":\"ls\"}"
        );
        let tools = body.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), 2, "hosted tool dropped: {tools:?}");
        assert_eq!(tools[0]["function"]["name"], "exec");
        assert_eq!(
            tools[0]["function"]["parameters"]["properties"]["input"]["type"],
            "string"
        );
        let names = body.get("_customToolNames").unwrap().as_array().unwrap();
        assert!(names.iter().any(|n| n == "exec"));
    }

    #[test]
    fn chat_to_responses_clamps_call_id_and_coerces_arguments() {
        // JS 406-408: call_id clamped, nameless calls skipped, name sliced
        // to 128; arguments coerced.
        let long_name = "n".repeat(200);
        let mut body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "y".repeat(100), "type": "function",
                     "function": {"name": long_name, "arguments": {"a": 1}}},
                    {"id": "z", "type": "function",
                     "function": {"name": "  ", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "t1", "content": {"ok": true}}
            ]
        });
        chat_to_openai_responses_request("gpt-4", &mut body, false, None);
        let input = body.get("input").unwrap().as_array().unwrap();
        assert_eq!(input[0]["call_id"].as_str().unwrap().len(), 64);
        assert_eq!(input[0]["name"].as_str().unwrap().len(), 128);
        assert_eq!(input[0]["arguments"], "{\"a\":1}");
        // Nameless call skipped: only 1 function_call + 1 output.
        assert_eq!(input.len(), 2);
        assert_eq!(input[1]["output"], "{\"ok\":true}");
    }
}

#[cfg(test)]
mod tools_mapping_tests {
    use super::chat_to_openai_responses_request;
    use serde_json::{json, Value};

    fn run_tools(body: Value) -> Vec<Value> {
        let mut b = body;
        chat_to_openai_responses_request("m", &mut b, false, None);
        b.get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn nameless_tools_skipped() {
        let out = run_tools(json!({"messages": [], "tools": [
            {"type": "function", "function": {"name": "  ", "description": "x"}},
            {"type": "function", "function": {"description": "no-name"}},
            {"type": "function", "function": {"name": "ok"}},
        ]}));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["name"], json!("ok"));
    }

    #[test]
    fn long_names_clamped_to_128() {
        let long = "n".repeat(200);
        let out = run_tools(json!({"messages": [], "tools": [
            {"type": "function", "function": {"name": long}},
        ]}));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["name"].as_str().unwrap().chars().count(), 128);
    }

    #[test]
    fn non_function_tools_pass_through() {
        let out = run_tools(json!({"messages": [], "tools": [
            {"type": "custom", "name": "keep-me"},
        ]}));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["name"], json!("keep-me"));
    }
}
