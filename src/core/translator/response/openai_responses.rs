//! OpenAI Responses API response translator (both directions).

use serde_json::Value;

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
    if !chunk
        .get("choices")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
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

    // Handle reasoning_content
    if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
        if !reasoning.is_empty() {
            if state.get("reasoningId").is_none() || state["reasoningId"].is_null() {
                let reasoning_id =
                    format!("rs_{}_{}", state["responseId"].as_str().unwrap_or(""), idx);
                state.insert(
                    "reasoningId".to_string(),
                    Value::String(reasoning_id.clone()),
                );
                state.insert("reasoningIndex".to_string(), Value::Number(idx.into()));

                emit(
                    &mut events,
                    state,
                    "response.output_item.added",
                    serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": idx,
                        "item": {"id": reasoning_id, "type": "reasoning", "summary": []}
                    }),
                );

                emit(
                    &mut events,
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

            let reasoning_id = state["reasoningId"].as_str().unwrap_or("").to_string();
            let reasoning_idx = state
                .get("reasoningIndex")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            state["reasoningBuf"] = Value::String(format!(
                "{}{}",
                state["reasoningBuf"].as_str().unwrap_or(""),
                reasoning
            ));

            emit(
                &mut events,
                state,
                "response.reasoning_summary_text.delta",
                serde_json::json!({
                    "type": "response.reasoning_summary_text.delta",
                    "item_id": reasoning_id,
                    "output_index": reasoning_idx,
                    "summary_index": 0,
                    "delta": reasoning
                }),
            );
        }
    }

    // Handle text content
    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
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

    // Handle tool_calls
    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
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

        for tc in tool_calls {
            let tc_idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let tc_idx_str = tc_idx.to_string();
            let call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let func_name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if !func_name.is_empty() {
                func_names[&tc_idx_str] = Value::String(func_name.to_string());
            }

            if func_call_ids.get(&tc_idx_str).is_none() && !call_id.is_empty() {
                func_call_ids[&tc_idx_str] = Value::String(call_id.to_string());

                emit(
                    &mut events,
                    state,
                    "response.output_item.added",
                    serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": tc_idx,
                        "item": {
                            "id": format!("fc_{}", call_id),
                            "type": "function_call",
                            "arguments": "",
                            "call_id": call_id,
                            "name": func_names.get(&tc_idx_str).and_then(|v| v.as_str()).unwrap_or("")
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
                        .unwrap_or(call_id);
                    if !ref_call_id.is_empty() {
                        emit(
                            &mut events,
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
    }

    // Handle finish_reason
    if choice.get("finish_reason").is_some() {
        let mut msg_item_added = state
            .get("msgItemAdded")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let mut msg_text_buf = state
            .get("msgTextBuf")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let mut msg_item_done = state
            .get("msgItemDone")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let func_call_ids = state
            .get("funcCallIds")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let func_args_buf = state
            .get("funcArgsBuf")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let func_names = state
            .get("funcNames")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let mut func_item_done = state
            .get("funcItemDone")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        for (k, _) in msg_item_added
            .as_object()
            .unwrap_or(&serde_json::Map::new())
        {
            if msg_item_done.get(k).is_none() {
                msg_item_done[k] = Value::Bool(true);
                let msg_id = state
                    .get(&format!("msgId_{}", k))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let full_text = msg_text_buf
                    .get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                emit(
                    &mut events,
                    state,
                    "response.output_text.done",
                    serde_json::json!({
                        "type": "response.output_text.done",
                        "item_id": msg_id,
                        "output_index": k.parse::<u64>().unwrap_or(0),
                        "content_index": 0,
                        "text": full_text,
                        "logprobs": []
                    }),
                );

                emit(
                    &mut events,
                    state,
                    "response.content_part.done",
                    serde_json::json!({
                        "type": "response.content_part.done",
                        "item_id": msg_id,
                        "output_index": k.parse::<u64>().unwrap_or(0),
                        "content_index": 0,
                        "part": {"type": "output_text", "annotations": [], "logprobs": [], "text": full_text}
                    }),
                );

                emit(
                    &mut events,
                    state,
                    "response.output_item.done",
                    serde_json::json!({
                        "type": "response.output_item.done",
                        "output_index": k.parse::<u64>().unwrap_or(0),
                        "item": {
                            "id": msg_id,
                            "type": "message",
                            "content": [{"type": "output_text", "annotations": [], "logprobs": [], "text": full_text}],
                            "role": "assistant"
                        }
                    }),
                );
            }
        }

        // Close reasoning
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
                &mut events,
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
                &mut events,
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
                &mut events,
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

        // Close tool calls
        for (k, v) in func_call_ids.as_object().unwrap_or(&serde_json::Map::new()) {
            if func_item_done.get(k).is_none() {
                func_item_done[k] = Value::Bool(true);
                let call_id = v.as_str().unwrap_or("");
                let args = func_args_buf
                    .get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let name = func_names.get(k).and_then(|v| v.as_str()).unwrap_or("");

                emit(
                    &mut events,
                    state,
                    "response.function_call_arguments.done",
                    serde_json::json!({
                        "type": "response.function_call_arguments.done",
                        "item_id": format!("fc_{}", call_id),
                        "output_index": k.parse::<u64>().unwrap_or(0),
                        "arguments": args
                    }),
                );

                emit(
                    &mut events,
                    state,
                    "response.output_item.done",
                    serde_json::json!({
                        "type": "response.output_item.done",
                        "output_index": k.parse::<u64>().unwrap_or(0),
                        "item": {
                            "id": format!("fc_{}", call_id),
                            "type": "function_call",
                            "arguments": args,
                            "call_id": call_id,
                            "name": name
                        }
                    }),
                );
            }
        }

        // Send completed
        if state.get("completedSent").and_then(|v| v.as_bool()) != Some(true) {
            state.insert("completedSent".to_string(), Value::Bool(true));
            emit(
                &mut events,
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

        state.insert("msgItemDone".to_string(), msg_item_done);
        state.insert("funcItemDone".to_string(), func_item_done);
    }

    events
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
/// Signature matches `registry::ResponseTransformFn`.
pub fn responses_to_chat_streaming(
    chunk: &[u8],
    state: &mut ResponseTransformState,
) -> Vec<String> {
    // Accumulate incoming bytes into the frame buffer.
    // SSE frames (delimited by double newline \n\n) can straddle TCP chunks,
    // so we must buffer across calls.
    state.responses.buffer.push_str(&String::from_utf8_lossy(chunk));

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

    loop {
        // Find the next \n\n frame delimiter
        let frame_end = match state.responses.buffer.find("\n\n") {
            Some(pos) => pos,
            None => break, // no complete frame yet, wait for next chunk
        };

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
