//! OpenAI to Gemini request translator
//!
//! Converts OpenAI Chat Completions format to Gemini API format.

use serde_json::Value;
use std::collections::HashMap;

/// Sanitize function names for Gemini API.
/// Gemini requires: starts with [a-zA-Z_], followed by [a-zA-Z0-9_.:\-], max 64 chars.
fn sanitize_gemini_function_name(name: &str) -> String {
    if name.is_empty() {
        return "_unknown".to_string();
    }
    let mut sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !sanitized
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        sanitized.insert(0, '_');
    }
    sanitized.truncate(64);
    sanitized
}

/// Try to parse JSON, return default on failure.
fn try_parse_json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or(Value::String(s.to_string()))
}

/// Extract text content from OpenAI content (string or array).
fn extract_text_content(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

/// Convert OpenAI content parts to Gemini parts.
fn convert_openai_content_to_parts(content: &Value) -> Vec<Value> {
    if let Some(s) = content.as_str() {
        if !s.is_empty() {
            return vec![serde_json::json!({"text": s})];
        }
        return vec![];
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for part in arr {
            let t = part.get("type").and_then(|v| v.as_str());
            match t {
                Some("text") => {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        parts.push(serde_json::json!({"text": text}));
                    }
                }
                Some("image_url") => {
                    if let Some(url_obj) = part.get("image_url") {
                        if let Some(url) = url_obj.get("url").and_then(|v| v.as_str()) {
                            if let Some(data_uri) = url.strip_prefix("data:") {
                                if let Some((mime, base64_data)) = data_uri.split_once(";base64,") {
                                    parts.push(serde_json::json!({
                                        "inlineData": {
                                            "mimeType": mime,
                                            "data": base64_data
                                        }
                                    }));
                                }
                            }
                        }
                    }
                }
                Some("image") => {
                    if let Some(source) = part.get("source") {
                        if source.get("type").and_then(|v| v.as_str()) == Some("base64") {
                            if let (Some(media_type), Some(data)) = (
                                source.get("media_type").and_then(|v| v.as_str()),
                                source.get("data").and_then(|v| v.as_str()),
                            ) {
                                parts.push(serde_json::json!({
                                    "inlineData": {
                                        "mimeType": media_type,
                                        "data": data
                                    }
                                }));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        return parts;
    }
    vec![]
}

/// Clean JSON schema for Gemini/Antigravity compatibility.
fn clean_json_schema(schema: &Value) -> Value {
    if !schema.is_object() {
        return schema.clone();
    }
    let mut result = schema.clone();
    let obj = result.as_object_mut().unwrap();

    // Normalize type to lowercase
    if let Some(t) = obj.get("type").and_then(|v| v.as_str()) {
        obj.insert("type".to_string(), Value::String(t.to_lowercase()));
    }

    // Strip enumDescriptions
    obj.remove("enumDescriptions");

    // Recurse into properties
    if let Some(props) = obj.get("properties") {
        if let Some(props_obj) = props.as_object() {
            let mut cleaned = serde_json::Map::new();
            for (k, v) in props_obj {
                cleaned.insert(k.clone(), clean_json_schema(v));
            }
            obj.insert("properties".to_string(), Value::Object(cleaned));
        }
    }

    // Recurse into items
    if let Some(items) = obj.get("items") {
        obj.insert("items".to_string(), clean_json_schema(items));
    }

    // Ensure properties exists for object type
    if obj.get("type").and_then(|v| v.as_str()) == Some("object") && obj.get("properties").is_none()
    {
        obj.insert("properties".to_string(), serde_json::json!({}));
    }

    result
}

/// Core: Convert OpenAI request to Gemini format.
fn openai_to_gemini_base(model: &str, body: &Value, stream: bool, _signature: &str) -> Value {
    let mut result = serde_json::json!({
        "model": model,
        "contents": [],
        "generationConfig": {},
        "safetySettings": [
            {"category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF"},
            {"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF"},
            {"category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF"},
            {"category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF"},
            {"category": "HARM_CATEGORY_CIVIC_INTEGRITY", "threshold": "OFF"}
        ]
    });

    // Generation config
    if let Some(temp) = body.get("temperature") {
        result["generationConfig"]["temperature"] = temp.clone();
    }
    if let Some(top_p) = body.get("top_p") {
        result["generationConfig"]["topP"] = top_p.clone();
    }
    if let Some(top_k) = body.get("top_k") {
        result["generationConfig"]["topK"] = top_k.clone();
    }
    if let Some(max_tokens) = body.get("max_tokens") {
        result["generationConfig"]["maxOutputTokens"] = max_tokens.clone();
    }

    // Build tool_call_id -> name map
    let mut tc_id_to_name: HashMap<String, String> = HashMap::new();
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if msg.get("role").and_then(|v| v.as_str()) == Some("assistant") {
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        if tc.get("type").and_then(|v| v.as_str()) == Some("function") {
                            if let (Some(id), Some(name)) = (
                                tc.get("id").and_then(|v| v.as_str()),
                                tc.get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|v| v.as_str()),
                            ) {
                                tc_id_to_name.insert(id.to_string(), name.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Build tool responses cache
    let mut tool_responses: HashMap<String, String> = HashMap::new();
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if msg.get("role").and_then(|v| v.as_str()) == Some("tool") {
                if let (Some(tool_call_id), Some(content)) = (
                    msg.get("tool_call_id").and_then(|v| v.as_str()),
                    msg.get("content"),
                ) {
                    tool_responses.insert(
                        tool_call_id.to_string(),
                        serde_json::to_string(content).unwrap_or_default(),
                    );
                }
            }
        }
    }

    // Convert messages
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for (i, msg) in messages.iter().enumerate() {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = msg.get("content").cloned().unwrap_or(Value::Null);

            // System message
            if role == "system" && messages.len() > 1 {
                let system_text = extract_text_content(&content);
                if !system_text.is_empty() {
                    result["systemInstruction"] = serde_json::json!({
                        "role": "user",
                        "parts": [{"text": system_text}]
                    });
                }
                continue;
            }

            // User message (or system-only)
            if role == "user" || (role == "system" && messages.len() == 1) {
                let parts = convert_openai_content_to_parts(&content);
                if !parts.is_empty() {
                    result["contents"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "role": "user",
                            "parts": parts
                        }));
                }
                continue;
            }

            // Assistant message
            if role == "assistant" {
                let mut parts = Vec::new();

                // Thinking/reasoning → thought part with signature
                if let Some(reasoning) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !reasoning.is_empty() {
                        parts.push(serde_json::json!({
                            "thought": true,
                            "text": reasoning
                        }));
                        parts.push(serde_json::json!({
                            "thoughtSignature": _signature,
                            "text": ""
                        }));
                    }
                }

                // Text content
                let text = extract_text_content(&content);
                if !text.is_empty() {
                    parts.push(serde_json::json!({"text": text}));
                }

                // Tool calls
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    let mut tool_call_ids: Vec<String> = Vec::new();
                    for tc in tool_calls {
                        if tc.get("type").and_then(|v| v.as_str()) != Some("function") {
                            continue;
                        }
                        let fn_obj = tc.get("function").cloned().unwrap_or(Value::Null);
                        let name = fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = fn_obj
                            .get("arguments")
                            .map(|v| try_parse_json(v.as_str().unwrap_or("{}")))
                            .unwrap_or(Value::Null);
                        let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");

                        parts.push(serde_json::json!({
                            "thoughtSignature": _signature,
                            "functionCall": {
                                "id": id,
                                "name": sanitize_gemini_function_name(name),
                                "args": args
                            }
                        }));
                        if !id.is_empty() {
                            tool_call_ids.push(id.to_string());
                        }
                    }

                    if !parts.is_empty() {
                        result["contents"]
                            .as_array_mut()
                            .unwrap()
                            .push(serde_json::json!({
                                "role": "model",
                                "parts": parts
                            }));
                    }

                    // Check if there are actual tool responses
                    let has_actual_responses = tool_call_ids
                        .iter()
                        .any(|fid| tool_responses.contains_key(fid));
                    if has_actual_responses {
                        let mut tool_parts = Vec::new();
                        for fid in &tool_call_ids {
                            if let Some(resp_str) = tool_responses.get(fid) {
                                let name = tc_id_to_name.get(fid).cloned().unwrap_or_else(|| {
                                    let id_parts: Vec<&str> = fid.split('-').collect();
                                    if id_parts.len() > 2 {
                                        id_parts[..id_parts.len() - 2].join("-")
                                    } else {
                                        fid.clone()
                                    }
                                });
                                let parsed_resp = try_parse_json(resp_str);
                                let final_resp =
                                    if parsed_resp.is_object() || parsed_resp.is_array() {
                                        parsed_resp
                                    } else {
                                        serde_json::json!({"result": parsed_resp})
                                    };
                                tool_parts.push(serde_json::json!({
                                    "functionResponse": {
                                        "id": fid,
                                        "name": sanitize_gemini_function_name(&name),
                                        "response": {"result": final_resp}
                                    }
                                }));
                            }
                        }
                        if !tool_parts.is_empty() {
                            result["contents"]
                                .as_array_mut()
                                .unwrap()
                                .push(serde_json::json!({
                                    "role": "user",
                                    "parts": tool_parts
                                }));
                        }
                    }
                } else if !parts.is_empty() {
                    result["contents"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "role": "model",
                            "parts": parts
                        }));
                }
            }
        }
    }

    // Convert tools
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let mut function_declarations = Vec::new();
        for t in tools {
            // Claude/Anthropic format (no type field, direct name/description/input_schema)
            if t.get("name").is_some() && t.get("input_schema").is_some() {
                let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let description = t.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let schema = clean_json_schema(
                    t.get("input_schema")
                        .unwrap_or(&serde_json::json!({"type": "object", "properties": {}})),
                );
                function_declarations.push(serde_json::json!({
                    "name": sanitize_gemini_function_name(name),
                    "description": description,
                    "parameters": schema
                }));
            }
            // OpenAI format
            else if t.get("type").and_then(|v| v.as_str()) == Some("function") {
                if let Some(fn_obj) = t.get("function") {
                    let name = fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let description = fn_obj
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let schema = clean_json_schema(
                        fn_obj
                            .get("parameters")
                            .unwrap_or(&serde_json::json!({"type": "object", "properties": {}})),
                    );
                    function_declarations.push(serde_json::json!({
                        "name": sanitize_gemini_function_name(name),
                        "description": description,
                        "parameters": schema
                    }));
                }
            }
        }
        if !function_declarations.is_empty() {
            result["tools"] = serde_json::json!([{"functionDeclarations": function_declarations}]);
        }
    }

    let _ = stream; // Gemini handles stream via URL path, not body param
    result
}

/// Main entry point for OpenAI to Gemini request translation.
pub fn openai_to_gemini_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let result = openai_to_gemini_base(model, body, stream, "");
    *body = result;
    true
}

/// OpenAI to Gemini CLI request (uses different thinking signature).
pub fn openai_to_gemini_cli_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let mut gemini = openai_to_gemini_base(model, body, stream, "");

    // Add thinking config for CLI
    if let Some(reasoning_effort) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        let budget = match reasoning_effort {
            "low" => 1024,
            "high" => 32768,
            _ => 8192, // medium
        };
        gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({
            "thinkingBudget": budget,
            "include_thoughts": true
        });
    }

    // Thinking config from Claude format
    if let Some(thinking) = body.get("thinking") {
        if thinking.get("type").and_then(|v| v.as_str()) == Some("enabled") {
            if let Some(budget) = thinking.get("budget_tokens").and_then(|v| v.as_u64()) {
                gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({
                    "thinkingBudget": budget,
                    "include_thoughts": true
                });
            }
        }
    }

    // Clean schema for tools
    if let Some(tools_arr) = gemini.get_mut("tools").and_then(|v| v.as_array_mut()) {
        if let Some(first_tool) = tools_arr.first_mut() {
            if let Some(func_decls) = first_tool
                .get_mut("functionDeclarations")
                .and_then(|v| v.as_array_mut())
            {
                for fn_decl in func_decls {
                    if let Some(params) = fn_decl.get_mut("parameters") {
                        let cleaned = clean_json_schema(params);
                        *params = cleaned;
                    }
                }
            }
        }
    }

    *body = gemini;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_openai_to_gemini() {
        let mut body: Value = serde_json::from_str(
            r#"{
                "model": "gemini-pro",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            }"#,
        )
        .unwrap();

        openai_to_gemini_request("gemini-pro", &mut body, true, None);

        assert!(body.get("contents").is_some());
        assert!(body.get("generationConfig").is_some());
    }
}
