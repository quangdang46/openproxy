//! OpenAI Responses API response translator (both directions).

use serde_json::Value;

/// Whether a tool name is a custom (freeform) tool, per translator-only
/// `_customToolNames` metadata threaded through streaming state (JS
/// `isCustomTool` in open-sse/translator/response/openai-responses.js:261).
fn is_custom_tool(state: &serde_json::Map<String, Value>, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let in_list = |key: &str| {
        state.get(key).and_then(|v| match v {
            Value::String(s) => Some(s.split(',').any(|n| n.trim() == name)),
            Value::Array(a) => Some(
                a.iter()
                    .filter_map(|n| n.as_str())
                    .any(|n| n.trim() == name),
            ),
            _ => None,
        })
    };
    in_list("customToolNames").unwrap_or(false)
}

/// Unwrap the Chat JSON wrapper Codex puts around custom-tool input:
/// `{"input":"..."}` → the freeform program. Falls back to the raw text for
/// incomplete fragments (JS `extractCustomToolInput`, same file:265-272).
fn extract_custom_tool_input(arguments_text: &str) -> String {
    if arguments_text.is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(arguments_text) {
        Ok(Value::Object(map)) => match map.get("input") {
            Some(Value::String(s)) => s.clone(),
            Some(_) => arguments_text.to_string(),
            None => arguments_text.to_string(),
        },
        _ => arguments_text.to_string(),
    }
}

fn start_reasoning(state: &mut serde_json::Map<String, Value>, events: &mut Vec<Value>, idx: u64) {
    if state.get("reasoningId").is_none() || state["reasoningId"].is_null() {
        let reasoning_id = format!("rs_{}_{}", state["responseId"].as_str().unwrap_or(""), idx);
        state.insert(
            "reasoningId".to_string(),
            Value::String(reasoning_id.clone()),
        );
        state.insert("reasoningIndex".to_string(), Value::Number(idx.into()));
        emit(
            events,
            state,
            "response.output_item.added",
            serde_json::json!({
                "type": "response.output_item.added",
                "output_index": idx,
                "item": {"id": reasoning_id, "type": "reasoning", "summary": []}
            }),
        );
        emit(
            events,
            state,
            "response.reasoning_summary_part.added",
            serde_json::json!({
                "type": "response.reasoning_summary_part.added",
                "item_id": reasoning_id,
                "output_index": idx,
                "summary_index": 0,
                "part": {"type": "summary_text", "text": ""}
            }),
        );
        state.insert("reasoningPartAdded".to_string(), Value::Bool(true));
    }
}

fn emit_reasoning_delta(
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    text: &str,
) {
    if text.is_empty() {
        return;
    }
    state["reasoningBuf"] = Value::String(format!(
        "{}{}",
        state["reasoningBuf"].as_str().unwrap_or(""),
        text
    ));
    let reasoning_id = state["reasoningId"].as_str().unwrap_or("").to_string();
    let reasoning_idx = state
        .get("reasoningIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    emit(
        events,
        state,
        "response.reasoning_summary_text.delta",
        serde_json::json!({
            "type": "response.reasoning_summary_text.delta",
            "item_id": reasoning_id,
            "output_index": reasoning_idx,
            "summary_index": 0,
            "delta": text
        }),
    );
}

fn close_reasoning(state: &mut serde_json::Map<String, Value>, events: &mut Vec<Value>) {
    if state.get("reasoningId").and_then(|v| v.as_str()).is_some()
        && state.get("reasoningDone").and_then(|v| v.as_bool()) == Some(false)
    {
        state.insert("reasoningDone".to_string(), Value::Bool(true));
        let reasoning_id = state["reasoningId"].as_str().unwrap_or("").to_string();
        let reasoning_idx = state
            .get("reasoningIndex")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let reasoning_buf = state["reasoningBuf"].as_str().unwrap_or("").to_string();
        emit(
            events,
            state,
            "response.reasoning_summary_text.done",
            serde_json::json!({
                "type": "response.reasoning_summary_text.done",
                "item_id": reasoning_id,
                "output_index": reasoning_idx,
                "summary_index": 0,
                "text": reasoning_buf
            }),
        );
        emit(
            events,
            state,
            "response.reasoning_summary_part.done",
            serde_json::json!({
                "type": "response.reasoning_summary_part.done",
                "item_id": reasoning_id,
                "output_index": reasoning_idx,
                "summary_index": 0,
                "part": {"type": "summary_text", "text": reasoning_buf}
            }),
        );
        emit(
            events,
            state,
            "response.output_item.done",
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": reasoning_idx,
                "item": {
                    "id": reasoning_id,
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": reasoning_buf}]
                }
            }),
        );
    }
}

fn close_message(
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    idx_key: &str,
) {
    let done = state
        .get("msgItemDone")
        .and_then(|v| v.get(idx_key))
        .is_some();
    let added = state
        .get("msgItemAdded")
        .and_then(|v| v.get(idx_key))
        .is_some();
    if !added || done {
        return;
    }
    if let Some(done_map) = state.get_mut("msgItemDone").and_then(|v| v.as_object_mut()) {
        done_map.insert(idx_key.to_string(), Value::Bool(true));
    }
    let msg_id = state
        .get(&format!("msgId_{}", idx_key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let full_text = state
        .get("msgTextBuf")
        .and_then(|v| v.get(idx_key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let idx_num: u64 = idx_key.parse().unwrap_or(0);
    emit(
        events,
        state,
        "response.output_text.done",
        serde_json::json!({
            "type": "response.output_text.done",
            "item_id": msg_id,
            "output_index": idx_num,
            "content_index": 0,
            "text": full_text,
            "logprobs": []
        }),
    );
    emit(
        events,
        state,
        "response.content_part.done",
        serde_json::json!({
            "type": "response.content_part.done",
            "item_id": msg_id,
            "output_index": idx_num,
            "content_index": 0,
            "part": {"type": "output_text", "annotations": [], "logprobs": [], "text": full_text}
        }),
    );
    emit(
        events,
        state,
        "response.output_item.done",
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": idx_num,
            "item": {
                "id": msg_id,
                "type": "message",
                "content": [{"type": "output_text", "annotations": [], "logprobs": [], "text": full_text}],
                "role": "assistant"
            }
        }),
    );
}

fn close_tool_call(
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    idx_key: &str,
) {
    let call_id = state
        .get("funcCallIds")
        .and_then(|v| v.get(idx_key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if call_id.is_empty() {
        return;
    }
    let done = state
        .get("funcItemDone")
        .and_then(|v| v.get(idx_key))
        .is_some();
    if done {
        return;
    }
    if let Some(done_map) = state
        .get_mut("funcItemDone")
        .and_then(|v| v.as_object_mut())
    {
        done_map.insert(idx_key.to_string(), Value::Bool(true));
    }
    if let Some(done_map) = state
        .get_mut("funcArgsDone")
        .and_then(|v| v.as_object_mut())
    {
        done_map.insert(idx_key.to_string(), Value::Bool(true));
    }
    let args = state
        .get("funcArgsBuf")
        .and_then(|v| v.get(idx_key))
        .and_then(|v| v.as_str())
        .unwrap_or("{}")
        .to_string();
    let name = state
        .get("funcNames")
        .and_then(|v| v.get(idx_key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let custom = is_custom_tool(state, &name);
    let idx_num: u64 = idx_key.parse().unwrap_or(0);
    if custom {
        let input = extract_custom_tool_input(&args);
        emit(
            events,
            state,
            "response.custom_tool_call_input.delta",
            serde_json::json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": format!("ctc_{}", call_id),
                "output_index": idx_num,
                "delta": input
            }),
        );
        emit(
            events,
            state,
            "response.custom_tool_call_input.done",
            serde_json::json!({
                "type": "response.custom_tool_call_input.done",
                "item_id": format!("ctc_{}", call_id),
                "output_index": idx_num,
                "input": input
            }),
        );
    } else {
        emit(
            events,
            state,
            "response.function_call_arguments.done",
            serde_json::json!({
                "type": "response.function_call_arguments.done",
                "item_id": format!("fc_{}", call_id),
                "output_index": idx_num,
                "arguments": args
            }),
        );
    }
    emit(
        events,
        state,
        "response.output_item.done",
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": idx_num,
            "item": if custom {
                serde_json::json!({
                    "id": format!("ctc_{}", call_id),
                    "type": "custom_tool_call",
                    "input": extract_custom_tool_input(&args),
                    "call_id": call_id,
                    "name": name
                })
            } else {
                serde_json::json!({
                    "id": format!("fc_{}", call_id),
                    "type": "function_call",
                    "arguments": args,
                    "call_id": call_id,
                    "name": name
                })
            }
        }),
    );
}

fn send_completed(state: &mut serde_json::Map<String, Value>, events: &mut Vec<Value>) {
    if state.get("completedSent").and_then(|v| v.as_bool()) != Some(true) {
        state.insert("completedSent".to_string(), Value::Bool(true));
        emit(
            events,
            state,
            "response.completed",
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": state.get("responseId").and_then(|v| v.as_str()).unwrap_or(""),
                    "object": "response",
                    "created_at": state.get("created").and_then(|v| v.as_i64()).unwrap_or(0),
                    "status": "completed",
                    "background": false,
                    "error": null
                }
            }),
        );
    }
}

/// Tool-call + finish_reason tail of `chat_to_responses_response`, shared by
/// the normal path and the <think>-consumed early return (JS 102-116).
fn emit_tool_calls_and_finish(
    chunk: &Value,
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    idx: u64,
    idx_str: &str,
    delta: &Value,
) -> Vec<Value> {
    let _ = (idx, idx_str);
    if let Some(tool_calls) = delta
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .filter(|tc| !tc.is_empty())
    {
        emit_tool_calls_block(state, events, tool_calls);
    }
    if chunk
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("finish_reason"))
        .is_some()
    {
        emit_finish_block(state, events);
    }
    std::mem::take(events)
}

/// Increment the running `seq` counter in the SSE state map and return the new value.
fn next_seq(state: &mut serde_json::Map<String, Value>) -> u64 {
    let s = state.get("seq").and_then(|v| v.as_u64()).unwrap_or(0) + 1;
    state.insert("seq".to_string(), Value::Number(s.into()));
    s
}

/// Emit an SSE event into `events`, stamping `data.sequence_number` with the
/// next value from `state`.
fn emit(
    events: &mut Vec<Value>,
    state: &mut serde_json::Map<String, Value>,
    event_type: &str,
    data: Value,
) {
    let mut d = data;
    if let Some(obj) = d.as_object_mut() {
        obj.insert(
            "sequence_number".to_string(),
            Value::Number(next_seq(state).into()),
        );
    }
    events.push(serde_json::json!({"event": event_type, "data": d}));
}

pub fn chat_to_responses_response(
    chunk: &Value,
    state: &mut serde_json::Map<String, Value>,
) -> Vec<Value> {
    if chunk
        .get("choices")
        .and_then(|v| v.as_array())
        .is_none_or(|a| a.is_empty())
    {
        return vec![];
    }

    let mut events: Vec<Value> = Vec::new();

    if !state.contains_key("started") {
        state.insert("started".to_string(), Value::Bool(true));
        let resp_id = chunk
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| format!("resp_{}", s))
            .unwrap_or_else(|| format!("resp_{}", chrono::Utc::now().timestamp_millis()));
        state.insert("responseId".to_string(), Value::String(resp_id.clone()));
        state.insert(
            "created".to_string(),
            Value::Number(chrono::Utc::now().timestamp().into()),
        );
        state.insert("seq".to_string(), Value::Number(0.into()));
        state.insert("msgItemAdded".to_string(), serde_json::json!({}));
        state.insert("msgContentAdded".to_string(), serde_json::json!({}));
        state.insert("msgTextBuf".to_string(), serde_json::json!({}));
        state.insert("msgItemDone".to_string(), serde_json::json!({}));
        state.insert("funcNames".to_string(), serde_json::json!({}));
        state.insert("funcCallIds".to_string(), serde_json::json!({}));
        state.insert("funcArgsBuf".to_string(), serde_json::json!({}));
        state.insert("funcItemDone".to_string(), serde_json::json!({}));
        state.insert("funcArgsDone".to_string(), serde_json::json!({}));
        state.insert("reasoningId".to_string(), Value::Null);
        state.insert("reasoningBuf".to_string(), Value::String(String::new()));
        state.insert("reasoningDone".to_string(), Value::Bool(false));
        state.insert("reasoningPartAdded".to_string(), Value::Bool(false));
        state.insert("inThinking".to_string(), Value::Bool(false));
        state.insert("completedSent".to_string(), Value::Bool(false));

        let seq1 = next_seq(state);
        let seq2 = next_seq(state);

        events.push(serde_json::json!({
            "event": "response.created",
            "data": {
                "type": "response.created",
                "sequence_number": seq1,
                "response": {
                    "id": resp_id.clone(),
                    "object": "response",
                    "created_at": chrono::Utc::now().timestamp(),
                    "status": "in_progress",
                    "background": false,
                    "error": null,
                    "output": []
                }
            }
        }));

        events.push(serde_json::json!({
            "event": "response.in_progress",
            "data": {
                "type": "response.in_progress",
                "sequence_number": seq2,
                "response": {
                    "id": resp_id,
                    "object": "response",
                    "created_at": chrono::Utc::now().timestamp(),
                    "status": "in_progress"
                }
            }
        }));
    }

    let choice = &chunk["choices"][0];
    let idx = choice.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
    let idx_str = idx.to_string();
    let delta = choice
        .get("delta")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    // Handle reasoning across vendor shapes (JS concerns/reasoning.js
    // extractReasoningText): reasoning_content (GLM/Qwen/DeepSeek/Kimi) →
    // reasoning (compat layers) → reasoning_details[] (MiniMax
    // reasoning_split=true: [{text|content}]).
    let reasoning_text = delta
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            delta
                .get("reasoning")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            delta.get("reasoning_details").and_then(|d| {
                if let Some(arr) = d.as_array() {
                    let joined = arr
                        .iter()
                        .map(|e| match e {
                            Value::String(s) => s.clone(),
                            _ => e
                                .get("text")
                                .or_else(|| e.get("content"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        })
                        .collect::<String>();
                    if joined.is_empty() {
                        None
                    } else {
                        Some(joined)
                    }
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();
    if !reasoning_text.is_empty() {
        start_reasoning(state, &mut events, idx);
        emit_reasoning_delta(state, &mut events, &reasoning_text);
    }

    // Handle text content, including the <think> state machine (JS
    // openai-responses.js:76-100): <think> routes content into reasoning,
    // </think> splits buffered thinking from resumed text.
    if let Some(raw_content) = delta.get("content").and_then(|v| v.as_str()) {
        if !raw_content.is_empty() {
            let mut content = raw_content.to_string();
            let in_thinking = state
                .get("inThinking")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut return_after_thinking = false;

            if content.contains("<think>") {
                state.insert("inThinking".to_string(), Value::Bool(true));
                content = content.replacen("<think>", "", 1);
                start_reasoning(state, &mut events, idx);
            }

            if content.contains("</think>") {
                let parts: Vec<&str> = content.splitn(2, "</think>").collect();
                let think_part = parts.first().copied().unwrap_or("");
                let text_part = parts.get(1).copied().unwrap_or("");
                if !think_part.is_empty() {
                    emit_reasoning_delta(state, &mut events, think_part);
                }
                close_reasoning(state, &mut events);
                state.insert("inThinking".to_string(), Value::Bool(false));
                content = text_part.to_string();
            } else if in_thinking
                || state
                    .get("inThinking")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            {
                if !content.is_empty() {
                    emit_reasoning_delta(state, &mut events, &content);
                }
                return_after_thinking = true;
            }

            if return_after_thinking {
                return events;
            }
            if content.is_empty() {
                // All consumed by the think machine — fall through to
                // tool_calls / finish_reason handling below.
                return emit_tool_calls_and_finish(
                    chunk,
                    state,
                    &mut events,
                    idx,
                    &idx_str,
                    &delta,
                );
            }
            let content: &str = &content;
            let mut msg_item_added = state
                .get("msgItemAdded")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let mut msg_content_added = state
                .get("msgContentAdded")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let mut msg_text_buf = state
                .get("msgTextBuf")
                .cloned()
                .unwrap_or(serde_json::json!({}));

            if msg_item_added.get(&idx_str).is_none() {
                msg_item_added[&idx_str] = Value::Bool(true);
                let msg_id = format!("msg_{}_{}", state["responseId"].as_str().unwrap_or(""), idx);
                state.insert(format!("msgId_{}", idx), Value::String(msg_id.clone()));

                emit(
                    &mut events,
                    state,
                    "response.output_item.added",
                    serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": idx,
                        "item": {"id": msg_id, "type": "message", "content": [], "role": "assistant"}
                    }),
                );

                emit(
                    &mut events,
                    state,
                    "response.content_part.added",
                    serde_json::json!({
                        "type": "response.content_part.added",
                        "item_id": msg_id,
                        "output_index": idx,
                        "content_index": 0,
                        "part": {"type": "output_text", "annotations": [], "logprobs": [], "text": ""}
                    }),
                );
                msg_content_added[&idx_str] = Value::Bool(true);
            }

            let msg_id = state
                .get(&format!("msgId_{}", idx))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            emit(
                &mut events,
                state,
                "response.output_text.delta",
                serde_json::json!({
                    "type": "response.output_text.delta",
                    "item_id": msg_id,
                    "output_index": idx,
                    "content_index": 0,
                    "delta": content,
                    "logprobs": []
                }),
            );

            let existing = msg_text_buf
                .get(&idx_str)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            msg_text_buf[&idx_str] = Value::String(format!("{}{}", existing, content));

            state.insert("msgItemAdded".to_string(), msg_item_added);
            state.insert("msgContentAdded".to_string(), msg_content_added);
            state.insert("msgTextBuf".to_string(), msg_text_buf);
        }
    }

    // Handle tool_calls (skip empty arrays — 9router parity: empty array is
    // truthy; require a real call to avoid premature message close).
    if let Some(tool_calls) = delta
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .filter(|tc| !tc.is_empty())
    {
        emit_tool_calls_block(state, &mut events, tool_calls);
    }

    // Handle finish_reason (JS 110-116): close all messages, reasoning, and
    // tool calls, then send completed. Null finish_reason means mid-stream.
    if !choice.get("finish_reason").is_none_or(Value::is_null) {
        emit_finish_block(state, &mut events);
    }

    events
}

/// Tool-call arm of `chat_to_responses_response` (JS 102-108): close any open
/// message, then emit/accumulate per tool call. Custom tools wait for both
/// call id AND function name before announcing, and stream no argument deltas
/// (input is emitted once at close after unwrapping the Chat JSON wrapper).
fn emit_tool_calls_block(
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    tool_calls: &[Value],
) {
    let mut func_call_ids = state
        .get("funcCallIds")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let mut func_args_buf = state
        .get("funcArgsBuf")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let mut func_names = state
        .get("funcNames")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let mut func_item_added = state
        .get("funcItemAdded")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    for tc in tool_calls.iter() {
        let tc_idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
        let tc_idx_str = tc_idx.to_string();
        let new_call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let func_name = tc
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if !func_name.is_empty() {
            func_names[&tc_idx_str] = Value::String(func_name.to_string());
        }
        if !new_call_id.is_empty() {
            func_call_ids[&tc_idx_str] = Value::String(new_call_id.to_string());
        }

        // Wait for both id and name before deciding custom vs function;
        // otherwise a split-chunk call can be irreversibly announced wrong.
        let call_id = func_call_ids
            .get(&tc_idx_str)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let name = func_names
            .get(&tc_idx_str)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if func_item_added.get(&tc_idx_str).is_none() && !call_id.is_empty() && !name.is_empty() {
            func_item_added[&tc_idx_str] = Value::Bool(true);
            let custom = is_custom_tool(state, &name);
            // Close any open text message first (JS 104: closeMessage).
            close_message(state, events, &tc_idx_str);
            emit(
                events,
                state,
                "response.output_item.added",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": tc_idx,
                    "item": if custom {
                        serde_json::json!({
                            "id": format!("ctc_{}", call_id),
                            "type": "custom_tool_call",
                            "input": "",
                            "call_id": call_id,
                            "name": name
                        })
                    } else {
                        serde_json::json!({
                            "id": format!("fc_{}", call_id),
                            "type": "function_call",
                            "arguments": "",
                            "call_id": call_id,
                            "name": name
                        })
                    }
                }),
            );
        }

        if let Some(args) = tc
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(|v| v.as_str())
        {
            if !args.is_empty() {
                let ref_call_id = func_call_ids
                    .get(&tc_idx_str)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let is_custom_now = is_custom_tool(
                    state,
                    func_names
                        .get(&tc_idx_str)
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                );
                // Custom input is emitted once at close, after the Chat JSON
                // wrapper can be parsed. Streaming raw fragments would expose
                // {"input":"..."} instead of the freeform program.
                if func_item_added.get(&tc_idx_str).is_some()
                    && !ref_call_id.is_empty()
                    && !is_custom_now
                {
                    emit(
                        events,
                        state,
                        "response.function_call_arguments.delta",
                        serde_json::json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": format!("fc_{}", ref_call_id),
                            "output_index": tc_idx,
                            "delta": args
                        }),
                    );
                }
                let existing = func_args_buf
                    .get(&tc_idx_str)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                func_args_buf[&tc_idx_str] = Value::String(format!("{}{}", existing, args));
            }
        }
    }

    state.insert("funcCallIds".to_string(), func_call_ids);
    state.insert("funcArgsBuf".to_string(), func_args_buf);
    state.insert("funcNames".to_string(), func_names);
    state.insert("funcItemAdded".to_string(), func_item_added);
}

/// finish_reason arm of `chat_to_responses_response` (JS 110-116): close
/// every open message, reasoning, and tool call, then send completed.
fn emit_finish_block(state: &mut serde_json::Map<String, Value>, events: &mut Vec<Value>) {
    let msg_keys: Vec<String> = state
        .get("msgItemAdded")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    for k in &msg_keys {
        close_message(state, events, k);
    }

    close_reasoning(state, events);
    let func_keys: Vec<String> = state
        .get("funcCallIds")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    for k in &func_keys {
        close_tool_call(state, events, k);
    }
    send_completed(state, events);
}

pub fn responses_to_chat_response(
    chunk: &Value,
    state: &mut serde_json::Map<String, Value>,
) -> Vec<Value> {
    if chunk.is_null() {
        if state.get("finishReasonSent").and_then(|v| v.as_bool()) == Some(true)
            || state.get("started").and_then(|v| v.as_bool()) != Some(true)
        {
            return vec![];
        }

        let finish_reason = if state
            .get("toolCallIndex")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            > 0
            || state.get("currentToolCallId").is_some_and(|v| !v.is_null())
        {
            "tool_calls"
        } else {
            "stop"
        };

        state.insert("finishReasonSent".to_string(), Value::Bool(true));
        state.insert(
            "finishReason".to_string(),
            Value::String(finish_reason.to_string()),
        );

        let mut final_chunk = serde_json::json!({
            "id": state.get("chatId").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "object": "chat.completion.chunk",
            "created": state.get("created").and_then(|v| v.as_i64()).unwrap_or(0),
            "model": state.get("model").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": finish_reason
            }]
        });

        if let Some(usage) = state.get("usage") {
            if usage.is_object() {
                final_chunk["usage"] = usage.clone();
            }
        }
        return vec![final_chunk];
    }

    let event_type = chunk
        .get("type")
        .or_else(|| chunk.get("event"))
        .and_then(|v| v.as_str());
    let data = chunk.get("data").unwrap_or(chunk);

    if state.get("started").and_then(|v| v.as_bool()) != Some(true) {
        state.insert("started".to_string(), Value::Bool(true));
        state.insert(
            "chatId".to_string(),
            Value::String(format!(
                "chatcmpl-{}",
                chrono::Utc::now().timestamp_millis()
            )),
        );
        state.insert(
            "created".to_string(),
            Value::Number(chrono::Utc::now().timestamp().into()),
        );
        state.insert("toolCallIndex".to_string(), Value::Number(0.into()));
        state.insert("currentToolCallId".to_string(), Value::Null);
    }

    let chat_id = state
        .get("chatId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let created = state.get("created").and_then(|v| v.as_i64()).unwrap_or(0);
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    match event_type {
        Some("response.output_text.delta") => {
            let delta = data.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if delta.is_empty() {
                return vec![];
            }
            vec![serde_json::json!({
                "id": chat_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {"content": delta},
                    "finish_reason": null
                }]
            })]
        }
        Some("response.output_text.done") => vec![],
        Some("response.output_item.added") => {
            let item_type = data
                .get("item")
                .and_then(|i| i.get("type"))
                .and_then(|v| v.as_str());
            if item_type == Some("function_call") || item_type == Some("custom_tool_call") {
                let item = &data["item"];
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call_{}", chrono::Utc::now().timestamp_millis()));
                state.insert(
                    "currentToolCallId".to_string(),
                    Value::String(call_id.clone()),
                );

                let tool_idx = state
                    .get("toolCallIndex")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                vec![serde_json::json!({
                    "id": chat_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": {
                            "tool_calls": [{
                                "index": tool_idx,
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                    "arguments": ""
                                }
                            }]
                        },
                        "finish_reason": null
                    }]
                })]
            } else {
                vec![]
            }
        }
        Some("response.function_call_arguments.delta")
        | Some("response.custom_tool_call_input.delta") => {
            let delta = data.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if delta.is_empty() {
                return vec![];
            }
            let tool_idx = state
                .get("toolCallIndex")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            vec![serde_json::json!({
                "id": chat_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": tool_idx,
                            "function": {"arguments": delta}
                        }]
                    },
                    "finish_reason": null
                }]
            })]
        }
        Some("response.output_item.done") => {
            let item_type = data
                .get("item")
                .and_then(|i| i.get("type"))
                .and_then(|v| v.as_str());
            if item_type == Some("function_call") || item_type == Some("custom_tool_call") {
                let tool_idx = state
                    .get("toolCallIndex")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                state.insert(
                    "toolCallIndex".to_string(),
                    Value::Number((tool_idx + 1).into()),
                );
            }
            vec![]
        }
        Some("response.completed") => {
            if let Some(response) = data.get("response") {
                if let Some(usage) = response.get("usage") {
                    let input_tokens = usage
                        .get("input_tokens")
                        .or_else(|| usage.get("prompt_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let output_tokens = usage
                        .get("output_tokens")
                        .or_else(|| usage.get("completion_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cache_read = usage
                        .get("input_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    let mut usage_obj = serde_json::json!({
                        "prompt_tokens": input_tokens,
                        "completion_tokens": output_tokens,
                        "total_tokens": input_tokens + output_tokens
                    });
                    if cache_read > 0 {
                        usage_obj["prompt_tokens_details"] =
                            serde_json::json!({"cached_tokens": cache_read});
                    }
                    state.insert("usage".to_string(), usage_obj);
                }
            }

            if state.get("finishReasonSent").and_then(|v| v.as_bool()) != Some(true) {
                let finish_reason = if state
                    .get("toolCallIndex")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    > 0
                    || state.get("currentToolCallId").is_some_and(|v| !v.is_null())
                {
                    "tool_calls"
                } else {
                    "stop"
                };
                state.insert("finishReasonSent".to_string(), Value::Bool(true));
                state.insert(
                    "finishReason".to_string(),
                    Value::String(finish_reason.to_string()),
                );

                let mut final_chunk = serde_json::json!({
                    "id": chat_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": finish_reason
                    }]
                });

                if let Some(usage) = state.get("usage") {
                    if usage.is_object() {
                        final_chunk["usage"] = usage.clone();
                    }
                }
                return vec![final_chunk];
            }
            vec![]
        }
        Some("error") | Some("response.failed") => {
            if state.get("finishReasonSent").and_then(|v| v.as_bool()) == Some(true) {
                return vec![];
            }
            let error = data
                .get("error")
                .or_else(|| data.get("response").and_then(|r| r.get("error")));
            if let Some(err) = error {
                let msg = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        serde_json::to_string(err).unwrap_or_else(|_| "unknown".to_string())
                    });
                state.insert("finishReasonSent".to_string(), Value::Bool(true));
                vec![serde_json::json!({
                    "id": chat_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": {"content": format!("[Error] {}", msg)},
                        "finish_reason": "stop"
                    }]
                })]
            } else {
                vec![]
            }
        }
        // response.created carries the model name assigned by the backend.
        // Capture it so subsequent chunks emit a meaningful model field instead of "unknown".
        Some("response.created") => {
            if let Some(response) = data.get("response") {
                if let Some(model_name) = response.get("model").and_then(|v| v.as_str()) {
                    if !model_name.is_empty() {
                        state.insert("model".to_string(), Value::String(model_name.to_string()));
                    }
                }
            }
            vec![]
        }
        _ => vec![],
    }
}

use crate::core::translator::registry::ResponseTransformState;

/// Registry-compatible streaming wrapper: Responses API -> OpenAI chat completion chunks.
///
/// Handles two input formats:
///   1. Bare JSON: `{"type":"response.completed","response":{...}}`
///   2. SSE-framed: `event: response.completed\ndata: {"type":"response.completed",...}\n\n`
///
/// Registry adapter: OpenAI chat SSE/JSON → Responses API SSE events.
/// Signature matches `registry::ResponseTransformFn`.
pub fn chat_to_responses_streaming(
    chunk: &[u8],
    state: &mut crate::core::translator::registry::ResponseTransformState,
) -> Vec<String> {
    let text = String::from_utf8_lossy(chunk);
    let payload = {
        let line = text.trim();
        if let Some(rest) = line.strip_prefix("data:") {
            rest.trim()
        } else {
            line.lines()
                .find_map(|p| p.strip_prefix("data:").map(|r| r.trim()))
                .unwrap_or(line)
        }
    };
    if payload.is_empty() || payload == "[DONE]" {
        return vec![];
    }
    let val: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let inner = &mut state.responses.state;
    let events = chat_to_responses_response(&val, inner);
    events
        .into_iter()
        .map(|v| {
            let et = v
                .get("event")
                .and_then(|e| e.as_str())
                .or_else(|| v.get("type").and_then(|t| t.as_str()))
                .unwrap_or("message");
            let data = v.get("data").cloned().unwrap_or(v.clone());
            format!(
                "event: {et}\ndata: {}\n\n",
                serde_json::to_string(&data).unwrap_or_default()
            )
        })
        .collect()
}

/// Signature matches `registry::ResponseTransformFn`.
pub fn responses_to_chat_streaming(
    chunk: &[u8],
    state: &mut ResponseTransformState,
) -> Vec<String> {
    // Accumulate incoming bytes into the frame buffer.
    // SSE frames (delimited by double newline \n\n) can straddle TCP chunks,
    // so we must buffer across calls.
    state
        .responses
        .buffer
        .push_str(&String::from_utf8_lossy(chunk));

    // Try as bare JSON first (when the upstream delivers data: lines without event: prefix,
    // or when the full SSE event lands as one line, or on the final flush of a single frame).
    if let Ok(val) = serde_json::from_slice::<Value>(chunk) {
        // Only treat as bare JSON if the buffer is its natural size (nothing left over
        // from a previous partial frame) — otherwise fall through to SSE extraction.
        if state.responses.buffer.len() <= chunk.len() {
            let inner = &mut state.responses.state;
            let results = responses_to_chat_response(&val, inner);
            // Clear buffer — we consumed everything via the JSON path
            state.responses.buffer.clear();
            return results
                .into_iter()
                .map(|v| {
                    format!(
                        "data: {}\n\n",
                        serde_json::to_string(&v).unwrap_or_default()
                    )
                })
                .collect();
        }
    }

    // SSE-framed data: the buffer may contain one or more complete frames.
    // Split on \n\n (SSE frame delimiter), process complete frames, store leftovers.
    let mut results = Vec::new();

    while let Some(frame_end) = state.responses.buffer.find("\n\n") {
        let frame = state.responses.buffer[..frame_end].to_string();
        state.responses.buffer.drain(..frame_end + 2);

        for line in frame.lines() {
            let trimmed = line.trim();
            if let Some(data_content) = trimmed.strip_prefix("data: ") {
                if data_content == "[DONE]" {
                    continue;
                }
                if let Ok(val) = serde_json::from_str::<Value>(data_content) {
                    let inner = &mut state.responses.state;
                    for v in responses_to_chat_response(&val, inner) {
                        results.push(format!(
                            "data: {}\n\n",
                            serde_json::to_string(&v).unwrap_or_default()
                        ));
                    }
                }
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::translator::registry::ResponseTransformState;
    use serde_json::json;

    #[test]
    fn empty_tool_calls_array_does_not_trigger_tool_call_processing() {
        // Regression test for openproxy-mfs3.4 (9router v0.5.55 parity).
        // Some providers attach an empty tool_calls: [] to every SSE chunk.
        // Without the guard, this would enter the tool_calls block and could
        // cause the message to close prematurely on the first content token.
        let mut state = ResponseTransformState::default();

        // First chunk: content + empty tool_calls (chat completions format input
        // to chat_to_responses_response, which converts to Responses API format).
        let chunk1 = json!({
            "choices": [{
                "delta": {
                    "content": "Hello",
                    "tool_calls": []
                },
                "finish_reason": null
            }]
        });
        let events = chat_to_responses_response(&chunk1, &mut state.responses.state);

        // The empty tool_calls should NOT produce any function_call/tool_use events.
        // (response.output_item events for message content are expected.)
        let has_function_call = events.iter().any(|e| {
            let s = serde_json::to_string(e).unwrap_or_default();
            s.contains("function_call")
                || s.contains("tool_use")
                || s.contains("function_call_output")
        });
        assert!(
            !has_function_call,
            "empty tool_calls array should not produce function_call/tool_use events, got: {:?}",
            events
        );

        // Content "Hello" should still be present in the output.
        let has_content = events.iter().any(|e| {
            let s = serde_json::to_string(e).unwrap_or_default();
            s.contains("Hello")
        });
        assert!(
            has_content,
            "content should still be present, got: {:?}",
            events
        );
    }

    #[test]
    fn non_empty_tool_calls_still_processed() {
        // Verify that real tool_calls still work correctly.
        let mut state = ResponseTransformState::default();

        let chunk = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });

        let events = chat_to_responses_response(&chunk, &mut state.responses.state);

        // Should produce function_call events for the tool call.
        let has_function_call = events.iter().any(|e| {
            let s = serde_json::to_string(e).unwrap_or_default();
            s.contains("function_call") || s.contains("get_weather")
        });
        assert!(
            has_function_call,
            "non-empty tool_calls should produce function_call events, got: {:?}",
            events
        );
    }

    #[test]
    fn reasoning_vendor_shapes_fall_back() {
        // JS concerns/reasoning.js extractReasoningText: reasoning_content →
        // reasoning → reasoning_details[].
        for (delta, expect) in [
            (
                serde_json::json!({"reasoning": "compat-text"}),
                "compat-text",
            ),
            (
                serde_json::json!({"reasoning_details": [{"text": "a"}, {"content": "b"}]}),
                "ab",
            ),
        ] {
            let mut state = ResponseTransformState::default();
            let chunk = serde_json::json!({
                "id": "x", "choices": [{"index": 0, "delta": delta, "finish_reason": null}]
            });
            let events = chat_to_responses_response(&chunk, &mut state.responses.state);
            let found = events.iter().any(|e| {
                serde_json::to_string(e)
                    .unwrap_or_default()
                    .contains(expect)
            });
            assert!(found, "expected {expect} in {events:?}");
        }
    }

    #[test]
    fn think_tags_route_into_reasoning() {
        // JS 76-100 <think> state machine: <think> content → reasoning
        // deltas, </think> closes reasoning and resumes text.
        let mut state = ResponseTransformState::default();
        let chunk = serde_json::json!({
            "id": "x",
            "choices": [{"index": 0,
                "delta": {"content": "<think>hmm</think>hi"},
                "finish_reason": null}]
        });
        let events = chat_to_responses_response(&chunk, &mut state.responses.state);
        let s = serde_json::to_string(&events).unwrap_or_default();
        assert!(
            s.contains("reasoning_summary_text.delta"),
            "think → reasoning: {s}"
        );
        assert!(
            s.contains("reasoning_summary_text.done"),
            "think closed: {s}"
        );
        assert!(s.contains("output_text.delta"), "resumed text: {s}");
    }

    #[test]
    fn custom_tool_calls_emit_custom_events() {
        // JS 261-366: custom tools announce custom_tool_call and close with
        // unwrapped input ({"input":"..."} → freeform).
        let mut state = ResponseTransformState::default();
        state
            .responses
            .state
            .insert("customToolNames".to_string(), serde_json::json!("exec"));
        let chunk = serde_json::json!({
            "id": "x",
            "choices": [{"index": 0,
                "delta": {"tool_calls": [{
                    "index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "exec", "arguments": "{\"input\":\"ls\"}"}
                }]},
                "finish_reason": null}]
        });
        let events = chat_to_responses_response(&chunk, &mut state.responses.state);
        let s = serde_json::to_string(&events).unwrap_or_default();
        assert!(s.contains("custom_tool_call"), "custom announce: {s}");
        assert!(
            !s.contains("function_call_arguments.delta"),
            "no streamed JSON fragments for custom: {s}"
        );
        // Close path: finish_reason flushes custom input unwrapped.
        let done = serde_json::json!({
            "id": "x", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
        });
        let events2 = chat_to_responses_response(&done, &mut state.responses.state);
        let s2 = serde_json::to_string(&events2).unwrap_or_default();
        assert!(
            s2.contains("custom_tool_call_input.done"),
            "custom close: {s2}"
        );
        assert!(s2.contains("\"input\":\"ls\""), "unwrapped input: {s2}");
    }
}
