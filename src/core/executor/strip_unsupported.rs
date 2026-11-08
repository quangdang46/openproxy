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

    // { provider: "opencode-go", match: /^deepseek/i, drop: ["response_format"] }
    // The relay 400s on response_format for its deepseek family (bare model
    // echo or "This response_format type is unavailable now").
    if field == "response_format"
        && provider == "opencode-go"
        && model.to_ascii_lowercase().starts_with("deepseek")
    {
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

/// 9router paramSupport.js renameParams: move `from` → `to`. The JS copies
/// only when `body[to] === undefined` (key absent) — a present target, even
/// null, is kept. `from` is always removed. (The looser null/empty-string
/// tolerance applies only to renameMessageFields — see `target_is_empty`.)
fn rename_param(obj: &mut serde_json::Map<String, Value>, from: &str, to: &str) {
    let Some(from_val) = obj.remove(from) else {
        return;
    };
    if !obj.contains_key(to) {
        obj.insert(to.to_string(), from_val);
    }
}

/// Whether an existing target value should be overwritten by a rename.
/// Mirrors the JS check: overwrite when existing is undefined, null, or an
/// empty/whitespace string; keep it otherwise.
fn target_is_empty(v: &Value) -> bool {
    v.is_null() || v.as_str().map(|s| s.trim().is_empty()).unwrap_or(false)
}

/// 9router paramSupport.js renameMessageFields: nvidia NIM treats `reasoning`
/// and `reasoning_content` as aliases for one field; sending both makes its
/// deserializer report a duplicate field. Keep the OpenAI canonical spelling
/// (`reasoning_content`) on assistant history. Only `role == "assistant"`
/// messages are touched, matching upstream; a present non-empty
/// `reasoning_content` wins over the renamed `reasoning` value.
fn apply_message_field_rules(obj: &mut serde_json::Map<String, Value>, provider: &str) {
    if provider != "nvidia" {
        return;
    }

    let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(m) = msg.as_object_mut() else {
            continue;
        };
        // JS: if msg.reasoning !== undefined → maybe set
        // msg.reasoning_content, then always delete msg.reasoning.
        if let Some(reasoning) = m.remove("reasoning") {
            let overwrite = m
                .get("reasoning_content")
                .map(target_is_empty)
                .unwrap_or(true);
            if overwrite {
                m.insert("reasoning_content".to_string(), reasoning);
            }
        }
    }
}

/// Remove unsupported fields from `body` for the given `provider` and `model`.
///
/// Mutates `body` in place. No-op for providers that aren't in the filter map.
pub fn strip_unsupported_params(provider: &str, model: &str, body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    // Upstream paramSupport.js:65 — no rules apply without a model id.
    if model.is_empty() {
        return;
    }

    // 9router paramSupport.js renameParams (port): rename runs before drops,
    // and keeps an explicit target value if the client already sent one.
    // { provider: "openai", renameParams: { max_tokens: "max_completion_tokens" } }
    // (#7c42d8f — gpt-5.x/gpt-6.x 400 on `max_tokens`.)
    if provider == "openai" {
        rename_param(obj, "max_tokens", "max_completion_tokens");
    }

    let fields_to_check: Vec<String> = obj.keys().cloned().collect();
    for field in &fields_to_check {
        if should_strip(provider, model, field) {
            obj.remove(field.as_str());
        }
    }

    // Also check nested `extra_body` for unsupported fields
    if let Some(extra) = obj.get_mut("extra_body").and_then(|v| v.as_object_mut()) {
        if provider == "openai" {
            rename_param(extra, "max_tokens", "max_completion_tokens");
        }
        let nested_fields: Vec<String> = extra.keys().cloned().collect();
        for field in &nested_fields {
            if should_strip(provider, model, field) {
                extra.remove(field.as_str());
            }
        }
    }

    // Assistant-message renameMessageFields rules (nvidia reasoning →
    // reasoning_content); dropMessageFields for groq/mistral/cerebras is
    // applied below alongside the other provider passes.
    apply_message_field_rules(obj, provider);

    // flattenContent — cloudflare-ai (9router paramSupport.js:16, 47-56):
    // replace each message's content array with the joined text parts.
    if provider == "cloudflare-ai" {
        flatten_content(obj);
    }

    // MiMo Desktop Preview models on the account-service route (9router 73cb8914):
    // content must be a plain string, rejects OpenAI content-part arrays.
    // Cloud models keep their parts (mimo-v2-omni is multi-modal).
    if (provider == "xiaomi-mimo" || provider == "mimo")
        && model.to_ascii_lowercase().contains("preview")
    {
        flatten_content(obj);
    }

    // clampToModelMaxOutput / maxOutputCap — volcengine-ark (9router
    // paramSupport.js:17-23, 57-71): clamp max_tokens / max_completion_tokens
    // / max_output_tokens to the per-model ceiling when they exceed it.
    if provider == "volcengine-ark" {
        clamp_max_output(obj, model);
    }

    // dropMessageFields — groq / mistral / cerebras (9router paramSupport.js:
    // 27-29, 59-66, #4220): strict OpenAI-compatible validators reject unknown
    // assistant-message fields. Clients echo prior-turn reasoning back on every
    // assistant message; Groq 400s and Mistral 422s ("extra_forbidden") on it.
    // Providers that REQUIRE the field (DeepSeek, Kimi) are handled elsewhere
    // and are deliberately not listed here.
    if matches!(provider, "groq" | "mistral" | "cerebras") {
        drop_message_fields(
            obj,
            &["reasoning_content", "reasoning", "reasoning_details"],
        );
    }
}

/// 9router paramSupport.js dropMessageFields: remove `keys` from every
/// assistant-role message in `body.messages`. Non-assistant messages are
/// untouched, and a missing `messages` array is a no-op.
fn drop_message_fields(obj: &mut serde_json::Map<String, Value>, keys: &[&str]) {
    let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for msg in messages {
        if msg.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(map) = msg.as_object_mut() {
            for key in keys {
                map.remove(*key);
            }
        }
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

    #[test]
    fn flattens_content_for_mimo_preview_models() {
        // 9router 73cb8914: Preview models on the account-service route need
        // plain-string content; cloud models keep their parts.
        let mut body = json!({
            "messages": [{"role": "user", "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}]
        });
        strip_unsupported_params("xiaomi-mimo", "mimo-x-pro-preview", &mut body);
        assert_eq!(body["messages"][0]["content"], json!("ab"));

        let mut body = json!({
            "messages": [{"role": "user", "content": [{"type": "text", "text": "a"}]}]
        });
        strip_unsupported_params("xiaomi-mimo", "mimo-v2.5-pro", &mut body);
        assert!(body["messages"][0]["content"].is_array());
    }

    fn replayed_reasoning_body() -> Value {
        json!({
            "model": "llama-3.3-70b-versatile",
            "messages": [
                {"role": "user", "content": "hi"},
                {
                    "role": "assistant",
                    "content": "thinking out loud",
                    "reasoning_content": "let me think",
                    "reasoning": "more thinking",
                    "reasoning_details": [{"type": "text", "text": "detail"}]
                },
                {"role": "tool", "content": "tool result", "reasoning_content": "kept"}
            ]
        })
    }

    #[test]
    fn strips_replayed_reasoning_fields_for_groq() {
        let mut body = replayed_reasoning_body();
        strip_unsupported_params("groq", "llama-3.3-70b-versatile", &mut body);
        let assistant = &body["messages"][1];
        assert!(assistant.get("reasoning_content").is_none());
        assert!(assistant.get("reasoning").is_none());
        assert!(assistant.get("reasoning_details").is_none());
        assert_eq!(assistant.get("content"), Some(&json!("thinking out loud")));
        // Non-assistant messages keep the fields (upstream only touches role==assistant).
        assert_eq!(body["messages"][2]["reasoning_content"], json!("kept"));
    }

    #[test]
    fn strips_replayed_reasoning_fields_for_mistral_and_cerebras() {
        for provider in ["mistral", "cerebras"] {
            let mut body = replayed_reasoning_body();
            strip_unsupported_params(provider, "test-model", &mut body);
            assert!(body["messages"][1].get("reasoning_content").is_none());
            assert!(body["messages"][1].get("reasoning").is_none());
            assert!(body["messages"][1].get("reasoning_details").is_none());
        }
    }

    #[test]
    fn leaves_reasoning_fields_untouched_for_openai() {
        let mut body = replayed_reasoning_body();
        let original = body.clone();
        strip_unsupported_params("openai", "gpt-4o", &mut body);
        assert_eq!(body, original);
    }

    // --- 9router paramSupport.js renameParams: openai max_tokens →
    // max_completion_tokens (bead openproxy-4d5h.26, upstream #7c42d8f) ---

    #[test]
    fn renames_max_tokens_for_openai() {
        let mut body = json!({
            "model": "gpt-5.4",
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("openai", "gpt-5.4", &mut body);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 4096);
    }

    #[test]
    fn openai_rename_keeps_explicit_max_completion_tokens() {
        // JS: body[to] === undefined is the only copy condition — a present
        // target wins even when max_tokens is also sent.
        let mut body = json!({
            "model": "gpt-5.4",
            "max_tokens": 4096,
            "max_completion_tokens": 8192,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("openai", "gpt-5.4", &mut body);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 8192);
    }

    #[test]
    fn openai_rename_does_not_touch_other_providers() {
        let mut body = json!({
            "model": "llama-3",
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("groq", "llama-3", &mut body);
        assert_eq!(body["max_tokens"], 4096);
        assert!(body.get("max_completion_tokens").is_none());
    }

    // --- 9router paramSupport.js renameMessageFields: nvidia
    // reasoning → reasoning_content (bead openproxy-4d5h.25) ---

    #[test]
    fn nvidia_renames_reasoning_on_assistant_messages() {
        let mut body = json!({
            "model": "meta/llama-3.1-70b-instruct",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello", "reasoning": "thought"},
                {"role": "assistant", "content": "more", "reasoning": "t2",
                 "reasoning_content": "kept"}
            ]
        });
        strip_unsupported_params("nvidia", "meta/llama-3.1-70b-instruct", &mut body);
        let msgs = body["messages"].as_array().unwrap();
        // First assistant: reasoning renamed to reasoning_content.
        assert!(msgs[1].get("reasoning").is_none());
        assert_eq!(msgs[1]["reasoning_content"], "thought");
        // Second: existing non-empty reasoning_content wins; reasoning dropped.
        assert!(msgs[2].get("reasoning").is_none());
        assert_eq!(msgs[2]["reasoning_content"], "kept");
        // User messages untouched.
        assert!(msgs[0].get("reasoning").is_none());
        assert!(msgs[0].get("reasoning_content").is_none());
    }

    #[test]
    fn nvidia_rename_overwrites_empty_reasoning_content() {
        // JS: existing === null or "" → overwritten by the reasoning value.
        let mut body = json!({
            "messages": [
                {"role": "assistant", "content": "a", "reasoning": "real",
                 "reasoning_content": ""}
            ]
        });
        strip_unsupported_params("nvidia", "some-model", &mut body);
        assert_eq!(body["messages"][0]["reasoning_content"], "real");
        assert!(body["messages"][0].get("reasoning").is_none());
    }

    // --- 9router paramSupport.js: opencode-go deepseek response_format ---

    #[test]
    fn strips_response_format_for_opencode_go_deepseek() {
        let mut body = json!({
            "model": "deepseek-v4.1-flash",
            "response_format": {"type": "json_object"},
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("opencode-go", "deepseek-v4.1-flash", &mut body);
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn keeps_response_format_for_opencode_go_non_deepseek() {
        let mut body = json!({
            "model": "glm-5",
            "response_format": {"type": "json_object"},
            "messages": [{"role": "user", "content": "hi"}]
        });
        strip_unsupported_params("opencode-go", "glm-5", &mut body);
        assert!(body.get("response_format").is_some());
    }
}
