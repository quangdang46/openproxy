use serde_json::{Map, Value};

const SEP: &str = "\n\n";

/// Inject a system prompt into the request body, dispatching by format.
/// 9router systemInject.js `injectSystemPrompt(body, format, prompt)`.
///
/// - `"kiro"` → the first `conversationState.history[].userInputMessage`, or
///   `currentMessage.userInputMessage`.
/// - `"claude"` → `body.system` (string or array, inserted before the last
///   cache_control block).
/// - `"gemini"` / `"gemini-cli"` / `"vertex"` / `"antigravity"` →
///   `body.systemInstruction` / `body.request.systemInstruction` (`{parts:[{text}]}`).
/// - Everything else (OpenAI chat / Responses / codex / cursor / ollama) →
///   `messages[]` / `input[]` / `instructions`.
///
/// Returns `true` if the body was modified.
pub fn inject_system_prompt(body: &mut Value, format: &str, prompt: &str) -> bool {
    if prompt.trim().is_empty() {
        return false;
    }
    match format {
        "kiro" => inject_kiro_system(body, prompt),
        "claude" => inject_claude_system(body, prompt),
        "gemini" | "gemini-cli" | "vertex" | "antigravity" => inject_gemini_system(body, prompt),
        _ => inject_messages_system(body, prompt),
    }
}

/// OpenAI-shaped: `messages[]` (chat) or `input[]` (responses) or
/// `instructions` (responses top-level string). 9router injectSystemPrompt.
fn inject_messages_system(body: &mut Value, prompt: &str) -> bool {
    // OpenAI Responses API: top-level string field.
    if let Some(Value::String(instructions)) = body.get_mut("instructions") {
        if !instructions.is_empty() {
            instructions.push_str(SEP);
        }
        instructions.push_str(prompt);
        return true;
    }

    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        return inject_chat_system(messages, prompt);
    }

    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        return inject_responses_input(input, prompt);
    }

    false
}

/// Chat `messages[]` takes a bare `{role, content}` system entry.
/// 9router injectChatSystem.
fn inject_chat_system(messages: &mut Vec<Value>, prompt: &str) -> bool {
    let idx = messages.iter().position(|m| {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        role == "system" || role == "developer"
    });
    match idx {
        Some(i) => append_to_openai_message(&mut messages[i], prompt),
        None => {
            messages.insert(
                0,
                serde_json::json!({ "role": "system", "content": prompt }),
            );
            true
        }
    }
}

/// Responses `input[]` items are typed: only `type == "message"` entries with a
/// system/developer role qualify, and the injected content is an `input_text`
/// part. 9router injectResponsesInputSystem.
fn inject_responses_input(input: &mut Vec<Value>, prompt: &str) -> bool {
    let idx = input.iter().position(|item| {
        item.get("type").and_then(Value::as_str) == Some("message")
            && matches!(
                item.get("role").and_then(Value::as_str),
                Some("system" | "developer")
            )
    });
    match idx {
        Some(i) => append_to_responses_message(&mut input[i], prompt),
        None => {
            input.insert(
                0,
                serde_json::json!({
                    "type": "message",
                    "role": "system",
                    "content": [{ "type": "input_text", "text": prompt }]
                }),
            );
            true
        }
    }
}

/// Append a prompt to a Responses message item, which unlike a chat message
/// takes typed `input_text` content. 9router appendToResponsesMessage.
fn append_to_responses_message(message: &mut Value, prompt: &str) -> bool {
    match message.get_mut("content") {
        Some(Value::String(content)) => {
            if !content.is_empty() {
                content.push_str(SEP);
            }
            content.push_str(prompt);
            true
        }
        Some(Value::Array(parts)) => {
            parts.push(serde_json::json!({ "type": "input_text", "text": prompt }));
            true
        }
        _ => {
            message["content"] = serde_json::json!([{ "type": "input_text", "text": prompt }]);
            true
        }
    }
}

/// Kiro wire shape: append to the first user turn's `content` string, the same
/// place the Kiro translator mirrors system text via its `contentPrefix`.
/// 9router injectKiroSystem.
fn inject_kiro_system(body: &mut Value, prompt: &str) -> bool {
    let Some(state) = body
        .get_mut("conversationState")
        .and_then(Value::as_object_mut)
    else {
        return false;
    };

    if let Some(history) = state.get_mut("history").and_then(Value::as_array_mut) {
        for item in history.iter_mut() {
            if let Some(message) = item
                .get_mut("userInputMessage")
                .and_then(Value::as_object_mut)
            {
                return append_kiro_prompt(message, prompt);
            }
        }
    }

    state
        .get_mut("currentMessage")
        .and_then(|current| current.get_mut("userInputMessage"))
        .and_then(Value::as_object_mut)
        .is_some_and(|message| append_kiro_prompt(message, prompt))
}

fn append_kiro_prompt(message: &mut Map<String, Value>, prompt: &str) -> bool {
    let current = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let next = if current.is_empty() {
        prompt.to_string()
    } else {
        format!("{current}{SEP}{prompt}")
    };
    message.insert("content".into(), Value::String(next));
    true
}

/// Append a prompt to an OpenAI message (string content, array of parts, or
/// replace). 9router appendToOpenAIMessage.
fn append_to_openai_message(msg: &mut Value, prompt: &str) -> bool {
    match msg.get_mut("content") {
        Some(Value::String(content)) => {
            if !content.is_empty() {
                content.push_str(SEP);
            }
            content.push_str(prompt);
            true
        }
        Some(Value::Array(parts)) => {
            parts.push(serde_json::json!({ "type": "input_text", "text": prompt }));
            true
        }
        _ => {
            msg["content"] = Value::String(prompt.to_string());
            true
        }
    }
}

/// Claude shape: `body.system` as string or array of `{type:"text",text}`.
/// Insert before the last cache_control block to keep injection inside the
/// cached prefix. 9router injectClaudeSystem.
fn inject_claude_system(body: &mut Value, prompt: &str) -> bool {
    match body.get_mut("system") {
        Some(Value::String(content)) => {
            if !content.is_empty() {
                content.push_str(SEP);
            }
            content.push_str(prompt);
            true
        }
        Some(Value::Array(blocks)) => {
            let block = serde_json::json!({ "type": "text", "text": prompt });
            let last_cache = blocks
                .iter()
                .rposition(|b| b.get("cache_control").is_some());
            match last_cache {
                Some(i) => blocks.insert(i, block),
                None => blocks.push(block),
            }
            true
        }
        _ => {
            body["system"] = Value::String(prompt.to_string());
            true
        }
    }
}

/// Gemini shape: `body.system_instruction` / `body.systemInstruction` /
/// `body.request.systemInstruction` as `{ parts: [{ text }] }`.
/// 9router injectGeminiSystem.
fn inject_gemini_system(body: &mut Value, prompt: &str) -> bool {
    let target = if body.get("request").map(Value::is_object).unwrap_or(false) {
        body.get_mut("request").unwrap()
    } else {
        body
    };
    let use_snake = target.get("system_instruction").is_some();
    let key = if use_snake {
        "system_instruction"
    } else {
        "systemInstruction"
    };
    let sys = target.get_mut(key);
    if let Some(sys) = sys {
        if let Some(parts) = sys.get_mut("parts").and_then(Value::as_array_mut) {
            parts.push(serde_json::json!({ "text": prompt }));
            return true;
        }
    }
    target[key] = serde_json::json!({ "parts": [{ "text": prompt }] });
    true
}

/// Check if system injection is enabled in a raw JSON config value.
///
/// Looks for `systemInject` boolean key in the provided settings Value.
pub fn system_inject_enabled(settings: &Value) -> bool {
    settings
        .get("systemInject")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inject_into_empty_messages() {
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "Hello" }
            ]
        });
        assert!(inject_system_prompt(
            &mut body,
            "openai",
            "You are a helpful assistant."
        ));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are a helpful assistant.");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn append_to_existing_system_message() {
        let mut body = json!({
            "messages": [
                { "role": "system", "content": "Existing rules" },
                { "role": "user", "content": "Hi" }
            ]
        });
        assert!(inject_system_prompt(
            &mut body,
            "openai",
            "Additional instruction."
        ));
        let content = body["messages"][0]["content"].as_str().unwrap();
        assert!(content.starts_with("Existing rules"));
        assert!(content.contains("Additional instruction."));
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn no_modification_for_empty_prompt() {
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "Hello" }
            ]
        });
        assert!(!inject_system_prompt(&mut body, "openai", ""));
        assert!(!inject_system_prompt(&mut body, "openai", "   "));
    }

    #[test]
    fn no_modification_when_no_messages_array() {
        let mut body = json!({ "model": "gpt-4" });
        assert!(!inject_system_prompt(&mut body, "openai", "test"));
    }

    #[test]
    fn system_inject_enabled_checks_config() {
        let config = json!({ "systemInject": true });
        assert!(system_inject_enabled(&config));

        let disabled = json!({ "systemInject": false });
        assert!(!system_inject_enabled(&disabled));

        let missing = json!({ "other": "value" });
        assert!(!system_inject_enabled(&missing));

        let wrong_type = json!({ "systemInject": "yes" });
        assert!(!system_inject_enabled(&wrong_type));
    }

    #[test]
    fn inject_preserves_existing_messages_order() {
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "First" },
                { "role": "assistant", "content": "Response" }
            ]
        });
        assert!(inject_system_prompt(&mut body, "openai", "System prompt."));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "System prompt.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
    }

    #[test]
    fn claude_system_string_appends() {
        let mut body = json!({ "system": "Base rules", "messages": [] });
        assert!(inject_system_prompt(&mut body, "claude", "Extra."));
        assert_eq!(body["system"], "Base rules\n\nExtra.");
    }

    #[test]
    fn claude_system_array_inserts_before_cache_control() {
        let mut body = json!({
            "system": [
                { "type": "text", "text": "cached" },
                { "type": "text", "text": "after", "cache_control": { "type": "ephemeral" } }
            ]
        });
        assert!(inject_system_prompt(&mut body, "claude", "Injected."));
        let blocks = body["system"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        // Injected block sits before the cache_control block.
        assert_eq!(blocks[1]["text"], "Injected.");
        assert!(blocks[2]["cache_control"].is_object());
    }

    #[test]
    fn gemini_system_instruction_parts() {
        let mut body = json!({ "systemInstruction": { "parts": [{ "text": "a" }] } });
        assert!(inject_system_prompt(&mut body, "gemini", "b"));
        let parts = body["systemInstruction"]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["text"], "b");
    }

    #[test]
    fn gemini_request_wrapped_system_instruction() {
        let mut body =
            json!({ "request": { "systemInstruction": { "parts": [{ "text": "a" }] } } });
        assert!(inject_system_prompt(&mut body, "gemini", "b"));
        let parts = body["request"]["systemInstruction"]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn responses_instructions_string() {
        let mut body = json!({ "instructions": "Be terse", "input": [] });
        assert!(inject_system_prompt(&mut body, "openai", "Also helpful."));
        assert_eq!(body["instructions"], "Be terse\n\nAlso helpful.");
    }

    #[test]
    fn responses_input_array_appends_to_developer() {
        let mut body = json!({
            "input": [
                {
                    "type": "message",
                    "role": "developer",
                    "content": [ { "type": "input_text", "text": "a" } ]
                }
            ]
        });
        assert!(inject_system_prompt(&mut body, "openai", "b"));
        let parts = body["input"][0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["text"], "b");
    }

    #[test]
    fn responses_input_inserts_typed_message_item() {
        let mut body = json!({
            "input": [
                { "type": "message", "role": "user", "content": [ { "type": "input_text", "text": "hi" } ] }
            ]
        });
        assert!(inject_system_prompt(&mut body, "openai", "PROMPT"));
        assert_eq!(
            body["input"][0],
            json!({
                "type": "message",
                "role": "system",
                "content": [ { "type": "input_text", "text": "PROMPT" } ]
            })
        );
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn responses_input_ignores_untyped_role_item() {
        // A bare `{role, content}` entry is not a Responses message item, so it
        // must be left alone and a typed item inserted ahead of it.
        let mut body = json!({
            "input": [ { "role": "system", "content": "pre-existing" } ]
        });
        assert!(inject_system_prompt(&mut body, "openai", "PROMPT"));
        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][1]["content"], "pre-existing");
    }

    #[test]
    fn kiro_format_arm_appends_to_first_user_turn() {
        let mut body = json!({
            "conversationState": {
                "history": [
                    { "userInputMessage": { "content": "first" } },
                    { "userInputMessage": { "content": "second" } }
                ]
            }
        });
        assert!(inject_system_prompt(&mut body, "kiro", "PROMPT"));
        assert_eq!(
            body["conversationState"]["history"][0]["userInputMessage"]["content"],
            "first\n\nPROMPT"
        );
        assert_eq!(
            body["conversationState"]["history"][1]["userInputMessage"]["content"],
            "second"
        );
    }

    #[test]
    fn kiro_format_arm_falls_back_to_current_message() {
        let mut body = json!({
            "conversationState": { "currentMessage": { "userInputMessage": { "content": "hi" } } }
        });
        assert!(inject_system_prompt(&mut body, "kiro", "PROMPT"));
        assert_eq!(
            body["conversationState"]["currentMessage"]["userInputMessage"]["content"],
            "hi\n\nPROMPT"
        );
    }

    #[test]
    fn kiro_format_arm_without_user_turn_is_a_noop() {
        let mut body = json!({ "conversationState": { "history": [] } });
        assert!(!inject_system_prompt(&mut body, "kiro", "PROMPT"));
    }
}
