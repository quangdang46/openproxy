//! Provider-specific parameter filtering.
//!
//! Some providers reject request fields they don't support (e.g. `max_completion_tokens`
//! on providers without a native `completion_tokens` concept). This module removes
//! those fields before the request is sent upstream.

use serde_json::Value;

/// Provider+model parameter filter map.
///
/// Returns `true` if the field should be **removed** (stripped) from the body.
type ParamFilter = fn(provider: &str, model: &str, field: &str) -> bool;

/// Composite filter that checks against all known unsupported-parameter rules.
fn should_strip(provider: &str, model: &str, field: &str) -> bool {
    // Anthropic-compatible providers (kimi, minimax, glm, agentrouter, etc.)
    // don't support `max_completion_tokens` — they use `max_tokens` instead.
    if field == "max_completion_tokens" && is_anthropic_compatible(provider) {
        return true;
    }

    // Providers without native `reasoning_effort` support.
    // Notably Gemini uses its own `thinking` config.
    if field == "reasoning_effort" && (provider == "gemini" || provider == "vertex") {
        return true;
    }

    // Some providers (non-Anthropic compat) don't support `max_tokens` alias.
    if field == "max_tokens" && provider == "gemini" {
        return true;
    }

    false
}

fn is_anthropic_compatible(provider: &str) -> bool {
    matches!(
        provider,
        "claude" | "glm" | "kimi" | "kimi-coding" | "minimax" | "minimax-cn" | "agentrouter"
    )
}

/// Remove unsupported fields from `body` for the given `provider` and `model`.
///
/// Mutates `body` in place. No-op for providers that aren't in the filter map.
pub fn strip_unsupported_params(provider: &str, model: &str, body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    let fields_to_check: Vec<String> = obj.keys().cloned().collect();
    for field in &fields_to_check {
        if should_strip(provider, model, field) {
            obj.remove(field.as_str());
        }
    }

    // Also check nested `extra_body` for unsupported fields
    if let Some(extra) = obj.get_mut("extra_body").and_then(|v| v.as_object_mut()) {
        let nested_fields: Vec<String> = extra.keys().cloned().collect();
        for field in &nested_fields {
            if should_strip(provider, model, field) {
                extra.remove(field.as_str());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_max_completion_tokens_for_anthropic_compatible() {
        let mut body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_completion_tokens": 8192,
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("claude", "claude-sonnet-4-20250514", &mut body);
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("max_tokens").is_some());
    }

    #[test]
    fn keeps_max_completion_tokens_for_openai() {
        let mut body = json!({
            "model": "gpt-4o",
            "max_completion_tokens": 8192,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("openai", "gpt-4o", &mut body);
        assert!(body.get("max_completion_tokens").is_some());
    }

    #[test]
    fn strips_reasoning_effort_for_gemini() {
        let mut body = json!({
            "model": "gemini-2.5-pro",
            "reasoning_effort": "high",
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("gemini", "gemini-2.5-pro", &mut body);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn noop_for_unlisted_providers() {
        let mut body = json!({
            "model": "gpt-4o",
            "max_completion_tokens": 8192,
            "reasoning_effort": "high",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let original = body.clone();
        strip_unsupported_params("openai", "gpt-4o", &mut body);
        assert_eq!(body, original);
    }
}
