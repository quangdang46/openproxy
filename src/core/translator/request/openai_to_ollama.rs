//! OpenAI to Ollama request translator

use serde_json::Value;

fn normalize_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|b| {
                b.get("type")
                    .and_then(|t| t.as_str())
                    .filter(|&t| t == "text")
                    .and_then(|_| b.get("text"))
                    .and_then(|t| t.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn extract_images(content: &Value) -> Vec<String> {
    let Some(arr) = content.as_array() else {
        return vec![];
    };
    arr.iter()
        .filter_map(|block| {
            let t = block.get("type").and_then(|v| v.as_str())?;
            if t != "image_url" {
                return None;
            }
            let url = block
                .get("image_url")
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())?;
            url.strip_prefix("data:")
                .and_then(|s| s.split(";base64,").nth(1))
                .map(String::from)
        })
        .collect()
}

pub fn openai_to_ollama_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let Some(body_obj) = body.as_object() else {
        return false;
    };

    let mut tool_call_map: serde_json::Map<String, Value> = serde_json::Map::new();
    if let Some(messages) = body_obj.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if msg.get("role").and_then(|v| v.as_str()) == Some("assistant") {
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        if let (Some(id), Some(name)) = (
                            tc.get("id").and_then(|v| v.as_str()),
                            tc.get("function").and_then(|f| f.get("name")),
                        ) {
                            tool_call_map.insert(id.to_string(), name.clone());
                        }
                    }
                }
            }
        }
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(msgs) = body_obj.get("messages").and_then(|v| v.as_array()) {
        for msg in msgs {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            match role {
                "tool" => {
                    let content = normalize_content(
                        msg.get("content").unwrap_or(&Value::String(String::new())),
                    );
                    if content.is_empty() {
                        continue;
                    }
                    let tool_name = msg
                        .get("tool_call_id")
                        .and_then(|id| tool_call_map.get(id.as_str()?))
                        .or_else(|| msg.get("name"))
                        .cloned()
                        .unwrap_or(Value::String("unknown_tool".to_string()));
                    let tool_name_str = tool_name.as_str().unwrap_or("unknown_tool").to_string();
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_name": tool_name_str,
                        "content": content
                    }));
                }
                "assistant" => {
                    let content = normalize_content(
                        msg.get("content").unwrap_or(&Value::String(String::new())),
                    );
                    let mut out = serde_json::json!({
                        "role": "assistant",
                        "content": content
                    });
                    if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                        let ollama_tool_calls: Vec<Value> = tool_calls.iter().enumerate().map(|(i, tc)| {
                            let args = tc.get("function").and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                                .and_then(|s| serde_json::from_str(s).ok())
                                .unwrap_or(Value::Object(serde_json::Map::new()));
                            serde_json::json!({
                                "type": "function",
                                "function": {
                                    "index": i,
                                    "name": tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
                                    "arguments": args
                                }
                            })
                        }).collect();
                        out["tool_calls"] = Value::Array(ollama_tool_calls);
                    }
                    let images =
                        extract_images(msg.get("content").unwrap_or(&Value::Array(vec![])));
                    if !images.is_empty() {
                        out["images"] =
                            Value::Array(images.into_iter().map(Value::String).collect());
                    }
                    messages.push(out);
                }
                _ => {
                    let content = normalize_content(
                        msg.get("content").unwrap_or(&Value::String(String::new())),
                    );
                    if content.is_empty() && role != "assistant" {
                        continue;
                    }
                    let mut out = serde_json::json!({
                        "role": role,
                        "content": content
                    });
                    let images =
                        extract_images(msg.get("content").unwrap_or(&Value::Array(vec![])));
                    if !images.is_empty() {
                        out["images"] =
                            Value::Array(images.into_iter().map(Value::String).collect());
                    }
                    messages.push(out);
                }
            }
        }
    }

    let mut result = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": stream
    });

    if let Some(temp) = body_obj.get("temperature") {
        result["options"] = serde_json::json!({ "temperature": temp });
    }
    if let Some(max_tokens) = body_obj.get("max_tokens") {
        if let Some(opts) = result.get_mut("options").and_then(|v| v.as_object_mut()) {
            opts.insert("num_predict".to_string(), max_tokens.clone());
        } else {
            result["options"] = serde_json::json!({ "num_predict": max_tokens });
        }
    }
    if let Some(top_p) = body_obj.get("top_p") {
        if let Some(opts) = result.get_mut("options").and_then(|v| v.as_object_mut()) {
            opts.insert("top_p".to_string(), top_p.clone());
        } else {
            result["options"] = serde_json::json!({ "top_p": top_p });
        }
    }
    if let Some(tools) = body_obj.get("tools") {
        result["tools"] = tools.clone();
    }
    if let Some(tool_choice) = body_obj.get("tool_choice") {
        result["tool_choice"] = tool_choice.clone();
    }

    *body = result;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_openai_to_ollama() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "llama3",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        }"#,
        )
        .unwrap();

        openai_to_ollama_request("llama3", &mut body, false, None);

        assert_eq!(body.get("model").unwrap().as_str().unwrap(), "llama3");
        assert!(!body.get("messages").unwrap().as_array().unwrap().is_empty());
    }
}
