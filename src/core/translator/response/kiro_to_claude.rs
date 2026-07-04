//! Kiro SSE → Claude SSE response translator
//!
//! Converts Kiro/AWS CodeWhisperer streaming SSE events to the Anthropic
//! Messages API SSE format (`message_start`, `content_block_*`,
//! `message_delta`, `message_stop`).
//!
//! Maps Kiro events:
//! - `assistantResponseEvent` → Claude text `content_block_start` + `content_block_delta`
//! - `reasoningContentEvent` → Claude thinking `content_block_start` + `content_block_delta`
//! - `toolUseEvent` → Claude tool_use `content_block_start`
//! - `usageEvent` → accumulate usage for `message_delta`
//! - `messageStopEvent` / `done` → Claude `content_block_stop` + `message_delta` + `message_stop`

use serde_json::{json, Value};

/// Convert one Kiro SSE byte chunk into zero or more Claude SSE event strings.
///
/// Uses `state.anthropic` to track:
/// - `claude_state`: generic map used by the Claude→OpenAI streaming wrapper
///   (message_id, model, toolCalls, etc.)
/// - `line_buffer`: buffering for partial JSON lines
/// - `current_block_index`: current content block index
/// - `text_accumulator`: accumulated text content
/// - `thinking_buffer`: accumulated thinking content
/// - `in_thinking`: whether currently in a thinking block
/// - `message_id`: the message ID from the first event
/// - `model`: the model name
pub fn kiro_to_claude_streaming(
    chunk: &[u8],
    state: &mut crate::core::translator::registry::ResponseTransformState,
) -> Vec<String> {
    let val: Value = match serde_json::from_slice(chunk) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let anthropic = &mut state.anthropic;
    let inner = &mut anthropic.claude_state;

    let mut results: Vec<String> = Vec::new();

    let event_type = val
        .get("_eventType")
        .or_else(|| val.get("event"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // ── helper to emit a Claude SSE event as a data: line ────────────────
    let mut emit = |event: Value| {
        results.push(format!(
            "data: {}\n\n",
            serde_json::to_string(&event).unwrap_or_default()
        ));
    };

    // ── assistantResponseEvent → text content block ────────────────────
    if event_type == "assistantResponseEvent" || val.get("assistantResponseEvent").is_some() {
        let event = val.get("assistantResponseEvent").unwrap_or(&val);
        let content = event
            .get("content")
            .or_else(|| val.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if content.is_empty() {
            return results;
        }

        ensure_message_start(inner, &val, &mut emit);

        // Close thinking block if open
        if anthropic.in_thinking {
            let block_idx = anthropic.current_block_index.unwrap_or(0);
            emit(json!({"type": "content_block_stop", "index": block_idx}));
            anthropic.in_thinking = false;
        }

        let block_idx = anthropic.current_block_index.unwrap_or(0);

        // Start a text block if not already started
        if anthropic.text_accumulator.is_empty() {
            emit(json!({
                "type": "content_block_start",
                "index": block_idx,
                "content_block": {"type": "text", "text": ""}
            }));
        }

        anthropic.text_accumulator.push_str(content);

        emit(json!({
            "type": "content_block_delta",
            "index": block_idx,
            "delta": {"type": "text_delta", "text": content}
        }));

        return results;
    }

    // ── reasoningContentEvent → thinking content block ──────────────────
    if event_type == "reasoningContentEvent" || val.get("reasoningContentEvent").is_some() {
        let event = val.get("reasoningContentEvent").unwrap_or(&val);
        let content = event
            .get("text")
            .or_else(|| event.get("content"))
            .and_then(|v| v.as_str())
            .or_else(|| val.get("content").and_then(|v| v.as_str()))
            .unwrap_or("");

        if content.is_empty() {
            return results;
        }

        ensure_message_start(inner, &val, &mut emit);

        if !anthropic.in_thinking {
            let block_idx = anthropic.current_block_index.unwrap_or(0);
            anthropic.in_thinking = true;
            emit(json!({
                "type": "content_block_start",
                "index": block_idx,
                "content_block": {"type": "thinking", "thinking": ""}
            }));
        }

        let block_idx = anthropic.current_block_index.unwrap_or(0);
        anthropic.thinking_buffer.push_str(content);

        emit(json!({
            "type": "content_block_delta",
            "index": block_idx,
            "delta": {"type": "thinking_delta", "thinking": content}
        }));

        return results;
    }

    // ── toolUseEvent → tool_use content block ──────────────────────────
    if event_type == "toolUseEvent" || val.get("toolUseEvent").is_some() {
        let event = val.get("toolUseEvent").unwrap_or(&val);
        let tool_use_id = event
            .get("toolUseId")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let tool_name = event
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let tool_input = event.get("input").cloned().unwrap_or(Value::Null);

        ensure_message_start(inner, &val, &mut emit);

        // Close any open block (text or thinking)
        let block_idx = anthropic.current_block_index.unwrap_or(0);
        if !anthropic.text_accumulator.is_empty() || anthropic.in_thinking {
            emit(json!({"type": "content_block_stop", "index": block_idx}));
            anthropic.text_accumulator.clear();
            anthropic.in_thinking = false;
            anthropic.thinking_buffer.clear();
        }

        // Create a new block index for the tool_use
        let next_idx = if let Some(idx) = inner.get("nextToolBlockIndex").and_then(|v| v.as_u64()) {
            idx
        } else {
            0u64
        };
        let tool_block_idx = anthropic.current_block_index.unwrap_or(0) + 1;
        inner.insert(
            "nextToolBlockIndex".into(),
            Value::from(tool_block_idx + 1),
        );
        inner.insert(
            "toolBlockIndex".into(),
            Value::from(tool_block_idx),
        );

        // Stash tool info
        inner.insert(
            "lastToolId".into(),
            Value::String(tool_use_id.clone()),
        );
        inner.insert(
            "lastToolName".into(),
            Value::String(tool_name.clone()),
        );

        emit(json!({
            "type": "content_block_start",
            "index": tool_block_idx,
            "content_block": {
                "type": "tool_use",
                "id": tool_use_id,
                "name": tool_name,
                "input": tool_input
            }
        }));

        // Emit tool input as a content_block_delta (input_json_delta)
        let input_str = serde_json::to_string(&tool_input).unwrap_or_else(|_| "{}".to_string());
        if !input_str.is_empty() && input_str != "{}" {
            emit(json!({
                "type": "content_block_delta",
                "index": tool_block_idx,
                "delta": {"type": "input_json_delta", "partial_json": input_str}
            }));
        }

        // Immediately close the tool block (Kiro sends complete tool use in one event)
        emit(json!({"type": "content_block_stop", "index": tool_block_idx}));

        return results;
    }

    // ── usageEvent → accumulate usage data ─────────────────────────────
    if event_type == "usageEvent" || val.get("usageEvent").is_some() {
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

            inner.insert(
                "usage".into(),
                json!({
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                }),
            );
        }
        return results;
    }

    // ── messageStopEvent / done → close blocks + message_delta + message_stop ─
    if event_type == "messageStopEvent"
        || event_type == "done"
        || val.get("messageStopEvent").is_some()
    {
        // Close any open content block
        let block_idx = anthropic.current_block_index.unwrap_or(0);
        if !anthropic.text_accumulator.is_empty() || anthropic.in_thinking {
            emit(json!({"type": "content_block_stop", "index": block_idx}));
            anthropic.text_accumulator.clear();
            anthropic.thinking_buffer.clear();
            anthropic.in_thinking = false;
        }

        // Close tool block if one was emitted
        if let Some(tool_block_idx) = inner.get("toolBlockIndex").and_then(|v| v.as_u64()) {
            emit(json!({"type": "content_block_stop", "index": tool_block_idx}));
            inner.remove("toolBlockIndex");
        }

        // Continue to next block index for the next message
        let next_idx = block_idx + 1;
        inner.insert("nextToolBlockIndex".into(), Value::from(next_idx));

        let usage = inner
            .get("usage")
            .cloned()
            .unwrap_or_else(|| json!({"input_tokens": 0, "output_tokens": 0}));

        emit(json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": usage
        }));

        emit(json!({"type": "message_stop"}));

        return results;
    }

    results
}

/// Emit the `message_start` event if it hasn't been sent yet for this stream.
/// Extracts model info and generates a message ID.
fn ensure_message_start(
    state: &mut serde_json::Map<String, Value>,
    val: &Value,
    emit: &mut impl FnMut(Value),
) {
    if state.contains_key("messageStartSent") {
        return;
    }
    state.insert("messageStartSent".into(), Value::Bool(true));

    let message_id = format!("msg_{}", chrono::Utc::now().timestamp_millis());
    state.insert("messageId".into(), Value::String(message_id.clone()));

    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    emit(json!({
        "type": "message_start",
        "message": {
            "id": message_id,
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0}
        }
    }));
}
