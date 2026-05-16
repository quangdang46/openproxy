//! OpenAI Responses API ↔ Chat Completions request translator.

use serde_json::Value;

fn normalize_tool_parameters(params: Option<&Value>) -> Value {
    match params {
        None => serde_json::json!({"type": "object", "properties": {}}),
        Some(p) => {
            if p.get("type").and_then(|v| v.as_str()) == Some("object") && p.get("properties").is_none() {
                let mut clone = p.clone();
                clone["properties"] = serde_json::json!({});
                clone
            } else {
                p.clone()
            }
        }
    }
}

fn clamp_call_id(id: &str) -> String {
    if id.len() > 64 {
        id[..64].to_string()
    } else {
        id.to_string()
    }
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
            result["messages"].as_array_mut().unwrap().push(serde_json::json!({
                "role": "system", "content": s
            }));
        }
    }

    let input_items = if let Some(arr) = input.and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return true;
    };

    let mut current_assistant_msg: Option<Value> = None;

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
                    result["messages"].as_array_mut().unwrap().push(serde_json::json!({
                        "role": role, "content": content
                    }));
                }
            }
            Some("function_call") => {
                let name = item.get("name").and_then(|v| v.as_str());
                if name.is_none() || name.map(|s| s.trim().is_empty()).unwrap_or(true) {
                    continue;
                }
                if current_assistant_msg.is_none() {
                    current_assistant_msg = Some(serde_json::json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": []
                    }));
                }
                if let Some(ref mut msg) = current_assistant_msg {
                    msg["tool_calls"].as_array_mut().unwrap().push(serde_json::json!({
                        "id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
                        "type": "function",
                        "function": {
                            "name": name.unwrap_or(""),
                            "arguments": item.get("arguments").cloned().unwrap_or(Value::String("{}".to_string()))
                        }
                    }));
                }
            }
            Some("function_call_output") => {
                if let Some(msg) = current_assistant_msg.take() {
                    result["messages"].as_array_mut().unwrap().push(msg);
                }
                let output = if let Some(s) = item.get("output").and_then(|v| v.as_str()) {
                    s.to_string()
                } else {
                    serde_json::to_string(&item.get("output").cloned().unwrap_or(Value::Null))
                        .unwrap_or_default()
                };
                result["messages"].as_array_mut().unwrap().push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
                    "content": output
                }));
            }
            Some("reasoning") => {}
            _ => {}
        }
    }

    if let Some(msg) = current_assistant_msg.take() {
        result["messages"].as_array_mut().unwrap().push(msg);
    }

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let converted: Vec<Value> = tools.iter().filter_map(|tool| {
            if tool.get("function").is_some() {
                Some(tool.clone())
            } else {
                let name = tool.get("name").and_then(|v| v.as_str());
                if name.is_none() || name.map(|s| s.trim().is_empty()).unwrap_or(true) {
                    return None;
                }
                Some(serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": name.unwrap_or(""),
                        "description": tool.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                        "parameters": normalize_tool_parameters(tool.get("parameters")),
                        "strict": tool.get("strict").cloned()
                    }
                }))
            }
        }).collect();
        result["tools"] = Value::Array(converted);
    }

    let obj = result.as_object_mut().unwrap();
    obj.remove("input");
    obj.remove("instructions");
    obj.remove("include");
    obj.remove("prompt_cache_key");
    obj.remove("store");
    obj.remove("reasoning");

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
    let messages = body.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    for msg in &messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        if role == "system" {
            if !has_system {
                result["instructions"] = msg.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string().into();
                has_system = true;
            }
            continue;
        }

        if role == "user" || role == "assistant" {
            let content_type = if role == "user" { "input_text" } else { "output_text" };
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

            if !content.is_empty() {
                result["input"].as_array_mut().unwrap().push(serde_json::json!({
                    "type": "message",
                    "role": role,
                    "content": content
                }));
            }
        }

        if role == "assistant" {
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    result["input"].as_array_mut().unwrap().push(serde_json::json!({
                        "type": "function_call",
                        "call_id": clamp_call_id(tc.get("id").and_then(|v| v.as_str()).unwrap_or("")),
                        "name": tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or("_unknown"),
                        "arguments": tc.get("function").and_then(|f| f.get("arguments")).cloned().unwrap_or(Value::String("{}".to_string()))
                    }));
                }
            }
        }

        if role == "tool" {
            let output = if let Some(s) = msg.get("content").and_then(|v| v.as_str()) {
                s.to_string()
            } else if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
                arr.iter().filter_map(|c| c.get("text").and_then(|v| v.as_str())).collect::<Vec<_>>().join("")
            } else {
                serde_json::to_string(&msg.get("content").cloned().unwrap_or(Value::Null))
                    .unwrap_or_default()
            };
            result["input"].as_array_mut().unwrap().push(serde_json::json!({
                "type": "function_call_output",
                "call_id": clamp_call_id(msg.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("")),
                "output": output
            }));
        }
    }

    if !has_system {
        result["instructions"] = Value::String(String::new());
    }

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let converted: Vec<Value> = tools.iter().map(|tool| {
            if tool.get("type").and_then(|v| v.as_str()) == Some("function") {
                if let Some(fn_obj) = tool.get("function") {
                    serde_json::json!({
                        "type": "function",
                        "name": fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "description": fn_obj.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                        "parameters": normalize_tool_parameters(fn_obj.get("parameters")),
                        "strict": fn_obj.get("strict").cloned()
                    })
                } else {
                    tool.clone()
                }
            } else {
                tool.clone()
            }
        }).collect();
        result["tools"] = Value::Array(converted);
    }

    if let Some(t) = body.get("temperature") { result["temperature"] = t.clone(); }
    if let Some(m) = body.get("max_tokens") { result["max_tokens"] = m.clone(); }
    if let Some(t) = body.get("top_p") { result["top_p"] = t.clone(); }

    *body = result;
    let _ = stream;
    true
}
