//! OpenAI Responses API support.
//!
//! Port of `open-sse/handlers/responsesHandler.js` plus
//! `transformer/streamToJsonConverter.js`. The format-conversion entry
//! point ([`convert_responses_api_format`]) wraps the existing translator
//! at `crate::core::translator::request::openai_responses`. Stream→JSON
//! collapsing is handled here directly.

use serde_json::{json, Map, Value};

use crate::core::translator::request::openai_responses::openai_responses_to_chat_request;

/// Convert OpenAI Responses-API style request `{ input, instructions, tools }`
/// to the standard Chat Completions shape `{ messages, tools }`. Wraps the
/// translator already living in `core::translator::request::openai_responses`.
///
/// Returns the rewritten body. Idempotent on Chat-shaped bodies (no `input`).
pub fn convert_responses_api_format(body: &Value) -> Value {
    let mut out = body.clone();
    // Coerce string `input` → singleton array so the translator's
    // array-only iteration applies. Mirrors `normalizeResponsesInput` in
    // the upstream JS, which also injects a "..." placeholder when the
    // string is empty (providers reject empty messages[]).
    if let Some(input) = out.get("input").cloned() {
        match input {
            Value::String(s) => {
                let text = if s.trim().is_empty() { "..." } else { s.as_str() };
                out["input"] = json!([{
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": text}]
                }]);
            }
            Value::Array(arr) if arr.is_empty() => {
                out["input"] = json!([{
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "..."}]
                }]);
            }
            _ => {}
        }
    }
    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let _ = openai_responses_to_chat_request("", &mut out, stream, None);
    out
}

/// Outcome of [`convert_responses_stream_to_json`]. Keeps the same fields
/// `convertResponsesStreamToJson` produced upstream so the calling axum
/// handler can return them verbatim.
#[derive(Debug, Clone)]
pub struct ResponsesStreamSummary {
    pub response_id: String,
    pub created: i64,
    pub status: String,
    pub items: Vec<Value>,
    pub usage: Value,
}

/// Walk an SSE Responses-API stream and collapse it into a single JSON
/// summary.  `input` is the raw stream text already accumulated by the
/// caller (since the byte stream needs to come from axum/reqwest first).
pub fn convert_responses_stream_to_json(input: &str) -> Value {
    let mut summary = ResponsesStreamSummary {
        response_id: String::new(),
        created: 0,
        status: "in_progress".to_string(),
        items: Vec::new(),
        usage: json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}),
    };

    // Frames are delimited by blank lines.
    for frame in input.split("\n\n") {
        if frame.trim().is_empty() {
            continue;
        }
        let mut event_name = None::<String>;
        let mut data = String::new();
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                event_name = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim_start());
            }
        }
        let Some(event) = event_name else {
            continue;
        };
        if data == "[DONE]" {
            continue;
        }
        let parsed: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match event.as_str() {
            "response.created" => {
                if let Some(id) = parsed.pointer("/response/id").and_then(|v| v.as_str()) {
                    summary.response_id = id.to_string();
                }
                if let Some(t) = parsed
                    .pointer("/response/created_at")
                    .and_then(|v| v.as_i64())
                {
                    summary.created = t;
                }
            }
            "response.output_item.done" => {
                if let Some(item) = parsed.get("item") {
                    let idx = parsed
                        .get("output_index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(summary.items.len() as u64) as usize;
                    if idx >= summary.items.len() {
                        summary.items.resize(idx + 1, Value::Null);
                    }
                    summary.items[idx] = item.clone();
                }
            }
            "response.completed" => {
                summary.status = "completed".into();
                if let Some(usage) = parsed.pointer("/response/usage") {
                    let map: Map<String, Value> = ["input_tokens", "output_tokens", "total_tokens"]
                        .into_iter()
                        .map(|k| (k.to_string(), usage.get(k).cloned().unwrap_or(json!(0))))
                        .collect();
                    summary.usage = Value::Object(map);
                }
            }
            "response.failed" => {
                summary.status = "failed".into();
            }
            _ => {}
        }
    }

    json!({
        "id": summary.response_id,
        "object": "response",
        "created_at": summary.created,
        "status": summary.status,
        "output": summary.items.into_iter().filter(|v| !v.is_null()).collect::<Vec<_>>(),
        "usage": summary.usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_responses_api_format_no_op_when_no_input() {
        let body = json!({"messages": [{"role": "user", "content": "hi"}]});
        let out = convert_responses_api_format(&body);
        // Without `input` the translator returns the body verbatim except
        // for any in-place rewrites — `messages` should still be present.
        assert!(out.get("messages").is_some());
    }

    #[test]
    fn convert_responses_api_format_lifts_input_to_messages() {
        let body = json!({
            "input": "hello world",
            "instructions": "be brief"
        });
        let out = convert_responses_api_format(&body);
        let messages = out["messages"].as_array().expect("messages");
        // First message should be a system message carrying the instructions.
        assert_eq!(messages[0]["role"], "system");
        // And there should be at least one user message with the prompt text.
        assert!(messages
            .iter()
            .any(|m| m["role"] == "user"));
    }

    #[test]
    fn stream_to_json_collapses_simple_completion() {
        let stream = "\
event: response.created\n\
data: {\"response\":{\"id\":\"resp_123\",\"created_at\":1234567890}}\n\n\
event: response.output_item.done\n\
data: {\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[]}}\n\n\
event: response.completed\n\
data: {\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"total_tokens\":30}}}\n\n\
data: [DONE]\n\n";
        let collapsed = convert_responses_stream_to_json(stream);
        assert_eq!(collapsed["id"], "resp_123");
        assert_eq!(collapsed["status"], "completed");
        assert_eq!(collapsed["created_at"], 1234567890);
        assert_eq!(collapsed["output"].as_array().unwrap().len(), 1);
        assert_eq!(collapsed["usage"]["total_tokens"], 30);
    }

    #[test]
    fn stream_to_json_handles_failed_state() {
        let stream = "\
event: response.created\n\
data: {\"response\":{\"id\":\"r1\"}}\n\n\
event: response.failed\n\
data: {\"error\":{\"message\":\"oops\"}}\n\n";
        let collapsed = convert_responses_stream_to_json(stream);
        assert_eq!(collapsed["status"], "failed");
        assert_eq!(collapsed["id"], "r1");
    }
}
