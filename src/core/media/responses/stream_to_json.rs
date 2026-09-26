//! Stream-to-JSON converter (#306).
//!
//! When a provider forces streaming but the client requested non-streaming,
//! this module converts the SSE stream back to a single `chat.completion`
//! JSON response.
//!
//! Two input formats are supported:
//! - **Chat Completions SSE** (`data: {...}` lines with `delta.content` /
//!   `delta.tool_calls`)
//! - **Responses API SSE** (`event:` / `data:` pairs from the OpenAI Responses
//!   API, which providers like Codex use on the wire)

use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Quick check whether raw bytes look like SSE data (start with `data:` or
/// `event:` or an SSE comment line `:`).
///
/// Normal JSON responses never begin with these, so a positive result is a
/// reliable indicator that the response body is SSE rather than a single JSON
/// object.
pub fn looks_like_sse(input: &[u8]) -> bool {
    if input.is_empty() || input.len() < 5 {
        return false;
    }
    let s = String::from_utf8_lossy(input);
    let trimmed = s.trim();
    trimmed.starts_with("data:") || trimmed.starts_with("event:") || trimmed.starts_with(':')
}

/// Convert SSE stream bytes to a single `chat.completion` JSON response.
///
/// Automatically detects the SSE format:
/// - OpenAI Chat Completions SSE (`data: {...}` lines)
/// - OpenAI Responses API SSE (`event:` / `data:` pairs)
///
/// Returns `None` when the input does not look like valid SSE or when parsing
/// yields no content.
pub fn sse_stream_to_json(input: &[u8], fallback_model: Option<&str>) -> Option<Value> {
    let input_str = String::from_utf8_lossy(input);
    let input_str = input_str.trim();

    if input_str.is_empty() || !looks_like_sse(input) {
        return None;
    }

    if input_str.starts_with("event:") || input_str.contains("\nevent:") {
        convert_responses_api_stream(input_str, fallback_model)
    } else {
        convert_chat_completion_stream(input_str, fallback_model)
    }
}

// ---------------------------------------------------------------------------
// Chat Completions SSE  →  chat.completion JSON
// ---------------------------------------------------------------------------

/// Accumulator state for a single choice index.
#[derive(Debug, Default)]
struct ChoiceAccum {
    role: Option<String>,
    content: String,
    /// Tool calls keyed by their SSE-index within this choice.
    /// Each entry maps metadata keys (id, type, function_name, function_arguments)
    /// to their accumulated values.
    tool_calls: Vec<BTreeMap<String, Value>>,
    finish_reason: Option<String>,
    refusal: String,
}

/// Convert OpenAI Chat Completions SSE (`data: {...}` lines) to a single
/// `chat.completion` JSON response.
///
/// Input looks like:
/// ```text
/// data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","created":123,...
/// data: {"choices":[{"index":0,"delta":{"content":"Hello"}}]}
/// data: [DONE]
/// ```
fn convert_chat_completion_stream(sse: &str, fallback_model: Option<&str>) -> Option<Value> {
    let mut id: Option<String> = None;
    let mut created: Option<i64> = None;
    let mut model: Option<String> = None;
    let mut usage: Option<Value> = None;
    let mut choices: BTreeMap<usize, ChoiceAccum> = BTreeMap::new();

    // Split by blank lines (SSE frame delimiter).
    for frame in sse.split("\n\n") {
        let frame = frame.trim();
        if frame.is_empty() {
            continue;
        }

        // Extract the `data:` line (skip `event:` lines if present).
        let data_str = frame
            .lines()
            .find(|line| line.trim().starts_with("data: "))
            .and_then(|line| line.trim().strip_prefix("data: "));

        let Some(data_str) = data_str else {
            continue;
        };

        if data_str == "[DONE]" {
            continue;
        }

        let Ok(data) = serde_json::from_str::<Value>(data_str) else {
            continue;
        };

        // Capture metadata from the very first data frame.
        if id.is_none() {
            id = data.get("id").and_then(|v| v.as_str()).map(String::from);
            created = data.get("created").and_then(|v| v.as_i64());
            model = data.get("model").and_then(|v| v.as_str()).map(String::from);
        }

        // Usage may appear in the final frames.
        if usage.is_none() {
            if let Some(u) = data.get("usage") {
                if !u.is_null() {
                    usage = Some(u.clone());
                }
            }
        }

        // Process the choices array.
        let Some(choices_arr) = data.get("choices").and_then(|v| v.as_array()) else {
            continue;
        };

        for choice_val in choices_arr {
            let idx = choice_val
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let entry = choices.entry(idx).or_default();

            // Finish reason — last non-null/non-empty value wins.
            if let Some(reason) = choice_val.get("finish_reason") {
                if reason.is_string() {
                    let r = reason.as_str().unwrap();
                    if !r.is_empty() && r != "null" {
                        entry.finish_reason = Some(r.to_string());
                    }
                }
            }

            let Some(delta) = choice_val.get("delta") else {
                continue;
            };

            // Role (only present in the very first chunk for each choice).
            if entry.role.is_none() {
                if let Some(role) = delta.get("role").and_then(|v| v.as_str()) {
                    entry.role = Some(role.to_string());
                }
            }

            // Content delta — append to accumulator.
            if let Some(content) = delta.get("content") {
                if content.is_string() {
                    entry.content.push_str(content.as_str().unwrap());
                }
            }

            // Refusal delta.
            if let Some(refusal) = delta.get("refusal").and_then(|v| v.as_str()) {
                entry.refusal.push_str(refusal);
            }

            // Tool calls delta — each chunk carries the full tool_call object
            // for its index, and we accumulate the function arguments across
            // chunks (same pattern as concat-ing content).
            if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let tc_idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                    // Ensure the tool_calls vec is large enough.
                    while entry.tool_calls.len() <= tc_idx {
                        entry.tool_calls.push(BTreeMap::new());
                    }

                    let map = &mut entry.tool_calls[tc_idx];

                    if let Some(tc_id) = tc.get("id").and_then(|v| v.as_str()) {
                        map.insert("id".to_string(), Value::String(tc_id.to_string()));
                    }
                    if let Some(tc_type) = tc.get("type").and_then(|v| v.as_str()) {
                        map.insert("type".to_string(), Value::String(tc_type.to_string()));
                    }

                    if let Some(func) = tc.get("function") {
                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                            map.insert(
                                "function_name".to_string(),
                                Value::String(name.to_string()),
                            );
                        }
                        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                            let existing = map
                                .get("function_arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            map.insert(
                                "function_arguments".to_string(),
                                Value::String(existing + args),
                            );
                        }
                    }
                }
            }
        }
    }

    if choices.is_empty() {
        return None;
    }

    // Build the response choices array.
    let mut response_choices: Vec<Value> = Vec::new();
    for (idx, accum) in &choices {
        let mut message = serde_json::Map::new();
        message.insert(
            "role".to_string(),
            Value::String(
                accum
                    .role
                    .clone()
                    .unwrap_or_else(|| "assistant".to_string()),
            ),
        );

        if !accum.tool_calls.is_empty() {
            message.insert("content".to_string(), Value::Null);

            let mut call_arr = Vec::new();
            for tc_map in &accum.tool_calls {
                let mut tc_obj = serde_json::Map::new();
                tc_obj.insert(
                    "id".to_string(),
                    tc_map
                        .get("id")
                        .cloned()
                        .unwrap_or_else(|| Value::String(format!("call_{}", idx))),
                );
                tc_obj.insert(
                    "type".to_string(),
                    tc_map
                        .get("type")
                        .cloned()
                        .unwrap_or_else(|| Value::String("function".to_string())),
                );

                let mut func_obj = serde_json::Map::new();
                func_obj.insert(
                    "name".to_string(),
                    tc_map
                        .get("function_name")
                        .cloned()
                        .unwrap_or(Value::String(String::new())),
                );
                func_obj.insert(
                    "arguments".to_string(),
                    tc_map
                        .get("function_arguments")
                        .cloned()
                        .unwrap_or(Value::String(String::new())),
                );
                tc_obj.insert("function".to_string(), Value::Object(func_obj));

                call_arr.push(Value::Object(tc_obj));
            }
            message.insert("tool_calls".to_string(), Value::Array(call_arr));
        } else {
            message.insert("content".to_string(), Value::String(accum.content.clone()));
        }

        response_choices.push(json!({
            "index": *idx,
            "message": Value::Object(message),
            "finish_reason": accum.finish_reason.clone().unwrap_or_else(|| "stop".to_string()),
        }));
    }

    let final_model = model
        .or_else(|| fallback_model.map(String::from))
        .unwrap_or_else(|| "unknown".to_string());

    Some(json!({
        "id": id.unwrap_or_else(|| {
            format!("chatcmpl-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000"))
        }),
        "object": "chat.completion",
        "created": created.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        }),
        "model": final_model,
        "choices": response_choices,
        "usage": usage.unwrap_or_else(|| json!({
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0,
        })),
    }))
}

// ---------------------------------------------------------------------------
// Responses API SSE  →  chat.completion JSON
// ---------------------------------------------------------------------------

/// Parsed summary of a Responses API SSE stream.
struct ResponsesStreamSummary {
    response_id: String,
    created: Option<i64>,
    status: String,
    output: Vec<Value>,
    usage: Value,
}

/// Parse a Responses API SSE stream (pairs of `event:` / `data:` lines) into
/// a summary struct.
fn parse_responses_api_stream(sse: &str) -> Option<ResponsesStreamSummary> {
    let mut summary = ResponsesStreamSummary {
        response_id: String::new(),
        created: None,
        status: "in_progress".to_string(),
        output: Vec::new(),
        usage: json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}),
    };

    for frame in sse.split("\n\n") {
        let frame = frame.trim();
        if frame.is_empty() {
            continue;
        }

        let mut event_name = None::<String>;
        let mut data_str = String::new();

        for line in frame.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("event:") {
                event_name = Some(rest.trim().to_string());
            } else if let Some(rest) = trimmed.strip_prefix("data:") {
                if !data_str.is_empty() {
                    data_str.push('\n');
                }
                data_str.push_str(rest.trim_start());
            }
        }

        let Some(event) = event_name else {
            continue;
        };

        if data_str == "[DONE]" {
            continue;
        }

        let Ok(parsed) = serde_json::from_str::<Value>(&data_str) else {
            continue;
        };

        match event.as_str() {
            "response.created" => {
                if let Some(id_val) = parsed.pointer("/response/id").and_then(|v| v.as_str()) {
                    summary.response_id = id_val.to_string();
                }
                if let Some(t) = parsed
                    .pointer("/response/created_at")
                    .and_then(|v| v.as_i64())
                {
                    summary.created = Some(t);
                }
            }
            "response.output_item.done" => {
                if let Some(item) = parsed.get("item") {
                    let idx = parsed
                        .get("output_index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(summary.output.len() as u64)
                        as usize;
                    if idx >= summary.output.len() {
                        summary.output.resize(idx + 1, Value::Null);
                    }
                    summary.output[idx] = item.clone();
                }
            }
            "response.completed" | "response.done" => {
                // 9router streamToJsonConverter.js:30 treats `response.done`
                // as the same terminal as `response.completed`; some
                // Responses-compatible upstreams send only the alias.
                summary.status = "completed".to_string();
                if let Some(usage) = parsed.pointer("/response/usage") {
                    let mut map = serde_json::Map::new();
                    // Keep the cache counters so the aggregation below can fold
                    // them into prompt_tokens + prompt_tokens_details (P1-F6).
                    for key in &[
                        "input_tokens",
                        "output_tokens",
                        "total_tokens",
                        "cache_read_input_tokens",
                        "cached_tokens",
                        "cache_creation_input_tokens",
                    ] {
                        map.insert(
                            key.to_string(),
                            usage.get(*key).cloned().unwrap_or(json!(0)),
                        );
                    }
                    summary.usage = Value::Object(map);
                }
            }
            "response.failed" => {
                summary.status = "failed".to_string();
            }
            _ => {}
        }
    }

    if summary.response_id.is_empty() {
        return None;
    }

    Some(summary)
}

/// The parts of a Responses SSE stream every projection needs, parsed once.
///
/// 9router collapses the stream in one place and then renders it three ways
/// depending on the CLIENT's format (sseToJsonHandler.js:226-282). Splitting
/// the parse from the projection is what lets the three shapes agree on the
/// text and the token counts instead of each re-deriving them.
struct CollapsedResponses {
    response_id: String,
    created_at: i64,
    status: String,
    /// The `output` items, gap-filled the way 9router does
    /// (streamToJsonConverter.js:90-93).
    output: Vec<Value>,
    /// `input_tokens` EXCLUDES cached tokens on cache-capable upstreams, so
    /// the cache counters stay separate until a projection folds them in.
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}

impl CollapsedResponses {
    /// Prompt tokens with the cache counters folded in, matching 9router
    /// sseToJsonHandler.js:237-239.
    fn folded_input(&self) -> u64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    /// The assistant text, from every `message` output item.
    fn text(&self) -> String {
        let mut text_parts: Vec<String> = Vec::new();
        for item in &self.output {
            if item.get("type").and_then(|v| v.as_str()) != Some("message") {
                continue;
            }
            let Some(content_arr) = item.get("content").and_then(|v| v.as_array()) else {
                continue;
            };
            for part in content_arr {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        text_parts.push(text.to_string());
                    }
                }
            }
        }
        text_parts.join("")
    }

    /// `function_call` output items lifted into OpenAI `tool_calls`
    /// (9router sseToJsonHandler.js:249-258).
    ///
    /// These were dropped entirely, so a forceStream request that called a tool
    /// reached the client as a text-only completion that had silently lost the
    /// call.
    fn tool_calls(&self) -> Vec<Value> {
        self.output
            .iter()
            .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("function_call"))
            .enumerate()
            .map(|(idx, item)| {
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| {
                        format!(
                            "call_{}_{}_{}",
                            name,
                            chrono::Utc::now().timestamp_millis(),
                            idx
                        )
                    });
                let arguments = match item.get("arguments") {
                    Some(Value::String(s)) => Value::String(s.clone()),
                    Some(other) if !other.is_null() => other.clone(),
                    _ => Value::String("{}".to_string()),
                };
                json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": arguments }
                })
            })
            .collect()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn collapse_responses_stream(sse: &str) -> Option<CollapsedResponses> {
    let summary = parse_responses_api_stream(sse)?;
    let now = now_secs();
    let usage = |key: &str| summary.usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    // Gaps in the output index are filled rather than emitted as nulls
    // (9router streamToJsonConverter.js:90-93).
    let output: Vec<Value> = summary
        .output
        .into_iter()
        .map(|item| {
            if item.is_null() {
                json!({"type": "message", "content": [], "role": "assistant"})
            } else {
                item
            }
        })
        .collect();
    Some(CollapsedResponses {
        response_id: summary.response_id,
        created_at: summary.created.unwrap_or(now),
        status: summary.status,
        output,
        input_tokens: usage("input_tokens"),
        output_tokens: usage("output_tokens"),
        total_tokens: usage("total_tokens"),
        cache_read_input_tokens: usage("cache_read_input_tokens") + usage("cached_tokens"),
        cache_creation_input_tokens: usage("cache_creation_input_tokens"),
    })
}

/// Collapse a Responses SSE stream back into the Responses object itself.
///
/// For a Responses client (9router sseToJsonHandler.js:227-229: "Client is
/// Responses API → return as-is"). Handing such a client a `chat.completion`
/// is not a degraded rendering — it is an object with no `output` array and no
/// `usage` in the shape the client reads, so the response is unusable.
pub fn responses_stream_to_responses(input: &[u8]) -> Option<Value> {
    let sse = String::from_utf8_lossy(input);
    let sse = sse.trim();
    if !looks_like_sse(input) {
        return None;
    }
    let collapsed = collapse_responses_stream(sse)?;
    Some(json!({
        "id": if collapsed.response_id.is_empty() {
            format!("resp_{}", chrono::Utc::now().timestamp_millis())
        } else {
            collapsed.response_id
        },
        "object": "response",
        "created_at": collapsed.created_at,
        "status": collapsed.status,
        "output": collapsed.output,
        "usage": {
            "input_tokens": collapsed.input_tokens,
            "output_tokens": collapsed.output_tokens,
            "total_tokens": collapsed.total_tokens,
        }
    }))
}

/// Collapse a Responses SSE stream into the Gemini `candidates` envelope a
/// Gemini, Gemini-CLI or Antigravity client expects
/// (9router sseToJsonHandler.js:260-268).
///
/// Without it these clients get a `chat.completion`, whose `choices[].message`
/// they do not read, and a model that answered normally looks like it returned
/// nothing.
pub fn responses_stream_to_gemini(input: &[u8], fallback_model: Option<&str>) -> Option<Value> {
    let sse = String::from_utf8_lossy(input);
    let sse = sse.trim();
    if !looks_like_sse(input) {
        return None;
    }
    let collapsed = collapse_responses_stream(sse)?;
    let in_tokens = collapsed.folded_input();
    let out_tokens = collapsed.output_tokens;
    let model = fallback_model.unwrap_or("unknown");
    Some(json!({
        "response": {
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": collapsed.text() }] },
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {
                "promptTokenCount": in_tokens,
                "candidatesTokenCount": out_tokens,
                "totalTokenCount": in_tokens + out_tokens
            },
            "modelVersion": model,
            "responseId": if collapsed.response_id.is_empty() {
                format!("resp_{}", chrono::Utc::now().timestamp_millis())
            } else {
                collapsed.response_id
            }
        }
    }))
}

/// Convert an OpenAI Responses API SSE stream to a single `chat.completion`
/// JSON response.
///
/// Input looks like:
/// ```text
/// event: response.created
/// data: {"type":"response.created","response":{"id":"resp_xxx",...}}
///
/// event: response.output_item.done
/// data: ...
///
/// event: response.completed
/// data: {"type":"response.completed","response":{"usage":{...}}}
/// ```
fn convert_responses_api_stream(sse: &str, fallback_model: Option<&str>) -> Option<Value> {
    let collapsed = collapse_responses_stream(sse)?;

    let content_text = collapsed.text();
    let tool_calls = collapsed.tool_calls();
    let has_tool_calls = !tool_calls.is_empty();
    let folded_input = collapsed.folded_input();
    let total_tokens = collapsed.total_tokens;

    // 9router sseToJsonHandler.js:240-245 — prompt_tokens_details only when a
    // cache counter is non-zero, so a client can tell a cache hit from a small
    // prompt.
    let mut usage_json = json!({
        "prompt_tokens": folded_input,
        "completion_tokens": collapsed.output_tokens,
        "total_tokens": total_tokens,
    });
    if collapsed.cache_read_input_tokens > 0 || collapsed.cache_creation_input_tokens > 0 {
        let mut details = serde_json::Map::new();
        if collapsed.cache_read_input_tokens > 0 {
            details.insert(
                "cached_tokens".into(),
                json!(collapsed.cache_read_input_tokens),
            );
        }
        if collapsed.cache_creation_input_tokens > 0 {
            details.insert(
                "cache_creation_tokens".into(),
                json!(collapsed.cache_creation_input_tokens),
            );
        }
        usage_json["prompt_tokens_details"] = Value::Object(details);
    }

    let model = fallback_model.unwrap_or("unknown");

    // 9router sseToJsonHandler.js:270-273 — tool calls replace the text, and
    // `finish_reason` follows them; a stream that only made tool calls carries a
    // null content so a client does not render an empty string as the answer.
    let message = if has_tool_calls {
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": tool_calls,
        })
    } else {
        json!({ "role": "assistant", "content": content_text })
    };
    let response_done = collapsed.status == "completed" || collapsed.status == "done";
    let finish_reason = if has_tool_calls {
        "tool_calls"
    } else if response_done {
        "stop"
    } else {
        collapsed.status.as_str()
    };

    Some(json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000")),
        "object": "chat.completion",
        "created": collapsed.created_at,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": usage_json,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_looks_like_sse_true_data() {
        assert!(looks_like_sse(b"data: {\"test\": 1}"));
    }

    #[test]
    fn test_looks_like_sse_true_event() {
        assert!(looks_like_sse(b"event: foo"));
    }

    #[test]
    fn test_looks_like_sse_true_comment() {
        assert!(looks_like_sse(b": keepalive"));
    }

    #[test]
    fn test_looks_like_sse_false_json() {
        assert!(!looks_like_sse(b"{\"test\": 1}"));
    }

    #[test]
    fn test_looks_like_sse_false_empty() {
        assert!(!looks_like_sse(b""));
        assert!(!looks_like_sse(b"  "));
    }

    #[test]
    fn test_chat_stream_simple_completion() {
        let sse = concat!(
            "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":1712345678,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":1712345678,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":1712345678,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":1712345678,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let result = sse_stream_to_json(sse.as_bytes(), None).unwrap();
        assert_eq!(result["id"], "chatcmpl-abc");
        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["choices"][0]["message"]["content"], "Hello world");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
        assert_eq!(result["model"], "gpt-4");
        assert_eq!(result["created"], 1712345678);
    }

    #[test]
    fn test_chat_stream_with_tool_calls() {
        // Build the SSE programmatically to avoid escaping issues.
        let chunks = [
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}),
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"location\":\"SF\"}"}}]},"finish_reason":null}]}),
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let sse: String = chunks
            .iter()
            .map(|c| format!("data: {}\n\n", serde_json::to_string(c).unwrap()))
            .collect::<Vec<_>>()
            .join("")
            + "data: [DONE]\n\n";

        let result = sse_stream_to_json(sse.as_bytes(), None).unwrap();
        assert_eq!(result["object"], "chat.completion");
        let msg = &result["choices"][0]["message"];
        assert!(msg["content"].is_null());
        let tcs = msg["tool_calls"].as_array().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["id"], "call_1");
        assert_eq!(tcs[0]["type"], "function");
        assert_eq!(tcs[0]["function"]["name"], "get_weather");
        assert!(tcs[0]["function"]["arguments"]
            .as_str()
            .unwrap()
            .contains("SF"));
        assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn test_chat_stream_with_usage() {
        let chunks = [
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant","content":"Hi"},"finish_reason":null}]}),
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
        ];
        let sse: String = chunks
            .iter()
            .map(|c| format!("data: {}\n\n", serde_json::to_string(c).unwrap()))
            .collect::<Vec<_>>()
            .join("")
            + "data: [DONE]\n\n";

        let result = sse_stream_to_json(sse.as_bytes(), None).unwrap();
        assert_eq!(result["usage"]["prompt_tokens"], 10);
        assert_eq!(result["usage"]["completion_tokens"], 5);
        assert_eq!(result["usage"]["total_tokens"], 15);
        assert_eq!(result["choices"][0]["message"]["content"], "Hi");
    }

    #[test]
    fn test_responses_api_stream_to_chat() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_123\",\"created_at\":1712345678}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}],\"role\":\"assistant\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_123\",\"status\":\"completed\",\"usage\":{\"input_tokens\":15,\"output_tokens\":25,\"total_tokens\":40}}}\n\n",
            "data: [DONE]\n\n",
        );

        let result = sse_stream_to_json(sse.as_bytes(), Some("claude-sonnet-4")).unwrap();
        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["model"], "claude-sonnet-4");
        assert_eq!(result["choices"][0]["message"]["content"], "Hello world");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
        assert_eq!(result["usage"]["prompt_tokens"], 15);
        assert_eq!(result["usage"]["completion_tokens"], 25);
        assert_eq!(result["usage"]["total_tokens"], 40);
    }

    #[test]
    fn test_responses_api_with_multiple_output_items() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_456\",\"created_at\":1712345680}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"thinking...\"}]}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Final answer\"}],\"role\":\"assistant\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":10,\"total_tokens\":15}}}\n\n",
        );

        let result = sse_stream_to_json(sse.as_bytes(), Some("codex/o4-mini")).unwrap();
        // Should only include the message text (reasoning items are skipped).
        assert_eq!(result["choices"][0]["message"]["content"], "Final answer");
        assert_eq!(result["usage"]["total_tokens"], 15);
    }

    #[test]
    fn test_chat_stream_no_choices_returns_none() {
        let sse = "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"gpt-4\"}\n\ndata: [DONE]\n\n";
        let result = sse_stream_to_json(sse.as_bytes(), None);
        assert!(result.is_none());
    }

    #[test]
    fn test_sse_stream_to_json_rejects_plain_json() {
        let json = b"{\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion\",\"choices\":[]}";
        assert!(sse_stream_to_json(json, None).is_none());
    }

    #[test]
    fn test_sse_stream_to_json_empty() {
        assert!(sse_stream_to_json(b"", None).is_none());
    }

    #[test]
    fn test_cached_tokens_folded_into_prompt_tokens_with_details() {
        // 9router sseToJsonHandler.js: cache-capable upstreams exclude cached
        // tokens from input_tokens. Fold cache_read + cache_creation into
        // prompt_tokens and surface prompt_tokens_details.
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_cached\",\"created_at\":1712345678}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_cached\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15,\"cache_read_input_tokens\":20,\"cache_creation_input_tokens\":3}}}\n\n",
            "data: [DONE]\n\n",
        );
        let result = sse_stream_to_json(sse.as_bytes(), Some("gpt-4o")).unwrap();
        // 10 (input) + 20 (cache_read) + 3 (cache_creation) = 33.
        assert_eq!(result["usage"]["prompt_tokens"], 33);
        assert_eq!(result["usage"]["completion_tokens"], 5);
        // total_tokens keeps the upstream value (9router keeps upstream total).
        assert_eq!(result["usage"]["total_tokens"], 15);
        // prompt_tokens_details surfaces the cache counters.
        assert_eq!(
            result["usage"]["prompt_tokens_details"]["cached_tokens"],
            20
        );
        assert_eq!(
            result["usage"]["prompt_tokens_details"]["cache_creation_tokens"],
            3
        );
    }

    /// A canned stream carrying BOTH a `message` item and a `function_call`
    /// item, which is what a tool-using turn actually looks like.
    fn tool_calling_responses_sse() -> String {
        concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_tool\",\"created_at\":1712345678}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Looking that up\"}],\"role\":\"assistant\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_abc\",\"name\":\"get_weather\",\"arguments\":\"{\\\"location\\\":\\\"SF\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool\",\"status\":\"completed\",\"usage\":{\"input_tokens\":20,\"output_tokens\":8,\"total_tokens\":28}}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string()
    }

    /// A Responses client cannot read a `chat.completion` — it looks for
    /// `output` and `usage.input_tokens`, and neither exists there. 9router
    /// returns the collapsed object verbatim for this client
    /// (sseToJsonHandler.js:227-229).
    #[test]
    fn responses_source_returns_the_responses_object_not_chat_completion() {
        let sse = tool_calling_responses_sse();
        let result = responses_stream_to_responses(sse.as_bytes()).unwrap();
        assert_eq!(result["object"], "response");
        assert_eq!(result["id"], "resp_tool");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["created_at"], 1712345678);
        assert_eq!(result["usage"]["input_tokens"], 20);
        assert_eq!(result["usage"]["output_tokens"], 8);
        assert_eq!(result["usage"]["total_tokens"], 28);
        let output = result["output"].as_array().unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["type"], "message");
        assert_eq!(output[1]["type"], "function_call");
        assert!(
            result.get("choices").is_none(),
            "must not be a chat.completion"
        );
    }

    /// A Gemini-family client reads `response.candidates[0].content.parts`
    /// and `response.usageMetadata`, and nothing else.
    #[test]
    fn gemini_source_wraps_the_candidates_envelope() {
        let sse = tool_calling_responses_sse();
        let result = responses_stream_to_gemini(sse.as_bytes(), Some("gemini-2.5-pro")).unwrap();
        let text = &result["response"]["candidates"][0]["content"]["parts"][0]["text"];
        assert_eq!(text, "Looking that up");
        assert_eq!(result["response"]["candidates"][0]["finishReason"], "STOP");
        assert_eq!(result["response"]["candidates"][0]["index"], 0);
        let usage = &result["response"]["usageMetadata"];
        assert_eq!(usage["promptTokenCount"], 20);
        assert_eq!(usage["candidatesTokenCount"], 8);
        assert_eq!(usage["totalTokenCount"], 28);
        assert_eq!(result["response"]["modelVersion"], "gemini-2.5-pro");
        assert_eq!(result["response"]["responseId"], "resp_tool");
        assert!(result.get("object").is_none(), "not a chat.completion");
    }

    /// `function_call` items were dropped, so a tool-using forceStream request
    /// reached the client as a text-only completion that had silently lost the
    /// call.
    #[test]
    fn function_call_items_become_openai_tool_calls() {
        let sse = tool_calling_responses_sse();
        let result = sse_stream_to_json(sse.as_bytes(), Some("gpt-4o")).unwrap();
        let message = &result["choices"][0]["message"];
        let calls = message["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call_abc");
        assert_eq!(calls[0]["type"], "function");
        assert_eq!(calls[0]["function"]["name"], "get_weather");
        assert_eq!(
            calls[0]["function"]["arguments"].as_str().unwrap(),
            r#"{"location":"SF"}"#
        );
        assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
        // A tool call is not text, so the text field is null rather than "".
        assert!(message["content"].is_null());
    }

    /// Text-only turns must keep their content — the null is for tool calls.
    #[test]
    fn a_text_only_turn_keeps_its_content_and_a_stop_reason() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_text\",\"created_at\":1}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}],\"role\":\"assistant\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1,\"total_tokens\":4}}}\n\n",
        );
        let result = sse_stream_to_json(sse.as_bytes(), Some("gpt-4o")).unwrap();
        assert_eq!(result["choices"][0]["message"]["content"], "hi");
        assert!(result["choices"][0]["message"].get("tool_calls").is_none());
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
    }

    /// Cache counters fold into the Gemini prompt count, as they do for the
    /// chat.completion projection.
    #[test]
    fn the_gemini_projection_folds_cache_counters_into_the_prompt_count() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_c\",\"created_at\":1}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15,\"cache_read_input_tokens\":20}}}\n\n",
        );
        let result = responses_stream_to_gemini(sse.as_bytes(), Some("gemini-2.5-pro")).unwrap();
        let usage = &result["response"]["usageMetadata"];
        assert_eq!(usage["promptTokenCount"], 30);
        assert_eq!(usage["candidatesTokenCount"], 5);
        assert_eq!(usage["totalTokenCount"], 35);
    }

    /// A stream that is not SSE is not a Responses stream.
    #[test]
    fn the_responses_projections_reject_non_sse_input() {
        assert!(responses_stream_to_responses(b"{\"id\":\"x\"}").is_none());
        assert!(responses_stream_to_gemini(b"{\"id\":\"x\"}", None).is_none());
        assert!(responses_stream_to_responses(b"").is_none());
    }

    #[test]
    fn test_no_cache_counters_no_details() {
        // No cache counters → no prompt_tokens_details, prompt_tokens unchanged.
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_nocache\",\"created_at\":1712345678}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_nocache\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15}}}\n\n",
            "data: [DONE]\n\n",
        );
        let result = sse_stream_to_json(sse.as_bytes(), Some("gpt-4o")).unwrap();
        assert_eq!(result["usage"]["prompt_tokens"], 10);
        assert!(result["usage"].get("prompt_tokens_details").is_none());
    }
}
