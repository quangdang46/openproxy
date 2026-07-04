use std::str::FromStr;

use serde_json::{json, Map, Value};

use crate::core::rtk::system_inject::inject_system_prompt;
use crate::core::translator::ponytail::{inject_ponytail_prompt, PonytailLevel};
use crate::types::Settings;

pub mod apply_filter;
pub mod autodetect;
pub mod constants;
pub mod filters;
pub mod headroom;
pub mod smartcrusher;
pub mod system_inject;

use apply_filter::{safe_apply, RtkHit, RtkStats};
use autodetect::{auto_detect_filter, FilterFn};
use constants::*;

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
    WenyanLite,
    Wenyan,
    WenyanUltra,
}

impl CompressionLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
            Self::WenyanLite => "wenyan-lite",
            Self::Wenyan => "wenyan",
            Self::WenyanUltra => "wenyan-ultra",
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
            Self::WenyanLite => concat!(
                "Respond semi-classical Chinese. Use concise wenyan phrasing where natural, ",
                "but fall back to modern Chinese for complex technical terms. ",
                "Keep technical substance exact."
            ),
            Self::Wenyan => concat!(
                "Respond in Classical Chinese (wenyan). Use classical grammar and vocabulary. ",
                "Keep technical terms, code, and file paths in original form."
            ),
            Self::WenyanUltra => concat!(
                "Respond in ultra-terse Classical Chinese (wenyan). Maximum compression. ",
                "Abbreviate. Use classical idioms. Technical terms stay exact."
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
            "wenyan-lite" | "wenyan_lite" => Ok(Self::WenyanLite),
            "wenyan" => Ok(Self::Wenyan),
            "wenyan-ultra" | "wenyan_ultra" => Ok(Self::WenyanUltra),
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
    let mut modified = false;
    if settings.caveman_enabled {
        // 9router parity: always inject when enabled (no context-pressure guard).
        modified |= inject_caveman_prompt(
            body,
            CompressionLevel::parse_or_default(&settings.caveman_level),
        );
    }
    if settings.ponytail_enabled {
        // Ponytail has NO context-pressure auto-trigger — always applies if enabled.
        modified |= inject_ponytail_prompt(
            body,
            crate::core::translator::ponytail::PonytailLevel::parse_or_default(
                &settings.ponytail_level,
            ),
        );
    }
    // System prompt injection at RTK layer.
    // Reads `systemInject` (bool) and `systemPrompt` (string) from the settings
    // `extra` map — these are unstructured/extra config keys that live alongside
    // the known fields.
    modified |= apply_rtk_system_injection(body, settings);
    modified
}

/// Apply system prompt injection from settings extras if enabled.
fn apply_rtk_system_injection(body: &mut Value, settings: &Settings) -> bool {
    let system_inject = settings
        .extra
        .get("systemInject")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !system_inject {
        return false;
    }
    let prompt = settings
        .extra
        .get("systemPrompt")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());
    match prompt {
        Some(p) => inject_system_prompt(body, &p),
        None => false,
    }
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

pub fn compress_messages(body: &mut Value, enabled: bool) -> Option<RtkStats> {
    if !enabled {
        return None;
    }

    // Kiro format (AWS CodeWhisperer): the request body has no top-level
    // `messages`/`input` array — instead it's
    // `conversationState.{history[], currentMessage}` and each entry holds
    // its `tool_result` payload at
    // `userInputMessage.userInputMessageContext.toolResults[].content[].text`.
    // Ports `compressKiroFormat()` from upstream 9router (#1194,
    // open-sse/rtk/index.js).
    if body
        .as_object()
        .is_some_and(|fields| fields.contains_key("conversationState"))
    {
        return Some(compress_kiro_format(body));
    }

    let mut items = {
        let fields = body.as_object()?;
        let arr = fields.get("messages").and_then(Value::as_array);
        let input = fields.get("input").and_then(Value::as_array);
        arr.or(input)?.clone()
    };

    let mut stats = RtkStats {
        bytes_before: 0,
        bytes_after: 0,
        hits: Vec::new(),
    };

    for msg in items.iter_mut() {
        let msg_fields = msg.as_object_mut()?;
        let role = msg_fields.get("role").and_then(Value::as_str);

        if role == Some("tool") {
            if let Some(content) = msg_fields.get_mut("content") {
                match content {
                    Value::String(text) => {
                        let compressed =
                            compress_tool_text_owned(text.as_str(), &mut stats, "openai-tool");
                        if compressed.len() < text.len() {
                            *text = compressed;
                        }
                    }
                    Value::Array(parts) => {
                        for part in parts.iter_mut() {
                            if let Some(Value::String(text)) = part.get_mut("text") {
                                let compressed = compress_tool_text_owned(
                                    text.as_str(),
                                    &mut stats,
                                    "openai-tool-array",
                                );
                                if compressed.len() < text.len() {
                                    *text = compressed;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else if msg_fields.get("type").and_then(Value::as_str) == Some("function_call_output") {
            if let Some(output) = msg_fields.get_mut("output") {
                match output {
                    Value::String(text) => {
                        let compressed = compress_tool_text_owned(
                            text.as_str(),
                            &mut stats,
                            "openai-responses-string",
                        );
                        if compressed.len() < text.len() {
                            *text = compressed;
                        }
                    }
                    Value::Array(parts) => {
                        for part in parts.iter_mut() {
                            if let Some(Value::String(text)) = part.get_mut("text") {
                                let compressed = compress_tool_text_owned(
                                    text.as_str(),
                                    &mut stats,
                                    "openai-responses-array",
                                );
                                if compressed.len() < text.len() {
                                    *text = compressed;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else if let Some(content) = msg_fields.get_mut("content").and_then(Value::as_array_mut) {
            for block in content.iter_mut() {
                let block_fields = block.as_object_mut()?;
                if block_fields.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                if block_fields.get("is_error").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                if let Some(cv) = block_fields.get_mut("content") {
                    match cv {
                        Value::String(text) => {
                            let compressed = compress_tool_text_owned(
                                text.as_str(),
                                &mut stats,
                                "claude-string",
                            );
                            if compressed.len() < text.len() {
                                *text = compressed;
                            }
                        }
                        Value::Array(parts) => {
                            for part in parts.iter_mut() {
                                if let Some(Value::String(text)) = part.get_mut("text") {
                                    let compressed = compress_tool_text_owned(
                                        text.as_str(),
                                        &mut stats,
                                        "claude-array",
                                    );
                                    if compressed.len() < text.len() {
                                        *text = compressed;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Write the modified items back into the request body.
    let key = if body.as_object().is_some_and(|f| f.contains_key("messages")) {
        "messages"
    } else {
        "input"
    };
    body[key] = Value::Array(items);

    Some(stats)
}

/// Walk `conversationState.history[]` + `conversationState.currentMessage`
/// and compress every `toolResults[].content[].text` payload in place.
/// Error tool results (`status == "error"`) are preserved verbatim so the
/// LLM still sees the failure trace.
///
/// Ports `compressKiroFormat()` from upstream 9router (open-sse/rtk/index.js).
fn compress_kiro_format(body: &mut Value) -> RtkStats {
    let mut stats = RtkStats {
        bytes_before: 0,
        bytes_after: 0,
        hits: Vec::new(),
    };

    let Some(state) = body
        .get_mut("conversationState")
        .and_then(Value::as_object_mut)
    else {
        return stats;
    };

    // Build a flat list of message references: history entries first, then
    // currentMessage. Borrow rules force us to handle them in two passes
    // because we can't simultaneously hold mutable references into both
    // sibling fields of the same map.
    if let Some(history) = state.get_mut("history").and_then(Value::as_array_mut) {
        for msg in history.iter_mut() {
            compress_kiro_tool_results(msg, &mut stats);
        }
    }
    if let Some(current) = state.get_mut("currentMessage") {
        compress_kiro_tool_results(current, &mut stats);
    }

    stats
}

/// Compress `userInputMessage.userInputMessageContext.toolResults[].content[].text`
/// for a single Kiro message entry. Skips entries whose `status == "error"`.
fn compress_kiro_tool_results(msg: &mut Value, stats: &mut RtkStats) {
    let Some(tool_results) = msg
        .get_mut("userInputMessage")
        .and_then(|v| v.get_mut("userInputMessageContext"))
        .and_then(|v| v.get_mut("toolResults"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    for tr in tool_results.iter_mut() {
        if tr.get("status").and_then(Value::as_str) == Some("error") {
            continue;
        }
        let Some(content) = tr.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in content.iter_mut() {
            let Some(part_obj) = part.as_object_mut() else {
                continue;
            };
            let Some(text) = part_obj.get("text").and_then(Value::as_str) else {
                continue;
            };
            let original = text.to_string();
            let compressed = compress_tool_text_owned(&original, stats, "kiro-tool-result");
            if compressed.len() < original.len() {
                part_obj.insert("text".to_string(), Value::String(compressed));
            }
        }
    }
}

/// Compress a single text payload and return the (possibly-compressed) output.
/// Used when the caller needs to write the result back into the request body
/// so the upstream sees a smaller payload.
fn compress_tool_text_owned(text: &str, stats: &mut RtkStats, shape: &str) -> String {
    let bytes_in = text.len();
    stats.bytes_before += bytes_in;

    if !(MIN_COMPRESS_SIZE..=RAW_CAP).contains(&bytes_in) {
        stats.bytes_after += bytes_in;
        return text.to_string();
    }

    let Some(detected) = auto_detect_filter(text) else {
        stats.bytes_after += bytes_in;
        return text.to_string();
    };

    let out = safe_apply(detected.filter_fn, text, detected.filter_name);

    if out.is_empty() || out.len() >= bytes_in {
        stats.bytes_after += bytes_in;
        return text.to_string();
    }

    stats.bytes_after += out.len();
    stats.hits.push(RtkHit {
        shape: shape.to_string(),
        filter: detected.filter_name.to_string(),
        saved: bytes_in - out.len(),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::{
        apply_request_preprocessing, compress_messages, inject_caveman_prompt,
        normalize_caveman_level, should_auto_apply_caveman, CompressionLevel,
    };
    use crate::types::Settings;
    use serde_json::json;

    #[test]
    fn compression_level_parses_case_insensitively() {
        assert_eq!("lite".parse(), Ok(CompressionLevel::Lite));
        assert_eq!(" FULL ".parse(), Ok(CompressionLevel::Full));
        assert_eq!("Ultra".parse(), Ok(CompressionLevel::Ultra));
        assert_eq!("wenyan-lite".parse(), Ok(CompressionLevel::WenyanLite));
        assert_eq!("wenyan".parse(), Ok(CompressionLevel::Wenyan));
        assert_eq!("wenyan-ultra".parse(), Ok(CompressionLevel::WenyanUltra));
        assert_eq!("wenyan_lite".parse(), Ok(CompressionLevel::WenyanLite));
        assert_eq!("wenyan_ultra".parse(), Ok(CompressionLevel::WenyanUltra));
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

        // 9router parity: Caveman always injects when enabled, regardless of length.
        assert!(apply_request_preprocessing(
            &mut body,
            &settings,
            "gpt-4o-mini"
        ));
        assert_eq!(body["messages"].as_array().expect("messages").len(), 2);
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
    fn compression_level_all_six_levels_have_distinct_prompts() {
        let lite = CompressionLevel::Lite.prompt();
        let full = CompressionLevel::Full.prompt();
        let ultra = CompressionLevel::Ultra.prompt();
        let wl = CompressionLevel::WenyanLite.prompt();
        let w = CompressionLevel::Wenyan.prompt();
        let wu = CompressionLevel::WenyanUltra.prompt();
        assert!(!lite.is_empty());
        assert!(!full.is_empty());
        assert!(!ultra.is_empty());
        assert!(!wl.is_empty());
        assert!(!w.is_empty());
        assert!(!wu.is_empty());
        assert_ne!(lite, full);
        assert_ne!(full, ultra);
        assert_ne!(ultra, wl);
        assert_ne!(wl, w);
        assert_ne!(w, wu);
        assert!(wl.contains("wenyan"));
        assert!(w.contains("Classical Chinese"));
        assert!(wu.contains("ultra-terse"));
    }

    #[test]
    fn compression_level_as_str() {
        assert_eq!(CompressionLevel::Lite.as_str(), "lite");
        assert_eq!(CompressionLevel::Full.as_str(), "full");
        assert_eq!(CompressionLevel::Ultra.as_str(), "ultra");
        assert_eq!(CompressionLevel::WenyanLite.as_str(), "wenyan-lite");
        assert_eq!(CompressionLevel::Wenyan.as_str(), "wenyan");
        assert_eq!(CompressionLevel::WenyanUltra.as_str(), "wenyan-ultra");
    }

    /// Build a Kiro `conversationState` request body whose toolResult[0]
    /// content[0].text is the given string. Mirrors the fixtures used in
    /// upstream `tests/unit/rtkKiro.test.js`.
    fn make_kiro_body(text: &str, in_history: bool) -> serde_json::Value {
        let entry = json!({
            "userInputMessage": {
                "content": "stub",
                "modelId": "claude-sonnet-4.5",
                "userInputMessageContext": {
                    "toolResults": [{
                        "toolUseId": "tool_1",
                        "status": "success",
                        "content": [{"text": text}]
                    }]
                }
            }
        });
        if in_history {
            json!({
                "conversationState": {
                    "chatTriggerType": "MANUAL",
                    "conversationId": "test",
                    "history": [entry],
                    "currentMessage": {
                        "userInputMessage": {
                            "content": "what happened?",
                            "modelId": "claude-sonnet-4.5"
                        }
                    }
                }
            })
        } else {
            json!({
                "conversationState": {
                    "chatTriggerType": "MANUAL",
                    "conversationId": "test",
                    "history": [],
                    "currentMessage": entry
                }
            })
        }
    }

    /// 18 npm-install lines so we comfortably exceed MIN_COMPRESS_SIZE (500 B)
    /// and let `buildOutput` strip most of the noise.
    fn npm_install_log() -> String {
        let lines = [
            "npm warn deprecated har-validator@5.1.5: this library is no longer supported",
            "npm warn deprecated uuid@3.4.0: uuid@10 and below is no longer supported",
            "npm warn deprecated request@2.88.2: request has been deprecated",
            "npm warn deprecated inflight@1.0.6: This module is not supported",
            "npm warn deprecated glob@7.2.3: Glob versions prior to v9 are no longer supported",
            "npm warn deprecated rimraf@2.7.1: Rimraf versions prior to v4 are no longer supported",
            "",
            "added 47 packages, and audited 48 packages in 13s",
            "",
            "3 packages are looking for funding",
            "  run `npm fund` for details",
            "",
            "4 vulnerabilities (2 moderate, 2 critical)",
            "",
            "Some issues need review, and may require choosing",
            "a different dependency.",
            "",
            "Run `npm audit` for details.",
        ];
        lines.join("\n")
    }

    #[test]
    fn kiro_compresses_tool_results_in_current_message() {
        let mut body = make_kiro_body(&npm_install_log(), false);
        let stats = compress_messages(&mut body, true).expect("kiro path returns stats");

        assert!(stats.bytes_before > 500, "fixture must exceed min size");
        assert!(stats.bytes_after < stats.bytes_before, "should shrink");
        assert_eq!(stats.hits.len(), 1);
        assert_eq!(stats.hits[0].filter, "build-output");
        assert_eq!(stats.hits[0].shape, "kiro-tool-result");

        // Verify the compressed payload was actually written back into the
        // request body (so the upstream sees the smaller version).
        let new_text = body["conversationState"]["currentMessage"]["userInputMessage"]
            ["userInputMessageContext"]["toolResults"][0]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(new_text.len() < npm_install_log().len());
    }

    #[test]
    fn kiro_compresses_tool_results_in_history() {
        // 20 cargo compile lines + finished line — enough to trigger buildOutput.
        let mut log = String::new();
        for i in 1..=20 {
            log.push_str(&format!("   Compiling package-{} v1.0.{}\n", i, i));
        }
        log.push_str("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.34s");

        let mut body = make_kiro_body(&log, true);
        let stats = compress_messages(&mut body, true).expect("kiro path returns stats");

        assert_eq!(stats.hits.len(), 1);
        assert_eq!(stats.hits[0].filter, "build-output");
        assert!(stats.bytes_after < stats.bytes_before);
    }

    #[test]
    fn kiro_preserves_error_tool_results() {
        // status=error → must be left untouched so the LLM still sees the trace.
        let raw_error_log = npm_install_log();
        let mut body = json!({
            "conversationState": {
                "history": [],
                "currentMessage": {
                    "userInputMessage": {
                        "content": "install foo",
                        "userInputMessageContext": {
                            "toolResults": [{
                                "toolUseId": "t1",
                                "status": "error",
                                "content": [{"text": raw_error_log}]
                            }]
                        }
                    }
                }
            }
        });
        let stats = compress_messages(&mut body, true).expect("kiro path returns stats");
        assert!(
            stats.hits.is_empty(),
            "error results must not be compressed"
        );
        let preserved = body["conversationState"]["currentMessage"]["userInputMessage"]
            ["userInputMessageContext"]["toolResults"][0]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(preserved, npm_install_log());
    }

    #[test]
    fn kiro_compresses_both_history_and_current_message() {
        let mut history_lines: Vec<String> = (1..=12)
            .map(|i| format!("npm warn deprecated package-{i}@1.0.0: This version is deprecated"))
            .collect();
        history_lines.push("added 50 packages in 5s".to_string());
        let history_log = history_lines.join("\n");

        let mut current_lines: Vec<String> = (1..=12)
            .map(|i| {
                format!("npm warn deprecated lib-{i}@2.0.0: This library is no longer supported")
            })
            .collect();
        current_lines.push("added 1 package in 2s".to_string());
        let current_log = current_lines.join("\n");

        let mut body = json!({
            "conversationState": {
                "history": [{
                    "userInputMessage": {
                        "userInputMessageContext": {
                            "toolResults": [{
                                "toolUseId": "t1",
                                "status": "success",
                                "content": [{"text": history_log}]
                            }]
                        }
                    }
                }],
                "currentMessage": {
                    "userInputMessage": {
                        "userInputMessageContext": {
                            "toolResults": [{
                                "toolUseId": "t2",
                                "status": "success",
                                "content": [{"text": current_log}]
                            }]
                        }
                    }
                }
            }
        });
        let stats = compress_messages(&mut body, true).expect("kiro path returns stats");
        assert_eq!(stats.hits.len(), 2);
        assert!(stats.hits.iter().all(|h| h.filter == "build-output"));
        assert!(stats.hits.iter().all(|h| h.shape == "kiro-tool-result"));
    }

    #[test]
    fn kiro_short_payloads_below_min_size_are_skipped() {
        let mut body = make_kiro_body("short", false);
        let stats = compress_messages(&mut body, true).expect("kiro path returns stats");
        assert!(stats.hits.is_empty(), "below 500B must skip");
        // Payload untouched.
        let after = body["conversationState"]["currentMessage"]["userInputMessage"]
            ["userInputMessageContext"]["toolResults"][0]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(after, "short");
    }

    #[test]
    fn kiro_path_handles_empty_history_and_no_current_tool_results() {
        let mut body = json!({
            "conversationState": {
                "history": [],
                "currentMessage": {
                    "userInputMessage": {"content": "hi", "modelId": "claude-sonnet-4.5"}
                }
            }
        });
        let stats = compress_messages(&mut body, true).expect("kiro path returns stats");
        assert_eq!(stats.bytes_before, 0);
        assert_eq!(stats.bytes_after, 0);
        assert!(stats.hits.is_empty());
    }

    #[test]
    fn kiro_path_ignores_non_object_content_parts() {
        let mut body = json!({
            "conversationState": {
                "history": [],
                "currentMessage": {
                    "userInputMessage": {
                        "userInputMessageContext": {
                            "toolResults": [{
                                "toolUseId": "t1",
                                "status": "success",
                                "content": ["a", null, 42, {"text": "tiny"}]
                            }]
                        }
                    }
                }
            }
        });
        // Should not panic; non-object parts and missing-text entries are ignored.
        let stats = compress_messages(&mut body, true).expect("kiro path returns stats");
        assert!(stats.hits.is_empty());
    }

    #[test]
    fn kiro_disabled_returns_none() {
        let mut body = make_kiro_body(&npm_install_log(), false);
        assert!(compress_messages(&mut body, false).is_none());
    }

    #[test]
    fn non_kiro_request_still_goes_through_messages_path() {
        // Sanity check that the conversationState shortcut doesn't accidentally
        // hijack regular OpenAI requests.
        let mut body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "hi"}
            ]
        });
        let stats = compress_messages(&mut body, true);
        assert!(stats.is_some());
        // No tool messages → no hits, but the function still returns Some.
        assert_eq!(stats.unwrap().hits.len(), 0);
    }
}
