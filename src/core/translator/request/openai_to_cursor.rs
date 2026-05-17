//! OpenAI to Cursor request translator.
//!
//! Converts OpenAI messages to Cursor ask/agent format with XML tool_result blocks.

use serde_json::Value;

fn extract_content(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter(|p| {
                p.get("type").and_then(|v| v.as_str()) == Some("text")
                    && p.get("text").and_then(|v| v.as_str()).is_some()
            })
            .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

fn sanitize_result_text(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(*c as u32, 0..=8 | 0xB | 0xC | 0xE..=0x1F | 0x7F))
        .collect()
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn build_tool_result_block(tool_name: &str, tool_call_id: &str, result_text: &str) -> String {
    let clean = sanitize_result_text(result_text);
    format!(
        "<tool_result>\n<tool_name>{}</tool_name>\n<tool_call_id>{}</tool_call_id>\n<result>{}</result>\n</tool_result>",
        escape_xml(tool_name),
        escape_xml(tool_call_id),
        escape_xml(&clean)
    )
}

fn normalize_tool_call_id(id: &str) -> String {
    id.split('\n').next().unwrap_or("").to_string()
}

pub fn openai_to_cursor_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut tool_call_meta: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for msg in &messages {
        if msg.get("role").and_then(|v| v.as_str()) == Some("assistant") {
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    let id = tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool")
                        .to_string();
                    tool_call_meta.insert(id.clone(), name.clone());
                    let normalized = normalize_tool_call_id(&id);
                    if normalized != id {
                        tool_call_meta.insert(normalized, name);
                    }
                }
            }
            if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
                for part in arr {
                    if part.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                        let id = part
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = part
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        tool_call_meta.insert(id.clone(), name.clone());
                        let normalized = normalize_tool_call_id(&id);
                        if normalized != id {
                            tool_call_meta.insert(normalized, name);
                        }
                    }
                }
            }
        }
    }

    let mut result_messages: Vec<Value> = Vec::new();
    for msg in &messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        if role == "system" {
            result_messages.push(serde_json::json!({
                "role": "user",
                "content": format!("[System Instructions]\n{}", extract_content(msg.get("content").unwrap_or(&Value::Null)))
            }));
            continue;
        }

        if role == "tool" {
            let tool_content = extract_content(msg.get("content").unwrap_or(&Value::Null));
            let tool_call_id = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tool_name = msg
                .get("name")
                .and_then(|v| v.as_str())
                .or_else(|| tool_call_meta.get(tool_call_id).map(|s| s.as_str()))
                .unwrap_or("tool");
            result_messages.push(serde_json::json!({
                "role": "user",
                "content": build_tool_result_block(tool_name, tool_call_id, &tool_content)
            }));
            continue;
        }

        if role == "user" {
            if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
                let mut parts: Vec<String> = Vec::new();
                for block in arr {
                    match block.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                parts.push(t.to_string());
                            }
                        }
                        Some("tool_result") => {
                            let tool_call_id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let tool_name = tool_call_meta
                                .get(tool_call_id)
                                .or_else(|| {
                                    tool_call_meta.get(&normalize_tool_call_id(tool_call_id))
                                })
                                .map(|s| s.as_str())
                                .unwrap_or("tool");
                            let tool_content =
                                extract_content(block.get("content").unwrap_or(&Value::Null));
                            parts.push(build_tool_result_block(
                                tool_name,
                                tool_call_id,
                                &tool_content,
                            ));
                        }
                        _ => {}
                    }
                }
                let joined = parts
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !joined.is_empty() {
                    result_messages.push(serde_json::json!({
                        "role": "user",
                        "content": joined
                    }));
                }
                continue;
            }
        }

        if role == "user" || role == "assistant" {
            let content = extract_content(msg.get("content").unwrap_or(&Value::Null));

            if role == "assistant" {
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    if !tool_calls.is_empty() {
                        let stripped: Vec<Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                let mut obj = tc.clone();
                                if let Some(o) = obj.as_object_mut() {
                                    o.remove("index");
                                }
                                obj
                            })
                            .collect();
                        result_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": content,
                            "tool_calls": stripped
                        }));
                        continue;
                    }
                }
                if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
                    let extracted: Vec<Value> = arr.iter()
                        .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                        .filter_map(|b| {
                            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if id.is_empty() { return None; }
                            Some(serde_json::json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": b.get("name").and_then(|v| v.as_str()).unwrap_or("tool"),
                                    "arguments": serde_json::to_string(b.get("input").unwrap_or(&Value::Object(serde_json::Map::new()))).unwrap_or_else(|_| "{}".to_string())
                                }
                            }))
                        })
                        .collect();
                    if !extracted.is_empty() {
                        result_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": content,
                            "tool_calls": extracted
                        }));
                        continue;
                    }
                }
            }

            if !content.is_empty() {
                result_messages.push(serde_json::json!({
                    "role": role,
                    "content": content
                }));
            }
        }
    }

    let mut rest = body.clone();
    rest.as_object_mut().unwrap().remove("user");
    rest.as_object_mut().unwrap().remove("metadata");
    rest.as_object_mut().unwrap().remove("tool_choice");
    rest.as_object_mut().unwrap().remove("stream_options");
    rest.as_object_mut().unwrap().remove("system");
    rest["messages"] = Value::Array(result_messages);
    rest["max_tokens"] = Value::Number(32000.into());
    rest["model"] = Value::String(model.to_string());
    rest["stream"] = Value::Bool(stream);

    *body = rest;
    true
}
