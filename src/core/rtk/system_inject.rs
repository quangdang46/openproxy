use serde_json::Value;

/// Inject a system prompt into the OpenAI-shaped request body.
///
/// Prepends to the `messages` array if no system message exists,
/// appends to the content of the existing system message otherwise.
/// Returns `true` if the body was modified.
pub fn inject_system_prompt(body: &mut Value, prompt: &str) -> bool {
    if prompt.trim().is_empty() {
        return false;
    }

    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };

    if let Some(sys_msg) = messages
        .iter_mut()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("system"))
    {
        match sys_msg.get_mut("content") {
            Some(Value::String(content)) => {
                if !content.is_empty() {
                    content.push_str("\n\n");
                }
                content.push_str(prompt);
                true
            }
            _ => false,
        }
    } else {
        messages.insert(
            0,
            serde_json::json!({ "role": "system", "content": prompt }),
        );
        true
    }
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
        assert!(inject_system_prompt(&mut body, "Additional instruction."));
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
        assert!(!inject_system_prompt(&mut body, ""));
        assert!(!inject_system_prompt(&mut body, "   "));
    }

    #[test]
    fn no_modification_when_no_messages_array() {
        let mut body = json!({ "model": "gpt-4" });
        assert!(!inject_system_prompt(&mut body, "test"));
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
        assert!(inject_system_prompt(&mut body, "System prompt."));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "System prompt.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
    }
}
