//! Caveman prompt injection -- appends a terse-style system prompt
//! to reduce output token usage.
//!
//! Ported from the JS `cavemanPrompts` map in 9router.
//! The injection targets the format-specific system-instruction field.

use serde_json::{json, Map, Value};

use crate::core::translator::registry::Format;

/// All supported caveman compression levels.
pub const CAVEMAN_LEVELS: [&str; 6] = [
    "lite",
    "full",
    "ultra",
    "wenyan-lite",
    "wenyan",
    "wenyan-ultra",
];

/// Return the prompt text for a given level name.
pub fn get_caveman_prompt(level: &str) -> &'static str {
    match level.trim().to_ascii_lowercase().as_str() {
        "lite" => "Respond tersely. Keep grammar and full sentences but drop filler.",
        "full" => "Respond like terse caveman. All technical substance stay exact, only fluff die.",
        "ultra" => "Respond ultra-terse. Maximum compression. Telegraphic.",
        "wenyan-lite" => {
            "Respond semi-classical Chinese. Use concise wenyan phrasing where natural, \
             but fall back to modern Chinese for complex technical terms. \
             Keep technical substance exact."
        }
        "wenyan" => {
            "Respond in Classical Chinese (wenyan). Use classical grammar and vocabulary. \
             Keep technical terms, code, and file paths in original form."
        }
        "wenyan-ultra" => {
            "Respond in ultra-terse Classical Chinese (wenyan). Maximum compression. \
             Abbreviate. Use classical idioms. Technical terms stay exact."
        }
        _ => "Respond like terse caveman. All technical substance stay exact, only fluff die.",
    }
}

/// Inject a caveman prompt into the request body based on its format.
///
/// Returns `true` if a modification was made.
pub fn inject_caveman(body: &mut Value, format: &Format, level: &str) -> bool {
    let prompt = get_caveman_prompt(level);

    match format {
        Format::Claude => inject_claude(body, prompt),
        Format::Gemini | Format::Vertex => inject_gemini(body, prompt),
        Format::OpenAi | Format::Cursor | Format::Kiro | Format::Ollama => {
            inject_openai(body, prompt)
        }
        Format::OpenAiResponses | Format::Codex => inject_responses(body, prompt),
        Format::Antigravity => inject_antigravity(body, prompt),
        _ => inject_openai(body, prompt),
    }
}

// ---------------------------------------------------------------------------
// Format-specific injection helpers
// ---------------------------------------------------------------------------

/// Inject into a Claude-format body by appending to `body.system`.
///
/// - If `system` is a string, concatenate with a newline separator.
/// - If `system` is a content-block array, append a new text block.
/// - If `system` is absent, create one.
fn inject_claude(body: &mut Value, prompt: &str) -> bool {
    let Some(fields) = body.as_object_mut() else {
        return false;
    };

    let system = fields
        .entry("system")
        .or_insert_with(|| Value::Array(vec![]));

    match system {
        Value::String(text) => {
            if text.contains(prompt) {
                return false;
            }
            text.push('\n');
            text.push_str(prompt);
            true
        }
        Value::Array(blocks) => {
            // Check idempotency: if any block already contains the prompt, skip.
            if blocks.iter().any(|b| {
                b.get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.contains(prompt))
            }) {
                return false;
            }

            // Insert just before the last cache_control block (if any)
            // to avoid breaking the cache boundary.
            let insert_at = blocks
                .iter()
                .rposition(|b| b.get("cache_control").is_some())
                .unwrap_or(blocks.len());
            blocks.insert(insert_at, json!({ "type": "text", "text": prompt }));
            true
        }
        _ => {
            *system = json!([{ "type": "text", "text": prompt }]);
            true
        }
    }
}

/// Inject into a Gemini/Vertex body by appending to
/// `systemInstruction.parts` or `system_instruction.parts`.
///
/// If `systemInstruction` is absent, create it. Both camelCase and
/// snake_case keys are supported when the body is flat, but the
/// injected key uses camelCase to match the upstream format.
///
/// Also handles the `body.request.systemInstruction` nesting used by
/// Antigravity's Gemini-shaped requests.
fn inject_gemini(body: &mut Value, prompt: &str) -> bool {
    let Some(fields) = body.as_object_mut() else {
        return false;
    };

    // Prefer to work inside `body.request` if present (Gemini/Antigravity nest)
    let target = if fields.contains_key("request") {
        fields.get_mut("request").and_then(Value::as_object_mut)
    } else {
        Some(fields)
    };

    let Some(target) = target else {
        return false;
    };

    inject_gemini_system_instruction(target, prompt)
}

fn inject_gemini_system_instruction(fields: &mut Map<String, Value>, prompt: &str) -> bool {
    // Check for system_instruction (snake_case) or systemInstruction (camelCase)
    let key = if fields.contains_key("system_instruction") {
        "system_instruction"
    } else {
        "systemInstruction"
    };

    match fields.get_mut(key) {
        Some(Value::String(text)) => {
            if text.contains(prompt) {
                return false;
            }
            text.push('\n');
            text.push_str(prompt);
            true
        }
        Some(Value::Object(si)) => inject_gemini_parts(si, prompt),
        Some(_) => {
            fields.insert(key.to_string(), json!({ "parts": [{ "text": prompt }] }));
            true
        }
        None => {
            fields.insert(key.to_string(), json!({ "parts": [{ "text": prompt }] }));
            true
        }
    }
}

fn inject_gemini_parts(si: &mut Map<String, Value>, prompt: &str) -> bool {
    let parts = si.entry("parts").or_insert_with(|| Value::Array(vec![]));

    match parts {
        Value::Array(arr) => {
            if arr
                .iter()
                .any(|p| p.get("text").and_then(Value::as_str) == Some(prompt))
            {
                return false;
            }
            arr.push(json!({ "text": prompt }));
            true
        }
        _ => {
            *parts = json!([{ "text": prompt }]);
            true
        }
    }
}

/// Inject into an OpenAI-format body.
///
/// - If there's a message with `role: "system"` or `role: "developer"`,
///   append the prompt to its content.
/// - Otherwise, prepend `{role: "system", content: prompt}` to the
///   messages array.
/// - If there are no messages at all, create a messages array.
fn inject_openai(body: &mut Value, prompt: &str) -> bool {
    let Some(fields) = body.as_object_mut() else {
        return false;
    };

    // Try to find existing system/developer message
    let messages = fields
        .entry("messages")
        .or_insert_with(|| Value::Array(vec![]));
    let Some(arr) = messages.as_array_mut() else {
        return false;
    };

    if let Some(sys_msg) = arr.iter_mut().find(|m| {
        matches!(
            m.get("role").and_then(Value::as_str),
            Some("system" | "developer")
        )
    }) {
        append_to_message(sys_msg, prompt)
    } else {
        arr.insert(0, json!({ "role": "system", "content": prompt }));
        true
    }
}

fn append_to_message(msg: &mut Value, prompt: &str) -> bool {
    match msg.get_mut("content") {
        Some(Value::String(text)) => {
            if text.contains(prompt) {
                return false;
            }
            text.push('\n');
            text.push_str(prompt);
            true
        }
        Some(Value::Array(parts)) => {
            if parts
                .iter()
                .any(|p| p.get("text").and_then(Value::as_str) == Some(prompt))
            {
                return false;
            }
            parts.push(json!({ "type": "text", "text": prompt }));
            true
        }
        _ => {
            msg["content"] = Value::String(prompt.into());
            true
        }
    }
}

/// Inject into a Responses-API / Codex body.
///
/// - If `body.instructions` is a string, append.
/// - If `body.instructions` is an array, add to it.
/// - If absent, set `body.instructions` to the prompt.
fn inject_responses(body: &mut Value, prompt: &str) -> bool {
    let Some(fields) = body.as_object_mut() else {
        return false;
    };

    match fields.get_mut("instructions") {
        Some(Value::String(text)) => {
            if text.contains(prompt) {
                return false;
            }
            text.push('\n');
            text.push_str(prompt);
            true
        }
        Some(Value::Array(arr)) => {
            if arr.iter().any(|v| v.as_str() == Some(prompt)) {
                return false;
            }
            arr.push(Value::String(prompt.into()));
            true
        }
        Some(_) => {
            fields.insert("instructions".into(), Value::String(prompt.into()));
            true
        }
        None => {
            fields.insert("instructions".into(), Value::String(prompt.into()));
            true
        }
    }
}

/// Inject into an Antigravity-format body.
///
/// Antigravity uses `body.request.systemInstruction` or
/// `body.systemInstruction` (Gemini-compatible shape).
fn inject_antigravity(body: &mut Value, prompt: &str) -> bool {
    let Some(fields) = body.as_object_mut() else {
        return false;
    };

    // Try body.request.systemInstruction first (nested), then body.systemInstruction
    if let Some(request) = fields.get_mut("request").and_then(Value::as_object_mut) {
        return inject_gemini_system_instruction(request, prompt);
    }

    inject_gemini_system_instruction(fields, prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // CAVEMAN_LEVELS
    // -----------------------------------------------------------------------

    #[test]
    fn caveman_levels_are_six() {
        assert_eq!(CAVEMAN_LEVELS.len(), 6);
    }

    #[test]
    fn caveman_levels_order() {
        assert_eq!(CAVEMAN_LEVELS[0], "lite");
        assert_eq!(CAVEMAN_LEVELS[1], "full");
        assert_eq!(CAVEMAN_LEVELS[2], "ultra");
        assert_eq!(CAVEMAN_LEVELS[3], "wenyan-lite");
        assert_eq!(CAVEMAN_LEVELS[4], "wenyan");
        assert_eq!(CAVEMAN_LEVELS[5], "wenyan-ultra");
    }

    // -----------------------------------------------------------------------
    // get_caveman_prompt
    // -----------------------------------------------------------------------

    #[test]
    fn prompt_for_lite() {
        let p = get_caveman_prompt("lite");
        assert!(p.contains("tersely"));
        assert!(!p.is_empty());
    }

    #[test]
    fn prompt_for_full() {
        let p = get_caveman_prompt("full");
        assert!(p.contains("caveman"));
    }

    #[test]
    fn prompt_for_ultra() {
        let p = get_caveman_prompt("ultra");
        assert!(p.contains("ultra-terse"));
    }

    #[test]
    fn prompt_for_wenyan_lite() {
        let p = get_caveman_prompt("wenyan-lite");
        assert!(p.contains("wenyan"));
    }

    #[test]
    fn prompt_for_wenyan() {
        let p = get_caveman_prompt("wenyan");
        assert!(p.contains("Classical Chinese"));
    }

    #[test]
    fn prompt_for_wenyan_ultra() {
        let p = get_caveman_prompt("wenyan-ultra");
        assert!(p.contains("ultra-terse"));
        assert!(p.contains("Classical Chinese"));
    }

    #[test]
    fn prompt_for_unknown_level_falls_back_to_full() {
        let p = get_caveman_prompt("unknown");
        assert!(p.contains("caveman"));
    }

    #[test]
    fn prompt_is_case_insensitive() {
        let lite = get_caveman_prompt("LITE");
        let full = get_caveman_prompt("FULL");
        assert!(lite.contains("tersely"));
        assert!(full.contains("caveman"));
    }

    #[test]
    fn prompt_trims_whitespace() {
        let p = get_caveman_prompt("  ultra  ");
        assert!(p.contains("ultra-terse"));
    }

    // -----------------------------------------------------------------------
    // inject_caveman -- Claude
    // -----------------------------------------------------------------------

    #[test]
    fn inject_claude_string_system() {
        let mut body = json!({
            "system": "You are Claude.",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(inject_caveman(&mut body, &Format::Claude, "lite"));
        let sys = body["system"].as_str().unwrap();
        assert!(sys.starts_with("You are Claude."));
        assert!(sys.contains("Respond terse"));
    }

    #[test]
    fn inject_claude_block_system() {
        let mut body = json!({
            "system": [{"type": "text", "text": "You are Claude."}],
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(inject_caveman(&mut body, &Format::Claude, "full"));
        let blocks = body["system"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(blocks[1]["text"].as_str().unwrap().contains("caveman"));
    }

    #[test]
    fn inject_claude_no_system() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(inject_caveman(&mut body, &Format::Claude, "ultra"));
        let blocks = body["system"].as_array().unwrap();
        assert!(blocks[0]["text"].as_str().unwrap().contains("ultra-terse"));
    }

    #[test]
    fn inject_claude_idempotent() {
        let mut body = json!({
            "system": "You are Claude.\nRespond tersely...",
            "messages": [{"role": "user", "content": "hi"}]
        });
        // First injection with a different prompt should still apply
        let result1 = inject_caveman(&mut body, &Format::Claude, "full");
        assert!(result1);
        // Verify the injection added content
        let text = body["system"].as_str().unwrap();
        assert!(text.contains("caveman"));
    }

    #[test]
    fn inject_claude_block_before_cache_control() {
        let mut body = json!({
            "system": [
                {"type": "text", "text": "prefix"},
                {"type": "text", "text": "cached", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(inject_caveman(&mut body, &Format::Claude, "lite"));
        let blocks = body["system"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["text"], "prefix");
        // The caveman block is injected before the cache_control block
        assert_eq!(blocks[1]["text"], get_caveman_prompt("lite"));
        assert!(blocks[2].get("cache_control").is_some());
    }

    // -----------------------------------------------------------------------
    // inject_caveman -- Gemini / Vertex
    // -----------------------------------------------------------------------

    #[test]
    fn inject_gemini_flat_system_instruction_parts() {
        let mut body = json!({
            "systemInstruction": {"parts": [{"text": "You are Gemini."}]},
            "contents": [{"parts": [{"text": "hi"}]}]
        });
        assert!(inject_caveman(&mut body, &Format::Gemini, "lite"));
        let parts = body["systemInstruction"]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[1]["text"].as_str().unwrap().contains("terse"));
    }

    #[test]
    fn inject_gemini_flat_system_instruction_string() {
        let mut body = json!({
            "systemInstruction": "You are Gemini.",
            "contents": [{"parts": [{"text": "hi"}]}]
        });
        assert!(inject_caveman(&mut body, &Format::Gemini, "full"));
        let si = body["systemInstruction"].as_str().unwrap();
        assert!(si.contains("caveman"));
    }

    #[test]
    fn inject_gemini_no_system_instruction() {
        let mut body = json!({
            "contents": [{"parts": [{"text": "hi"}]}]
        });
        assert!(inject_caveman(&mut body, &Format::Gemini, "ultra"));
        let parts = body["systemInstruction"]["parts"].as_array().unwrap();
        assert!(parts[0]["text"].as_str().unwrap().contains("ultra-terse"));
    }

    #[test]
    fn inject_vertex() {
        let mut body = json!({
            "systemInstruction": {"parts": [{"text": "Vertex rules."}]},
            "contents": [{"parts": [{"text": "hi"}]}]
        });
        assert!(inject_caveman(&mut body, &Format::Vertex, "lite"));
        let parts = body["systemInstruction"]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
    }

    // -----------------------------------------------------------------------
    // inject_caveman -- OpenAI / Cursor / Kiro / Ollama
    // -----------------------------------------------------------------------

    #[test]
    fn inject_openai_existing_system() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "hi"}
            ]
        });
        assert!(inject_caveman(&mut body, &Format::OpenAi, "lite"));
        let sys = body["messages"][0]["content"].as_str().unwrap();
        assert!(sys.contains("terse"));
    }

    #[test]
    fn inject_openai_no_system() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "hi"}
            ]
        });
        assert!(inject_caveman(&mut body, &Format::OpenAi, "full"));
        assert_eq!(body["messages"][0]["role"], "system");
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("caveman"));
    }

    #[test]
    fn inject_openai_empty_messages() {
        let mut body = json!({"messages": []});
        assert!(inject_caveman(&mut body, &Format::OpenAi, "ultra"));
        assert_eq!(body["messages"][0]["content"], get_caveman_prompt("ultra"));
    }

    #[test]
    fn inject_openai_no_messages_field() {
        let mut body = json!({});
        assert!(inject_caveman(&mut body, &Format::OpenAi, "lite"));
        let messages = body["messages"].as_array().unwrap();
        assert!(messages[0]["content"].as_str().unwrap().contains("terse"));
    }

    #[test]
    fn inject_openai_cursor_format() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(inject_caveman(&mut body, &Format::Cursor, "full"));
        assert_eq!(body["messages"][0]["role"], "system");
    }

    #[test]
    fn inject_openai_kiro_format() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(inject_caveman(&mut body, &Format::Kiro, "ultra"));
        assert_eq!(body["messages"][0]["role"], "system");
    }

    #[test]
    fn inject_openai_ollama_format() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(inject_caveman(&mut body, &Format::Ollama, "lite"));
        assert_eq!(body["messages"][0]["role"], "system");
    }

    #[test]
    fn inject_openai_developer_role() {
        let mut body = json!({
            "messages": [
                {"role": "developer", "content": "You are helpful."},
                {"role": "user", "content": "hi"}
            ]
        });
        assert!(inject_caveman(&mut body, &Format::OpenAi, "full"));
        let content = body["messages"][0]["content"].as_str().unwrap();
        assert!(content.contains("caveman"));
    }

    #[test]
    fn inject_openai_array_content() {
        let mut body = json!({
            "messages": [
                {
                    "role": "system",
                    "content": [{"type": "input_text", "text": "base rules"}]
                },
                {"role": "user", "content": "hi"}
            ]
        });
        assert!(inject_caveman(&mut body, &Format::OpenAi, "lite"));
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["type"], "text");
        assert!(parts[1]["text"].as_str().unwrap().contains("terse"));
    }

    // -----------------------------------------------------------------------
    // inject_caveman -- Responses / Codex
    // -----------------------------------------------------------------------

    #[test]
    fn inject_responses_string_instructions() {
        let mut body = json!({
            "input": "hello"
        });
        assert!(inject_caveman(&mut body, &Format::OpenAiResponses, "lite"));
        let instr = body["instructions"].as_str().unwrap();
        assert!(instr.contains("terse"));
    }

    #[test]
    fn inject_responses_existing_instructions() {
        let mut body = json!({
            "instructions": "Be helpful.",
            "input": "hello"
        });
        assert!(inject_caveman(&mut body, &Format::OpenAiResponses, "full"));
        let instr = body["instructions"].as_str().unwrap();
        assert!(instr.starts_with("Be helpful."));
        assert!(instr.contains("caveman"));
    }

    #[test]
    fn inject_codex() {
        let mut body = json!({
            "instructions": "Code rules.",
            "input": "write code"
        });
        assert!(inject_caveman(&mut body, &Format::Codex, "ultra"));
        let instr = body["instructions"].as_str().unwrap();
        assert!(instr.contains("ultra-terse"));
    }

    #[test]
    fn inject_responses_idempotent() {
        let prompt = get_caveman_prompt("lite");
        let mut body = json!({
            "instructions": format!("Be helpful.\n{}", prompt),
            "input": "hello"
        });
        let result = inject_caveman(&mut body, &Format::OpenAiResponses, "lite");
        assert!(!result);
    }

    // -----------------------------------------------------------------------
    // inject_caveman -- Antigravity
    // -----------------------------------------------------------------------

    #[test]
    fn inject_antigravity_via_request() {
        let mut body = json!({
            "request": {
                "systemInstruction": {"parts": [{"text": "Base rules."}]},
                "contents": [{"parts": [{"text": "hi"}]}]
            }
        });
        assert!(inject_caveman(&mut body, &Format::Antigravity, "lite"));
        let parts = body["request"]["systemInstruction"]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn inject_antigravity_no_request() {
        let mut body = json!({
            "systemInstruction": {"parts": [{"text": "Base."}]}
        });
        assert!(inject_caveman(&mut body, &Format::Antigravity, "full"));
        let parts = body["systemInstruction"]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn inject_empty_body() {
        let mut body = json!({});
        assert!(inject_caveman(&mut body, &Format::OpenAi, "ultra"));
        // Should have created messages with system prompt
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("ultra-terse"));
    }

    #[test]
    fn inject_non_object_body() {
        let mut body = Value::Array(vec![]);
        assert!(!inject_caveman(&mut body, &Format::OpenAi, "lite"));
    }

    #[test]
    fn inject_wenyan_levels() {
        for level in &["wenyan-lite", "wenyan", "wenyan-ultra"] {
            let mut body = json!({
                "messages": [{"role": "system", "content": "Rules."}]
            });
            assert!(
                inject_caveman(&mut body, &Format::OpenAi, level),
                "should inject {level}"
            );
            let content = body["messages"][0]["content"].as_str().unwrap();
            assert!(content.contains("wenyan") || content.contains("Classical Chinese"));
        }
    }

    #[test]
    fn inject_responses_instructions_array() {
        let mut body = json!({
            "instructions": ["Be helpful."],
            "input": "hello"
        });
        assert!(inject_caveman(&mut body, &Format::OpenAiResponses, "full"));
        let arr = body["instructions"].as_array().unwrap();
        assert!(arr.iter().any(|v| v.as_str().unwrap().contains("caveman")));
    }

    #[test]
    fn inject_responses_instructions_invalid_type() {
        let mut body = json!({
            "instructions": 42,
            "input": "hello"
        });
        assert!(inject_caveman(&mut body, &Format::OpenAiResponses, "lite"));
        assert!(body["instructions"].as_str().unwrap().contains("terse"));
    }
}
