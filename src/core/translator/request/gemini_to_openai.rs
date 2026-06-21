//! Gemini to OpenAI request translator
//!
//! Converts Gemini API request format to OpenAI-compatible format.

use serde_json::Value;

/// Convert Gemini content to OpenAI message
fn convert_gemini_content(content: &Value) -> Option<Value> {
    let role = content
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("user");
    let openai_role = if role == "user" { "user" } else { "assistant" };

    let parts = content.get("parts").and_then(|v| v.as_array())?;
    if parts.is_empty() {
        return None;
    }

    let mut text_parts: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut reasoning_text: Option<String> = None;

    for part in parts {
        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                if part.get("thought").and_then(|v| v.as_bool()).unwrap_or(false) {
                    reasoning_text.get_or_insert_with(String::new).push_str(text);
                } else {
                    text_parts.push(serde_json::json!({
                        "type": "text",
                        "text": text
                    }));
                }
            }
        }

        if let Some(inline_data) = part.get("inlineData").or_else(|| part.get("inline_data")) {
            if let Some(mime_type) = inline_data.get("mimeType").and_then(|v| v.as_str()) {
                if let Some(data) = inline_data.get("data").and_then(|v| v.as_str()) {
                    text_parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", mime_type, data)
                        }
                    }));
                }
            }
        }

        if let Some(func_call) = part.get("functionCall") {
            let name = func_call.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = func_call.get("args").cloned().unwrap_or(Value::Null);
            let id = format!("call_{}_{}", name, chrono::Utc::now().timestamp_millis());
            tool_calls.push(serde_json::json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string())
                }
            }));
        }

        if let Some(func_response) = part.get("functionResponse") {
            let tool_call_id = func_response
                .get("id")
                .or_else(|| func_response.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let response_content = func_response
                .get("response")
                .and_then(|r| r.get("result"))
                .or_else(|| func_response.get("response"))
                .cloned()
                .unwrap_or(serde_json::json!({}));

            return Some(serde_json::json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": serde_json::to_string(&response_content).unwrap_or_default()
            }));
        }
    }

    if !tool_calls.is_empty() {
        let mut result = serde_json::json!({ "role": "assistant" });
        if text_parts.len() == 1 {
            if let Some(text) = text_parts[0].get("text").and_then(|v| v.as_str()) {
                result["content"] = Value::String(text.to_string());
            }
        } else if !text_parts.is_empty() {
            result["content"] = Value::Array(text_parts);
        }
        if let Some(reasoning) = &reasoning_text {
            result["reasoning_content"] = Value::String(reasoning.clone());
        }
        result["tool_calls"] = Value::Array(tool_calls);
        return Some(result);
    }

    if text_parts.len() == 1 {
        if let Some(text) = text_parts[0].get("text").and_then(|v| v.as_str()) {
            let mut msg = serde_json::json!({
                "role": openai_role,
                "content": text
            });
            if let Some(reasoning) = &reasoning_text {
                msg["reasoning_content"] = Value::String(reasoning.clone());
            }
            return Some(msg);
        }
    } else if !text_parts.is_empty() {
        let mut msg = serde_json::json!({
            "role": openai_role,
            "content": text_parts
        });
        if let Some(reasoning) = &reasoning_text {
            msg["reasoning_content"] = Value::String(reasoning.clone());
        }
        return Some(msg);
    }

    // Only reasoning content present, no regular text parts or tool calls
    if let Some(reasoning) = reasoning_text {
        return Some(serde_json::json!({
            "role": openai_role,
            "reasoning_content": reasoning
        }));
    }

    None
}

/// Extract text from Gemini content
fn extract_gemini_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(parts) = content.get("parts").and_then(|v| v.as_array()) {
        return parts
            .iter()
            .filter(|p| !p.get("thought").and_then(|t| t.as_bool()).unwrap_or(false))
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

/// Main entry point for Gemini to OpenAI request translation.
pub fn gemini_to_openai_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let Some(body_obj) = body.as_object() else {
        return false;
    };

    let mut result = serde_json::json!({
        "model": model,
        "messages": [],
        "stream": stream
    });

    if let Some(config) = body_obj.get("generationConfig") {
        if let Some(max_output) = config.get("maxOutputTokens").and_then(|v| v.as_u64()) {
            let has_tools = body_obj.get("tools").is_some();
            let adjusted = if has_tools && max_output < 32000 {
                32000
            } else {
                max_output
            };
            result["max_tokens"] = serde_json::json!(adjusted);
        }
        if let Some(temp) = config.get("temperature") {
            result["temperature"] = temp.clone();
        }
        if let Some(top_p) = config.get("topP") {
            result["top_p"] = top_p.clone();
        }
    }

    if let Some(system) = body_obj.get("systemInstruction") {
        let system_text = extract_gemini_text(system);
        if !system_text.is_empty() {
            result["messages"] = serde_json::json!([
                { "role": "system", "content": system_text }
            ]);
        }
    }

    if let Some(contents) = body_obj.get("contents").and_then(|v| v.as_array()) {
        let mut messages: Vec<Value> = Vec::new();
        for content in contents {
            if let Some(converted) = convert_gemini_content(content) {
                messages.push(converted);
            }
        }
        if let Some(msg_arr) = result["messages"].as_array_mut() {
            msg_arr.extend(messages);
        }
    }

    if let Some(tools) = body_obj.get("tools").and_then(|v| v.as_array()) {
        let mut converted_tools: Vec<Value> = Vec::new();
        for tool in tools {
            if let Some(func_decls) = tool.get("functionDeclarations").and_then(|v| v.as_array()) {
                for func in func_decls {
                    let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let description = func
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let parameters = func
                        .get("parameters")
                        .cloned()
                        .unwrap_or(serde_json::json!({ "type": "object", "properties": {} }));
                    converted_tools.push(serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": description,
                            "parameters": parameters
                        }
                    }));
                }
            }
        }
        if !converted_tools.is_empty() {
            result["tools"] = Value::Array(converted_tools);
        }
    }

    *body = result;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_gemini_to_openai() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "gemini-pro",
            "contents": [
                {"role": "user", "parts": [{"text": "Hello"}]}
            ]
        }"#,
        )
        .unwrap();

        gemini_to_openai_request("gemini-pro", &mut body, false, None);

        let model = body.get("model").unwrap().as_str().unwrap();
        assert_eq!(model, "gemini-pro");
        assert!(!body.get("messages").unwrap().as_array().unwrap().is_empty());
    }
}
