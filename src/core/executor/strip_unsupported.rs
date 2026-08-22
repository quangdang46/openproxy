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

    // 9router paramSupport.js STRIP_RULES (port):
    // { match: /claude/i, drop: ["temperature"] } — no provider field, so it
    // applies to EVERY provider when the model id matches /claude/i.
    if field == "temperature" && model.to_ascii_lowercase().contains("claude") {
        return true;
    }

    // { provider: "github", match: /gpt-5\.4/i, drop: ["temperature"] }
    if field == "temperature"
        && provider == "github"
        && model.to_ascii_lowercase().contains("gpt-5.4")
    {
        return true;
    }

    // { provider: "github", match: (m) => /claude/i.test(m) &&
    //   !/claude.*(opus|sonnet).*4\.6/i.test(m), drop: ["thinking", "reasoning_effort"] }
    if provider == "github" && (field == "thinking" || field == "reasoning_effort") {
        let m = model.to_ascii_lowercase();
        let is_claude_except_46 = m.contains("claude")
            && !(m.contains("claude")
                && (m.contains("opus") || m.contains("sonnet"))
                && m.contains("4.6"));
        if is_claude_except_46 {
            return true;
        }
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

    // flattenContent — cloudflare-ai (9router paramSupport.js:16, 47-56):
    // replace each message's content array with the joined text parts.
    if provider == "cloudflare-ai" {
        flatten_content(obj);
    }

    // clampToModelMaxOutput / maxOutputCap — volcengine-ark (9router
    // paramSupport.js:17-23, 57-71): clamp max_tokens / max_completion_tokens
    // / max_output_tokens to the per-model ceiling when they exceed it.
    if provider == "volcengine-ark" {
        clamp_max_output(obj, model);
    }
}

/// 9router paramSupport.js flattenContent: collapse a message's content
/// array into a plain string of its text parts (cloudflare requires string
/// content). Non-text parts (image_url etc.) contribute "".
fn flatten_content(obj: &mut serde_json::Map<String, Value>) {
    let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for msg in messages {
        let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let joined: String = content
            .iter()
            .map(|b| {
                if b.get("type").and_then(Value::as_str) == Some("text") {
                    b.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                } else {
                    String::new()
                }
            })
            .collect();
        msg["content"] = Value::String(joined);
    }
}

/// 9router paramSupport.js clamp: clamp max_tokens / max_completion_tokens /
/// max_output_tokens to the model ceiling. Only fires when the current value
/// is a finite number greater than the ceiling (0/null untouched).
fn clamp_max_output(obj: &mut serde_json::Map<String, Value>, model: &str) {
    let m = model.to_ascii_lowercase();
    // volcengine-ark: /glm-5/i → model maxOutput; /kimi/i → 32768 cap.
    // The Rust catalog has no maxOutput column, so use a documented ceiling
    // for glm-5 models (256k tokens) and the JS 32768 cap for kimi; Math.min
    // semantics preserve the smaller.
    let mut cap: Option<u64> = None;
    if m.contains("glm-5") {
        cap = Some(262_144);
    }
    if m.contains("kimi") {
        cap = Some(cap.map_or(32_768, |c| c.min(32_768)));
    }
    let Some(ceiling) = cap else { return };

    for key in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
        if let Some(n) = obj.get(key).and_then(Value::as_u64) {
            if n > ceiling {
                obj.insert(key.to_string(), Value::from(ceiling));
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

    #[test]
    fn flatten_content_for_cloudflare_ai() {
        let mut body = json!({
            "model": "@cf/meta/llama-3",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "hi"},
                        {"type": "image_url", "image_url": {"url": "x"}}
                    ]
                }
            ]
        });
        strip_unsupported_params("cloudflare-ai", "@cf/meta/llama-3", &mut body);
        // Array replaced with joined text parts (image_url contributes "").
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn clamps_kimi_max_tokens_to_32768() {
        let mut body = json!({
            "model": "kimi-k2.7-code",
            "max_tokens": 50000,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("volcengine-ark", "kimi-k2.7-code", &mut body);
        assert_eq!(body["max_tokens"], 32768);
    }

    #[test]
    fn clamp_leaves_values_below_ceiling_untouched() {
        let mut body = json!({
            "model": "kimi-k2.7-code",
            "max_tokens": 1000,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("volcengine-ark", "kimi-k2.7-code", &mut body);
        assert_eq!(body["max_tokens"], 1000);
    }

    #[test]
    fn strips_temperature_for_claude_models() {
        let mut body = json!({
            "temperature": 0.7,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("openai", "claude-sonnet-4.5", &mut body);
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn keeps_temperature_for_non_claude() {
        let mut body = json!({
            "temperature": 0.7,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("openai", "gpt-4o", &mut body);
        assert_eq!(body.get("temperature"), Some(&json!(0.7)));
    }

    #[test]
    fn strips_thinking_and_effort_for_github_claude_except_46() {
        // claude-sonnet-4.5 on github: thinking + reasoning_effort stripped.
        let mut body = json!({
            "thinking": {"type": "enabled"},
            "reasoning_effort": "high",
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("github", "claude-sonnet-4.5", &mut body);
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());

        // claude-opus-4.6 on github: the 4.6 exception KEEPS both.
        let mut body = json!({
            "thinking": {"type": "enabled"},
            "reasoning_effort": "high",
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("github", "claude-sonnet-4.6", &mut body);
        assert!(body.get("thinking").is_some());
        assert!(body.get("reasoning_effort").is_some());
    }

    #[test]
    fn strips_github_temperature_for_gpt_5_4() {
        let mut body = json!({
            "temperature": 1.0,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("github", "gpt-5.4", &mut body);
        assert!(body.get("temperature").is_none());
    }
}
