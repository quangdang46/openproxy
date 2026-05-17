//! Antigravity to OpenAI request translator.
//!
//! Unwraps Cloud Code envelope and converts Gemini contents → OpenAI messages.

use serde_json::Value;

fn normalize_schema_types(schema: &Value) -> Value {
    if !schema.is_object() {
        return schema.clone();
    }
    let mut result = schema.clone();
    let obj = result.as_object_mut().unwrap();

    if let Some(t) = obj.get("type").and_then(|v| v.as_str()) {
        obj.insert("type".to_string(), Value::String(t.to_lowercase()));
    }
    obj.remove("enumDescriptions");

    if let Some(props) = obj.get("properties") {
        if let Some(props_obj) = props.as_object() {
            let mut cleaned = serde_json::Map::new();
            for (k, v) in props_obj {
                cleaned.insert(k.clone(), normalize_schema_types(v));
            }
            obj.insert("properties".to_string(), Value::Object(cleaned));
        }
    }
    if let Some(items) = obj.get("items") {
        obj.insert("items".to_string(), normalize_schema_types(items));
    }
    result
}

fn extract_text(instruction: &Value) -> String {
    if let Some(s) = instruction.as_str() {
        return s.to_string();
    }
    if let Some(parts) = instruction.get("parts").and_then(|v| v.as_array()) {
        return parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

fn convert_content(content: &Value) -> Option<Value> {
    let role = content.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let openai_role = match role {
        "model" => "assistant",
        "user" => "user",
        _ => role,
    };

    let parts = content.get("parts").and_then(|v| v.as_array());
    parts?;
    let parts = parts.unwrap();

    let mut text_parts: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut tool_results: Vec<Value> = Vec::new();
    let mut reasoning_content = String::new();

    for part in parts {
        if part.get("thought").and_then(|v| v.as_bool()) == Some(true) {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                reasoning_content.push_str(text);
            }
            continue;
        }
        if part.get("thoughtSignature").is_some() {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                text_parts.push(serde_json::json!({"type": "text", "text": text}));
            }
            continue;
        }
        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
            text_parts.push(serde_json::json!({"type": "text", "text": text}));
        }
        if let Some(inline_data) = part.get("inlineData") {
            if let (Some(mime), Some(data)) = (
                inline_data.get("mimeType").and_then(|v| v.as_str()),
                inline_data.get("data").and_then(|v| v.as_str()),
            ) {
                text_parts.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": {"url": format!("data:{};base64,{}", mime, data)}
                }));
            }
        }
        if let Some(func_call) = part.get("functionCall") {
            let id = func_call
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "call_{}_{}",
                        chrono::Utc::now().timestamp_millis(),
                        rand::random::<u32>() % 1000000
                    )
                });
            tool_calls.push(serde_json::json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": func_call.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "arguments": serde_json::to_string(func_call.get("args").unwrap_or(&Value::Object(serde_json::Map::new()))).unwrap_or_else(|_| "{}".to_string())
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
                .unwrap_or(Value::Object(serde_json::Map::new()));
            tool_results.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": serde_json::to_string(&response_content).unwrap_or_default()
            }));
        }
    }

    if !tool_results.is_empty() {
        return Some(Value::Array(tool_results));
    }

    if !tool_calls.is_empty() {
        let mut msg = serde_json::json!({"role": openai_role});
        if text_parts.len() == 1 {
            if let Some(text) = text_parts[0].get("text").and_then(|v| v.as_str()) {
                msg["content"] = Value::String(text.to_string());
            }
        } else if !text_parts.is_empty() {
            msg["content"] = Value::Array(text_parts);
        }
        if !reasoning_content.is_empty() {
            msg["reasoning_content"] = Value::String(reasoning_content);
        }
        msg["tool_calls"] = Value::Array(tool_calls);
        return Some(msg);
    }

    if !text_parts.is_empty() || !reasoning_content.is_empty() {
        let mut msg = serde_json::json!({"role": openai_role});
        if text_parts.len() == 1 {
            if let Some(text) = text_parts[0].get("text").and_then(|v| v.as_str()) {
                msg["content"] = Value::String(text.to_string());
            }
        } else if !text_parts.is_empty() {
            msg["content"] = Value::Array(text_parts);
        }
        if !reasoning_content.is_empty() {
            msg["reasoning_content"] = Value::String(reasoning_content);
        }
        return Some(msg);
    }

    None
}

pub fn antigravity_to_openai_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let req = body.get("request").cloned().unwrap_or_else(|| body.clone());

    let mut result = serde_json::json!({
        "model": model,
        "messages": [],
        "stream": stream
    });

    if let Some(config) = req.get("generationConfig") {
        if let Some(max_output) = config.get("maxOutputTokens").and_then(|v| v.as_u64()) {
            let has_tools = req.get("tools").is_some();
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
        if let Some(top_k) = config.get("topK") {
            result["top_k"] = top_k.clone();
        }
        if let Some(thinking_config) = config.get("thinkingConfig") {
            if let Some(budget) = thinking_config
                .get("thinkingBudget")
                .and_then(|v| v.as_u64())
            {
                if budget > 0 {
                    let effort = if budget <= 2048 {
                        "low"
                    } else if budget <= 16384 {
                        "medium"
                    } else {
                        "high"
                    };
                    result["reasoning_effort"] = Value::String(effort.to_string());
                }
            }
        }
    }

    if let Some(system_instruction) = req.get("systemInstruction") {
        let system_text = extract_text(system_instruction);
        if !system_text.is_empty() {
            result["messages"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "role": "system", "content": system_text
                }));
        }
    }

    if let Some(contents) = req.get("contents").and_then(|v| v.as_array()) {
        for content in contents {
            if let Some(converted) = convert_content(content) {
                if let Value::Array(arr) = converted {
                    result["messages"].as_array_mut().unwrap().extend(arr);
                } else {
                    result["messages"].as_array_mut().unwrap().push(converted);
                }
            }
        }
    }

    if let Some(tools) = req.get("tools").and_then(|v| v.as_array()) {
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
                        .map(normalize_schema_types)
                        .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));
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
