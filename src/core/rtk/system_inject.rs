use serde_json::Value;

const SEP: &str = "\n\n";

/// Inject a system prompt into the request body, dispatching by format.
/// 9router systemInject.js `injectSystemPrompt(body, format, prompt)`.
///
/// - `"claude"` → `body.system` (string or array, inserted before the last
///   cache_control block).
/// - `"gemini"` / `"gemini-cli"` / `"vertex"` / `"antigravity"` →
///   `body.systemInstruction` / `body.request.systemInstruction` (`{parts:[{text}]}`).
/// - Everything else (OpenAI chat / Responses / codex / cursor / kiro /
///   ollama) → `messages[]` / `input[]` / `instructions`.
///
/// Returns `true` if the body was modified.
pub fn inject_system_prompt(body: &mut Value, format: &str, prompt: &str) -> bool {
    if prompt.trim().is_empty() {
        return false;
    }
    match format {
        "claude" => inject_claude_system(body, prompt),
        "gemini" | "gemini-cli" | "vertex" | "antigravity" => inject_gemini_system(body, prompt),
        _ => inject_messages_system(body, prompt),
    }
}

/// OpenAI-shaped: `messages[]` (chat) or `input[]` (responses) or
/// `instructions` (responses top-level string). 9router injectMessagesSystem.
fn inject_messages_system(body: &mut Value, prompt: &str) -> bool {
    // OpenAI Responses API: top-level string field.
    if let Some(Value::String(instructions)) = body.get_mut("instructions") {
        if !instructions.is_empty() {
            instructions.push_str(SEP);
        }
        instructions.push_str(prompt);
        return true;
    }

    let arr = if body.get("messages").is_some() {
        body.get_mut("messages").and_then(Value::as_array_mut)
    } else {
        body.get_mut("input").and_then(Value::as_array_mut)
    };
    let Some(arr) = arr else {
        return false;
    };

    let idx = arr.iter().position(|m| {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        role == "system" || role == "developer"
    });
    match idx {
        Some(i) => append_to_openai_message(&mut arr[i], prompt),
        None => {
            arr.insert(
                0,
                serde_json::json!({ "role": "system", "content": prompt }),
            );
            true
        }
    }
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
                { "role": "developer", "content": [ { "type": "input_text", "text": "a" } ] }
            ]
        });
        assert!(inject_system_prompt(&mut body, "openai", "b"));
        let parts = body["input"][0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["text"], "b");
    }
}
