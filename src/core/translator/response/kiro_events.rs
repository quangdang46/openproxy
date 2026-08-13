//! Assemble OpenAI `chat.completion.chunk` SSE from decoded Kiro
//! (AWS EventStream) events. Ports the core of 9router
//! `open-sse/executors/kiro.js` `transformEventStreamToSSE`:
//!   - `chatcmpl-{timestamp_ms}` id / `created` / model on every chunk
//!   - first chunk carries `delta.role = "assistant"`
//!   - `<thinking>`/`</thinking>` stripped from assistantResponseEvent content
//!   - reasoningContentEvent → `delta.reasoning_content`
//!   - codeEvent → `delta.content`
//!   - toolUseEvent input buffered per tool id; emitted as TWO deltas:
//!     `{id,name,function:{name,arguments:""}}` then
//!     `{function:{arguments:JSON.stringify(input)}}`
//!   - messageStopEvent / metadataEvent stop reasons merged by severity
//!   - metricsEvent / meteringEvent → usage (prompt/completion tokens, kiro credits)

use std::collections::HashMap;

use serde_json::{json, Map, Value};

/// Normalized stop reasons (9router normalizeStopReason).
fn normalize_stop_reason(value: &str) -> Option<String> {
    let raw = value.trim();
    if raw.is_empty() {
        return None;
    }
    // camelCase → snake_case, whitespace/hyphens → underscores.
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_uppercase() {
            if let Some(&next) = chars.peek() {
                if next.is_lowercase() && !out.is_empty() && !out.ends_with('_') {
                    out.push('_');
                }
            }
            out.push(c.to_ascii_lowercase());
        } else if c.is_whitespace() || c == '-' {
            out.push('_');
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    let normalized = match out.as_str() {
        "endturn" | "end_turn" | "stop" | "stop_sequence" => "end_turn",
        "tooluse" | "tool_use" | "tool_calls" => "tool_use",
        "maxtokens" | "max_tokens" | "max_output_tokens" | "length" => "max_tokens",
        other => other,
    };
    Some(normalized.to_string())
}

/// Severity used to merge competing stop reasons (9router mergeStopReason).
fn stop_reason_severity(reason: &str) -> u8 {
    match reason {
        "refusal" | "malformed_model_output" | "invalid_model_output" => 6,
        "cancelled" | "pause_turn" | "model_context_window_exceeded" => 5,
        "unknown_failure" => 4,
        "retryable_protocol_failure" => 3,
        "max_tokens" => 2,
        _ => 1,
    }
}

/// Merge two stop reasons keeping the higher severity (9router mergeStopReason).
fn merge_stop_reason(current: &Option<String>, incoming: &Option<String>) -> Option<String> {
    match (current, incoming) {
        (None, incoming) => incoming.clone(),
        (Some(c), None) => Some(c.clone()),
        (Some(c), Some(i)) => {
            if stop_reason_severity(i) > stop_reason_severity(c) {
                Some(i.clone())
            } else {
                Some(c.clone())
            }
        }
    }
}

/// State for one streaming response assembly.
#[derive(Debug, Clone, Default)]
pub struct KiroSseAssembler {
    pub response_id: String,
    pub created: i64,
    pub model: String,
    pub tool_index: u64,
    pub tools: HashMap<String, KiroToolBuf>,
    pub stop_reason: Option<String>,
    pub saw_tool_use: bool,
    pub in_thinking: bool,
    pub usage: Option<Value>,
    /// First-chunk flag — ensures the very first emitted chunk has role.
    pub emitted_any: bool,
}

#[derive(Debug, Clone)]
pub struct KiroToolBuf {
    pub id: String,
    pub name: String,
    pub input_parts: Vec<Value>,
}

impl KiroSseAssembler {
    pub fn new(model: &str) -> Self {
        Self {
            response_id: format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()),
            created: chrono::Utc::now().timestamp(),
            model: model.to_string(),
            ..Default::default()
        }
    }

    /// Build a chunk envelope with the shared id/created/model/role fields.
    fn envelope(&self, delta: Map<String, Value>, finish_reason: Option<&str>) -> Value {
        let mut delta = delta;
        if !self.emitted_any {
            delta.insert("role".to_string(), json!("assistant"));
        }
        let mut chunk = Map::new();
        chunk.insert("id".to_string(), json!(self.response_id));
        chunk.insert("object".to_string(), json!("chat.completion.chunk"));
        chunk.insert("created".to_string(), json!(self.created));
        chunk.insert("model".to_string(), json!(self.model));
        chunk.insert(
            "choices".to_string(),
            json!([{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
            }]),
        );
        if let Some(usage) = &self.usage {
            chunk.insert("usage".to_string(), usage.clone());
        }
        Value::Object(chunk)
    }

    /// Flush all buffered tool calls as two-delta sequences.
    /// 9router emitTools: for each buffered tool emit
    ///   `{id, name, type:function, function:{name, arguments:""}}` then
    ///   `{function:{arguments:JSON.stringify(input)}}`.
    fn flush_tools(&mut self) -> Vec<Value> {
        if self.tools.is_empty() {
            return Vec::new();
        }
        let mut chunks = Vec::new();
        let mut tool_calls = Vec::new();
        for tool in self.tools.values() {
            let index = self.tool_index;
            self.tool_index += 1;
            // First delta: declaration with empty arguments.
            let mut delta1 = Map::new();
            delta1.insert(
                "tool_calls".to_string(),
                json!([{
                    "index": index,
                    "id": tool.id,
                    "type": "function",
                    "function": { "name": tool.name, "arguments": "" }
                }]),
            );
            tool_calls.push(delta1);
            // Second delta: the input.
            let input = if tool.input_parts.len() == 1 {
                tool.input_parts[0].clone()
            } else {
                // Multiple fragments — wrap as a JSON array of parts.
                json!(tool.input_parts)
            };
            let input_str = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
            let mut delta2 = Map::new();
            delta2.insert(
                "tool_calls".to_string(),
                json!([{ "index": index, "function": { "arguments": input_str } }]),
            );
            tool_calls.push(delta2);
        }
        self.tools.clear();
        for delta in tool_calls {
            chunks.push(self.envelope(delta, None));
        }
        chunks
    }

    /// Process one decoded Kiro event, returning the OpenAI SSE chunks to emit.
    pub fn process_event(
        &mut self,
        event: &crate::core::executor::KiroEvent,
    ) -> Result<Vec<Value>, String> {
        let mut chunks = Vec::new();
        let event_type = &event.event_type;

        match event_type.as_str() {
            "assistantResponseEvent" => {
                let payload = event
                    .payload
                    .as_ref()
                    .ok_or("assistantResponseEvent has no payload")?;
                let mut content = payload
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or("assistantResponseEvent payload has no string content")?
                    .to_string();
                // Strip <thinking>...</thinking> (9router L759-784). Track an
                // open thinking block across chunks.
                if self.in_thinking {
                    let end = content.find("</thinking>");
                    match end {
                        Some(end) => {
                            self.in_thinking = false;
                            content = content[end + "</thinking>".len()..]
                                .trim_start_matches('\n')
                                .to_string();
                        }
                        None => content = String::new(),
                    }
                } else {
                    let start = content.find("<thinking>");
                    if let Some(start) = start {
                        let end = content[start + "<thinking>".len()..].find("</thinking>");
                        match end {
                            Some(end) => {
                                let close = start + "<thinking>".len() + end + "</thinking>".len();
                                content = format!("{}{}", &content[..start], &content[close..])
                                    .trim_start_matches('\n')
                                    .to_string();
                            }
                            None => {
                                self.in_thinking = true;
                                content = content[..start].to_string();
                            }
                        }
                    }
                }
                if !content.is_empty() {
                    let mut delta = Map::new();
                    delta.insert("content".to_string(), json!(content));
                    chunks.push(self.envelope(delta, None));
                }
            }
            "reasoningContentEvent" => {
                let payload = event.payload.as_ref();
                // 9router: `event.payload?.reasoningContentEvent || event.payload`
                // then extract `text` or `content`.
                let inner = payload
                    .and_then(|p| p.get("reasoningContentEvent"))
                    .filter(|v| v.is_object())
                    .or(payload);
                let content = inner
                    .and_then(|v| v.get("text").or_else(|| v.get("content")))
                    .and_then(Value::as_str)
                    .or_else(|| payload.and_then(Value::as_str))
                    .unwrap_or("");
                if !content.is_empty() {
                    let mut delta = Map::new();
                    delta.insert("reasoning_content".to_string(), json!(content));
                    chunks.push(self.envelope(delta, None));
                }
            }
            "codeEvent" => {
                if let Some(payload) = event.payload.as_ref() {
                    if let Some(content) = payload.get("content").and_then(Value::as_str) {
                        if !content.is_empty() {
                            let mut delta = Map::new();
                            delta.insert("content".to_string(), json!(content));
                            chunks.push(self.envelope(delta, None));
                        }
                    }
                }
            }
            "toolUseEvent" => {
                self.saw_tool_use = true;
                let payload = event
                    .payload
                    .as_ref()
                    .ok_or("toolUseEvent has no payload")?;
                let values: Vec<&Value> = match payload {
                    Value::Array(arr) => arr.iter().collect(),
                    other => std::slice::from_ref(&other).to_vec(),
                };
                for value in values {
                    let name = value
                        .get("name")
                        .and_then(Value::as_str)
                        .map(|s| s.trim())
                        .unwrap_or("");
                    if name.is_empty() {
                        return Err("Kiro toolUseEvent is missing a tool name".to_string());
                    }
                    let id = value
                        .get("toolUseId")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            format!("call_{}_{}", self.created, self.tools.len() + 1)
                        });
                    let input = value.get("input").cloned().unwrap_or(Value::Null);
                    if let Some(tool) = self.tools.get_mut(&id) {
                        if tool.name != name {
                            return Err("Kiro tool name changed between fragments".to_string());
                        }
                        tool.input_parts.push(input);
                    } else {
                        self.tools.insert(
                            id.clone(),
                            KiroToolBuf {
                                id,
                                name: name.to_string(),
                                input_parts: vec![input],
                            },
                        );
                    }
                }
            }
            "messageStopEvent" => {
                let payload = event.payload.as_ref();
                let reason = payload
                    .and_then(|p| p.get("stopReason").or_else(|| p.get("stop_reason")))
                    .and_then(Value::as_str)
                    .and_then(normalize_stop_reason)
                    .or_else(|| {
                        if self.saw_tool_use {
                            Some("tool_use".to_string())
                        } else {
                            Some("end_turn".to_string())
                        }
                    });
                self.stop_reason = merge_stop_reason(&self.stop_reason, &reason);
                // 9router emits the terminal chunk at the stop event; flush any
                // buffered tool calls first so finish_reason lands last.
                chunks.extend(self.flush_tools());
                chunks.push(self.terminal_chunk());
            }
            "metadataEvent" | "MetadataEvent" => {
                let payload = event.payload.as_ref();
                let metadata = payload
                    .and_then(|p| p.get("metadataEvent").or_else(|| p.get("metadata")))
                    .or(payload);
                let reason = metadata
                    .and_then(|m| m.get("stopReason").or_else(|| m.get("stop_reason")))
                    .and_then(Value::as_str)
                    .and_then(normalize_stop_reason);
                if let Some(reason) = reason {
                    self.stop_reason = merge_stop_reason(&self.stop_reason, &Some(reason));
                    // If this is a terminal stop (end_turn / tool_use / max_tokens),
                    // flush tools and emit the terminal chunk.
                    if matches!(
                        self.stop_reason.as_deref(),
                        Some("end_turn" | "tool_use" | "max_tokens")
                    ) {
                        chunks.extend(self.flush_tools());
                        chunks.push(self.terminal_chunk());
                    }
                }
            }
            "contextUsageEvent" => {
                // Usage fallback (9router L1032-1043): contextUsagePercentage is
                // a proxy when no token counts arrive.
                if let Some(payload) = event.payload.as_ref() {
                    if let Some(pct) = payload
                        .get("contextUsagePercentage")
                        .and_then(Value::as_f64)
                    {
                        let _ = pct;
                    }
                }
            }
            "meteringEvent" => {
                if let Some(payload) = event.payload.as_ref() {
                    let metering = payload
                        .get("meteringEvent")
                        .or(Some(payload))
                        .cloned()
                        .unwrap_or_default();
                    if let Some(credits) = metering.get("usage").and_then(Value::as_f64) {
                        let mut usage = self.usage.clone().unwrap_or_else(|| json!({}));
                        usage["kiro_credits"] = json!(credits);
                        let unit = metering
                            .get("unit")
                            .and_then(Value::as_str)
                            .unwrap_or("credit");
                        usage["kiro_credit_unit"] = json!(unit);
                        self.usage = Some(usage);
                    }
                }
            }
            "metricsEvent" => {
                if let Some(payload) = event.payload.as_ref() {
                    let metrics = payload
                        .get("metricsEvent")
                        .or(Some(payload))
                        .cloned()
                        .unwrap_or_default();
                    let prompt = metrics
                        .get("inputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let completion = metrics
                        .get("outputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if prompt > 0 || completion > 0 {
                        let mut usage = self.usage.clone().unwrap_or_else(|| json!({}));
                        usage["prompt_tokens"] = json!(prompt);
                        usage["completion_tokens"] = json!(completion);
                        usage["total_tokens"] = json!(prompt + completion);
                        let cache_read = metrics
                            .get("cacheReadInputTokens")
                            .or_else(|| metrics.get("cache_read_input_tokens"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        if cache_read > 0 {
                            usage["cache_read_input_tokens"] = json!(cache_read);
                        }
                        let cache_create = metrics
                            .get("cacheCreationInputTokens")
                            .or_else(|| metrics.get("cache_creation_input_tokens"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        if cache_create > 0 {
                            usage["cache_creation_input_tokens"] = json!(cache_create);
                        }
                        self.usage = Some(usage);
                    }
                }
            }
            _ => {
                // Unknown/error event types produce nothing.
            }
        }

        // Mark chunk emission happened (role was attached).
        if !chunks.is_empty() {
            self.emitted_any = true;
        }
        Ok(chunks)
    }

    /// The terminal chunk with finish_reason (no tool flush — callers flush
    /// first via `flush_tools`).
    pub fn terminal_chunk(&self) -> Value {
        let reason = match self.stop_reason.as_deref() {
            Some("tool_use") => "tool_calls",
            Some(r) if r == "end_turn" || r == "stop" => "stop",
            Some(r) => r,
            None => "stop",
        };
        let delta = Map::new();
        self.envelope(delta, Some(reason))
    }

    /// Emit the terminal chunk with finish_reason + any buffered tools flushed
    /// first. Call exactly once at stream end (e.g. upstream connection close).
    pub fn finish(&mut self) -> Vec<Value> {
        let mut chunks = self.flush_tools();
        chunks.push(self.terminal_chunk());
        chunks
    }
}

/// Transform a JSON envelope that already carries `_eventType` (the legacy
/// path used by kiro_to_openai_response) into the same chunk shape.
pub fn event_json_to_chunk(event: &Value, state: &mut HashMap<String, Value>) -> Option<Value> {
    let event_type = event
        .get("_eventType")
        .or_else(|| event.get("event"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if event_type == "assistantResponseEvent" || event.get("assistantResponseEvent").is_some() {
        let content = event
            .get("assistantResponseEvent")
            .and_then(|v| v.get("content"))
            .or_else(|| event.get("content"))
            .and_then(Value::as_str)?;
        let chunk_idx = state.get("chunkIndex").and_then(Value::as_u64).unwrap_or(0);
        let response_id = state
            .get("responseId")
            .and_then(Value::as_str)
            .unwrap_or("chatcmpl-0")
            .to_string();
        let created = state.get("created").and_then(Value::as_i64).unwrap_or(0);
        let model = state
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("kiro")
            .to_string();
        let mut delta = serde_json::Map::new();
        if chunk_idx == 0 {
            delta.insert("role".to_string(), Value::String("assistant".to_string()));
        }
        delta.insert("content".to_string(), Value::String(content.to_string()));
        state.insert(
            "chunkIndex".to_string(),
            Value::Number((chunk_idx + 1).into()),
        );
        return Some(json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": null }]
        }));
    }
    if event_type == "reasoningContentEvent" || event.get("reasoningContentEvent").is_some() {
        let reasoning = event.get("reasoningContentEvent").unwrap_or(event);
        let content = reasoning
            .get("text")
            .or_else(|| reasoning.get("content"))
            .and_then(Value::as_str)
            .or_else(|| event.get("content").and_then(Value::as_str))?;
        let chunk_idx = state.get("chunkIndex").and_then(Value::as_u64).unwrap_or(0);
        let response_id = state
            .get("responseId")
            .and_then(Value::as_str)
            .unwrap_or("chatcmpl-0")
            .to_string();
        let created = state.get("created").and_then(Value::as_i64).unwrap_or(0);
        let model = state
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("kiro")
            .to_string();
        let mut delta = serde_json::Map::new();
        if chunk_idx == 0 {
            delta.insert("role".to_string(), Value::String("assistant".to_string()));
        }
        delta.insert(
            "reasoning_content".to_string(),
            Value::String(content.to_string()),
        );
        state.insert(
            "chunkIndex".to_string(),
            Value::Number((chunk_idx + 1).into()),
        );
        return Some(json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": null }]
        }));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::executor::KiroEvent;
    use serde_json::json;

    fn event(event_type: &str, payload: Value) -> KiroEvent {
        KiroEvent {
            message_type: "event".to_string(),
            event_type: event_type.to_string(),
            content_type: "application/json".to_string(),
            payload: Some(payload),
        }
    }

    #[test]
    fn test_eventstream_tool_use_emits_two_deltas() {
        // Acceptance guard test: a toolUseEvent carrying {toolUseId, name,
        // input:{x:1}} → output SSE contains a chunk with
        // tool_calls[0].function.arguments == "" and a later chunk with
        // tool_calls[0].function.arguments == "{\"x\":1}".
        let mut asm = KiroSseAssembler::new("test-model");
        let tool_event = event(
            "toolUseEvent",
            json!({
                "toolUseId": "tool_1",
                "name": "get_weather",
                "input": { "x": 1 }
            }),
        );
        let chunks = asm.process_event(&tool_event).unwrap();
        // Buffering emits no chunks yet.
        assert!(chunks.is_empty());
        // Flush the tool calls.
        let flushed = asm.flush_tools();
        assert_eq!(flushed.len(), 2);

        let first = &flushed[0]["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(first["id"], "tool_1");
        assert_eq!(first["function"]["name"], "get_weather");
        assert_eq!(first["function"]["arguments"], "");

        let second = &flushed[1]["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(second["function"]["arguments"], "{\"x\":1}");
    }

    #[test]
    fn test_message_stop_flushes_tools_and_emits_terminal() {
        let mut asm = KiroSseAssembler::new("test-model");
        asm.process_event(&event(
            "toolUseEvent",
            json!({ "toolUseId": "t1", "name": "f", "input": { "a": 2 } }),
        ))
        .unwrap();
        let chunks = asm
            .process_event(&event(
                "messageStopEvent",
                json!({ "stopReason": "tool_use" }),
            ))
            .unwrap();
        // tools flushed (2) + terminal (1)
        assert_eq!(chunks.len(), 3);
        let last = chunks.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn test_assistant_response_strips_thinking_tags() {
        let mut asm = KiroSseAssembler::new("test-model");
        let chunks = asm
            .process_event(&event(
                "assistantResponseEvent",
                json!({ "content": "before <thinking>hidden</thinking> after" }),
            ))
            .unwrap();
        assert_eq!(chunks.len(), 1);
        let content = chunks[0]["choices"][0]["delta"]["content"]
            .as_str()
            .unwrap();
        assert!(!content.contains("<thinking>"));
        assert!(!content.contains("</thinking>"));
        assert!(content.contains("before"));
        assert!(content.contains("after"));
        // First chunk carries role assistant.
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
    }

    #[test]
    fn test_reasoning_event_emits_reasoning_content() {
        let mut asm = KiroSseAssembler::new("test-model");
        let chunks = asm
            .process_event(&event(
                "reasoningContentEvent",
                json!({ "reasoningContentEvent": { "text": "thinking..." } }),
            ))
            .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0]["choices"][0]["delta"]["reasoning_content"],
            "thinking..."
        );
    }

    #[test]
    fn test_metrics_event_produces_usage() {
        let mut asm = KiroSseAssembler::new("test-model");
        asm.process_event(&event(
            "metricsEvent",
            json!({ "inputTokens": 10, "outputTokens": 20 }),
        ))
        .unwrap();
        let terminal = asm.terminal_chunk();
        assert_eq!(terminal["usage"]["prompt_tokens"], 10);
        assert_eq!(terminal["usage"]["completion_tokens"], 20);
        assert_eq!(terminal["usage"]["total_tokens"], 30);
    }
}
