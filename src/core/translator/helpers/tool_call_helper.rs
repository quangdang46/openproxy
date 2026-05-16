//! Port of `open-sse/translator/helpers/toolCallHelper.js`.
//!
//! Anthropic's Messages API requires `tool_use.id` and `tool_result.tool_use_id`
//! to match `^[a-zA-Z0-9_-]+$`. OpenAI clients sometimes emit ids that
//! contain `:` or other punctuation; this module sanitises every id in
//! the request body and synthesises deterministic placeholders when an
//! id can't be salvaged.
//!
//! Also enforces:
//!   - `tool_calls[i].function.arguments` is a string (never an object).
//!   - `tool_calls[i].type = "function"` is always set.
//!   - When an assistant message has tool calls but the next message is
//!     not a `role:tool` reply, an empty placeholder reply is inserted
//!     so the conversation history is well-formed for upstream.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Map, Value};

static TOOL_ID_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap());

/// Build a deterministic tool-call id from message + call indices and an
/// optional tool name. Cache-friendly because the same tool_call slot in
/// the same conversation always yields the same id.
pub fn generate_tool_call_id(msg_index: usize, tc_index: usize, tool_name: Option<&str>) -> String {
    let name_part = match tool_name {
        Some(n) if !n.is_empty() => {
            // Drop characters outside the allowed set.
            let cleaned: String = n
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if cleaned.is_empty() {
                String::new()
            } else {
                format!("_{cleaned}")
            }
        }
        _ => String::new(),
    };
    format!("call_msg{msg_index}_tc{tc_index}{name_part}")
}

/// Strip disallowed characters from `id`. Returns `None` if nothing
/// survives.
fn sanitize_tool_id(id: &str) -> Option<String> {
    let cleaned: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Validate / repair every tool_call id in `body.messages`. Mutates in
/// place. Mirrors `ensureToolCallIds` in 9router.
pub fn ensure_tool_call_ids(body: &mut Value) {
    let Some(messages) = body
        .get_mut("messages")
        .and_then(|v| v.as_array_mut())
    else {
        return;
    };

    for (i, msg) in messages.iter_mut().enumerate() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };

        let role = obj.get("role").and_then(|v| v.as_str()).map(str::to_string);

        // OpenAI assistant messages carrying tool_calls.
        if role.as_deref() == Some("assistant") {
            if let Some(tcs) = obj.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                for (j, tc) in tcs.iter_mut().enumerate() {
                    let Some(tc_obj) = tc.as_object_mut() else {
                        continue;
                    };
                    let original_id = tc_obj
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let valid = original_id
                        .as_deref()
                        .map(|s| TOOL_ID_PATTERN.is_match(s))
                        .unwrap_or(false);
                    if !valid {
                        let function_name = tc_obj
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        let new_id = original_id
                            .as_deref()
                            .and_then(sanitize_tool_id)
                            .unwrap_or_else(|| {
                                generate_tool_call_id(i, j, function_name.as_deref())
                            });
                        tc_obj.insert("id".into(), Value::String(new_id));
                    }

                    if !tc_obj.contains_key("type") {
                        tc_obj.insert("type".into(), Value::String("function".to_string()));
                    }

                    // Stringify arguments if present as object.
                    if let Some(func) = tc_obj.get_mut("function").and_then(|v| v.as_object_mut())
                    {
                        if let Some(args) = func.get_mut("arguments") {
                            if !args.is_string() {
                                let serialised = serde_json::to_string(args).unwrap_or_default();
                                *args = Value::String(serialised);
                            }
                        }
                    }
                }
            }
        }

        // OpenAI tool reply: validate tool_call_id.
        if role.as_deref() == Some("tool") {
            let id_owned = obj
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if let Some(id) = id_owned {
                if !TOOL_ID_PATTERN.is_match(&id) {
                    let new_id = sanitize_tool_id(&id)
                        .unwrap_or_else(|| generate_tool_call_id(i, 0, None));
                    obj.insert("tool_call_id".into(), Value::String(new_id));
                }
            }
        }

        // Claude content blocks: tool_use / tool_result.
        if let Some(content) = obj.get_mut("content").and_then(|v| v.as_array_mut()) {
            for (k, block) in content.iter_mut().enumerate() {
                let Some(block_obj) = block.as_object_mut() else {
                    continue;
                };
                let block_type = block_obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                match block_type.as_deref() {
                    Some("tool_use") => {
                        let id_owned = block_obj
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        if let Some(id) = id_owned {
                            if !TOOL_ID_PATTERN.is_match(&id) {
                                let name = block_obj
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                                let new_id = sanitize_tool_id(&id)
                                    .unwrap_or_else(|| generate_tool_call_id(i, k, name.as_deref()));
                                block_obj.insert("id".into(), Value::String(new_id));
                            }
                        }
                    }
                    Some("tool_result") => {
                        let id_owned = block_obj
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        if let Some(id) = id_owned {
                            if !TOOL_ID_PATTERN.is_match(&id) {
                                let new_id = sanitize_tool_id(&id)
                                    .unwrap_or_else(|| generate_tool_call_id(i, k, None));
                                block_obj.insert("tool_use_id".into(), Value::String(new_id));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Collect every tool-call id from an assistant message. Looks at both
/// OpenAI `tool_calls` and Claude `tool_use` blocks.
pub fn get_tool_call_ids(msg: &Value) -> Vec<String> {
    if msg.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return Vec::new();
    }
    let mut ids = Vec::new();

    if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tcs {
            if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                ids.push(id.to_string());
            }
        }
    }

    if let Some(content) = msg.get("content").and_then(|v| v.as_array()) {
        for block in content {
            if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                    ids.push(id.to_string());
                }
            }
        }
    }

    ids
}

/// Returns true if `msg` is a tool reply for at least one of `tool_call_ids`.
pub fn has_tool_results(msg: &Value, tool_call_ids: &[String]) -> bool {
    if tool_call_ids.is_empty() {
        return false;
    }
    let role = msg.get("role").and_then(|v| v.as_str());

    // OpenAI: role=tool with tool_call_id.
    if role == Some("tool") {
        if let Some(id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
            return tool_call_ids.iter().any(|x| x == id);
        }
    }

    // Claude: user message with tool_result blocks.
    if role == Some("user") {
        if let Some(content) = msg.get("content").and_then(|v| v.as_array()) {
            for block in content {
                if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                    if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                        if tool_call_ids.iter().any(|x| x == id) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Insert empty `role:tool` replies for any assistant tool_calls that
/// the next message did not answer. Mutates `body.messages` in place.
pub fn fix_missing_tool_responses(body: &mut Value) {
    let Some(messages) = body
        .get("messages")
        .and_then(|v| v.as_array())
    else {
        return;
    };
    let original = messages.clone();
    let mut new_messages: Vec<Value> = Vec::with_capacity(original.len() + 4);

    for (i, msg) in original.iter().enumerate() {
        new_messages.push(msg.clone());
        let tool_call_ids = get_tool_call_ids(msg);
        if tool_call_ids.is_empty() {
            continue;
        }
        let next = original.get(i + 1);
        let satisfied = next
            .map(|n| has_tool_results(n, &tool_call_ids))
            .unwrap_or(false);
        if !satisfied {
            for id in &tool_call_ids {
                new_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": ""
                }));
            }
        }
    }

    if let Some(obj) = body.as_object_mut() {
        obj.insert("messages".into(), Value::Array(new_messages));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_id_strips_invalid_chars_in_name() {
        let id = generate_tool_call_id(2, 5, Some("my:bad/name"));
        assert_eq!(id, "call_msg2_tc5_mybadname");
    }

    #[test]
    fn ensure_repairs_invalid_id() {
        let mut body = json!({"messages": [
            {"role": "assistant", "tool_calls": [{
                "id": "bad:id:value",
                "function": {"name": "WebSearch", "arguments": "{}"}
            }]}
        ]});
        ensure_tool_call_ids(&mut body);
        let id = body["messages"][0]["tool_calls"][0]["id"].as_str().unwrap();
        assert!(TOOL_ID_PATTERN.is_match(id));
        assert_eq!(id, "badidvalue");
    }

    #[test]
    fn ensure_synthesises_when_id_completely_invalid() {
        let mut body = json!({"messages": [
            {"role": "assistant", "tool_calls": [{
                "id": ":::",
                "function": {"name": "WebSearch", "arguments": "{}"}
            }]}
        ]});
        ensure_tool_call_ids(&mut body);
        let id = body["messages"][0]["tool_calls"][0]["id"].as_str().unwrap();
        assert_eq!(id, "call_msg0_tc0_WebSearch");
    }

    #[test]
    fn ensure_stringifies_object_arguments() {
        let mut body = json!({"messages": [
            {"role": "assistant", "tool_calls": [{
                "id": "call_1",
                "function": {"name": "x", "arguments": {"k": "v"}}
            }]}
        ]});
        ensure_tool_call_ids(&mut body);
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["arguments"],
            "{\"k\":\"v\"}"
        );
    }

    #[test]
    fn ensure_repairs_claude_tool_use_block_id() {
        let mut body = json!({"messages": [
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu:1", "name": "WebSearch", "input": {}}
            ]}
        ]});
        ensure_tool_call_ids(&mut body);
        let id = body["messages"][0]["content"][0]["id"].as_str().unwrap();
        assert_eq!(id, "tu1");
    }

    #[test]
    fn fix_missing_inserts_placeholder_tool_replies() {
        let mut body = json!({"messages": [
            {"role": "assistant", "tool_calls": [
                {"id": "call_1", "function": {"name": "x", "arguments": "{}"}}
            ]},
            {"role": "user", "content": "thanks"}
        ]});
        fix_missing_tool_responses(&mut body);
        let arr = body["messages"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[1]["role"], "tool");
        assert_eq!(arr[1]["tool_call_id"], "call_1");
        assert_eq!(arr[2]["role"], "user");
    }

    #[test]
    fn fix_missing_no_op_when_response_present() {
        let mut body = json!({"messages": [
            {"role": "assistant", "tool_calls": [
                {"id": "call_1", "function": {"name": "x", "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": "call_1", "content": "done"}
        ]});
        let before = body.clone();
        fix_missing_tool_responses(&mut body);
        assert_eq!(body, before);
    }

    #[test]
    fn has_tool_results_handles_claude_format() {
        let assistant = json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "tu1", "name": "x", "input": {}}
        ]});
        let user_with_result = json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "tu1", "content": "ok"}
        ]});
        let ids = get_tool_call_ids(&assistant);
        assert!(has_tool_results(&user_with_result, &ids));
    }
}
