//! Port of `open-sse/utils/reasoningContentInjector.js`.
//!
//! Some thinking-mode providers (DeepSeek, Kimi, …) require a non-empty
//! `reasoning_content` field on assistant messages. Clients in OpenAI
//! shape don't supply one, so we inject a placeholder. This module also
//! handles the synthetic `deepseek-v4-pro-{max,none}` aliases that
//! pre-set `extra_body.thinking.type` and `reasoning_effort`.

use serde_json::{json, Value};

const PLACEHOLDER: &str = " ";

/// Where the placeholder should be injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Inject on every assistant message that lacks reasoning_content.
    All,
    /// Inject only when the assistant message also carries `tool_calls`.
    ToolCalls,
}

/// Pick the rule (provider then model) that applies to this request.
fn rule_for(provider: &str, model: &str) -> Option<Scope> {
    // Provider-level rules first.
    if provider == "deepseek" {
        return Some(Scope::All);
    }
    // Model-level fallback rules.
    if model.starts_with("kimi-") {
        return Some(Scope::ToolCalls);
    }
    if model.starts_with("deepseek-") {
        return Some(Scope::All);
    }
    None
}

fn should_inject(message: &Value, scope: Scope) -> bool {
    if message.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return false;
    }
    let rc = message.get("reasoning_content");
    if rc.and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
        return false;
    }
    match scope {
        Scope::All => true,
        Scope::ToolCalls => message
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false),
    }
}

fn apply_scope(body: &mut Value, scope: Scope) {
    let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for msg in messages.iter_mut() {
        if should_inject(msg, scope) {
            if let Some(obj) = msg.as_object_mut() {
                obj.insert(
                    "reasoning_content".to_string(),
                    Value::String(PLACEHOLDER.to_string()),
                );
            }
        }
    }
}

/// Apply the synthetic `deepseek-v4-pro-{max,none}` alias rewriting:
/// the body's `model` is replaced with `deepseek-v4-pro` and
/// `extra_body.thinking.type` + `reasoning_effort` are pinned to the
/// alias's intent.
fn apply_deepseek_v4_pro_alias(provider: &str, model: &str, body: &mut Value) {
    if provider != "deepseek" {
        return;
    }
    let (thinking_type, reasoning_effort) = match model {
        "deepseek-v4-pro-max" => ("enabled", Some("max")),
        "deepseek-v4-pro-none" => ("disabled", None),
        _ => return,
    };

    let Some(obj) = body.as_object_mut() else {
        return;
    };
    obj.insert(
        "model".to_string(),
        Value::String("deepseek-v4-pro".to_string()),
    );

    // Ensure extra_body.thinking exists and pin its type.
    let extra = obj
        .entry("extra_body".to_string())
        .or_insert_with(|| json!({}));
    if let Some(eb) = extra.as_object_mut() {
        let thinking = eb
            .entry("thinking".to_string())
            .or_insert_with(|| json!({}));
        if let Some(t) = thinking.as_object_mut() {
            t.insert("type".to_string(), Value::String(thinking_type.to_string()));
        }
    }

    match reasoning_effort {
        Some(level) => {
            obj.insert(
                "reasoning_effort".to_string(),
                Value::String(level.to_string()),
            );
        }
        None => {
            obj.remove("reasoning_effort");
        }
    }
}

/// Run both the alias-rewriting and the placeholder-injection pipeline
/// against `body`. Mutates `body` in place. `provider` and `model` are
/// the resolved upstream identifiers (post-translation).
pub fn inject_reasoning_content(provider: &str, model: &str, body: &mut Value) {
    apply_deepseek_v4_pro_alias(provider, model, body);
    if let Some(scope) = rule_for(provider, model) {
        apply_scope(body, scope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_provider_injects_on_all_assistant_messages() {
        let mut body = json!({
            "model": "deepseek-chat",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
            ]
        });
        inject_reasoning_content("deepseek", "deepseek-chat", &mut body);
        assert_eq!(
            body["messages"][1]["reasoning_content"],
            Value::String(PLACEHOLDER.to_string())
        );
        // user message untouched
        assert!(body["messages"][0].get("reasoning_content").is_none());
    }

    #[test]
    fn kimi_only_injects_when_tool_calls_present() {
        let mut body = json!({
            "model": "kimi-k2",
            "messages": [
                {"role": "assistant", "content": "no tools"},
                {"role": "assistant", "content": "", "tool_calls": [{"id": "t1"}]},
            ]
        });
        inject_reasoning_content("kimi", "kimi-k2", &mut body);
        assert!(body["messages"][0].get("reasoning_content").is_none());
        assert_eq!(
            body["messages"][1]["reasoning_content"],
            Value::String(PLACEHOLDER.to_string())
        );
    }

    #[test]
    fn does_not_overwrite_existing_reasoning() {
        let mut body = json!({
            "messages": [
                {"role": "assistant", "content": "x", "reasoning_content": "preset"}
            ]
        });
        inject_reasoning_content("deepseek", "deepseek-chat", &mut body);
        assert_eq!(body["messages"][0]["reasoning_content"], "preset");
    }

    #[test]
    fn deepseek_v4_pro_alias_rewrites_model() {
        let mut body = json!({"model": "deepseek-v4-pro-max", "messages": []});
        inject_reasoning_content("deepseek", "deepseek-v4-pro-max", &mut body);
        assert_eq!(body["model"], "deepseek-v4-pro");
        assert_eq!(body["extra_body"]["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "max");
    }

    #[test]
    fn deepseek_v4_pro_none_clears_effort() {
        let mut body = json!({
            "model": "deepseek-v4-pro-none",
            "reasoning_effort": "low",
            "messages": []
        });
        inject_reasoning_content("deepseek", "deepseek-v4-pro-none", &mut body);
        assert_eq!(body["model"], "deepseek-v4-pro");
        assert_eq!(body["extra_body"]["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_effort").is_none());
    }
}
