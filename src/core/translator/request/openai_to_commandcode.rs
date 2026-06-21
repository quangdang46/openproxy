//! OpenAI to CommandCode request translator.
//!
//! Converts to CommandCode /alpha/generate schema.

use serde_json::Value;

fn flatten_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for p in arr {
            if let Some(s) = p.as_str() {
                parts.push(s.to_string());
            } else if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                parts.push(t.to_string());
            }
        }
        return parts.join("\n");
    }
    if content.is_null() {
        return String::new();
    }
    content.to_string()
}

fn to_content_blocks(content: &Value) -> Vec<Value> {
    if content.is_null() {
        return vec![serde_json::json!({"type": "text", "text": ""})];
    }
    if let Some(s) = content.as_str() {
        return vec![serde_json::json!({"type": "text", "text": s})];
    }
    if let Some(arr) = content.as_array() {
        let mut blocks = Vec::new();
        for part in arr {
            if let Some(s) = part.as_str() {
                blocks.push(serde_json::json!({"type": "text", "text": s}));
            } else if let Some(obj) = part.as_object() {
                if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                        blocks.push(serde_json::json!({"type": "text", "text": t}));
                    }
                } else if matches!(
                    part.get("type").and_then(|v| v.as_str()),
                    Some("image_url") | Some("image")
                ) {
                    blocks.push(serde_json::json!({"type": "text", "text": "[image omitted]"}));
                } else if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    blocks.push(serde_json::json!({"type": "text", "text": t}));
                }
            }
        }
        if !blocks.is_empty() {
            return blocks;
        }
    }
    vec![serde_json::json!({"type": "text", "text": content.to_string()})]
}

fn convert_messages(messages: &[Value]) -> (Vec<Value>, String) {
    let mut out = Vec::new();
    let mut system_texts = Vec::new();

    for m in messages {
        if m.is_null() {
            continue;
        }
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");

        if role == "system" || role == "developer" {
            let t = flatten_text(m.get("content").unwrap_or(&Value::Null));
            if !t.is_empty() {
                system_texts.push(t);
            }
            continue;
        }

        if role == "tool" {
            let value = if let Some(s) = m.get("content").and_then(|v| v.as_str()) {
                s.to_string()
            } else {
                flatten_text(m.get("content").unwrap_or(&Value::Null))
            };
            out.push(serde_json::json!({
                "role": "tool",
                "content": [{
                    "type": "tool-result",
                    "toolCallId": m.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or(""),
                    "toolName": m.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "output": {"type": "text", "value": value}
                }]
            }));
            continue;
        }

        if role == "assistant" {
            let mut blocks = Vec::new();
            let text = flatten_text(m.get("content").unwrap_or(&Value::Null));
            if !text.is_empty() {
                blocks.push(serde_json::json!({"type": "text", "text": text}));
            }
            if let Some(tool_calls) = m.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    let fn_obj = tc.get("function").cloned().unwrap_or(Value::Null);
                    let args = fn_obj
                        .get("arguments")
                        .map(|v| {
                            if let Some(s) = v.as_str() {
                                serde_json::from_str(s)
                                    .unwrap_or(Value::Object(serde_json::Map::new()))
                            } else {
                                v.clone()
                            }
                        })
                        .unwrap_or(Value::Object(serde_json::Map::new()));
                    blocks.push(serde_json::json!({
                        "type": "tool-call",
                        "toolCallId": tc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "toolName": fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "input": args
                    }));
                }
            }
            if blocks.is_empty() {
                blocks.push(serde_json::json!({"type": "text", "text": ""}));
            }
            out.push(serde_json::json!({
                "role": "assistant",
                "content": blocks
            }));
            continue;
        }

        out.push(serde_json::json!({
            "role": "user",
            "content": to_content_blocks(m.get("content").unwrap_or(&Value::Null))
        }));
    }

    (out, system_texts.join("\n\n"))
}

fn convert_tools(tools: Option<&Value>) -> Option<Vec<Value>> {
    let tools = tools.and_then(|v| v.as_array())?;
    if tools.is_empty() {
        return None;
    }
    let mut result = Vec::new();
    for t in tools {
        if t.is_null() {
            continue;
        }
        if t.get("type").and_then(|v| v.as_str()) == Some("function") {
            if let Some(fn_obj) = t.get("function") {
                result.push(serde_json::json!({
                    "name": fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "description": fn_obj.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                    "input_schema": fn_obj.get("parameters").cloned().unwrap_or(serde_json::json!({"type": "object"}))
                }));
            }
        } else if t.get("name").is_some()
            && (t.get("input_schema").is_some() || t.get("parameters").is_some())
        {
            result.push(serde_json::json!({
                "name": t.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "description": t.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                "input_schema": t.get("input_schema").or_else(|| t.get("parameters")).cloned().unwrap()
            }));
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

pub fn openai_to_commandcode_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let (messages, system) = convert_messages(
        body.get("messages")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[]),
    );

    let mut params = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": stream,
        "max_tokens": body.get("max_tokens").or_else(|| body.get("max_output_tokens")).cloned().unwrap_or(Value::Number(64000.into())),
        "temperature": body.get("temperature").cloned().unwrap_or(Value::Number(serde_json::Number::from_f64(0.3).unwrap_or(serde_json::Number::from(0)))),
    });

    if !system.is_empty() {
        params["system"] = Value::String(system);
    }

    if let Some(tools) = convert_tools(body.get("tools")) {
        params["tools"] = Value::Array(tools);
    }

    if let Some(top_p) = body.get("top_p") {
        params["top_p"] = top_p.clone();
    }

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    *body = serde_json::json!({
        "threadId": "auto-generated",
        "memory": "",
        "config": {
            "workingDir": ".",
            "date": today,
            "environment": "linux",
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": []
        },
        "params": params
    });
    true
}
