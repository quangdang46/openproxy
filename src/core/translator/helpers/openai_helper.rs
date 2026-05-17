//! OpenAI-shape helpers shared across translators.
//!
//! Ports `open-sse/translator/helpers/openaiHelper.js` from upstream 9router.
//! Currently the only invariant we enforce is the `developer` → `system` role
//! rewrite: Codex CLI (and a few other OAI-compat clients) emit messages with
//! `role: "developer"` per the newer OpenAI spec, but most OAI-compat
//! providers (DeepSeek, Groq, Ollama, …) reject anything outside the
//! `system | user | assistant | tool` quartet. Rewriting on the way out keeps
//! the request payload portable.

use serde_json::Value;

/// Rewrite `role: "developer"` → `role: "system"` on every message in
/// `body.messages[]`. No-op when the body has no `messages` array.
///
/// Mirrors the head of upstream `filterToOpenAIFormat()` in
/// `open-sse/translator/helpers/openaiHelper.js`.
pub fn normalize_developer_role(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for msg in messages.iter_mut() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        if obj.get("role").and_then(Value::as_str) == Some("developer") {
            obj.insert("role".to_string(), Value::String("system".to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rewrites_developer_to_system() {
        let mut body = json!({
            "model": "deepseek-chat",
            "messages": [
                {"role": "developer", "content": "You are an assistant."},
                {"role": "user", "content": "hi"}
            ]
        });
        normalize_developer_role(&mut body);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn leaves_other_roles_alone() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "u"},
                {"role": "assistant", "content": "a"},
                {"role": "tool", "tool_call_id": "x", "content": "t"}
            ]
        });
        let before = body.clone();
        normalize_developer_role(&mut body);
        assert_eq!(body, before);
    }

    #[test]
    fn ignores_body_without_messages() {
        let mut body = json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]});
        let before = body.clone();
        normalize_developer_role(&mut body);
        assert_eq!(body, before);
    }

    #[test]
    fn preserves_content_and_metadata() {
        let mut body = json!({
            "messages": [
                {
                    "role": "developer",
                    "content": [{"type": "text", "text": "spec"}],
                    "name": "spec-author"
                }
            ]
        });
        normalize_developer_role(&mut body);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][0]["text"], "spec");
        assert_eq!(body["messages"][0]["name"], "spec-author");
    }

    #[test]
    fn handles_empty_messages() {
        let mut body = json!({"messages": []});
        normalize_developer_role(&mut body);
        assert_eq!(body["messages"], json!([]));
    }
}
