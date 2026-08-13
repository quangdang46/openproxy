//! Claude to Kiro request translator
//!
//! Converts Claude Messages API format (with `messages`, `system`, `max_tokens`,
//! `tools`, etc.) to Kiro/AWS CodeWhisperer format.
//!
//! The Kiro format is the same target as `openai_to_kiro_request` produces,
//! so this implementation mirrors that approach but reads Claude-format input.

use crate::core::config::kiro_constants::{
    build_kiro_additional_model_request_fields_for_model, build_thinking_system_prefix,
    resolve_kiro_thinking_budget, uses_kiro_native_gpt_effort, HeaderLookup,
};
use crate::core::translator::concerns::kiro_conversation::{
    canonicalize_kiro_conversation, normalize_kiro_tool_specs,
};
use serde_json::Value;

/// Convert a Claude Messages API request body to Kiro format.
///
/// Handles:
/// - `system` at the top level → prepended as instructions
/// - `messages` array with `content` blocks (text, image, tool_use, tool_result, thinking)
/// - `tools` with `input_schema` (Claude's name for the JSON schema)
/// - `tool_choice`, `max_tokens`, `temperature`, `top_p`, `thinking`
/// - `reasoning_effort`
pub fn claude_to_kiro_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    credentials: Option<&Value>,
) -> bool {
    let messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let tools = body.get("tools").cloned().unwrap_or(Value::Null);
    let system = body
        .get("system")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            body.get("system").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        })
        .or_else(|| {
            body.get("system")
                .and_then(|v| v.as_object())
                .and_then(|obj| {
                    obj.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                })
        });

    let mut history: Vec<Value> = Vec::new();
    let mut pending_user_content: Vec<String> = Vec::new();
    let mut pending_assistant_content: Vec<String> = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();
    let mut pending_images: Vec<Value> = Vec::new();
    let mut current_role: Option<String> = None;

    let flush_pending = |history: &mut Vec<Value>,
                         pending_user_content: &mut Vec<String>,
                         pending_assistant_content: &mut Vec<String>,
                         pending_tool_results: &mut Vec<Value>,
                         pending_images: &mut Vec<Value>,
                         current_role: &Option<String>| {
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
    while i < messages.len() {
        let msg = &messages[i];
        let mut role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Claude API uses "assistant" and "user" roles.
        // System messages come from the top-level `system` field.
        // "tool" messages (tool results) map to "user" role in Kiro.
        if role == "tool" || role == "system" || role == "developer" {
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
            );
        }
        current_role = Some(role.clone());

        if role == "user" {
            let content = msg.get("content").cloned().unwrap_or(Value::Null);

            // Claude content can be a string or an array of content blocks
            if let Some(s) = content.as_str() {
                pending_user_content.push(s.to_string());
            } else if let Some(arr) = content.as_array() {
                let mut text_parts: Vec<String> = Vec::new();
                for c in arr {
                    match c.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
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
                                // 9router claude-to-kiro.js:110 — status reflects is_error.
                                let is_err =
                                    c.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                                pending_tool_results.push(serde_json::json!({
                                    "toolUseId": tool_use_id,
                                    "status": if is_err { "error" } else { "success" },
                                    "content": [{"text": tool_text}]
                                }));
                            }
                        }
                        Some("thinking") => {
                            if let Some(thinking) = c.get("thinking").and_then(|v| v.as_str()) {
                                text_parts.push(thinking.to_string());
                            }
                        }
                        Some("redacted_thinking") => {
                            if let Some(data) = c.get("data").and_then(|v| v.as_str()) {
                                text_parts.push(data.to_string());
                            }
                        }
                        Some("signature") => {
                            if let Some(sig) = c.get("signature").and_then(|v| v.as_str()) {
                                text_parts.push(sig.to_string());
                            }
                        }
                        _ => {}
                    }
                }
                let text_content = text_parts.join("\n");
                if !text_content.is_empty() {
                    pending_user_content.push(text_content);
                }
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

                tool_uses = arr
                    .iter()
                    .filter(|c| c.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                    .cloned()
                    .collect();
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
                );

                if let Some(last) = history.last_mut() {
                    if last.get("assistantResponseMessage").is_some() {
                        let converted: Vec<Value> = tool_uses
                            .iter()
                            .map(|tc| {
                                let id = tc
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let name = tc
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let input = tc
                                    .get("input")
                                    .cloned()
                                    .unwrap_or(Value::Object(serde_json::Map::new()));
                                serde_json::json!({
                                    "toolUseId": id,
                                    "name": name,
                                    "input": input
                                })
                            })
                            .collect();
                        last["assistantResponseMessage"]["toolUses"] = Value::Array(converted);
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

    // If there's no current_message (e.g. conversation was empty or only had
    // tool calls), create one from the system prompt or a placeholder.
    if current_message.is_none() {
        current_message = Some(serde_json::json!({
            "userInputMessage": {
                "content": "continue",
                "modelId": ""
            }
        }));
    }

    // Build system / volatile prefixes (9router claude-to-kiro + applyKiroSessionReplay).
    // System-stable content goes into content_prefix (frozen on msg0); volatile
    // current-time only goes into current_content_prefix (current turn only).
    let mut system_prompt_parts: Vec<String> = Vec::new();

    // Prepend system prompt if present
    if let Some(ref sys) = system {
        if !sys.trim().is_empty() {
            system_prompt_parts.push(format!("<system>{}</system>", sys));
        }
    }

    // Check for -agentic suffix
    let is_agentic = model.ends_with("-agentic");
    if is_agentic {
        system_prompt_parts.push(
            "[Agentic mode enabled: Use chunked file writes for large operations.]".to_string(),
        );
    }

    let upstream_model = if is_agentic {
        model.trim_end_matches("-agentic")
    } else {
        model
    };

    // Resolve the Kiro thinking budget (9router resolveKiroThinkingBudget) and
    // push the legacy `<thinking_mode>`/`<max_thinking_length>` prefix when a
    // budget exists and the model does not use native GPT effort fields.
    let raw_headers = crate::core::utils::session_manager::credentials_raw_headers(credentials);
    let headers_lookup = raw_headers.as_ref().map(|h| h as &dyn HeaderLookup);
    let thinking_budget = resolve_kiro_thinking_budget(body, headers_lookup, upstream_model);
    let uses_native_gpt_effort = uses_kiro_native_gpt_effort(upstream_model, body);
    if let Some(budget) = thinking_budget {
        if !uses_native_gpt_effort {
            system_prompt_parts.push(build_thinking_system_prefix(Some(budget)));
        }
    }

    let system_prompt = system_prompt_parts.join("\n\n");
    let timestamp = chrono::Utc::now().to_rfc3339();
    let current_time_context = format!("[Context: Current time is {timestamp}]");
    let content_prefix = [system_prompt.as_str(), current_time_context.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n\n");

    // Resolve conversation-stable session identity (client header / body field,
    // or ephemeral one-shot for Kiro when no client id is present).
    let connection_id = crate::core::utils::session_manager::credentials_connection_id(credentials);
    let session_identity = crate::core::utils::session_manager::resolve_session_identity(
        raw_headers.as_ref(),
        // Body still has original Claude shape here (we replace *body later).
        Some(body),
        connection_id.as_deref(),
        "kiro",
    );
    let conversation_id = session_identity.session_id.clone();
    let continuation_id = crate::core::utils::session_manager::resolve_continuation_id(
        &conversation_id,
        connection_id.as_deref(),
        "kiro",
        session_identity.ephemeral,
    );

    let base_current = current_message.unwrap_or_else(|| {
        serde_json::json!({
            "userInputMessage": {
                "content": "continue",
                "modelId": upstream_model
            }
        })
    });

    let replay = crate::core::utils::kiro_session_replay::apply_kiro_session_replay(
        Some(&conversation_id),
        connection_id.as_deref(),
        upstream_model,
        &system_prompt,
        &content_prefix,
        &current_time_context,
        &merged_history,
        &base_current,
    );

    // Canonicalize the replayed conversation into the strict Kiro wire shape:
    // alternating user/assistant turns, adjacent tool-use/tool-result pairs with
    // reserved ids, and tool specs only on the final (current) user message.
    // Port of 9router `canonicalizeKiroConversation` (kiroConversation.js).
    let (specs, name_map) = normalize_kiro_tool_specs(&tools);
    let (canonical_history, canonical_current, _repairs, _valid) = canonicalize_kiro_conversation(
        &replay.history,
        &replay.current_message,
        upstream_model,
        &specs,
        &name_map,
    );

    let replay_current = canonical_current
        .get("userInputMessage")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({ "content": "" }));

    let mut user_input_message = serde_json::json!({
        "content": replay_current.get("content").and_then(|v| v.as_str()).unwrap_or(""),
        "modelId": upstream_model,
        "origin": "AI_EDITOR"
    });
    if let Some(ctx) = replay_current.get("userInputMessageContext") {
        user_input_message["userInputMessageContext"] = ctx.clone();
    }
    if let Some(images) = replay_current.get("images") {
        if images.as_array().is_some_and(|a| !a.is_empty()) {
            user_input_message["images"] = images.clone();
        }
    }

    let mut payload = serde_json::json!({
        "conversationState": {
            "chatTriggerType": "MANUAL",
            "conversationId": conversation_id,
            "agentContinuationId": continuation_id,
            "agentTaskType": "vibe",
            "currentMessage": {
                "userInputMessage": user_input_message
            },
            "history": canonical_history
        },
        "agentMode": "vibe"
    });

    if !system_prompt.is_empty() {
        payload["systemPrompt"] = Value::String(system_prompt);
    }

    // Native effort fields for supported models (9router
    // buildKiroAdditionalModelRequestFieldsForModel).
    if let Some(amrf) = build_kiro_additional_model_request_fields_for_model(body, upstream_model) {
        payload["additionalModelRequestFields"] = amrf;
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn current_message_content(body: &Value) -> String {
        body["conversationState"]["currentMessage"]["userInputMessage"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// A leading orphan tool_result (no preceding assistant tool_use) is
    /// flattened to text by canonicalizeKiroConversation — parity with JS
    /// `openai-to-kiro.test.js` ("salvage orphaned tool_result content as
    /// text"). The is_error flag survives as the `(error)` suffix.
    #[test]
    fn tool_result_is_error_maps_to_error_text() {
        let mut body = json!({
            "model": "claude-sonnet-4",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "t1",
                         "is_error": true, "content": "boom"}
                    ]
                }
            ]
        });
        claude_to_kiro_request("kiro-model", &mut body, false, None);
        let content = current_message_content(&body);
        assert!(content.contains("[Tool result (error): boom]"));
        // No structured toolResults survive — they were flattened to text.
        assert!(
            body["conversationState"]["currentMessage"]["userInputMessage"]
                ["userInputMessageContext"]
                .get("toolResults")
                .is_none()
        );
    }

    #[test]
    fn tool_result_no_is_error_maps_to_success_text() {
        let mut body = json!({
            "model": "claude-sonnet-4",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "t1",
                         "content": "ok"}
                    ]
                }
            ]
        });
        claude_to_kiro_request("kiro-model", &mut body, false, None);
        let content = current_message_content(&body);
        assert!(content.contains("[Tool result: ok]"));
    }
}
