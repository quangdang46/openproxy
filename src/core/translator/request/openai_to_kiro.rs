//! OpenAI to Kiro request translator
//!
//! Converts OpenAI Chat Completions to Kiro/AWS CodeWhisperer format.

use crate::core::config::kiro_constants::{
    build_kiro_additional_model_request_fields_for_model, build_thinking_system_prefix,
    resolve_kiro_thinking_budget, uses_kiro_native_gpt_effort, HeaderLookup,
};
use crate::core::translator::concerns::kiro_conversation::{
    canonicalize_kiro_conversation, normalize_kiro_tool_specs,
};
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

pub fn openai_to_kiro_request(
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
                        Some("image_url") => {
                            if let Some(url) = c
                                .get("image_url")
                                .and_then(|u| u.get("url"))
                                .and_then(|v| v.as_str())
                            {
                                if let Some(data_uri) = url.strip_prefix("data:") {
                                    if let Some((mime, b64)) = data_uri.split_once(";base64,") {
                                        let format = mime.split('/').nth(1).unwrap_or(mime);
                                        pending_images.push(serde_json::json!({
                                            "format": format,
                                            "source": {"bytes": b64}
                                        }));
                                    } else {
                                        text_parts.push(format!("[Image: {}]", url));
                                    }
                                } else if url.starts_with("http://") || url.starts_with("https://")
                                {
                                    text_parts.push(format!("[Image: {}]", url));
                                }
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
                                // 9router openai-to-kiro.js:148 — status reflects is_error.
                                let is_err =
                                    c.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                                pending_tool_results.push(serde_json::json!({
                                    "toolUseId": tool_use_id,
                                    "status": if is_err { "error" } else { "success" },
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
                    // 9router openai-to-kiro.js:160 — is_error OR status === "error".
                    let is_err = msg
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        || msg.get("status").and_then(Value::as_str) == Some("error");
                    pending_tool_results.push(serde_json::json!({
                        "toolUseId": tool_call_id,
                        "status": if is_err { "error" } else { "success" },
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

                tool_uses = arr
                    .iter()
                    .filter(|c| c.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                    .cloned()
                    .collect();
            } else if let Some(s) = content.as_str() {
                text_content = s.trim().to_string();
            }

            if let Some(tc_arr) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                if !tc_arr.is_empty() {
                    tool_uses = tc_arr.to_vec();
                }
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
                                if let Some(func) = tc.get("function") {
                                    let id = tc
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown")
                                        .to_string();
                                    let name = func
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let args = func
                                        .get("arguments")
                                        .map(|v| {
                                            if let Some(s) = v.as_str() {
                                                safe_parse_json(s)
                                            } else {
                                                v.clone()
                                            }
                                        })
                                        .unwrap_or(Value::Object(serde_json::Map::new()));
                                    serde_json::json!({
                                        "toolUseId": id,
                                        "name": name,
                                        "input": args
                                    })
                                } else {
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
                                }
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

    // Build system / volatile prefixes (9router openai-to-kiro + applyKiroSessionReplay).
    // System-stable content goes into content_prefix (frozen on msg0); volatile
    // current-time only goes into current_content_prefix (current turn only).
    let mut system_prompt_parts: Vec<String> = Vec::new();

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
        // Body still has original OpenAI shape here (we replace *body later).
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
                "content": "",
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

    // JS parity (openai-to-kiro.js:309, 416-421): inferenceConfig is always
    // emitted with the hardcoded constant maxTokens = 32000 (JS ignores
    // body.max_tokens here).
    let mut config = serde_json::json!({"maxTokens": 32000u64});
    if let Some(t) = body.get("temperature") {
        config["temperature"] = t.clone();
    }
    if let Some(t) = body.get("top_p") {
        config["topP"] = t.clone();
    }
    payload["inferenceConfig"] = config;

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

    #[test]
    fn tool_msg_is_error_maps_to_error_text() {
        let mut body = json!({
            "model": "gpt-4",
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
        openai_to_kiro_request("kiro-model", &mut body, false, None);
        let content = current_message_content(&body);
        assert!(content.contains("[Tool result (error): boom]"));
    }

    #[test]
    fn tool_msg_status_error_maps_to_error_text() {
        let mut body = json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "tool",
                    "tool_call_id": "t2",
                    "content": "err",
                    "status": "error"
                }
            ]
        });
        openai_to_kiro_request("kiro-model", &mut body, false, None);
        let content = current_message_content(&body);
        assert!(content.contains("[Tool result (error): err]"));
    }

    #[test]
    fn tool_msg_success_maps_to_success_text() {
        let mut body = json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "tool",
                    "tool_call_id": "t3",
                    "content": "ok"
                }
            ]
        });
        openai_to_kiro_request("kiro-model", &mut body, false, None);
        let content = current_message_content(&body);
        assert!(content.contains("[Tool result: ok]"));
    }

    /// Guard test per bead P0-A6 (JS openai-to-kiro.test.js:289-302):
    /// reasoning_effort low for claude-sonnet-4.6 emits
    /// `<max_thinking_length>1024</max_thinking_length>` AND
    /// additionalModelRequestFields with the adaptive-thinking shape.
    #[test]
    fn reasoning_effort_low_emits_max_thinking_length_1024() {
        let mut body = json!({
            "model": "claude-sonnet-4.6",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "low"
        });
        openai_to_kiro_request("claude-sonnet-4.6", &mut body, false, None);
        let system_prompt = body["systemPrompt"].as_str().unwrap_or("");
        assert!(
            system_prompt.contains("<max_thinking_length>1024</max_thinking_length>"),
            "systemPrompt should carry <max_thinking_length>1024</max_thinking_length>, got: {system_prompt}"
        );
        assert!(
            system_prompt.contains("<thinking_mode>enabled</thinking_mode>"),
            "systemPrompt should carry <thinking_mode>enabled</thinking_mode>, got: {system_prompt}"
        );
        assert_eq!(
            body["additionalModelRequestFields"],
            json!({
                "thinking": {"type": "adaptive", "display": "summarized"},
                "output_config": {"effort": "low"}
            })
        );
    }

    /// Guard test per bead P0-A6 (JS lines 386-400): reasoning_effort none →
    /// no legacy prompt tags and no additionalModelRequestFields.
    #[test]
    fn reasoning_effort_none_emits_nothing() {
        let mut body = json!({
            "model": "claude-sonnet-4.6",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "none"
        });
        openai_to_kiro_request("claude-sonnet-4.6", &mut body, false, None);
        let system_prompt = body["systemPrompt"].as_str().unwrap_or("");
        assert!(
            !system_prompt.contains("<thinking_mode>"),
            "systemPrompt should not contain <thinking_mode>, got: {system_prompt}"
        );
        assert!(
            !system_prompt.contains("<max_thinking_length>"),
            "systemPrompt should not contain <max_thinking_length>, got: {system_prompt}"
        );
        assert!(
            body.get("additionalModelRequestFields").is_none(),
            "additionalModelRequestFields should be absent, got: {}",
            body
        );
    }

    /// Guard test per bead P0-A6 (JS lines 323-338): GPT-5.6 reasoning.effort
    /// high → additionalModelRequestFields {reasoning:{effort:high}} with NO
    /// legacy prompt tags.
    #[test]
    fn gpt56_reasoning_effort_maps_to_reasoning_fields() {
        let mut body = json!({
            "model": "gpt-5.6-sol",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning": {"effort": "high"}
        });
        openai_to_kiro_request("gpt-5.6-sol", &mut body, false, None);
        let system_prompt = body["systemPrompt"].as_str().unwrap_or("");
        assert!(
            !system_prompt.contains("<thinking_mode>"),
            "systemPrompt should not contain <thinking_mode>, got: {system_prompt}"
        );
        assert_eq!(
            body["additionalModelRequestFields"],
            json!({"reasoning": {"effort": "high"}})
        );
    }

    /// Guard test per bead P0-A6 (JS lines 370-384): unsupported efforts
    /// (auto/minimal/ultra) → NO additionalModelRequestFields but legacy
    /// `<thinking_mode>enabled</thinking_mode>` + `<max_thinking_length>`
    /// fallback.
    #[test]
    fn unsupported_effort_falls_back_to_legacy_tags() {
        for effort in ["auto", "minimal", "ultra"] {
            let mut body = json!({
                "model": "claude-sonnet-4.6",
                "messages": [{"role": "user", "content": "hi"}],
                "reasoning_effort": effort
            });
            openai_to_kiro_request("claude-sonnet-4.6", &mut body, false, None);
            let system_prompt = body["systemPrompt"].as_str().unwrap_or("");
            assert!(
                system_prompt.contains("<thinking_mode>enabled</thinking_mode>"),
                "effort {effort}: expected legacy <thinking_mode> tag, got: {system_prompt}"
            );
            assert!(
                system_prompt.contains("<max_thinking_length>"),
                "effort {effort}: expected legacy <max_thinking_length>, got: {system_prompt}"
            );
            assert!(
                body.get("additionalModelRequestFields").is_none(),
                "effort {effort}: additionalModelRequestFields should be absent, got: {}",
                body
            );
        }
    }

    /// Guard test per bead P0-A7 (JS openai-to-kiro.js:309, 416-421):
    /// inferenceConfig is ALWAYS emitted with the constant maxTokens = 32000,
    /// even with no temperature/topP in the body.
    #[test]
    fn inference_config_always_emitted_max_tokens_32000() {
        let mut body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}]
        });
        openai_to_kiro_request("gpt-4", &mut body, false, None);
        assert_eq!(
            body["inferenceConfig"],
            json!({"maxTokens": 32000}),
            "inferenceConfig should always be emitted with maxTokens=32000, got: {}",
            body
        );
    }
}
