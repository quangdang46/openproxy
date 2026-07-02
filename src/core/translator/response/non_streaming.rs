//! Non-streaming response transforms for cross-format providers.
//!
//! These transforms convert complete (non-streaming) response JSON bodies from
//! provider-specific formats (Claude Messages API, Gemini, Ollama, Kiro, etc.)
//! to the OpenAI chat.completion format (or vice versa).
//!
//! Unlike the streaming transforms in sibling modules, these operate on the
//! entire response body at once and mutate it in-place.

use serde_json::Value;

/// Claude Messages API -> OpenAI chat.completion (non-streaming).
pub fn claude_to_openai_non_streaming(response: &mut Value) -> bool {
    // Already in OpenAI format, skip.
    if response.get("object").and_then(|v| v.as_str()) == Some("chat.completion") {
        return false;
    }

    let id = response
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("msg_unknown")
        .to_string();
    let model = response
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Extract text content and tool calls from Claude's content array.
    let mut text_content = String::new();
    let mut tool_calls = Vec::new();

    if let Some(content_arr) = response.get("content").and_then(|v| v.as_array()) {
        for block in content_arr {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        text_content.push_str(text);
                    }
                }
                Some("tool_use") => {
                    let tool_id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let tool_name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let tool_input = block.get("input").cloned().unwrap_or(Value::Null);
                    let args_str =
                        serde_json::to_string(&tool_input).unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(serde_json::json!({
                        "id": tool_id,
                        "type": "function",
                        "function": {
                            "name": tool_name,
                            "arguments": args_str
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    // Map stop_reason.
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else {
        match response.get("stop_reason").and_then(|v| v.as_str()) {
            Some("end_turn") | Some("stop_sequence") => "stop",
            Some("max_tokens") => "length",
            Some("tool_use") => "tool_calls",
            _ => "stop",
        }
    };

    // Map usage.
    let usage = if let Some(usage) = response.get("usage") {
        let input = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_create = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let prompt = input + cache_read + cache_create;
        serde_json::json!({
            "prompt_tokens": prompt,
            "completion_tokens": output,
            "total_tokens": prompt + output
        })
    } else {
        serde_json::json!({
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        })
    };

    let mut choice = serde_json::json!({
        "index": 0,
        "message": {
            "role": "assistant",
            "content": text_content
        },
        "finish_reason": finish_reason,
        "logprobs": null
    });

    if !tool_calls.is_empty() {
        choice["message"]["tool_calls"] = Value::Array(tool_calls);
    }

    let created = chrono::Utc::now().timestamp();

    *response = serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [choice],
        "usage": usage
    });

    true
}

/// Gemini -> OpenAI chat.completion (non-streaming).
///
/// Handles both standard Gemini and GeminiCli / Antigravity formats.
pub fn gemini_to_openai_non_streaming(response: &mut Value) -> bool {
    if response.get("object").and_then(|v| v.as_str()) == Some("chat.completion") {
        return false;
    }

    let candidate = response
        .get("candidates")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());

    let (text_content, tool_calls, finish_reason) = if let Some(cand) = candidate {
        let parts = cand
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|v| v.as_array());

        let mut text = String::new();
        let mut tools = Vec::new();
        let mut has_function_call = false;

        if let Some(parts_arr) = parts {
            for part in parts_arr {
                let is_thought = part
                    .get("thought")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                // Skip thinking blocks for the non-streaming content body.
                if is_thought {
                    continue;
                }
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    text.push_str(t);
                }
                if let Some(fc) = part.get("functionCall") {
                    has_function_call = true;
                    let name = fc
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = fc.get("args").cloned().unwrap_or(Value::Null);
                    let args_str =
                        serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                    let tool_id = format!("call_{}", chrono::Utc::now().timestamp_millis());
                    tools.push(serde_json::json!({
                        "id": tool_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": args_str
                        }
                    }));
                }
            }
        }

        let fr = cand
            .get("finishReason")
            .and_then(|v| v.as_str())
            .map(|r| match r {
                "STOP" => "stop",
                "MAX_TOKENS" => "length",
                "SAFETY" | "BLOCKLIST" | "PROHIBITED_CONTENT" => "content_filter",
                "RECITATION" => "content_filter",
                "SPII" => "content_filter",
                "OTHER" => "stop",
                _ => "stop",
            })
            .unwrap_or("stop");

        let effective_fr = if has_function_call { "tool_calls" } else { fr };

        (text, tools, effective_fr.to_string())
    } else {
        (String::new(), vec![], "stop".to_string())
    };

    let mut choice = serde_json::json!({
        "index": 0,
        "message": {
            "role": "assistant",
            "content": text_content
        },
        "finish_reason": finish_reason,
        "logprobs": null
    });

    if !tool_calls.is_empty() {
        choice["message"]["tool_calls"] = Value::Array(tool_calls);
    }

    // Map usage from Gemini's usageMetadata.
    let usage = if let Some(usage) = response.get("usageMetadata") {
        let prompt = usage
            .get("promptTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let completion = usage
            .get("candidatesTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let total = usage
            .get("totalTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let mut openai_usage = serde_json::json!({
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": total
        });

        if let Some(cached) = usage
            .get("cachedContentTokenCount")
            .and_then(|v| v.as_u64())
        {
            if cached > 0 {
                openai_usage["prompt_tokens_details"] =
                    serde_json::json!({ "cached_tokens": cached });
            }
        }

        let thoughts = usage
            .get("thoughtsTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if thoughts > 0 {
            openai_usage["completion_tokens_details"] =
                serde_json::json!({ "reasoning_tokens": thoughts });
        }

        openai_usage
    } else {
        serde_json::json!({
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        })
    };

    let model = response
        .get("modelVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("gemini")
        .to_string();
    let created = chrono::Utc::now().timestamp();
    let id = format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis());

    *response = serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [choice],
        "usage": usage
    });

    true
}

/// Ollama -> OpenAI chat.completion (non-streaming).
pub fn ollama_to_openai_non_streaming(response: &mut Value) -> bool {
    if response.get("object").and_then(|v| v.as_str()) == Some("chat.completion") {
        return false;
    }

    let model = response
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("ollama")
        .to_string();

    let mut text_content = String::new();
    let mut tool_calls = Vec::new();

    if let Some(message) = response.get("message") {
        text_content = message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if let Some(tcs) = message.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                if let Some(func) = tc.get("function") {
                    let name = func
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = func.get("arguments").cloned().unwrap_or(Value::Null);
                    let args_str =
                        serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                    let tool_id = format!("call_{}", chrono::Utc::now().timestamp_millis());
                    tool_calls.push(serde_json::json!({
                        "id": tool_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": args_str
                        }
                    }));
                }
            }
        }
    }

    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else {
        "stop"
    };

    let mut choice = serde_json::json!({
        "index": 0,
        "message": {
            "role": "assistant",
            "content": text_content
        },
        "finish_reason": finish_reason,
        "logprobs": null
    });

    if !tool_calls.is_empty() {
        choice["message"]["tool_calls"] = Value::Array(tool_calls);
    }

    let prompt_tokens = response
        .get("prompt_eval_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = response
        .get("eval_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let usage = serde_json::json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens
    });

    let created = chrono::Utc::now().timestamp();
    let id = format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis());

    *response = serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [choice],
        "usage": usage
    });

    true
}

/// Kiro -> OpenAI chat.completion (non-streaming).
///
/// Kiro's non-streaming response may already be in OpenAI-compatible format.
/// If the response has a `choices` array, we add the `object` field if missing.
pub fn kiro_to_openai_non_streaming(response: &mut Value) -> bool {
    if response.get("object").and_then(|v| v.as_str()) == Some("chat.completion") {
        return false;
    }

    // If it has choices but no object, fix it.
    if response
        .get("choices")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
    {
        if let Some(obj) = response.as_object_mut() {
            obj.insert(
                "object".to_string(),
                Value::String("chat.completion".to_string()),
            );
        }
        return true;
    }

    false
}

/// CommandCode -> OpenAI chat.completion (non-streaming).
///
/// CommandCode's non-streaming response may already be in OpenAI-compatible format.
/// If the response has a `choices` array, we add the `object` field if missing.
pub fn commandcode_to_openai_non_streaming(response: &mut Value) -> bool {
    if response.get("object").and_then(|v| v.as_str()) == Some("chat.completion") {
        return false;
    }

    if response
        .get("choices")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
    {
        if let Some(obj) = response.as_object_mut() {
            obj.insert(
                "object".to_string(),
                Value::String("chat.completion".to_string()),
            );
        }
        return true;
    }

    false
}

/// OpenAI chat.completion -> Claude Messages API (non-streaming).
///
/// Used when a Claude-format client sends a request to an OpenAI-compatible provider.
/// Converts the OpenAI response back to Claude Messages API format.
pub fn openai_to_claude_non_streaming(response: &mut Value) -> bool {
    // Already in Claude format, skip.
    if response.get("type").and_then(|v| v.as_str()) == Some("message") {
        return false;
    }

    // If not an OpenAI chat completion, skip.
    if response.get("object").and_then(|v| v.as_str()) != Some("chat.completion") {
        return false;
    }

    let choice = response
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());

    let Some(choice) = choice else {
        return false;
    };

    let message = choice.get("message");

    // Build content array.
    let mut content_arr: Vec<Value> = Vec::new();
    let mut stop_reason = "end_turn";

    if let Some(msg) = message {
        if let Some(text) = msg.get("content").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                content_arr.push(serde_json::json!({
                    "type": "text",
                    "text": text
                }));
            }
        }

        if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            stop_reason = "tool_use";
            for tc in tool_calls {
                let tool_id = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_args: Value = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| {
                        if a.is_string() {
                            serde_json::from_str(a.as_str().unwrap_or("{}")).ok()
                        } else {
                            Some(a.clone())
                        }
                    })
                    .unwrap_or(Value::Object(serde_json::Map::new()));
                content_arr.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tool_id,
                    "name": tool_name,
                    "input": tool_args
                }));
            }
        }
    }

    // Map finish_reason.
    if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
        stop_reason = match fr {
            "stop" => "end_turn",
            "length" | "max_tokens" => "max_tokens",
            "tool_calls" => "tool_use",
            "content_filter" => "end_turn",
            _ => "end_turn",
        };
    }

    // Map usage.
    let usage = if let Some(usage) = response.get("usage") {
        let input = usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        serde_json::json!({
            "input_tokens": input,
            "output_tokens": output
        })
    } else {
        serde_json::json!({
            "input_tokens": 0,
            "output_tokens": 0
        })
    };

    let id = response
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("msg_unknown")
        .to_string();
    let model = response
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    *response = serde_json::json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "content": content_arr,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage
    });

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Claude -> OpenAI ──────────────────────────────────────────────

    #[test]
    fn test_claude_to_openai_simple_text() {
        let mut resp = json!({
            "id": "msg_abc123",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello world!"}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 20}
        });
        let result = claude_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["object"], "chat.completion");
        assert_eq!(resp["choices"][0]["message"]["content"], "Hello world!");
        assert_eq!(resp["choices"][0]["finish_reason"], "stop");
        assert_eq!(resp["usage"]["prompt_tokens"], 10);
        assert_eq!(resp["usage"]["completion_tokens"], 20);
        assert_eq!(resp["model"], "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_claude_to_openai_with_tool_calls() {
        let mut resp = json!({
            "id": "msg_abc456",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "I'll search for that."},
                {"type": "tool_use", "id": "tu_123", "name": "WebSearch", "input": {"query": "rust"}}
            ],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 30}
        });
        let result = claude_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            resp["choices"][0]["message"]["content"],
            "I'll search for that."
        );
        let tc = &resp["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["id"], "tu_123");
        assert_eq!(tc["function"]["name"], "WebSearch");
        assert!(tc["function"]["arguments"]
            .as_str()
            .unwrap()
            .contains("rust"));
    }

    #[test]
    fn test_claude_to_openai_empty_content() {
        let mut resp = json!({
            "id": "msg_empty",
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 0}
        });
        let result = claude_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["object"], "chat.completion");
        assert_eq!(resp["choices"][0]["message"]["content"], "");
        assert_eq!(resp["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn test_claude_to_openai_skips_already_openai() {
        let mut resp = json!({"object": "chat.completion", "choices": []});
        let result = claude_to_openai_non_streaming(&mut resp);
        assert!(!result);
    }

    // ── Gemini -> OpenAI ──────────────────────────────────────────────

    #[test]
    fn test_gemini_to_openai_simple_text() {
        let mut resp = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello from Gemini!"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 20,
                "totalTokenCount": 30
            },
            "modelVersion": "gemini-2.0-flash"
        });
        let result = gemini_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["object"], "chat.completion");
        assert_eq!(
            resp["choices"][0]["message"]["content"],
            "Hello from Gemini!"
        );
        assert_eq!(resp["usage"]["prompt_tokens"], 10);
        assert_eq!(resp["usage"]["completion_tokens"], 20);
        assert_eq!(resp["model"], "gemini-2.0-flash");
    }

    #[test]
    fn test_gemini_to_openai_with_function_call() {
        let mut resp = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "search",
                            "args": {"q": "rust"}
                        }
                    }]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {"totalTokenCount": 15},
            "modelVersion": "gemini-2.0-flash"
        });
        let result = gemini_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["choices"][0]["finish_reason"], "tool_calls");
        let tc = &resp["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["function"]["name"], "search");
        assert!(tc["function"]["arguments"]
            .as_str()
            .unwrap()
            .contains("rust"));
    }

    #[test]
    fn test_gemini_to_openai_skips_thought_parts() {
        let mut resp = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"thought": true, "text": "thinking step"},
                        {"text": "Hello world"}
                    ]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {"totalTokenCount": 20},
            "modelVersion": "gemini-2.0-flash"
        });
        let result = gemini_to_openai_non_streaming(&mut resp);
        assert!(result);
        // Only the non-thought text should appear in content.
        assert_eq!(resp["choices"][0]["message"]["content"], "Hello world");
    }

    #[test]
    fn test_gemini_to_openai_empty_candidates() {
        let mut resp = json!({
            "candidates": [],
            "usageMetadata": {"totalTokenCount": 0},
            "modelVersion": "gemini-2.0-flash"
        });
        let result = gemini_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["choices"][0]["message"]["content"], "");
    }

    // ── Ollama -> OpenAI ──────────────────────────────────────────────

    #[test]
    fn test_ollama_to_openai_simple_text() {
        let mut resp = json!({
            "model": "llama3",
            "created_at": "2024-01-01T00:00:00Z",
            "message": {"role": "assistant", "content": "Hello from Ollama!"},
            "done": true,
            "prompt_eval_count": 10,
            "eval_count": 20
        });
        let result = ollama_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["object"], "chat.completion");
        assert_eq!(
            resp["choices"][0]["message"]["content"],
            "Hello from Ollama!"
        );
        assert_eq!(resp["choices"][0]["finish_reason"], "stop");
        assert_eq!(resp["usage"]["prompt_tokens"], 10);
        assert_eq!(resp["usage"]["completion_tokens"], 20);
    }

    #[test]
    fn test_ollama_to_openai_with_tools() {
        let mut resp = json!({
            "model": "llama3",
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{"function": {"name": "search", "arguments": {"q": "rust"}}}]
            },
            "done": true,
            "prompt_eval_count": 5,
            "eval_count": 10
        });
        let result = ollama_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["choices"][0]["finish_reason"], "tool_calls");
        let tc = &resp["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["function"]["name"], "search");
    }

    #[test]
    fn test_ollama_to_openai_skips_already_openai() {
        let mut resp = json!({"object": "chat.completion", "choices": []});
        let result = ollama_to_openai_non_streaming(&mut resp);
        assert!(!result);
    }

    // ── Kiro -> OpenAI ────────────────────────────────────────────────

    #[test]
    fn test_kiro_to_openai_already_openai() {
        let mut resp = json!({"object": "chat.completion", "choices": [{"index": 0}]});
        let result = kiro_to_openai_non_streaming(&mut resp);
        assert!(!result);
    }

    #[test]
    fn test_kiro_to_openai_adds_object() {
        let mut resp = json!({"choices": [{"index": 0, "message": {"content": "hello"}}]});
        let result = kiro_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["object"], "chat.completion");
    }

    #[test]
    fn test_kiro_to_openai_skips_unrecognized() {
        let mut resp = json!({"some_field": "value"});
        let result = kiro_to_openai_non_streaming(&mut resp);
        assert!(!result);
    }

    // ── CommandCode -> OpenAI ──────────────────────────────────────────

    #[test]
    fn test_commandcode_to_openai_adds_object() {
        let mut resp = json!({"choices": [{"index": 0}]});
        let result = commandcode_to_openai_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["object"], "chat.completion");
    }

    #[test]
    fn test_commandcode_to_openai_skips_unknown() {
        let mut resp = json!({"type": "error"});
        let result = commandcode_to_openai_non_streaming(&mut resp);
        assert!(!result);
    }

    // ── OpenAI -> Claude ──────────────────────────────────────────────

    #[test]
    fn test_openai_to_claude_conversion() {
        let mut resp = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello from GPT!"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
        });
        let result = openai_to_claude_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["type"], "message");
        assert_eq!(resp["role"], "assistant");
        assert_eq!(resp["content"][0]["type"], "text");
        assert_eq!(resp["content"][0]["text"], "Hello from GPT!");
        assert_eq!(resp["stop_reason"], "end_turn");
    }

    #[test]
    fn test_openai_to_claude_with_tool_calls() {
        let mut resp = json!({
            "id": "chatcmpl-456",
            "object": "chat.completion",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Using tools.",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {"name": "search", "arguments": "{\"q\": \"hello\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 10}
        });
        let result = openai_to_claude_non_streaming(&mut resp);
        assert!(result);
        assert_eq!(resp["type"], "message");
        assert_eq!(resp["stop_reason"], "tool_use");
        assert_eq!(resp["content"][1]["type"], "tool_use");
        assert_eq!(resp["content"][1]["name"], "search");
        assert_eq!(resp["content"][1]["input"]["q"], "hello");
    }

    #[test]
    fn test_openai_to_claude_skips_claude() {
        let mut resp = json!({"type": "message", "role": "assistant"});
        let result = openai_to_claude_non_streaming(&mut resp);
        assert!(!result);
    }

    #[test]
    fn test_openai_to_claude_skips_non_openai() {
        let mut resp = json!({"some": "thing"});
        let result = openai_to_claude_non_streaming(&mut resp);
        assert!(!result);
    }

    // ── Idempotency guards ────────────────────────────────────────────

    #[test]
    fn test_all_skips_already_openai() {
        let mut resp = json!({"object": "chat.completion", "choices": []});
        assert!(!claude_to_openai_non_streaming(&mut resp));
        assert!(!gemini_to_openai_non_streaming(&mut resp));
        assert!(!ollama_to_openai_non_streaming(&mut resp));
    }
}
