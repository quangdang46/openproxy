use std::str::FromStr;

use serde_json::{json, Map, Value};

use crate::types::Settings;

const PROMPT_SEPARATOR: &str = "\n\n";
const CHARS_PER_TOKEN: usize = 4;
const MIN_TRIGGER_TOKENS: usize = 512;
const MAX_TRIGGER_TOKENS: usize = 2048;
const DEFAULT_CONTEXT_WINDOW: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionLevel {
    Lite,
    Full,
    Ultra,
}

impl CompressionLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
        }
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Self::Lite => concat!(
                "Respond tersely. Keep grammar and full sentences but drop filler, hedging and ",
                "pleasantries (just/really/basically/sure/of course/I'd be happy to). ",
                "Pattern: state thing, action, reason. Then next step. ",
                "Code blocks, file paths, commands, errors, URLs: keep exact. ",
                "Security warnings, irreversible action confirmations, multi-step ordered ",
                "sequences: write normal. Resume terse style after. ",
                "Active every response until user asks for normal mode."
            ),
            Self::Full => concat!(
                "Respond like terse caveman. All technical substance stay exact, only fluff die. ",
                "Drop: articles (a/an/the), filler (just/really/basically/actually/simply), ",
                "pleasantries, hedging. Fragments OK. Short synonyms (big not extensive, ",
                "fix not implement solution for). ",
                "Pattern: [thing] [action] [reason]. [next step]. ",
                "Code blocks, file paths, commands, errors, URLs: keep exact. ",
                "Security warnings, irreversible action confirmations, multi-step ordered ",
                "sequences: write normal. Resume terse style after. ",
                "Active every response until user asks for normal mode."
            ),
            Self::Ultra => concat!(
                "Respond ultra-terse. Maximum compression. Telegraphic. ",
                "Abbreviate (DB/auth/config/req/res/fn/impl), strip conjunctions, use arrows ",
                "for causality (X -> Y). One word when one word enough. ",
                "Pattern: [thing] -> [result]. [fix]. ",
                "Code blocks, file paths, commands, errors, URLs: keep exact. ",
                "Security warnings, irreversible action confirmations, multi-step ordered ",
                "sequences: write normal. Resume terse style after. ",
                "Active every response until user asks for normal mode."
            ),
        }
    }

    pub fn parse_or_default(value: &str) -> Self {
        value.parse().unwrap_or(Self::Full)
    }
}

impl FromStr for CompressionLevel {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "lite" => Ok(Self::Lite),
            "full" => Ok(Self::Full),
            "ultra" => Ok(Self::Ultra),
            _ => Err(()),
        }
    }
}

pub fn normalize_caveman_level(value: &str) -> &'static str {
    CompressionLevel::parse_or_default(value).as_str()
}

pub fn apply_request_preprocessing(body: &mut Value, settings: &Settings, model: &str) -> bool {
    // JS keeps RTK compression and Caveman prompting as separate toggles.
    // The RTK body-compression pass is tracked independently; this hook only
    // owns context-pressure-triggered Caveman injection.
    if !settings.caveman_enabled {
        return false;
    }

    if !should_auto_apply_caveman(body, model) {
        return false;
    }

    inject_caveman_prompt(
        body,
        CompressionLevel::parse_or_default(&settings.caveman_level),
    )
}

pub fn should_auto_apply_caveman(body: &Value, model: &str) -> bool {
    estimate_context_tokens(body) >= caveman_trigger_threshold(model)
}

pub fn inject_caveman_prompt(body: &mut Value, level: CompressionLevel) -> bool {
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

fn estimate_context_tokens(body: &Value) -> usize {
    estimate_context_chars(body).saturating_add(CHARS_PER_TOKEN - 1) / CHARS_PER_TOKEN
}

fn estimate_context_chars(body: &Value) -> usize {
    let Some(fields) = body.as_object() else {
        return 0;
    };

    let mut total = 0;
    total += fields
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::len)
        .unwrap_or_default();
    total += count_message_array(fields.get("messages"));
    total += count_message_array(fields.get("input"));
    total += count_claude_system(fields.get("system"));
    total += count_gemini_system(fields);
    total += count_gemini_contents(fields.get("contents"));

    if let Some(request) = fields.get("request").and_then(Value::as_object) {
        total += count_gemini_system(request);
        total += count_gemini_contents(request.get("contents"));
    }

    total
}

fn count_message_array(value: Option<&Value>) -> usize {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().map(count_message_item).sum())
        .unwrap_or_default()
}

fn count_message_item(item: &Value) -> usize {
    let Some(fields) = item.as_object() else {
        return 0;
    };

    count_text_node(fields.get("content"))
        + count_text_node(fields.get("output"))
        + fields
            .get("text")
            .and_then(Value::as_str)
            .map(str::len)
            .unwrap_or_default()
}

fn count_claude_system(value: Option<&Value>) -> usize {
    match value {
        Some(Value::String(system)) => system.len(),
        Some(Value::Array(blocks)) => blocks.iter().map(count_text_node_value).sum(),
        _ => 0,
    }
}

fn count_gemini_system(fields: &Map<String, Value>) -> usize {
    ["systemInstruction", "system_instruction"]
        .into_iter()
        .filter_map(|key| fields.get(key))
        .map(count_text_node_value)
        .sum()
}

fn count_gemini_contents(value: Option<&Value>) -> usize {
    value
        .and_then(Value::as_array)
        .map(|contents| contents.iter().map(count_text_node_value).sum())
        .unwrap_or_default()
}

fn count_text_node(value: Option<&Value>) -> usize {
    value.map(count_text_node_value).unwrap_or_default()
}

fn count_text_node_value(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        Value::Array(items) => items.iter().map(count_text_node_value).sum(),
        Value::Object(fields) => {
            fields
                .get("text")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or_default()
                + count_text_node(fields.get("content"))
                + count_text_node(fields.get("output"))
                + count_text_node(fields.get("parts"))
        }
        _ => 0,
    }
}

fn caveman_trigger_threshold(model: &str) -> usize {
    infer_context_window(model)
        .saturating_div(8)
        .clamp(MIN_TRIGGER_TOKENS, MAX_TRIGGER_TOKENS)
}

fn infer_context_window(model: &str) -> usize {
    let model = model.trim().to_ascii_lowercase();

    if ["claude", "sonnet", "opus", "haiku"]
        .iter()
        .any(|needle| model.contains(needle))
    {
        200_000
    } else if ["gemini-1.5", "gemini-2.0", "gemini-2.5"]
        .iter()
        .any(|needle| model.contains(needle))
    {
        1_000_000
    } else if ["gemini", "gpt-4.1", "gpt-4o", "o1", "o3", "o4"]
        .iter()
        .any(|needle| model.contains(needle))
    {
        128_000
    } else if ["deepseek", "qwen", "mistral", "llama"]
        .iter()
        .any(|needle| model.contains(needle))
    {
        64_000
    } else {
        DEFAULT_CONTEXT_WINDOW
    }
}

fn part_text(value: &Value) -> Option<&str> {
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.get("content").and_then(Value::as_str))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_request_preprocessing, inject_caveman_prompt, normalize_caveman_level,
        should_auto_apply_caveman, CompressionLevel,
    };
    use crate::types::Settings;
    use serde_json::json;

    #[test]
    fn compression_level_parses_case_insensitively() {
        assert_eq!("lite".parse(), Ok(CompressionLevel::Lite));
        assert_eq!(" FULL ".parse(), Ok(CompressionLevel::Full));
        assert_eq!("Ultra".parse(), Ok(CompressionLevel::Ultra));
        assert!("unknown".parse::<CompressionLevel>().is_err());
        assert_eq!(
            CompressionLevel::parse_or_default("unknown"),
            CompressionLevel::Full
        );
        assert_eq!(normalize_caveman_level(" ULTRA "), "ultra");
        assert_eq!(normalize_caveman_level("bad-value"), "full");
    }

    #[test]
    fn inject_caveman_appends_to_existing_system_message() {
        let mut body = json!({
            "messages": [
                { "role": "system", "content": "Existing rules" },
                { "role": "user", "content": "Hi" }
            ]
        });

        assert!(inject_caveman_prompt(&mut body, CompressionLevel::Full));
        let content = body["messages"][0]["content"]
            .as_str()
            .expect("system content");
        assert!(content.starts_with("Existing rules"));
        assert!(content.contains(CompressionLevel::Full.prompt()));
    }

    #[test]
    fn inject_caveman_inserts_new_system_message_when_missing() {
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "Hi" }
            ]
        });

        assert!(inject_caveman_prompt(&mut body, CompressionLevel::Lite));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][0]["content"],
            CompressionLevel::Lite.prompt()
        );
    }

    #[test]
    fn inject_caveman_appends_to_instructions_and_part_arrays() {
        let mut instructions = json!({
            "instructions": "Be accurate"
        });
        assert!(inject_caveman_prompt(
            &mut instructions,
            CompressionLevel::Ultra
        ));
        assert!(instructions["instructions"]
            .as_str()
            .expect("instructions")
            .contains(CompressionLevel::Ultra.prompt()));

        let mut array_content = json!({
            "messages": [
                {
                    "role": "developer",
                    "content": [{ "type": "input_text", "text": "Keep format" }]
                }
            ]
        });
        assert!(inject_caveman_prompt(
            &mut array_content,
            CompressionLevel::Lite
        ));
        let parts = array_content["messages"][0]["content"]
            .as_array()
            .expect("parts");
        assert_eq!(
            parts.last().expect("last part"),
            &json!({ "type": "text", "text": CompressionLevel::Lite.prompt() })
        );
    }

    #[test]
    fn inject_caveman_uses_responses_part_type_for_input_arrays() {
        let mut body = json!({
            "input": [
                {
                    "role": "developer",
                    "content": [{ "type": "input_text", "text": "Keep format" }]
                }
            ]
        });

        assert!(inject_caveman_prompt(&mut body, CompressionLevel::Lite));
        let parts = body["input"][0]["content"].as_array().expect("parts");
        assert_eq!(
            parts.last().expect("last part"),
            &json!({ "type": "input_text", "text": CompressionLevel::Lite.prompt() })
        );
    }

    #[test]
    fn inject_caveman_inserts_before_claude_cache_control_block() {
        let mut body = json!({
            "system": [
                { "type": "text", "text": "Existing prefix" },
                { "type": "text", "text": "Cache me", "cache_control": { "type": "ephemeral" } }
            ],
            "messages": [{ "role": "user", "content": "Hi" }]
        });

        assert!(inject_caveman_prompt(&mut body, CompressionLevel::Full));
        let system = body["system"].as_array().expect("system array");
        assert_eq!(
            system[1],
            json!({ "type": "text", "text": CompressionLevel::Full.prompt() })
        );
        assert!(system[2].get("cache_control").is_some());
    }

    #[test]
    fn inject_caveman_updates_nested_gemini_request_shape() {
        let mut body = json!({
            "request": {
                "contents": [{ "parts": [{ "text": "Hello" }] }],
                "systemInstruction": { "parts": [{ "text": "Existing guidance" }] }
            }
        });

        assert!(inject_caveman_prompt(&mut body, CompressionLevel::Ultra));
        let parts = body["request"]["systemInstruction"]["parts"]
            .as_array()
            .expect("gemini parts");
        assert_eq!(
            parts.last().expect("last gemini part"),
            &json!({ "text": CompressionLevel::Ultra.prompt() })
        );
    }

    #[test]
    fn inject_caveman_preserves_string_gemini_instruction() {
        let mut body = json!({
            "systemInstruction": "Existing guidance",
            "contents": [{ "parts": [{ "text": "Hello" }] }]
        });

        assert!(inject_caveman_prompt(&mut body, CompressionLevel::Full));
        let parts = body["systemInstruction"]["parts"]
            .as_array()
            .expect("gemini parts");
        assert_eq!(parts[0], json!({ "text": "Existing guidance" }));
        assert_eq!(
            parts.last().expect("last gemini part"),
            &json!({ "text": CompressionLevel::Full.prompt() })
        );
    }

    #[test]
    fn inject_caveman_is_idempotent_for_existing_prompt() {
        let prompt = CompressionLevel::Lite.prompt();
        let mut body = json!({
            "messages": [
                { "role": "system", "content": prompt },
                { "role": "user", "content": "Need update" }
            ]
        });

        assert!(!inject_caveman_prompt(&mut body, CompressionLevel::Lite));
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn apply_request_preprocessing_skips_short_requests() {
        let settings = Settings {
            caveman_enabled: true,
            caveman_level: "ultra".into(),
            ..Settings::default()
        };
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "short prompt" }
            ]
        });

        assert!(!should_auto_apply_caveman(&body, "gpt-4o-mini"));
        assert!(!apply_request_preprocessing(
            &mut body,
            &settings,
            "gpt-4o-mini"
        ));
        assert_eq!(body["messages"].as_array().expect("messages").len(), 1);
    }

    #[test]
    fn apply_request_preprocessing_injects_for_long_requests() {
        let settings = Settings {
            caveman_enabled: true,
            caveman_level: "full".into(),
            ..Settings::default()
        };
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "x".repeat(9000) }
            ]
        });

        assert!(should_auto_apply_caveman(&body, "gpt-4o-mini"));
        assert!(apply_request_preprocessing(
            &mut body,
            &settings,
            "gpt-4o-mini"
        ));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][0]["content"],
            CompressionLevel::Full.prompt()
        );
    }

    #[test]
    fn apply_request_preprocessing_keeps_caveman_independent_from_rtk_toggle() {
        let settings = Settings {
            rtk_enabled: false,
            caveman_enabled: true,
            caveman_level: "lite".into(),
            ..Settings::default()
        };
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "x".repeat(9000) }
            ]
        });

        assert!(apply_request_preprocessing(
            &mut body,
            &settings,
            "gpt-4o-mini"
        ));
        assert_eq!(
            body["messages"][0]["content"],
            CompressionLevel::Lite.prompt()
        );
    }

    #[test]
    fn should_auto_apply_caveman_short_content_stays_short() {
        let body = json!({
            "messages": [
                { "role": "user", "content": "Hello world" }
            ]
        });
        assert!(!should_auto_apply_caveman(&body, "gpt-4o-mini"));
    }

    #[test]
    fn should_auto_apply_caveman_long_content_triggers() {
        let body = json!({
            "messages": [
                { "role": "user", "content": "x".repeat(16000) }
            ]
        });
        assert!(should_auto_apply_caveman(&body, "gpt-4o-mini"));
    }

    #[test]
    fn compression_level_all_three_levels_have_different_prompts() {
        let lite = CompressionLevel::Lite.prompt();
        let full = CompressionLevel::Full.prompt();
        let ultra = CompressionLevel::Ultra.prompt();
        assert!(!lite.is_empty());
        assert!(!full.is_empty());
        assert!(!ultra.is_empty());
        assert_ne!(lite, full);
        assert_ne!(full, ultra);
    }

    #[test]
    fn compression_level_as_str_returns_correct_strings() {
        assert_eq!(CompressionLevel::Lite.as_str(), "lite");
        assert_eq!(CompressionLevel::Full.as_str(), "full");
        assert_eq!(CompressionLevel::Ultra.as_str(), "ultra");
    }
}
