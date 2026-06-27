//! Ponytail prompt injection -- appends a "lazy senior dev" system prompt
//! to bias the model toward minimal, YAGNI-driven code output.
//!
//! Ported from 9router's `open-sse/rtk/ponytail.js` and
//! `open-sse/rtk/ponytailPrompt.js`. The injection targets the
//! format-specific system-instruction field, dispatching on body shape
//! (Claude / Gemini / OpenAI) the same way `inject_caveman_prompt` does
//! in `crate::core::rtk`.

use serde_json::{json, Map, Value};

const PROMPT_SEPARATOR: &str = "\n\n";

// ---------------------------------------------------------------------------
// Shared prompt fragments (verbatim from ponytailPrompt.js)
// ---------------------------------------------------------------------------

const SHARED_PERSONA: &str = "You are a lazy senior developer. Lazy means efficient, not careless. The best code is the code never written.";

const SHARED_LADDER: &str = "Before writing code, stop at the first rung that holds: 1) Does this need to exist at all? (YAGNI) 2) Stdlib does it? Use it. 3) Native platform feature covers it? Use it (CSS over JS, DB constraint over app code). 4) Already-installed dependency solves it? Use it; never add a new one for what a few lines can do. 5) Can it be one line? One line. 6) Only then: the minimum code that works.";

const SHARED_RULES: &str = "No unrequested abstractions (no interface with one implementation, no factory for one product, no config for a value that never changes). No boilerplate or scaffolding \"for later\". Deletion over addition. Boring over clever. Fewest files possible; shortest working diff wins. Two stdlib options the same size: take the edge-case-correct one. Mark deliberate simplifications with a `ponytail:` comment naming the ceiling and upgrade path.";

const SHARED_OUTPUT: &str = "Code first. Then at most three short lines: what was skipped, when to add it. No essays or design notes. Pattern: `[code] \u{2192} skipped: [X], add when [Y].`";

const SHARED_NOT_LAZY: &str = "Never simplify away: input validation at trust boundaries, error handling that prevents data loss, security, accessibility, anything explicitly requested. Non-trivial logic leaves ONE runnable check behind (an assert-based self-check or one small test file; no frameworks). Trivial one-liners need no test.";

const SHARED_PERSISTENCE: &str =
    "ACTIVE EVERY RESPONSE. No drift back to over-building. Still active if unsure.";

// ---------------------------------------------------------------------------
// PonytailLevel
// ---------------------------------------------------------------------------

/// The three Ponytail intensity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PonytailLevel {
    Lite,
    Full,
    Ultra,
}

impl PonytailLevel {
    /// Canonical lowercase name used in configuration and API fields.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
        }
    }

    /// The full prompt text for this level, assembled from the shared
    /// fragments with a single space between each part (matching the JS
    /// `Array.join(" ")` behaviour).
    pub fn prompt(self) -> &'static str {
        match self {
            Self::Lite => concat!(
                "You are a lazy senior developer. Lazy means efficient, not careless. The best code is the code never written.",
                " ",
                "Lite: build what's asked, but name the lazier alternative in one line. User picks.",
                " ",
                "Before writing code, stop at the first rung that holds: 1) Does this need to exist at all? (YAGNI) 2) Stdlib does it? Use it. 3) Native platform feature covers it? Use it (CSS over JS, DB constraint over app code). 4) Already-installed dependency solves it? Use it; never add a new one for what a few lines can do. 5) Can it be one line? One line. 6) Only then: the minimum code that works.",
                " ",
                "No unrequested abstractions (no interface with one implementation, no factory for one product, no config for a value that never changes). No boilerplate or scaffolding \"for later\". Deletion over addition. Boring over clever. Fewest files possible; shortest working diff wins. Two stdlib options the same size: take the edge-case-correct one. Mark deliberate simplifications with a `ponytail:` comment naming the ceiling and upgrade path.",
                " ",
                "Code first. Then at most three short lines: what was skipped, when to add it. No essays or design notes. Pattern: `[code] \u{2192} skipped: [X], add when [Y].`",
                " ",
                "Never simplify away: input validation at trust boundaries, error handling that prevents data loss, security, accessibility, anything explicitly requested. Non-trivial logic leaves ONE runnable check behind (an assert-based self-check or one small test file; no frameworks). Trivial one-liners need no test.",
                " ",
                "ACTIVE EVERY RESPONSE. No drift back to over-building. Still active if unsure."
            ),
            Self::Full => concat!(
                "You are a lazy senior developer. Lazy means efficient, not careless. The best code is the code never written.",
                " ",
                "Full: the ladder enforced. Stdlib and native first. Shortest diff, shortest explanation.",
                " ",
                "Before writing code, stop at the first rung that holds: 1) Does this need to exist at all? (YAGNI) 2) Stdlib does it? Use it. 3) Native platform feature covers it? Use it (CSS over JS, DB constraint over app code). 4) Already-installed dependency solves it? Use it; never add a new one for what a few lines can do. 5) Can it be one line? One line. 6) Only then: the minimum code that works.",
                " ",
                "No unrequested abstractions (no interface with one implementation, no factory for one product, no config for a value that never changes). No boilerplate or scaffolding \"for later\". Deletion over addition. Boring over clever. Fewest files possible; shortest working diff wins. Two stdlib options the same size: take the edge-case-correct one. Mark deliberate simplifications with a `ponytail:` comment naming the ceiling and upgrade path.",
                " ",
                "Code first. Then at most three short lines: what was skipped, when to add it. No essays or design notes. Pattern: `[code] \u{2192} skipped: [X], add when [Y].`",
                " ",
                "Never simplify away: input validation at trust boundaries, error handling that prevents data loss, security, accessibility, anything explicitly requested. Non-trivial logic leaves ONE runnable check behind (an assert-based self-check or one small test file; no frameworks). Trivial one-liners need no test.",
                " ",
                "ACTIVE EVERY RESPONSE. No drift back to over-building. Still active if unsure."
            ),
            Self::Ultra => concat!(
                "You are a lazy senior developer. Lazy means efficient, not careless. The best code is the code never written.",
                " ",
                "Ultra: YAGNI extremist. Deletion before addition. Ship the one-liner and challenge the rest of the requirement in the same response.",
                " ",
                "Before writing code, stop at the first rung that holds: 1) Does this need to exist at all? (YAGNI) 2) Stdlib does it? Use it. 3) Native platform feature covers it? Use it (CSS over JS, DB constraint over app code). 4) Already-installed dependency solves it? Use it; never add a new one for what a few lines can do. 5) Can it be one line? One line. 6) Only then: the minimum code that works.",
                " ",
                "No unrequested abstractions (no interface with one implementation, no factory for one product, no config for a value that never changes). No boilerplate or scaffolding \"for later\". Deletion over addition. Boring over clever. Fewest files possible; shortest working diff wins. Two stdlib options the same size: take the edge-case-correct one. Mark deliberate simplifications with a `ponytail:` comment naming the ceiling and upgrade path.",
                " ",
                "Code first. Then at most three short lines: what was skipped, when to add it. No essays or design notes. Pattern: `[code] \u{2192} skipped: [X], add when [Y].`",
                " ",
                "Never simplify away: input validation at trust boundaries, error handling that prevents data loss, security, accessibility, anything explicitly requested. Non-trivial logic leaves ONE runnable check behind (an assert-based self-check or one small test file; no frameworks). Trivial one-liners need no test.",
                " ",
                "ACTIVE EVERY RESPONSE. No drift back to over-building. Still active if unsure."
            ),
        }
    }

    /// Parse a level string (case-insensitive, trimmed), defaulting to
    /// `Full` on unrecognised input.
    pub fn parse_or_default(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "lite" => Self::Lite,
            "full" => Self::Full,
            "ultra" => Self::Ultra,
            _ => Self::Full,
        }
    }
}

// ---------------------------------------------------------------------------
// Public injection entry point
// ---------------------------------------------------------------------------

/// Inject a Ponytail prompt into the request body based on its shape.
///
/// Dispatches identically to [`crate::core::rtk::inject_caveman_prompt`]:
/// - If the body has a `"system"` key → Claude injection.
/// - If the body looks like a Gemini request → Gemini injection.
/// - Otherwise → OpenAI / Responses-API injection.
///
/// Returns `true` if a modification was made, `false` if the prompt was
/// already present (idempotent) or the body shape was unrecognised.
pub fn inject_ponytail_prompt(body: &mut Value, level: PonytailLevel) -> bool {
    let prompt = level.prompt();
    let Some(fields) = body.as_object_mut() else {
        return false;
    };

    if fields.contains_key("system") {
        return inject_claude_system(fields, prompt);
    }

    if is_gemini_shape(fields) {
        return inject_gemini_system(fields, prompt);
    }

    inject_openai_shape(fields, prompt)
}

// ---------------------------------------------------------------------------
// Format-specific injection helpers (identical to rtk/mod.rs)
// ---------------------------------------------------------------------------

fn inject_openai_shape(fields: &mut Map<String, Value>, prompt: &str) -> bool {
    if let Some(Value::String(instructions)) = fields.get_mut("instructions") {
        return append_prompt_text(instructions, prompt);
    }

    if let Some(messages) = fields.get_mut("messages").and_then(Value::as_array_mut) {
        return inject_openai_message_prompt(messages, prompt, "text");
    }

    if let Some(input) = fields.get_mut("input").and_then(Value::as_array_mut) {
        return inject_openai_message_prompt(input, prompt, "input_text");
    }

    false
}

fn inject_openai_message_prompt(messages: &mut Vec<Value>, prompt: &str, part_type: &str) -> bool {
    if let Some(existing) = messages.iter_mut().find(|message| {
        matches!(
            message.get("role").and_then(Value::as_str),
            Some("system" | "developer")
        )
    }) {
        append_to_openai_message(existing, prompt, part_type)
    } else {
        messages.insert(0, json!({ "role": "system", "content": prompt }));
        true
    }
}

fn append_to_openai_message(message: &mut Value, prompt: &str, part_type: &str) -> bool {
    let Some(fields) = message.as_object_mut() else {
        return false;
    };

    match fields.get_mut("content") {
        Some(Value::String(content)) => append_prompt_text(content, prompt),
        Some(Value::Array(parts)) => {
            if parts
                .iter()
                .any(|part| part_text(part).is_some_and(|text| text == prompt))
            {
                return false;
            }
            parts.push(json!({ "type": part_type, "text": prompt }));
            true
        }
        _ => {
            fields.insert("content".into(), Value::String(prompt.into()));
            true
        }
    }
}

fn inject_claude_system(fields: &mut Map<String, Value>, prompt: &str) -> bool {
    match fields.get_mut("system") {
        Some(Value::String(system)) => append_prompt_text(system, prompt),
        Some(Value::Array(blocks)) => inject_claude_system_blocks(blocks, prompt),
        Some(_) => {
            fields.insert("system".into(), Value::String(prompt.into()));
            true
        }
        None => {
            fields.insert("system".into(), Value::String(prompt.into()));
            true
        }
    }
}

fn inject_claude_system_blocks(blocks: &mut Vec<Value>, prompt: &str) -> bool {
    if blocks
        .iter()
        .any(|block| part_text(block).is_some_and(|text| text.contains(prompt)))
    {
        return false;
    }

    let insert_at = blocks
        .iter()
        .rposition(|block| {
            block
                .as_object()
                .is_some_and(|fields| fields.contains_key("cache_control"))
        })
        .unwrap_or(blocks.len());
    blocks.insert(insert_at, json!({ "type": "text", "text": prompt }));
    true
}

fn inject_gemini_system(fields: &mut Map<String, Value>, prompt: &str) -> bool {
    if let Some(request) = fields.get_mut("request").and_then(Value::as_object_mut) {
        return inject_gemini_system_object(request, prompt);
    }

    inject_gemini_system_object(fields, prompt)
}

fn inject_gemini_system_object(target: &mut Map<String, Value>, prompt: &str) -> bool {
    let key = if target.contains_key("system_instruction") {
        "system_instruction"
    } else {
        "systemInstruction"
    };

    match target.remove(key) {
        Some(Value::Object(mut system_instruction)) => {
            let mutated = append_gemini_prompt(&mut system_instruction, prompt);
            target.insert(key.into(), Value::Object(system_instruction));
            mutated
        }
        Some(existing) => {
            let mut parts = gemini_parts_from_value(existing);
            if parts
                .iter()
                .any(|part| part_text(part).is_some_and(|text| text == prompt))
            {
                target.insert(key.into(), json!({ "parts": parts }));
                return false;
            }

            parts.push(json!({ "text": prompt }));
            target.insert(key.into(), json!({ "parts": parts }));
            true
        }
        None => {
            target.insert(key.into(), json!({ "parts": [{ "text": prompt }] }));
            true
        }
    }
}

fn append_gemini_prompt(system_instruction: &mut Map<String, Value>, prompt: &str) -> bool {
    match system_instruction.get_mut("parts") {
        Some(Value::Array(parts)) => {
            if parts
                .iter()
                .any(|part| part_text(part).is_some_and(|text| text == prompt))
            {
                return false;
            }
            parts.push(json!({ "text": prompt }));
            true
        }
        _ => {
            system_instruction.insert("parts".into(), json!([{ "text": prompt }]));
            true
        }
    }
}

fn gemini_parts_from_value(value: Value) -> Vec<Value> {
    match value {
        Value::String(text) if !text.trim().is_empty() => vec![json!({ "text": text })],
        Value::Array(parts) => parts,
        Value::Object(fields) => fields
            .get("parts")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| {
                fields
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| vec![json!({ "text": text })])
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn is_gemini_shape(fields: &Map<String, Value>) -> bool {
    fields.contains_key("systemInstruction")
        || fields.contains_key("system_instruction")
        || fields.contains_key("contents")
        || fields
            .get("request")
            .and_then(Value::as_object)
            .is_some_and(|request| {
                request.contains_key("systemInstruction")
                    || request.contains_key("system_instruction")
                    || request.contains_key("contents")
            })
}

fn append_prompt_text(content: &mut String, prompt: &str) -> bool {
    if content.contains(prompt) {
        return false;
    }

    if content.trim().is_empty() {
        *content = prompt.into();
        return true;
    }

    content.push_str(PROMPT_SEPARATOR);
    content.push_str(prompt);
    true
}

fn part_text(value: &Value) -> Option<&str> {
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.get("content").and_then(Value::as_str))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // PonytailLevel basics
    // -----------------------------------------------------------------------

    #[test]
    fn ponytail_three_levels_have_different_prompts() {
        let lite = PonytailLevel::Lite.prompt();
        let full = PonytailLevel::Full.prompt();
        let ultra = PonytailLevel::Ultra.prompt();

        assert!(!lite.is_empty());
        assert!(!full.is_empty());
        assert!(!ultra.is_empty());
        assert_ne!(lite, full);
        assert_ne!(full, ultra);
        assert_ne!(lite, ultra);
    }

    #[test]
    fn ponytail_level_as_str() {
        assert_eq!(PonytailLevel::Lite.as_str(), "lite");
        assert_eq!(PonytailLevel::Full.as_str(), "full");
        assert_eq!(PonytailLevel::Ultra.as_str(), "ultra");
    }

    #[test]
    fn ponytail_level_parse_or_default() {
        assert_eq!(PonytailLevel::parse_or_default("lite"), PonytailLevel::Lite);
        assert_eq!(PonytailLevel::parse_or_default("FULL"), PonytailLevel::Full);
        assert_eq!(
            PonytailLevel::parse_or_default(" Ultra "),
            PonytailLevel::Ultra
        );
        assert_eq!(
            PonytailLevel::parse_or_default("unknown"),
            PonytailLevel::Full
        );
    }

    #[test]
    fn ponytail_prompts_contain_shared_fragments() {
        for level in [
            PonytailLevel::Lite,
            PonytailLevel::Full,
            PonytailLevel::Ultra,
        ] {
            let prompt = level.prompt();
            assert!(
                prompt.contains("lazy senior developer"),
                "{:?} missing SHARED_PERSONA",
                level
            );
            assert!(
                prompt.contains("first rung that holds"),
                "{:?} missing SHARED_LADDER",
                level
            );
            assert!(
                prompt.contains("No unrequested abstractions"),
                "{:?} missing SHARED_RULES",
                level
            );
            assert!(
                prompt.contains("Code first"),
                "{:?} missing SHARED_OUTPUT",
                level
            );
            assert!(
                prompt.contains("input validation at trust boundaries"),
                "{:?} missing SHARED_NOT_LAZY",
                level
            );
            assert!(
                prompt.contains("ACTIVE EVERY RESPONSE"),
                "{:?} missing SHARED_PERSISTENCE",
                level
            );
        }
    }

    #[test]
    fn ponytail_prompts_contain_level_specific_tagline() {
        assert!(PonytailLevel::Lite
            .prompt()
            .contains("Lite: build what's asked"));
        assert!(PonytailLevel::Full
            .prompt()
            .contains("Full: the ladder enforced"));
        assert!(PonytailLevel::Ultra
            .prompt()
            .contains("Ultra: YAGNI extremist"));
    }

    // -----------------------------------------------------------------------
    // inject_ponytail_prompt -- OpenAI shape
    // -----------------------------------------------------------------------

    #[test]
    fn ponytail_appends_to_system_message() {
        let mut body = json!({
            "messages": [
                { "role": "system", "content": "Existing rules" },
                { "role": "user", "content": "Hi" }
            ]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Full));
        let content = body["messages"][0]["content"]
            .as_str()
            .expect("system content");
        assert!(content.starts_with("Existing rules"));
        assert!(content.contains(PonytailLevel::Full.prompt()));
    }

    #[test]
    fn ponytail_inserts_new_system_message() {
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "Hi" }
            ]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], PonytailLevel::Lite.prompt());
    }

    #[test]
    fn ponytail_appends_to_instructions() {
        let mut body = json!({
            "instructions": "Be accurate"
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Ultra));
        let instr = body["instructions"].as_str().expect("instructions string");
        assert!(instr.starts_with("Be accurate"));
        assert!(instr.contains(PonytailLevel::Ultra.prompt()));
    }

    #[test]
    fn ponytail_is_idempotent() {
        let prompt = PonytailLevel::Lite.prompt();
        let mut body = json!({
            "messages": [
                { "role": "system", "content": prompt },
                { "role": "user", "content": "Hi" }
            ]
        });

        assert!(!inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn ponytail_uses_responses_part_type_for_input_arrays() {
        let mut body = json!({
            "input": [
                {
                    "role": "developer",
                    "content": [{ "type": "input_text", "text": "Keep format" }]
                }
            ]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
        let parts = body["input"][0]["content"].as_array().expect("parts");
        assert_eq!(
            parts.last().expect("last part"),
            &json!({ "type": "input_text", "text": PonytailLevel::Lite.prompt() })
        );
    }

    #[test]
    fn ponytail_openai_developer_role() {
        let mut body = json!({
            "messages": [
                { "role": "developer", "content": "You are helpful." },
                { "role": "user", "content": "hi" }
            ]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Full));
        let content = body["messages"][0]["content"].as_str().expect("content");
        assert!(content.contains(PonytailLevel::Full.prompt()));
    }

    #[test]
    fn ponytail_openai_array_content() {
        let mut body = json!({
            "messages": [
                {
                    "role": "system",
                    "content": [{ "type": "input_text", "text": "base rules" }]
                },
                { "role": "user", "content": "hi" }
            ]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
        let parts = body["messages"][0]["content"].as_array().expect("parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["type"], "text");
        assert!(parts[1]["text"]
            .as_str()
            .expect("text")
            .contains(PonytailLevel::Lite.prompt()));
    }

    // -----------------------------------------------------------------------
    // inject_ponytail_prompt -- Claude shape
    // -----------------------------------------------------------------------

    #[test]
    fn ponytail_claude_string_system() {
        let mut body = json!({
            "system": "You are Claude.",
            "messages": [{ "role": "user", "content": "hi" }]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
        let sys = body["system"].as_str().expect("system string");
        assert!(sys.starts_with("You are Claude."));
        assert!(sys.contains(PonytailLevel::Lite.prompt()));
    }

    #[test]
    fn ponytail_claude_system_array() {
        let mut body = json!({
            "system": [
                { "type": "text", "text": "Existing prefix" },
                { "type": "text", "text": "Cache me", "cache_control": { "type": "ephemeral" } }
            ],
            "messages": [{ "role": "user", "content": "Hi" }]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Full));
        let system = body["system"].as_array().expect("system array");
        // Inserted before the cache_control block
        assert_eq!(
            system[1],
            json!({ "type": "text", "text": PonytailLevel::Full.prompt() })
        );
        assert!(system[2].get("cache_control").is_some());
    }

    #[test]
    fn ponytail_claude_no_system() {
        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });

        // No "system" key and not gemini shape → falls through to OpenAI path
        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Ultra));
        assert_eq!(body["messages"][0]["role"], "system");
    }

    #[test]
    fn ponytail_claude_idempotent_string() {
        let prompt = PonytailLevel::Full.prompt();
        let mut body = json!({
            "system": format!("Existing\n\n{}", prompt),
            "messages": [{ "role": "user", "content": "hi" }]
        });

        assert!(!inject_ponytail_prompt(&mut body, PonytailLevel::Full));
    }

    #[test]
    fn ponytail_claude_idempotent_array() {
        let prompt = PonytailLevel::Lite.prompt();
        let mut body = json!({
            "system": [
                { "type": "text", "text": prompt }
            ],
            "messages": [{ "role": "user", "content": "hi" }]
        });

        assert!(!inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
    }

    // -----------------------------------------------------------------------
    // inject_ponytail_prompt -- Gemini shape
    // -----------------------------------------------------------------------

    #[test]
    fn ponytail_gemini_shape() {
        let mut body = json!({
            "systemInstruction": { "parts": [{ "text": "Existing guidance" }] },
            "contents": [{ "parts": [{ "text": "Hello" }] }]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Ultra));
        let parts = body["systemInstruction"]["parts"]
            .as_array()
            .expect("gemini parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts.last().expect("last gemini part"),
            &json!({ "text": PonytailLevel::Ultra.prompt() })
        );
    }

    #[test]
    fn ponytail_gemini_string_system_instruction() {
        let mut body = json!({
            "systemInstruction": "Existing guidance",
            "contents": [{ "parts": [{ "text": "Hello" }] }]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Full));
        let parts = body["systemInstruction"]["parts"]
            .as_array()
            .expect("gemini parts");
        assert_eq!(parts[0], json!({ "text": "Existing guidance" }));
        assert_eq!(
            parts.last().expect("last gemini part"),
            &json!({ "text": PonytailLevel::Full.prompt() })
        );
    }

    #[test]
    fn ponytail_gemini_no_system_instruction() {
        let mut body = json!({
            "contents": [{ "parts": [{ "text": "Hello" }] }]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
        let parts = body["systemInstruction"]["parts"]
            .as_array()
            .expect("gemini parts");
        assert_eq!(parts[0], json!({ "text": PonytailLevel::Lite.prompt() }));
    }

    #[test]
    fn ponytail_gemini_nested_request() {
        let mut body = json!({
            "request": {
                "contents": [{ "parts": [{ "text": "Hello" }] }],
                "systemInstruction": { "parts": [{ "text": "Existing guidance" }] }
            }
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Ultra));
        let parts = body["request"]["systemInstruction"]["parts"]
            .as_array()
            .expect("gemini parts");
        assert_eq!(
            parts.last().expect("last gemini part"),
            &json!({ "text": PonytailLevel::Ultra.prompt() })
        );
    }

    #[test]
    fn ponytail_gemini_snake_case() {
        let mut body = json!({
            "system_instruction": { "parts": [{ "text": "Existing" }] },
            "contents": [{ "parts": [{ "text": "hi" }] }]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Full));
        let parts = body["system_instruction"]["parts"]
            .as_array()
            .expect("gemini parts");
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn ponytail_gemini_idempotent() {
        let prompt = PonytailLevel::Ultra.prompt();
        let mut body = json!({
            "systemInstruction": { "parts": [{ "text": prompt }] },
            "contents": [{ "parts": [{ "text": "hi" }] }]
        });

        assert!(!inject_ponytail_prompt(&mut body, PonytailLevel::Ultra));
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn ponytail_non_object_body() {
        let mut body = Value::Array(vec![]);
        assert!(!inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
    }

    #[test]
    fn ponytail_empty_object_body() {
        let mut body = json!({});
        // No system, no gemini shape, no messages/instructions/input → false
        assert!(!inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
    }

    #[test]
    fn ponytail_responses_instructions_array() {
        let mut body = json!({
            "instructions": ["Be helpful."],
            "input": "hello"
        });
        // instructions is an array — not a String, so the openai shape
        // falls through to messages (absent) then input (absent as array).
        // The instructions array is not handled by inject_openai_shape
        // (it only handles String instructions), so this returns false.
        // This matches the rtk/mod.rs behaviour exactly.
        let result = inject_ponytail_prompt(&mut body, PonytailLevel::Full);
        // The body has "instructions" (array) and "input" (string, not array).
        // inject_openai_shape checks: instructions String? No (array).
        // messages array? No. input array? No (string). → false.
        assert!(!result);
    }

    #[test]
    fn ponytail_empty_system_string_gets_replaced() {
        let mut body = json!({
            "system": "",
            "messages": [{ "role": "user", "content": "hi" }]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Lite));
        let sys = body["system"].as_str().expect("system string");
        assert_eq!(sys, PonytailLevel::Lite.prompt());
    }

    #[test]
    fn ponytail_whitespace_only_system_gets_replaced() {
        let mut body = json!({
            "system": "   ",
            "messages": [{ "role": "user", "content": "hi" }]
        });

        assert!(inject_ponytail_prompt(&mut body, PonytailLevel::Full));
        let sys = body["system"].as_str().expect("system string");
        assert_eq!(sys, PonytailLevel::Full.prompt());
    }
}
