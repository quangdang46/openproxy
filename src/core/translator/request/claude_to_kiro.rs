//! Claude to Kiro request translator
//!
//! Converts Claude Messages API format to Kiro/AWS CodeWhisperer format.
//!
//! Claude format has a top-level `system` field, `messages[]` with
//! content blocks (text, tool_use, tool_result, thinking, image), and
//! tool definitions with `input_schema` instead of `parameters`.
//!
//! This converter achieves the same Kiro output as `openai_to_kiro_request`
//! but starts from Claude-shaped input.

use serde_json::Value;

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

fn safe_parse_json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or(Value::Object(serde_json::Map::new()))
}

/// Convert a tool definition from Claude format ({name, description, input_schema})
/// to Kiro format ({toolSpecification: {name, description, inputSchema: {json: ...}}}).
fn claude_tool_to_kiro(tool: &Value) -> Value {
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = tool
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = if description.trim().is_empty() {
        format!("Tool: {}", name)
    } else {
        description
    };
    let schema = tool
        .get("input_schema")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}, "required": []}));
    let normalized_schema = if schema.as_object().is_none_or(|o| o.is_empty()) {
        serde_json::json!({"type": "object", "properties": {}, "required": []})
    } else {
        let mut s = schema;
        if s.get("required").is_none() {
            s["required"] = serde_json::json!([]);
        }
        s
    };
    serde_json::json!({
        "toolSpecification": {
            "name": name,
            "description": description,
            "inputSchema": {"json": normalized_schema}
        }
    })
}

pub fn claude_to_kiro_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    credentials: Option<&Value>,
) -> bool {
    // Extract Claude-specific fields
    let claude_messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let claude_system = body
        .get("system")
        .and_then(|v| {
            if let Some(arr) = v.as_array() {
                Some(
                    arr.iter()
                        .filter_map(|b| {
                            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                                b.get("text").and_then(|t| t.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            } else if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    let claude_tools = body.get("tools").and_then(|v| v.as_array()).cloned();

    // Build Kiro conversation state
    let mut history: Vec<Value> = Vec::new();
    let mut pending_user_content: Vec<String> = Vec::new();
    let mut pending_assistant_content: Vec<String> = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();
    let mut pending_images: Vec<Value> = Vec::new();
    let mut current_role: Option<String> = None;

    // Prepend system prompt as first user message
    if !claude_system.is_empty() {
        pending_user_content.push(claude_system);
        current_role = Some("user".to_string());
    }

    let tools_array = claude_tools
        .as_ref()
        .map(|arr| arr.iter().map(claude_tool_to_kiro).collect::<Vec<_>>())
        .unwrap_or_default();

    let flush_pending = |history: &mut Vec<Value>,
                         pending_user_content: &mut Vec<String>,
                         pending_assistant_content: &mut Vec<String>,
                         pending_tool_results: &mut Vec<Value>,
                         pending_images: &mut Vec<Value>,
                         current_role: &Option<String>,
                         tools_arr: &[Value],
                         history_len: usize| {
        match current_role.as_deref() {
            Some("user") => {
                let content = pending_user_content.join("\n\n").trim().to_string();
                let content = if content.is_empty() {
                    "continue".to_string()
                } else {
                    content
                };
                let mut user_msg = serde_json::json!({
                    "userInputMessage": {
                        "content": content,
                        "modelId": ""
                    }
                });

                if !pending_images.is_empty() {
                    user_msg["userInputMessage"]["images"] = Value::Array(pending_images.clone());
                }

                if !pending_tool_results.is_empty() {
                    user_msg["userInputMessage"]["userInputMessageContext"] = serde_json::json!({
                        "toolResults": pending_tool_results.clone()
                    });
                }

                if !tools_arr.is_empty() && history_len == 0 {
                    if user_msg["userInputMessage"]["userInputMessageContext"].is_null() {
                        user_msg["userInputMessage"]["userInputMessageContext"] =
                            serde_json::json!({});
                    }
                    user_msg["userInputMessage"]["userInputMessageContext"]["tools"] =
                        Value::Array(tools_arr.to_vec());
                }

                history.push(user_msg);
            }
            Some("assistant") => {
                let content = pending_assistant_content.join("\n\n").trim().to_string();
                let content = if content.is_empty() {
                    "...".to_string()
                } else {
                    content
                };
                history.push(serde_json::json!({
                    "assistantResponseMessage": { "content": content }
                }));
            }
            _ => {}
        }
    };

    let mut i = 0;
    while i < claude_messages.len() {
        let msg = &claude_messages[i];
        let mut role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Claude/system/developer/tool map to "user" in Kiro's input message
        if role == "system" || role == "developer" || role == "tool" {
            role = "user".to_string();
        }

        if Some(&role) != current_role.as_ref() && current_role.is_some() {
            let hist_len = history.len();
            flush_pending(
                &mut history,
                &mut pending_user_content,
                &mut pending_assistant_content,
                &mut pending_tool_results,
                &mut pending_images,
                &current_role,
                &tools_array,
                hist_len,
            );
        }
        current_role = Some(role.clone());

        if role == "user" {
            let content = msg.get("content").cloned().unwrap_or(Value::Null);
            let mut text_content = String::new();

            if let Some(s) = content.as_str() {
                text_content = s.to_string();
            } else if let Some(arr) = content.as_array() {
                let mut text_parts = Vec::new();
                for c in arr {
                    match c.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                        // Claude image block: {type:"image", source:{type:"base64", media_type, data}}
                        Some("image") => {
                            if let Some(source) = c.get("source") {
                                if source.get("type").and_then(|v| v.as_str()) == Some("base64") {
                                    if let (Some(media_type), Some(data)) = (
                                        source.get("media_type").and_then(|v| v.as_str()),
                                        source.get("data").and_then(|v| v.as_str()),
                                    ) {
                                        let format =
                                            media_type.split('/').nth(1).unwrap_or(media_type);
                                        pending_images.push(serde_json::json!({
                                            "format": format,
                                            "source": {"bytes": data}
                                        }));
                                    }
                                }
                            }
                        }
                        // Claude tool_result block
                        Some("tool_result") => {
                            let tool_text =
                                if let Some(tc_arr) = c.get("content").and_then(|v| v.as_array()) {
                                    tc_arr
                                        .iter()
                                        .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                } else {
                                    c.get("content")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string()
                                };
                            if let Some(tool_use_id) = c.get("tool_use_id").and_then(|v| v.as_str())
                            {
                                pending_tool_results.push(serde_json::json!({
                                    "toolUseId": tool_use_id,
                                    "status": "success",
                                    "content": [{"text": tool_text}]
                                }));
                            }
                        }
                        _ => {}
                    }
                }
                text_content = text_parts.join("\n");
            }

            if msg.get("role").and_then(|v| v.as_str()) == Some("tool") {
                let tool_content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(tool_call_id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                    pending_tool_results.push(serde_json::json!({
                        "toolUseId": tool_call_id,
                        "status": "success",
                        "content": [{"text": tool_content}]
                    }));
                }
            } else if !text_content.is_empty() {
                pending_user_content.push(text_content);
            }
        } else if role == "assistant" {
            let content = msg.get("content").cloned().unwrap_or(Value::Null);
            let mut text_content = String::new();
            let mut tool_uses: Vec<Value> = Vec::new();

            if let Some(arr) = content.as_array() {
                let text_blocks: Vec<String> = arr
                    .iter()
                    .filter(|c| c.get("type").and_then(|v| v.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                    .map(|s| s.to_string())
                    .collect();
                text_content = text_blocks.join("\n").trim().to_string();

                // Claude tool_use blocks
                for c in arr {
                    if c.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                        let id = c
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let name = c
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = c
                            .get("input")
                            .cloned()
                            .unwrap_or(Value::Object(serde_json::Map::new()));
                        tool_uses.push(serde_json::json!({
                            "toolUseId": id,
                            "name": name,
                            "input": input
                        }));
                    }
                }
            } else if let Some(s) = content.as_str() {
                text_content = s.trim().to_string();
            }

            if !text_content.is_empty() {
                pending_assistant_content.push(text_content);
            }

            if !tool_uses.is_empty() {
                let hist_len = history.len();
                flush_pending(
                    &mut history,
                    &mut pending_user_content,
                    &mut pending_assistant_content,
                    &mut pending_tool_results,
                    &mut pending_images,
                    &current_role,
                    &tools_array,
                    hist_len,
                );

                if let Some(last) = history.last_mut() {
                    if last.get("assistantResponseMessage").is_some() {
                        last["assistantResponseMessage"]["toolUses"] = Value::Array(tool_uses);
                    }
                }
                current_role = None;
            }
        }
        i += 1;
    }

    if current_role.is_some() {
        let hist_len = history.len();
        flush_pending(
            &mut history,
            &mut pending_user_content,
            &mut pending_assistant_content,
            &mut pending_tool_results,
            &mut pending_images,
            &current_role,
            &tools_array,
            hist_len,
        );
    }

    // Pop last userInputMessage as currentMessage
    let mut current_message: Option<Value> = None;
    for i in (0..history.len()).rev() {
        if history[i].get("userInputMessage").is_some() {
            current_message = Some(history.remove(i));
            break;
        }
    }

    // Grab tools from first history item
    let first_history_tools = history
        .first()
        .and_then(|h| h.get("userInputMessage"))
        .and_then(|m| m.get("userInputMessageContext"))
        .and_then(|c| c.get("tools"))
        .cloned();

    // Clean up history
    for item in &mut history {
        if let Some(ctx) = item
            .get_mut("userInputMessage")
            .and_then(|m| m.get_mut("userInputMessageContext"))
        {
            if ctx.get("tools").is_some() {
                ctx.as_object_mut().unwrap().remove("tools");
            }
            if ctx.as_object().is_some_and(|o| o.is_empty()) {
                item["userInputMessage"]
                    .as_object_mut()
                    .unwrap()
                    .remove("userInputMessageContext");
            }
        }
        if let Some(model_id) = item
            .get_mut("userInputMessage")
            .and_then(|m| m.get_mut("modelId"))
        {
            if model_id.as_str().is_none_or(|s| s.is_empty()) {
                *model_id = Value::String(model.to_string());
            }
        }
    }

    // Merge consecutive user messages
    let mut merged_history: Vec<Value> = Vec::new();
    for item in &history {
        if item.get("userInputMessage").is_some()
            && merged_history
                .last()
                .and_then(|h| h.get("userInputMessage"))
                .is_some()
        {
            if let Some(prev) = merged_history.last_mut() {
                let prev_content = prev["userInputMessage"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let curr_content = item["userInputMessage"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                prev["userInputMessage"]["content"] =
                    Value::String(format!("{}\n\n{}", prev_content, curr_content));
            }
        } else {
            merged_history.push(item.clone());
        }
    }

    // Merge tools into currentMessage
    if let (Some(tools), Some(ref mut cm)) = (first_history_tools, &mut current_message) {
        if cm["userInputMessage"]["userInputMessageContext"]
            .get("tools")
            .is_none()
        {
            if cm["userInputMessage"]["userInputMessageContext"].is_null() {
                cm["userInputMessage"]["userInputMessageContext"] = serde_json::json!({});
            }
            cm["userInputMessage"]["userInputMessageContext"]["tools"] = tools;
        }
    }

    // Build final content with prefix
    let mut final_content = current_message
        .as_ref()
        .and_then(|m| m.get("userInputMessage"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut prefix_parts: Vec<String> = Vec::new();
    let thinking_enabled = body.get("reasoning_effort").is_some()
        || body
            .get("thinking")
            .and_then(|t| t.get("type"))
            .and_then(|v| v.as_str())
            == Some("enabled");
    if thinking_enabled {
        prefix_parts.push("<thinking_mode>enabled</thinking_mode>".to_string());
    }
    let timestamp = chrono::Utc::now().to_rfc3339();
    prefix_parts.push(format!("[Context: Current time is {}]", timestamp));

    let is_agentic = model.ends_with("-agentic");
    if is_agentic {
        prefix_parts.push(
            "[Agentic mode enabled: Use chunked file writes for large operations.]".to_string(),
        );
    }

    let upstream_model = if is_agentic {
        model.trim_end_matches("-agentic")
    } else {
        model
    };

    if !prefix_parts.is_empty() {
        final_content = format!("{}\n\n{}", prefix_parts.join("\n\n"), final_content);
    }

    let mut payload = serde_json::json!({
        "conversationState": {
            "chatTriggerType": "MANUAL",
            "conversationId": "auto-generated",
            "currentMessage": {
                "userInputMessage": {
                    "content": final_content,
                    "modelId": upstream_model,
                    "origin": "AI_EDITOR"
                }
            },
            "history": merged_history
        }
    });

    // Add profileArn if present
    if let Some(profile_arn) = credentials
        .and_then(|c| c.get("providerSpecificData"))
        .and_then(|d| d.get("profileArn"))
        .and_then(|v| v.as_str())
    {
        payload["profileArn"] = Value::String(profile_arn.to_string());
    }

    // Preserve client's max_tokens; fall back to 32000 default
    let client_max_tokens: u64 = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(|v| v.as_u64())
        .filter(|&t| t > 0)
        .unwrap_or(32000);
    let max_tokens = client_max_tokens;
    let temperature = body.get("temperature");
    let top_p = body.get("top_p");
    if temperature.is_some() || top_p.is_some() {
        let mut config = serde_json::json!({"maxTokens": max_tokens});
        if let Some(t) = temperature {
            config["temperature"] = t.clone();
        }
        if let Some(t) = top_p {
            config["topP"] = t.clone();
        }
        payload["inferenceConfig"] = config;
    }

    // Tag upstream model for executor routing
    payload["_kiroUpstreamModel"] = Value::String(upstream_model.to_string());

    *body = payload;
    let _ = stream;
    true
}
