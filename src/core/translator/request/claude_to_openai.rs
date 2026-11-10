//! Claude to OpenAI request translator
//!
//! Converts Claude API request format to OpenAI-compatible format.

use serde_json::Value;

const DEFAULT_MAX_TOKENS: u64 = 64000;
const DEFAULT_MIN_TOKENS: u64 = 32000;

/// Strip the `x-anthropic-billing-header:` line Claude Code prepends to the
/// system prompt. Mirrors `stripAnthropicBillingHeader` in
/// open-sse/translator/request/claude-to-openai.js.
fn strip_anthropic_billing_header(text: &str) -> String {
    // Remove a leading `x-anthropic-billing-header: ...` line (up to and
    // including its newline). Case-insensitive like the JS /i flag.
    let lower = text.to_ascii_lowercase();
    const PREFIX: &str = "x-anthropic-billing-header:";
    if lower.starts_with(PREFIX) {
        if let Some(idx) = text[PREFIX.len()..].find('\n') {
            return text[PREFIX.len() + idx + 1..].to_string();
        }
        // Header-only content: the whole string is the billing line.
        return String::new();
    }
    text.to_string()
}

/// Collapse an OpenAI content-part array: a lone text part becomes a plain
/// string, otherwise the array is returned as-is. Cache-aware targets retain
/// marked blocks. Mirrors `collapseTextParts` in
/// open-sse/translator/concerns/message.js.
fn collapse_text_parts(parts: Vec<Value>, preserve_cache_control: bool) -> Value {
    if parts.len() == 1
        && parts[0].get("type").and_then(Value::as_str) == Some("text")
        && !(preserve_cache_control && parts[0].get("cache_control").is_some())
    {
        return parts[0]
            .get("text")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new()));
    }
    Value::Array(parts)
}

/// Adjust max_tokens based on request context.
/// Mirrors the logic in open-sse/translator/helpers/maxTokensHelper.js
fn adjust_max_tokens(body: &serde_json::Map<String, Value>) -> Option<u64> {
    let max_tokens = body
        .get("max_tokens")?
        .as_u64()
        .unwrap_or(DEFAULT_MAX_TOKENS);

    // Auto-increase for tool calling to prevent truncated arguments
    let max_tokens = if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        if !tools.is_empty() && max_tokens < DEFAULT_MIN_TOKENS {
            DEFAULT_MIN_TOKENS
        } else {
            max_tokens
        }
    } else {
        max_tokens
    };

    // Ensure max_tokens > thinking.budget_tokens (Claude API requirement)
    if let Some(thinking) = body.get("thinking") {
        if let Some(budget_tokens) = thinking.get("budget_tokens").and_then(|v| v.as_u64()) {
            if max_tokens <= budget_tokens {
                return Some(budget_tokens + 1024);
            }
        }
    }

    Some(max_tokens)
}

/// Fix missing tool responses - OpenAI requires every tool_call to have a response.
/// This inserts empty responses for tool_calls that don't have one immediately after.
fn fix_missing_tool_responses(messages: &mut Vec<Value>) {
    let mut i = 0;
    while i < messages.len() {
        let msg = messages.get(i).unwrap();
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        if role == "assistant" {
            let tool_calls = msg.get("tool_calls").and_then(|v| v.as_array());
            if let Some(tool_calls_arr) = tool_calls {
                if !tool_calls_arr.is_empty() {
                    let tool_call_ids: Vec<String> = tool_calls_arr
                        .iter()
                        .filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(String::from))
                        .collect();

                    // Collect all tool response IDs that IMMEDIATELY follow this assistant message
                    let mut responded_ids = std::collections::HashSet::new();
                    let mut insert_position = i + 1;

                    let mut j = i + 1;
                    while j < messages.len() {
                        let next_msg = messages.get(j).unwrap();
                        let next_role = next_msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                        if next_role == "tool"
                            || (next_role == "user" && next_msg.get("tool_call_id").is_some())
                        {
                            if let Some(tc_id) =
                                next_msg.get("tool_call_id").and_then(|v| v.as_str())
                            {
                                responded_ids.insert(tc_id.to_string());
                                insert_position = j + 1;
                            }
                        } else {
                            break;
                        }
                        j += 1;
                    }

                    // Find missing responses and insert them
                    let missing_ids: Vec<String> = tool_call_ids
                        .into_iter()
                        .filter(|id| !responded_ids.contains(id))
                        .collect();

                    if !missing_ids.is_empty() {
                        let missing_responses: Vec<Value> = missing_ids
                            .into_iter()
                            .map(|id| {
                                serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": "[No response received]"
                                })
                            })
                            .collect();

                        // Insert all missing responses at the correct position
                        for (idx, resp) in missing_responses.iter().enumerate() {
                            messages.insert(insert_position + idx, resp.clone());
                        }
                        i = insert_position + missing_responses.len();
                    }
                }
            }
        }
        i += 1;
    }
}

/// Convert a single Claude message to OpenAI format.
/// Returns either a single message Value or an array of message Values.
fn convert_claude_message(msg: &Value, preserve_cache_control: bool) -> Option<Value> {
    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
    let converted_role = match role {
        "user" | "tool" => "user",
        // Preserve system messages — converting them to assistant corrupted
        // forwarded bodies on /v1/messages (openai-compatible upstreams
        // rejected the unexpected assistant turn).
        "system" => "system",
        _ => "assistant",
    };
    let final_role = if role == "tool" {
        "tool"
    } else {
        converted_role
    };

    // Simple string content
    if let Some(Value::String(content_str)) = msg.get("content") {
        let mut result = serde_json::json!({
            "role": final_role,
            "content": content_str.clone()
        });
        if role == "tool" || role == "assistant" {
            if let Some(tc_id) = msg.get("tool_call_id") {
                result["tool_call_id"] = tc_id.clone();
            }
        }
        if role == "assistant" {
            if let Some(tc) = msg.get("tool_calls") {
                result["tool_calls"] = tc.clone();
            }
        }
        return Some(result);
    }

    if let Some(content_arr) = msg.get("content").and_then(|v| v.as_array()) {
        let mut parts: Vec<Value> = Vec::new();
        let mut tool_calls: Vec<Value> = Vec::new();
        let mut tool_results: Vec<Value> = Vec::new();

        for block in content_arr {
            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match block_type {
                "text" => {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        let mut part = serde_json::json!({
                            "type": "text",
                            "text": text
                        });
                        // 9router #2069: cache-aware OpenAI targets (DashScope
                        // family) keep explicit cache_control markers so the
                        // prefix-cache boundary survives translation.
                        if preserve_cache_control {
                            if let Some(cc) = block.get("cache_control") {
                                part["cache_control"] = cc.clone();
                            }
                        }
                        parts.push(part);
                    }
                }
                "image" => {
                    if let Some(source) = block.get("source") {
                        let source_type = source.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if source_type == "base64" {
                            let media_type = source
                                .get("media_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("image/png");
                            let data = source.get("data").and_then(|v| v.as_str()).unwrap_or("");
                            parts.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", media_type, data)
                                }
                            }));
                        }
                    }
                }
                "tool_use" => {
                    let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    let arguments = if input.is_null() {
                        "{}".to_string()
                    } else {
                        serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string())
                    };
                    tool_calls.push(serde_json::json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments
                        }
                    }));
                }
                "tool_result" => {
                    let tool_use_id = block
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let mut result_content = String::new();

                    if let Some(content_val) = block.get("content") {
                        if let Value::String(s) = content_val {
                            result_content = s.clone();
                        } else if let Some(arr) = content_val.as_array() {
                            result_content = arr
                                .iter()
                                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n");
                            if result_content.is_empty() {
                                result_content =
                                    serde_json::to_string(content_val).unwrap_or_default();
                            }
                        } else if !content_val.is_null() {
                            result_content = serde_json::to_string(content_val).unwrap_or_default();
                        }
                    }

                    tool_results.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": result_content
                    }));
                }
                "thinking" => {
                    if let Some(thinking) = block.get("thinking").and_then(|v| v.as_str()) {
                        parts.push(serde_json::json!({
                            "type": "text",
                            "text": thinking
                        }));
                    }
                }
                "signature" => {
                    if let Some(sig) = block.get("signature").and_then(|v| v.as_str()) {
                        parts.push(serde_json::json!({
                            "type": "text",
                            "text": sig
                        }));
                    }
                }
                "redacted_thinking" => {
                    if let Some(data) = block.get("data").and_then(|v| v.as_str()) {
                        parts.push(serde_json::json!({
                            "type": "text",
                            "text": data
                        }));
                    }
                }
                _ => {}
            }
        }

        // If has tool results, return array of tool messages
        if !tool_results.is_empty() {
            if !parts.is_empty() {
                let mut result = tool_results;
                result.push(serde_json::json!({
                    "role": "user",
                    "content": collapse_text_parts(parts, preserve_cache_control)
                }));
                return Some(serde_json::json!(result));
            }
            return Some(serde_json::json!(tool_results));
        }

        // If has tool calls, return assistant message with tool_calls
        if !tool_calls.is_empty() {
            let mut result = serde_json::json!({
                "role": "assistant"
            });
            if !parts.is_empty() {
                result["content"] = collapse_text_parts(parts, preserve_cache_control);
            }
            result["tool_calls"] = serde_json::json!(tool_calls);
            return Some(result);
        }

        // Return content
        if !parts.is_empty() {
            return Some(serde_json::json!({
                "role": converted_role,
                "content": collapse_text_parts(parts, preserve_cache_control)
            }));
        }

        // Empty content array
        if content_arr.is_empty() {
            return Some(serde_json::json!({
                "role": converted_role,
                "content": ""
            }));
        }
    }

    None
}

/// Convert Claude tool_choice to OpenAI format
fn convert_tool_choice(choice: &Value) -> Value {
    if choice.is_null() {
        return Value::String("auto".to_string());
    }

    if let Some(s) = choice.as_str() {
        return Value::String(s.to_string());
    }

    if let Some(obj) = choice.as_object() {
        let tool_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match tool_type {
            "auto" => Value::String("auto".to_string()),
            "any" => Value::String("required".to_string()),
            "tool" => {
                let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                serde_json::json!({
                    "type": "function",
                    "function": { "name": name }
                })
            }
            _ => Value::String("auto".to_string()),
        }
    } else {
        Value::String("auto".to_string())
    }
}

/// Main entry point for Claude to OpenAI request translation.
/// Converts a Claude API request body to OpenAI-compatible format.
pub fn claude_to_openai_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    credentials: Option<&Value>,
) -> bool {
    let Some(body_obj) = body.as_object_mut() else {
        return false;
    };

    // 9router #2069: `preserveCacheControl` translation option — true only
    // for cache-aware OpenAI targets (DashScope/alicode family quirk). The
    // Rust pipeline threads the provider through `credentials`, matching how
    // `filter_to_openai_format` already derives the same flag.
    let preserve_cache_control = credentials
        .and_then(|c| c.get("provider").and_then(Value::as_str))
        .is_some_and(crate::core::translator::registry::provider_preserves_cache_control);

    // Max tokens - call before we have mutable borrow
    let max_tokens = adjust_max_tokens(body_obj);

    let mut result = serde_json::json!({
        "model": model,
        "messages": [],
        "stream": stream
    });

    if let Some(mt) = max_tokens {
        result["max_tokens"] = serde_json::json!(mt);
    }

    // Temperature
    if let Some(temp) = body_obj.get("temperature") {
        result["temperature"] = temp.clone();
    }

    // System message
    //
    // Supports all three Claude API formats:
    //   array:  [{"type": "text", "text": "..."}, ...]
    //   string: "system prompt text"
    //   object: {"type": "text", "text": "..."}
    //
    // 9router #2069: when the target preserves cache_control and the system
    // array carries markers, keep the block boundaries as OpenAI content
    // parts instead of flattening to a string. A billing-only block that is
    // stripped transfers its marker to the preceding retained block so the
    // prefix-cache boundary survives.
    if let Some(system) = body_obj.get("system") {
        let mut system_content: Value = Value::Null;
        let mut have_system = false;

        if preserve_cache_control {
            if let Some(arr) = system.as_array() {
                if arr.iter().any(|s| s.get("cache_control").is_some()) {
                    let mut parts: Vec<Value> = Vec::new();
                    for block in arr {
                        let text = strip_anthropic_billing_header(
                            block.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                        );
                        if !text.is_empty() {
                            let mut part = serde_json::json!({
                                "type": "text",
                                "text": text
                            });
                            if let Some(cc) = block.get("cache_control") {
                                part["cache_control"] = cc.clone();
                            }
                            parts.push(part);
                        } else if block.get("cache_control").is_some() && !parts.is_empty() {
                            // Removing a billing-only block must not remove
                            // its prefix cache boundary.
                            if let Some(last) = parts.last_mut() {
                                last["cache_control"] = block["cache_control"].clone();
                            }
                        }
                    }
                    if parts.iter().any(|p| p.get("cache_control").is_some()) {
                        system_content = Value::Array(parts);
                        have_system = true;
                    } else {
                        let joined = parts
                            .iter()
                            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !joined.is_empty() {
                            system_content = Value::String(joined);
                            have_system = true;
                        }
                    }
                }
            }
        }

        if !have_system {
            system_content = if let Some(arr) = system.as_array() {
                Value::String(
                    arr.iter()
                        .filter_map(|s| {
                            s.get("text")
                                .and_then(|t| t.as_str())
                                .map(strip_anthropic_billing_header)
                        })
                        .filter(|t| !t.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            } else if let Some(obj) = system.as_object() {
                Value::String(
                    obj.get("text")
                        .and_then(|t| t.as_str())
                        .map(strip_anthropic_billing_header)
                        .unwrap_or_default(),
                )
            } else if let Some(s) = system.as_str() {
                Value::String(strip_anthropic_billing_header(s))
            } else {
                Value::String(String::new())
            };
            have_system = system_content
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false);
        }

        if have_system {
            result["messages"] = serde_json::json!([
                {
                    "role": "system",
                    "content": system_content
                }
            ]);
        }
    }

    // Convert messages
    if let Some(messages) = body_obj.get("messages").and_then(|v| v.as_array()) {
        let mut converted_messages: Vec<Value> = Vec::new();

        for msg in messages {
            if let Some(converted) = convert_claude_message(msg, preserve_cache_control) {
                if let Some(arr) = converted.as_array() {
                    converted_messages.extend(arr.iter().cloned());
                } else {
                    converted_messages.push(converted);
                }
            }
        }

        // Fix missing tool responses
        fix_missing_tool_responses(&mut converted_messages);

        // Merge converted messages into result
        if let Some(msg_arr) = result["messages"].as_array_mut() {
            msg_arr.extend(converted_messages);
        }
    }

    // Tools
    if let Some(tools) = body_obj.get("tools").and_then(|v| v.as_array()) {
        let converted_tools: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let description = tool
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let input_schema = tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                Some(serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": input_schema
                    }
                }))
            })
            .collect();
        result["tools"] = serde_json::json!(converted_tools);
    }

    // Tool choice
    if let Some(tool_choice) = body_obj.get("tool_choice") {
        result["tool_choice"] = convert_tool_choice(tool_choice);
    }

    // Thinking budget configuration (e.g. {"type": "enabled", "budget_tokens": 10000})
    if let Some(thinking) = body_obj.get("thinking") {
        result["thinking"] = thinking.clone();
    }

    // reasoning_effort / reasoning passthrough (9router claude-to-openai.js:83-91):
    //   reasoning_effort = body.reasoning_effort ?? body.reasoning?.effort
    //   reasoning      = body.reasoning
    if let Some(e) = body_obj.get("reasoning_effort") {
        result["reasoning_effort"] = e.clone();
    } else if let Some(e) = body_obj.get("reasoning").and_then(|r| r.get("effort")) {
        result["reasoning_effort"] = e.clone();
    }
    if let Some(r) = body_obj.get("reasoning") {
        result["reasoning"] = r.clone();
    }

    *body = result;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_message_translation() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let model = body.get("model").unwrap().as_str().unwrap();
        assert_eq!(model, "gpt-4");
    }

    #[test]
    fn test_system_array_to_string() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "system": [
                {"type": "text", "text": "System prompt 1"},
                {"type": "text", "text": "System prompt 2"}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        let system_msg = messages
            .iter()
            .find(|m| m.get("role").unwrap().as_str().unwrap() == "system");
        assert!(system_msg.is_some());
        let content = system_msg
            .unwrap()
            .get("content")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(content.contains("System prompt 1"));
        assert!(content.contains("System prompt 2"));
    }

    #[test]
    fn test_system_object_to_string() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "system": {"type": "text", "text": "Single object system prompt"},
            "messages": [{"role": "user", "content": "Hello"}]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        let system_msg = messages
            .iter()
            .find(|m| m.get("role").unwrap().as_str().unwrap() == "system");
        assert!(system_msg.is_some());
        let content = system_msg
            .unwrap()
            .get("content")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(content, "Single object system prompt");
    }

    #[test]
    fn test_system_string_format_to_string() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "system": "Plain string system prompt",
            "messages": [{"role": "user", "content": "Hello"}]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        let system_msg = messages
            .iter()
            .find(|m| m.get("role").unwrap().as_str().unwrap() == "system");
        assert!(system_msg.is_some());
        let content = system_msg
            .unwrap()
            .get("content")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(content, "Plain string system prompt");
    }

    #[test]
    fn test_claude_content_blocks_to_openai() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Hello world"}]}
            ]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages[0].get("role").unwrap().as_str().unwrap(), "user");
        assert_eq!(
            messages[0].get("content").unwrap().as_str().unwrap(),
            "Hello world"
        );
    }

    #[test]
    fn test_image_block_with_base64() {
        let mut body: Value = serde_json::from_str(r#"{
            "model": "claude-3",
            "messages": [
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc123"}}
                ]}
            ]
        }"#).unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        let content = messages[0].get("content").unwrap().as_array().unwrap();
        assert_eq!(
            content[0].get("type").unwrap().as_str().unwrap(),
            "image_url"
        );
        let url = content[0]
            .get("image_url")
            .unwrap()
            .get("url")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(url.starts_with("data:image/png;base64,abc123"));
    }

    #[test]
    fn test_tool_use_conversion() {
        let mut body: Value = serde_json::from_str(r#"{
            "model": "claude-3",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tool_123", "name": "test_tool", "input": {"arg": "value"}}
                ]}
            ]
        }"#).unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        let msg = &messages[0];
        assert_eq!(msg.get("role").unwrap().as_str().unwrap(), "assistant");
        let tool_calls = msg.get("tool_calls").unwrap().as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(
            tool_calls[0].get("id").unwrap().as_str().unwrap(),
            "tool_123"
        );
        assert_eq!(
            tool_calls[0]
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str()
                .unwrap(),
            "test_tool"
        );
    }

    #[test]
    fn test_tool_result_conversion() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [
                {"role": "tool", "tool_call_id": "tool_123", "content": "tool result content"}
            ]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages[0].get("role").unwrap().as_str().unwrap(), "tool");
        assert_eq!(
            messages[0].get("tool_call_id").unwrap().as_str().unwrap(),
            "tool_123"
        );
        assert_eq!(
            messages[0].get("content").unwrap().as_str().unwrap(),
            "tool result content"
        );
    }

    #[test]
    fn test_tool_choice_conversion() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "tool_choice": {"type": "tool", "name": "my_tool"}
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let tool_choice = body.get("tool_choice").unwrap();
        assert_eq!(
            tool_choice.get("type").unwrap().as_str().unwrap(),
            "function"
        );
        assert_eq!(
            tool_choice
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str()
                .unwrap(),
            "my_tool"
        );
    }

    #[test]
    fn test_tool_choice_auto() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "tool_choice": {"type": "auto"}
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let tool_choice = body.get("tool_choice").unwrap();
        assert_eq!(tool_choice.as_str().unwrap(), "auto");
    }

    #[test]
    fn test_tool_choice_any_to_required() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "tool_choice": {"type": "any"}
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let tool_choice = body.get("tool_choice").unwrap();
        assert_eq!(tool_choice.as_str().unwrap(), "required");
    }

    #[test]
    fn test_max_tokens_adjustment() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        // Without tools, should preserve original max_tokens
        let max_tokens = body.get("max_tokens").unwrap().as_u64().unwrap();
        assert_eq!(max_tokens, 1000);
    }

    #[test]
    fn test_max_tokens_adjustment_with_tools() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000,
            "tools": [{"name": "test_tool", "description": "A test tool"}]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        // With tools and max_tokens < DEFAULT_MIN_TOKENS, should use DEFAULT_MIN_TOKENS
        let max_tokens = body.get("max_tokens").unwrap().as_u64().unwrap();
        assert_eq!(max_tokens, DEFAULT_MIN_TOKENS);
    }

    #[test]
    fn test_tools_conversion() {
        let mut body: Value = serde_json::from_str(r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "tools": [
                {"name": "test_tool", "description": "A test tool", "input_schema": {"type": "object", "properties": {"arg": {"type": "string"}}}}
            ]
        }"#).unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let tools = body.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].get("type").unwrap().as_str().unwrap(), "function");
        assert_eq!(
            tools[0]
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str()
                .unwrap(),
            "test_tool"
        );
        assert_eq!(
            tools[0]
                .get("function")
                .unwrap()
                .get("description")
                .unwrap()
                .as_str()
                .unwrap(),
            "A test tool"
        );
    }

    #[test]
    fn test_fix_missing_tool_responses() {
        let mut body: Value = serde_json::from_str(r#"{
            "model": "claude-3",
            "messages": [
                {"role": "assistant", "content": "Using tool", "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "tool1", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "result"}
            ]
        }"#).unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        // Both tool_call has response, no missing response should be added
        let messages = body.get("messages").unwrap().as_array().unwrap();
        let tool_count = messages
            .iter()
            .filter(|m| m.get("role").unwrap().as_str().unwrap() == "tool")
            .count();
        assert_eq!(tool_count, 1); // Only the original, no missing inserted
    }

    #[test]
    fn test_fix_missing_tool_responses_with_gap() {
        let mut body: Value = serde_json::from_str(r#"{
            "model": "claude-3",
            "messages": [
                {"role": "assistant", "content": "Using tools", "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "tool1", "arguments": "{}"}},
                    {"id": "call_2", "type": "function", "function": {"name": "tool2", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "result1"}
            ]
        }"#).unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        // call_2 is missing response - should be inserted
        let messages = body.get("messages").unwrap().as_array().unwrap();
        let tool_messages: Vec<&Value> = messages
            .iter()
            .filter(|m| m.get("role").unwrap().as_str().unwrap() == "tool")
            .collect();
        assert_eq!(tool_messages.len(), 2); // Original + inserted missing

        // Check the inserted missing response
        let missing_resp = tool_messages
            .iter()
            .find(|m| m.get("content").unwrap().as_str().unwrap() == "[No response received]")
            .unwrap();
        assert_eq!(
            missing_resp.get("tool_call_id").unwrap().as_str().unwrap(),
            "call_2"
        );
    }

    #[test]
    fn test_stream_parameter() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, true, None);

        assert!(body.get("stream").unwrap().as_bool().unwrap());
    }

    #[test]
    fn test_temperature_preserved() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "temperature": 0.7
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        assert_eq!(body.get("temperature").unwrap().as_f64().unwrap(), 0.7);
    }

    #[test]
    fn test_thinking_budget_tokens_adjustment() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000,
            "thinking": {"budget_tokens": 10000}
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        // max_tokens should be adjusted to be > budget_tokens
        let max_tokens = body.get("max_tokens").unwrap().as_u64().unwrap();
        assert!(max_tokens > 10000);
    }

    #[test]
    fn test_empty_content_array() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [
                {"role": "assistant", "content": []}
            ]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages[0].get("content").unwrap().as_str().unwrap(), "");
    }

    #[test]
    fn test_multiple_messages_with_mixed_content() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"},
                {"role": "user", "content": "How are you?"}
            ]
        }"#,
        )
        .unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].get("role").unwrap().as_str().unwrap(), "user");
        assert_eq!(
            messages[1].get("role").unwrap().as_str().unwrap(),
            "assistant"
        );
        assert_eq!(messages[2].get("role").unwrap().as_str().unwrap(), "user");
    }

    #[test]
    fn test_tool_result_with_text_parts() {
        let mut body: Value = serde_json::from_str(r#"{
            "model": "claude-3",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tool_1", "name": "get_weather", "input": {"city": "NYC"}}
                ]},
                {"role": "tool", "tool_call_id": "tool_1", "content": [
                    {"type": "text", "text": "The weather is sunny"}
                ]}
            ]
        }"#).unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        // Should have the tool result converted to user message with text content
        let has_content = messages.iter().any(|m| {
            m.get("content").is_some() && m.get("role").unwrap().as_str().unwrap() == "user"
        });
        assert!(has_content);
    }

    #[test]
    fn test_complex_message_with_text_and_tool_use() {
        let mut body: Value = serde_json::from_str(r#"{
            "model": "claude-3",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "text", "text": "I'll check the weather."},
                    {"type": "tool_use", "id": "tool_1", "name": "get_weather", "input": {"city": "NYC"}}
                ]}
            ]
        }"#).unwrap();

        claude_to_openai_request("gpt-4", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        let assistant_msg = &messages[0];
        assert_eq!(
            assistant_msg.get("role").unwrap().as_str().unwrap(),
            "assistant"
        );
        // Should have both content and tool_calls
        assert!(assistant_msg.get("content").is_some());
        assert!(assistant_msg.get("tool_calls").is_some());
        let tool_calls = assistant_msg.get("tool_calls").unwrap().as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(
            tool_calls[0]
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str()
                .unwrap(),
            "get_weather"
        );
    }

    #[test]
    fn test_reasoning_effort_passthrough() {
        // reasoning_effort at top level is forwarded directly.
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "reasoning_effort": "high"
        }"#,
        )
        .unwrap();
        claude_to_openai_request("gpt-4", &mut body, false, None);
        assert_eq!(
            body.get("reasoning_effort").unwrap().as_str().unwrap(),
            "high"
        );
    }

    #[test]
    fn test_reasoning_effort_from_reasoning_object() {
        // reasoning_effort falls back to reasoning.effort (JS claude-to-openai.js:83-91).
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "reasoning": {"effort": "medium", "summary": "auto"}
        }"#,
        )
        .unwrap();
        claude_to_openai_request("gpt-4", &mut body, false, None);
        assert_eq!(
            body.get("reasoning_effort").unwrap().as_str().unwrap(),
            "medium"
        );
        // reasoning object is also forwarded verbatim.
        assert!(body.get("reasoning").is_some());
        assert_eq!(body["reasoning"]["effort"], "medium");
    }

    // --- 9router v0.5.116 a61b5a6f (#2069): Claude cache markers for
    // DashScope targets -----------------------------------------------

    fn creds(provider: &str) -> Value {
        serde_json::json!({"provider": provider})
    }

    #[test]
    fn dashscope_preserves_system_and_user_cache_markers() {
        for provider in ["alicode", "alicode-intl", "alims-intl", "alitp-intl"] {
            let mut body: Value = serde_json::from_str(
                r#"{
                "system": [{"type":"text","text":"x-anthropic-billing-header: transient\nStable instructions","cache_control":{"type":"ephemeral"}}],
                "messages": [{"role":"user","content":[{"type":"text","text":"Stable question","cache_control":{"type":"ephemeral"}}]}],
                "max_tokens": 1024
            }"#,
            )
            .unwrap();
            claude_to_openai_request("qwen3-coder-plus", &mut body, false, Some(&creds(provider)));
            let messages = body["messages"].as_array().unwrap();
            assert_eq!(messages[0]["role"], "system", "{provider}");
            // Billing header stripped; marker retained on the text part.
            assert_eq!(
                messages[0]["content"],
                serde_json::json!([{"type":"text","text":"Stable instructions","cache_control":{"type":"ephemeral"}}]),
                "{provider}"
            );
            assert_eq!(
                messages[1]["content"],
                serde_json::json!([{"type":"text","text":"Stable question","cache_control":{"type":"ephemeral"}}]),
                "{provider}"
            );
        }
    }

    #[test]
    fn non_dashscope_flattens_and_drops_markers() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "system": [{"type":"text","text":"one"},{"type":"text","text":"two","cache_control":{"type":"ephemeral"}}],
            "messages": [{"role":"user","content":[{"type":"text","text":"hi","cache_control":{"type":"ephemeral"}}]}]
        }"#,
        )
        .unwrap();
        claude_to_openai_request("gpt-4", &mut body, false, Some(&creds("groq")));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "one\ntwo");
        assert_eq!(messages[1]["content"], "hi");
    }

    #[test]
    fn dashscope_marked_text_survives_next_to_tool_calls() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "messages": [
                {"role":"assistant","content":[
                    {"type":"text","text":"Calling tool","cache_control":{"type":"ephemeral"}},
                    {"type":"tool_use","id":"c1","name":"read","input":{}}
                ]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"done"}]}
            ]
        }"#,
        )
        .unwrap();
        claude_to_openai_request(
            "qwen3-coder-plus",
            &mut body,
            false,
            Some(&creds("alicode")),
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(
            messages[0]["content"],
            serde_json::json!([{"type":"text","text":"Calling tool","cache_control":{"type":"ephemeral"}}])
        );
        assert_eq!(messages[0]["tool_calls"][0]["id"], "c1");
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "c1");
    }

    #[test]
    fn dashscope_retains_system_block_boundaries_around_marker() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "system": [
                {"type":"text","text":"prefix"},
                {"type":"text","text":"cached","cache_control":{"type":"ephemeral"}},
                {"type":"text","text":"uncached tail"}
            ],
            "messages": [{"role":"user","content":"hi"}]
        }"#,
        )
        .unwrap();
        claude_to_openai_request(
            "qwen3-coder-plus",
            &mut body,
            false,
            Some(&creds("alicode")),
        );
        assert_eq!(
            body["messages"][0]["content"],
            serde_json::json!([
                {"type":"text","text":"prefix"},
                {"type":"text","text":"cached","cache_control":{"type":"ephemeral"}},
                {"type":"text","text":"uncached tail"}
            ])
        );
    }

    #[test]
    fn dashscope_marker_survives_billing_only_block_removal() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "system": [
                {"type":"text","text":"Stable prefix"},
                {"type":"text","text":"x-anthropic-billing-header: transient","cache_control":{"type":"ephemeral"}},
                {"type":"text","text":"Uncached tail"}
            ],
            "messages": [{"role":"user","content":"hi"}]
        }"#,
        )
        .unwrap();
        claude_to_openai_request(
            "qwen3-coder-plus",
            &mut body,
            false,
            Some(&creds("alicode")),
        );
        assert_eq!(
            body["messages"][0]["content"],
            serde_json::json!([
                {"type":"text","text":"Stable prefix","cache_control":{"type":"ephemeral"}},
                {"type":"text","text":"Uncached tail"}
            ])
        );
    }

    #[test]
    fn dashscope_unmarked_single_text_still_flattens() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "system": "instructions",
            "messages": [{"role":"user","content":[{"type":"text","text":"hi"}]}]
        }"#,
        )
        .unwrap();
        claude_to_openai_request(
            "qwen3-coder-plus",
            &mut body,
            false,
            Some(&creds("alicode")),
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "instructions");
        assert_eq!(messages[1]["content"], "hi");
    }
}
