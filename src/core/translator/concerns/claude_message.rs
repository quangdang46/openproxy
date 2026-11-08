//! OpenAI chat.completion → Claude `message` collapse.
//!
//! Port of 9router `open-sse/translator/concerns/claudeMessage.js` +
//! `concerns/finishReason.js` (`fromOpenAIFinish`, claude direction).
//!
//! Used by the forced SSE→JSON path: when a provider streams but the client
//! asked for JSON and speaks the Claude Messages format (`/v1/messages`), the
//! collapsed `chat.completion` must be re-rendered as a Claude `message`
//! object (text/thinking/tool_use blocks, `stop_reason`, `usage`) — a
//! `choices[]` body is unparseable for that client.

use serde_json::{json, Value};

/// 9router `parseToolArguments` (claudeMessage.js:5-13): falsy → `{}`;
/// objects/arrays pass through; strings are JSON-parsed, `{}` on failure.
fn parse_tool_arguments(value: Option<&Value>) -> Value {
    match value {
        None | Some(Value::Null) => json!({}),
        Some(Value::String(s)) => {
            if s.is_empty() {
                return json!({});
            }
            serde_json::from_str(s).unwrap_or_else(|_| json!({}))
        }
        Some(Value::Object(_)) | Some(Value::Array(_)) => value.cloned().unwrap_or(json!({})),
        // JS falsy scalars (0, false) hit the `!value` guard → {}.
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => json!({}),
        Some(Value::Bool(false)) => json!({}),
        // Other scalars: JS `JSON.parse` coerces via String(value), which
        // round-trips the scalar — return it as-is.
        Some(other) => other.clone(),
    }
}

/// 9router `fromOpenAIFinish(reason, "claude")` (finishReason.js:55-64):
/// stop→end_turn, length→max_tokens, tool_calls→tool_use,
/// content_filter→refusal, anything else (incl. missing)→end_turn.
fn from_openai_finish_claude(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("length") => "max_tokens",
        Some("tool_calls") => "tool_use",
        Some("content_filter") => "refusal",
        _ => "end_turn",
    }
}

/// `usage.prompt_tokens || usage.input_tokens || 0` — JS `||` treats 0 as
/// falsy, so a zero prompt_tokens falls through to input_tokens.
fn usage_tokens(usage: &Value, primary: &str, fallback: &str) -> u64 {
    [primary, fallback]
        .iter()
        .find_map(|key| usage.get(*key).and_then(|v| v.as_u64()).filter(|n| *n != 0))
        .unwrap_or(0)
}

/// Convert an OpenAI chat.completion JSON body into the Claude Messages API
/// shape (9router `openAICompletionToClaudeMessage`, claudeMessage.js:15-53).
///
/// Returns the body unchanged when it has no `choices[0]` (upstream guard at
/// claudeMessage.js:16), matching the JS early return.
pub fn openai_completion_to_claude_message(response_body: &Value) -> Value {
    let Some(choice) = response_body
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
    else {
        return response_body.clone();
    };
    let message = choice.get("message").cloned().unwrap_or(json!({}));

    let mut content: Vec<Value> = Vec::new();

    // reasoning_content → thinking block (claudeMessage.js:21-24). The
    // provider_specific_fields alias is checked too, as upstream does.
    let reasoning = message
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            message
                .pointer("/provider_specific_fields/reasoning_content")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        });
    if let Some(reasoning) = reasoning {
        content.push(json!({ "type": "thinking", "thinking": reasoning }));
    }

    if let Some(text) = message.get("content").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
    }

    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for tool_call in tool_calls {
            let function = tool_call.get("function");
            let name = function
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .or_else(|| tool_call.get("name").and_then(|v| v.as_str()))
                .unwrap_or("");
            let arguments = function
                .and_then(|f| f.get("arguments"))
                .or_else(|| tool_call.get("arguments"));
            let id = tool_call
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| {
                    format!(
                        "toolu_{}_{}",
                        chrono::Utc::now().timestamp_millis(),
                        content.len()
                    )
                });
            content.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": parse_tool_arguments(arguments),
            }));
        }
    }

    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "" }));
    }

    let usage = response_body.get("usage").cloned().unwrap_or(json!({}));

    // `String(id || msg_<now>).replace(/^chatcmpl-/, "")` — the chatcmpl-
    // prefix is stripped WITHOUT re-adding msg_ (claudeMessage.js:41).
    let id = response_body
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("msg_{}", chrono::Utc::now().timestamp_millis()));
    let id = id.strip_prefix("chatcmpl-").unwrap_or(&id).to_string();

    let model = response_body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": from_openai_finish_claude(
            choice.get("finish_reason").and_then(|v| v.as_str())
        ),
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": usage_tokens(&usage, "prompt_tokens", "input_tokens"),
            "output_tokens": usage_tokens(&usage, "completion_tokens", "output_tokens"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(content: &str, finish: &str) -> Value {
        json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "created": 1712345678,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": finish,
            }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18 },
        })
    }

    #[test]
    fn text_completion_becomes_claude_message() {
        let result = openai_completion_to_claude_message(&completion("Hello", "stop"));
        assert_eq!(result["type"], "message");
        assert_eq!(result["role"], "assistant");
        assert_eq!(result["model"], "claude-sonnet-4-6");
        // chatcmpl- prefix stripped, NOT re-prefixed with msg_ (upstream quirk).
        assert_eq!(result["id"], "abc123");
        assert_eq!(result["stop_reason"], "end_turn");
        assert_eq!(result["stop_sequence"], Value::Null);
        assert_eq!(
            result["content"],
            json!([{ "type": "text", "text": "Hello" }])
        );
        assert_eq!(result["usage"]["input_tokens"], 11);
        assert_eq!(result["usage"]["output_tokens"], 7);
        assert!(result.get("choices").is_none());
        assert!(result.get("object").is_none());
    }

    #[test]
    fn reasoning_becomes_thinking_block_first() {
        let mut body = completion("answer", "stop");
        body["choices"][0]["message"]["reasoning_content"] = json!("let me think");
        let result = openai_completion_to_claude_message(&body);
        assert_eq!(
            result["content"],
            json!([
                { "type": "thinking", "thinking": "let me think" },
                { "type": "text", "text": "answer" },
            ])
        );
    }

    #[test]
    fn tool_calls_become_tool_use_blocks() {
        let mut body = completion("", "tool_calls");
        body["choices"][0]["message"]["content"] = Value::Null;
        body["choices"][0]["message"]["tool_calls"] = json!([{
            "id": "call_1",
            "type": "function",
            "function": { "name": "get_weather", "arguments": "{\"location\":\"SF\"}" },
        }]);
        let result = openai_completion_to_claude_message(&body);
        assert_eq!(result["stop_reason"], "tool_use");
        assert_eq!(
            result["content"],
            json!([{
                "type": "tool_use",
                "id": "call_1",
                "name": "get_weather",
                "input": { "location": "SF" },
            }])
        );
    }

    #[test]
    fn invalid_tool_arguments_parse_to_empty_object() {
        let mut body = completion("", "tool_calls");
        body["choices"][0]["message"]["content"] = Value::Null;
        body["choices"][0]["message"]["tool_calls"] = json!([{
            "function": { "name": "f", "arguments": "{not json" },
        }]);
        let result = openai_completion_to_claude_message(&body);
        assert_eq!(result["content"][0]["input"], json!({}));
        // Missing id falls back to toolu_<ts>_<block index>.
        assert!(result["content"][0]["id"]
            .as_str()
            .unwrap()
            .starts_with("toolu_"));
    }

    #[test]
    fn finish_reason_mapping_matches_upstream() {
        for (finish, stop) in [
            ("stop", "end_turn"),
            ("length", "max_tokens"),
            ("tool_calls", "tool_use"),
            ("content_filter", "refusal"),
            ("anything_else", "end_turn"),
        ] {
            let body = completion("x", finish);
            assert_eq!(
                openai_completion_to_claude_message(&body)["stop_reason"],
                stop,
                "finish_reason={finish}"
            );
        }
        // Missing finish_reason also defaults to end_turn (JS `default` arm).
        let mut body = completion("x", "stop");
        body["choices"][0]
            .as_object_mut()
            .unwrap()
            .remove("finish_reason");
        assert_eq!(
            openai_completion_to_claude_message(&body)["stop_reason"],
            "end_turn"
        );
    }

    #[test]
    fn empty_content_gets_empty_text_block() {
        let result = openai_completion_to_claude_message(&completion("", "stop"));
        assert_eq!(result["content"], json!([{ "type": "text", "text": "" }]));
    }

    #[test]
    fn body_without_choices_passes_through_unchanged() {
        let body = json!({ "error": "upstream failed" });
        assert_eq!(openai_completion_to_claude_message(&body), body);
    }

    #[test]
    fn zero_prompt_tokens_falls_through_to_input_tokens() {
        // JS `usage.prompt_tokens || usage.input_tokens || 0`: 0 is falsy.
        let mut body = completion("x", "stop");
        body["usage"] = json!({ "prompt_tokens": 0, "input_tokens": 42, "completion_tokens": 0, "output_tokens": 9 });
        let result = openai_completion_to_claude_message(&body);
        assert_eq!(result["usage"]["input_tokens"], 42);
        assert_eq!(result["usage"]["output_tokens"], 9);
    }
}
