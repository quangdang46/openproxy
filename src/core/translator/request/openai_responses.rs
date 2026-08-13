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

fn clamp_call_id(id: &str) -> String {
    if id.len() > 64 {
        id[..64].to_string()
    } else {
        id.to_string()
    }
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

    let input_items = if let Some(arr) = input.and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return true;
    };

    let mut current_assistant_msg: Option<Value> = None;
    // Reasoning continuity: buffer reasoning text/encrypted_content across
    // input items and attach to the next assistant message (JS 33-34, 149-160).
    let mut pending_reasoning = String::new();
    let mut pending_reasoning_encrypted = String::new();

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
            Some("function_call") => {
                let name = item.get("name").and_then(|v| v.as_str());
                if name.is_none() || name.map(|s| s.trim().is_empty()).unwrap_or(true) {
                    continue;
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
                // Non-assistant items clear the pending reasoning buffers (JS 95-98).
                pending_reasoning.clear();
                pending_reasoning_encrypted.clear();
                let output = if let Some(s) = item.get("output").and_then(|v| v.as_str()) {
                    s.to_string()
                } else {
                    serde_json::to_string(&item.get("output").cloned().unwrap_or(Value::Null))
                        .unwrap_or_default()
                };
                result["messages"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
                        "content": output
                    }));
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
                arr.iter()
                    .filter_map(|c| c.get("text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
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
}
