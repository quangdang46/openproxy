//! Kiro to Claude response translator
//!
//! Converts Kiro SSE event stream to Anthropic Messages API SSE events
//! (message_start, content_block_start, content_block_delta,
//!  content_block_stop, message_delta, message_stop).
//!
//! Kiro events:
//!   assistantResponseEvent  → text delta
//!   reasoningContentEvent   → thinking block
//!   toolUseEvent            → tool_use content block
//!   messageStopEvent        → final stop
//!   usageEvent              → token usage metadata

use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::translator::registry::ResponseTransformState;

/// Helper: parse Kiro SSE JSON chunk and return typed Claude SSE lines.
pub fn kiro_to_claude_streaming(
    chunk: &[u8],
    state: &mut ResponseTransformState,
) -> Vec<String> {
    let val: Value = match serde_json::from_slice(chunk) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    // Fast path: already a Claude event
    if val
        .get("type")
        .and_then(|v| v.as_str())
        .map(|t| t.starts_with("message_") || t.starts_with("content_block_"))
        .unwrap_or(false)
    {
        return vec![format!(
            "data: {}\n\n",
            serde_json::to_string(&val).unwrap_or_default()
        )];
    }

    // Initialise Anthropic message-level state on first event
    let claude_state = &mut state.anthropic.claude_state;
    if !claude_state.contains_key("message_id") {
        let msg_id = format!(
            "msg_{:016x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        claude_state.insert("message_id".into(), Value::String(msg_id));
        claude_state.insert("model".into(), Value::String("kiro".to_string()));
        claude_state.insert(
            "created".into(),
            Value::Number(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .into(),
            ),
        );
        claude_state.insert("block_index".into(), Value::Number(0usize.into()));
        claude_state.insert("in_message".into(), Value::Bool(true));
        claude_state.insert("stop_emitted".into(), Value::Bool(false));
    }

    let msg_id = claude_state
        .get("message_id")
        .and_then(|v| v.as_str())
        .unwrap_or("msg_unknown")
        .to_string();
    let model = claude_state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("kiro")
        .to_string();

    let event_type = val
        .get("_eventType")
        .or_else(|| val.get("event"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut lines: Vec<String> = Vec::new();
    let block_index = claude_state
        .get("block_index")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    match event_type {
        "assistantResponseEvent" | _ if val.get("assistantResponseEvent").is_some() => {
            let content = val
                .get("assistantResponseEvent")
                .and_then(|v| v.get("content"))
                .or_else(|| val.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if content.is_empty() {
                return vec![];
            }

            let first_block = !claude_state
                .get("text_block_started")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if first_block {
                // Emit message_start if not yet emitted
                let msg_started = claude_state
                    .get("message_started")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !msg_started {
                    lines.push(format!(
                        "data: {}\n\n",
                        serde_json::json!({
                            "type": "message_start",
                            "message": {
                                "id": msg_id,
                                "type": "message",
                                "role": "assistant",
                                "content": [],
                                "model": model,
                                "stop_reason": null,
                                "stop_sequence": null,
                                "usage": {
                                    "input_tokens": 0,
                                    "output_tokens": 0
                                }
                            }
                        })
                    ));
                    claude_state.insert("message_started".into(), Value::Bool(true));
                }

                lines.push(format!(
                    "data: {}\n\n",
                    serde_json::json!({
                        "type": "content_block_start",
                        "index": block_index,
                        "content_block": {
                            "type": "text",
                            "text": ""
                        }
                    })
                ));
                claude_state.insert("text_block_started".into(), Value::Bool(true));
            }

            lines.push(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "text_delta",
                        "text": content
                    }
                })
            ));
        }

        "reasoningContentEvent" | _ if val.get("reasoningContentEvent").is_some() => {
            let reasoning = val.get("reasoningContentEvent").unwrap_or(&val);
            let content = reasoning
                .get("text")
                .or_else(|| reasoning.get("content"))
                .and_then(|v| v.as_str())
                .or_else(|| val.get("content").and_then(|v| v.as_str()))
                .unwrap_or("");

            if content.is_empty() {
                return vec![];
            }

            // Emit message_start if not yet emitted
            let msg_started = claude_state
                .get("message_started")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !msg_started {
                lines.push(format!(
                    "data: {}\n\n",
                    serde_json::json!({
                        "type": "message_start",
                        "message": {
                            "id": msg_id,
                            "type": "message",
                            "role": "assistant",
                            "content": [],
                            "model": model,
                            "stop_reason": null,
                            "stop_sequence": null,
                            "usage": {
                                "input_tokens": 0,
                                "output_tokens": 0
                            }
                        }
                    })
                ));
                claude_state.insert("message_started".into(), Value::Bool(true));
            }

            // Emit thinking content_block
            lines.push(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "type": "content_block_start",
                    "index": block_index,
                    "content_block": {
                        "type": "thinking",
                        "thinking": ""
                    }
                })
            ));

            lines.push(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": content
                    }
                })
            ));

            lines.push(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "type": "content_block_stop",
                    "index": block_index
                })
            ));

            claude_state.insert(
                "block_index".into(),
                Value::Number((block_index + 1).into()),
            );
        }

        "toolUseEvent" | _ if val.get("toolUseEvent").is_some() => {
            let tool_use = val.get("toolUseEvent").unwrap_or(&val);
            let tool_call_id = tool_use
                .get("toolUseId")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let tool_name = tool_use
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let tool_input = tool_use.get("input").cloned().unwrap_or(Value::Null);

            lines.push(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "type": "content_block_start",
                    "index": block_index,
                    "content_block": {
                        "type": "tool_use",
                        "id": tool_call_id,
                        "name": tool_name,
                        "input": tool_input
                    }
                })
            ));

            lines.push(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "type": "content_block_stop",
                    "index": block_index
                })
            ));

            claude_state.insert(
                "block_index".into(),
                Value::Number((block_index + 1).into()),
            );
        }

        "messageStopEvent" | "done" | _ if val.get("messageStopEvent").is_some() => {
            if claude_state
                .get("stop_emitted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return vec![];
            }

            // Close any open text block
            if claude_state
                .get("text_block_started")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                lines.push(format!(
                    "data: {}\n\n",
                    serde_json::json!({
                        "type": "content_block_stop",
                        "index": 0
                    })
                ));
            }

            let usage = claude_state.get("usage").cloned();
            let output_tokens = usage
                .as_ref()
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            lines.push(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": "end_turn",
                        "stop_sequence": null
                    },
                    "usage": {
                        "output_tokens": output_tokens
                    }
                })
            ));

            lines.push("data: {\"type\":\"message_stop\"}\n\n".to_string());

            claude_state.insert("stop_emitted".into(), Value::Bool(true));
        }

        "usageEvent" | _ if val.get("usageEvent").is_some() => {
            let usage = val.get("usageEvent").unwrap_or(&val);
            if let Some(usage_obj) = usage.as_object() {
                let input_tokens = usage_obj
                    .get("inputTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let output_tokens = usage_obj
                    .get("outputTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                claude_state.insert(
                    "usage".to_string(),
                    serde_json::json!({
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                    }),
                );
            }
            return vec![];
        }

        _ => {
            // Unknown event type — pass through as data-only line
            return vec![];
        }
    }

    lines
}
