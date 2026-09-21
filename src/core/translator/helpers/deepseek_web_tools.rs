//! DeepSeek-web-specific tool-call translation.
//!
//! Port of OmniRoute `open-sse/translator/deepseekWebTools.ts` (564 lines).
//! chat.deepseek.com has no native function calling, so the OpenAI `tools[]`
//! are serialized into a prompt contract and the model's text reply is parsed
//! back into OpenAI `tool_calls`. The canonical parser in [`super::web_tools`]
//! handles well-behaved `<tool>{json}</tool>` shapes; DeepSeek emits a wider
//! zoo of ad-hoc shapes, so this parser tokenizes tool tags and walks them
//! with a stack, reusing the shared JSON/fuzzy-match/range helpers.

use serde_json::Value;
use std::collections::{HashMap, HashSet};

use super::web_tools::{
    get_requested_tool_names, get_tool_nonce, parse_loose_json_object, parse_tool_calls_from_text,
    resolve_requested_tool_name, strip_ranges, to_arguments_string, OpenAIToolCall,
    RequestedToolName,
};

/// Extract the userToken from credentials (api_key preferred, access_token
/// fallback). Handles JSON-wrapped tokens (`{"value":"..."}`).
pub fn extract_user_token(api_key: Option<&str>, access_token: Option<&str>) -> Option<String> {
    let raw = api_key
        .filter(|s| !s.trim().is_empty())
        .or_else(|| access_token.filter(|s| !s.trim().is_empty()))?;
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) {
            if let Some(v) = map.get("value").and_then(Value::as_str) {
                return Some(v.to_string());
            }
        }
    }
    Some(trimmed.to_string())
}

fn message_text(content: &Value) -> String {
    match content {
        Value::Array(items) => items
            .iter()
            .filter(|i| i.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        _ => content.to_string(),
    }
}

/// Stricter, compact tool-use prompt (DeepSeek invents wrappers and merely
/// describes plans, so the wording forces the canonical shape). Returns
/// `(prompt, nonce)`.
pub fn serialize_deepseek_tool_prompt(tools: &Value) -> Option<(String, String)> {
    let arr = tools.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let nonce = get_tool_nonce(tools);
    if nonce.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for t in arr {
        let f = t.get("function")?;
        let name = f.get("name")?.as_str()?;
        if name.is_empty() {
            continue;
        }
        let desc = f.get("description").and_then(Value::as_str).unwrap_or("");
        let params = f
            .get("parameters")
            .map(|p| serde_json::to_string(p).unwrap_or_default())
            .unwrap_or_default();
        let mut line = format!("- {name}");
        if !desc.is_empty() {
            line.push_str(&format!(": {desc}"));
        }
        if !params.is_empty() {
            line.push_str(&format!("\n  parameters: {params}"));
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    let mut parts = vec![
        "You can call tools. To call a tool, output ONLY this exact block (no markdown fence):".to_string(),
        format!("<tool>{{\"name\": \"<tool_name>\", \"arguments\": {{ ... }}, \"_nonce\": \"{nonce}\"}}</tool>"),
        "Rules:".to_string(),
        "- Use exactly <tool>...</tool>. Do NOT use <tool:name>, <tool_call>, <name>, <parameter>, id=/name= attributes, or code fences.".to_string(),
        format!("- Include the secret binding \"_nonce\": \"{nonce}\" exactly as shown."),
        "- \"name\" must be one of the tools below; \"arguments\" must be a JSON object.".to_string(),
        "- When a tool is needed, emit the <tool> block instead of only describing the plan.".to_string(),
        "- Emit one <tool> block per call; you may put several blocks back to back.".to_string(),
        "- If no tool is needed, just answer normally without any <tool> block.".to_string(),
        String::new(),
        "Available tools:".to_string(),
    ];
    parts.extend(lines);
    Some((parts.join("\n"), nonce))
}

/// Build the single `prompt` string for an agentic (tool-using) turn: replay
/// the whole trajectory (prior `<tool>` calls + `role:"tool"` results) so the
/// model continues instead of restarting.
pub fn build_tool_conversation_prompt(messages: &[Value], tool_system_prompt: &str) -> String {
    let mut system_parts: Vec<String> = Vec::new();
    if !tool_system_prompt.is_empty() {
        system_parts.push(tool_system_prompt.to_string());
    }
    let mut lines: Vec<String> = Vec::new();
    let mut call_name_by_id: HashMap<String, String> = HashMap::new();
    let mut saw_tool_activity = false;
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "system" => {
                let t = message_text(m.get("content").unwrap_or(&Value::Null))
                    .trim()
                    .to_string();
                if !t.is_empty() {
                    system_parts.push(t);
                }
            }
            "user" => {
                let t = message_text(m.get("content").unwrap_or(&Value::Null))
                    .trim()
                    .to_string();
                if !t.is_empty() {
                    lines.push(format!("User: {t}"));
                }
            }
            "assistant" => {
                let t = message_text(m.get("content").unwrap_or(&Value::Null))
                    .trim()
                    .to_string();
                let mut parts: Vec<String> = Vec::new();
                if !t.is_empty() {
                    parts.push(t);
                }
                if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                    for c in calls {
                        let name = c
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let raw_args = c.get("function").and_then(|f| f.get("arguments"));
                        let args = match raw_args {
                            Some(Value::String(s)) if !s.is_empty() => s.clone(),
                            Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
                            None => "{}".into(),
                        };
                        if let Some(id) = c.get("id").and_then(Value::as_str) {
                            call_name_by_id.insert(id.to_string(), name.to_string());
                        }
                        parts.push(format!(
                            "<tool>{{\"name\": {}, \"arguments\": {args}}}</tool>",
                            serde_json::to_string(name).unwrap_or_default()
                        ));
                        saw_tool_activity = true;
                    }
                }
                if !parts.is_empty() {
                    lines.push(format!("Assistant: {}", parts.join("\n")));
                }
            }
            "tool" => {
                let t = message_text(m.get("content").unwrap_or(&Value::Null))
                    .trim()
                    .to_string();
                let name = m
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .and_then(|id| call_name_by_id.get(id))
                    .cloned()
                    .or_else(|| m.get("name").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_else(|| "tool".to_string());
                lines.push(format!(
                    "Tool result ({name}): {}",
                    if t.is_empty() { "(no output)" } else { &t }
                ));
                saw_tool_activity = true;
            }
            _ => {}
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if !system_parts.is_empty() {
        parts.push(system_parts.join("\n\n"));
    }
    if !lines.is_empty() {
        parts.push(lines.join("\n\n"));
    }
    if saw_tool_activity {
        parts.push(
            "Continue the task using the tool results above. Do NOT repeat tool calls that already succeeded; perform the next step or give the final answer.".to_string(),
        );
    }
    let joined = parts.join("\n\n");
    strip_markdown_images(&joined)
}

fn strip_markdown_images(s: &str) -> String {
    // Approximation of /!\[.*?\]\(.*?\)/g
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'!' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            if let Some(end) = s[i..].find(')') {
                // verify ](/ pattern exists inside
                let seg = &s[i..i + end + 1];
                if seg.contains("](") {
                    i += end + 1;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

const DEFAULT_AUTO_HISTORY_WINDOW: usize = 20;

/// Build the single prompt string for plain (non-tool) turns. Single-turn
/// requests keep system + last user message; multi-turn stitches a rolling
/// transcript (auto window 20 when unset, #10527).
pub fn messages_to_prompt(messages: &[Value], history_window: usize) -> String {
    let mut system_parts: Vec<String> = Vec::new();
    let mut conversation: Vec<(String, String)> = Vec::new();
    let mut call_name_by_id: HashMap<String, String> = HashMap::new();
    let mut last_user = String::new();
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        let text = message_text(m.get("content").unwrap_or(&Value::Null))
            .trim()
            .to_string();
        match role {
            "system" => {
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            "user" | "assistant" => {
                if !text.is_empty() {
                    conversation.push((role.to_string(), text.clone()));
                }
                if role == "user" && !text.is_empty() {
                    last_user = text;
                }
                if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                    for c in calls {
                        if let (Some(id), Some(name)) = (
                            c.get("id").and_then(Value::as_str),
                            c.get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(Value::as_str),
                        ) {
                            call_name_by_id.insert(id.to_string(), name.to_string());
                        }
                    }
                }
            }
            "tool" => {
                if !text.is_empty() {
                    let name = m
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .and_then(|id| call_name_by_id.get(id))
                        .cloned()
                        .or_else(|| m.get("name").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_else(|| "tool".to_string());
                    conversation.push(("tool".to_string(), format!("({name}) {text}")));
                }
            }
            _ => {}
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if !system_parts.is_empty() {
        parts.push(system_parts.join("\n\n"));
    }
    let effective = if history_window > 0 {
        history_window
    } else if conversation.len() > 1 {
        DEFAULT_AUTO_HISTORY_WINDOW
    } else {
        0
    };
    if effective > 0 && conversation.len() > 1 {
        let start = conversation.len().saturating_sub(effective);
        let transcript = conversation[start..]
            .iter()
            .map(|(role, text)| match role.as_str() {
                "assistant" => format!("Assistant: {text}"),
                "tool" => format!("Tool result {text}"),
                _ => format!("User: {text}"),
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        parts.push(transcript);
    } else if !last_user.is_empty() {
        parts.push(last_user);
    }
    strip_markdown_images(&parts.join("\n\n"))
}

// ── Stream content helpers (stream-format.ts) ───────────────────────────────

pub fn is_thinking_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("think") || m.contains("r1") || m.contains("reason")
}

pub fn is_search_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("search") || m.contains("fold")
}

fn clean_deepseek_token(text: &str) -> String {
    let mut out = text.replace("FINISHED", "");
    // strip leading SEARCH|WEB_SEARCH|SEARCHING + whitespace (case-insensitive)
    let upper = out.to_uppercase();
    for token in ["SEARCHING", "WEB_SEARCH", "SEARCH"] {
        if upper.starts_with(token) {
            let mut rest = out[token.len()..].to_string();
            while rest.starts_with([' ', '\t', '\n', '\r']) {
                rest.remove(0);
            }
            out = rest;
            break;
        }
    }
    out
}

/// Format one raw upstream text fragment for the client.
pub fn format_stream_content(raw: &str, model: &str) -> String {
    let text = clean_deepseek_token(raw);
    if !is_search_model(model) {
        return text;
    }
    if model.to_lowercase().contains("search-silent") {
        return replace_citations(&text, true);
    }
    replace_citations(&text, false)
}

fn replace_citations(text: &str, strip: bool) -> String {
    // [citation:N] → [N] (or removed for search-silent)
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if text[i..].starts_with("[citation:") {
            if let Some(end) = text[i..].find(']') {
                let num = &text[i + 10..i + end];
                if !strip {
                    out.push('[');
                    out.push_str(num);
                    out.push(']');
                }
                i += end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// A DeepSeek search result for citation appending.
#[derive(Debug, Clone, Default)]
pub struct DeepSeekSearchResult {
    pub cite_index: Option<i64>,
    pub title: Option<String>,
    pub url: Option<String>,
}

impl DeepSeekSearchResult {
    pub fn from_value(v: &Value) -> Self {
        Self {
            cite_index: v.get("cite_index").and_then(Value::as_i64),
            title: v.get("title").and_then(Value::as_str).map(str::to_string),
            url: v.get("url").and_then(Value::as_str).map(str::to_string),
        }
    }
}

/// Append sorted `[n]: [title](url)` citations (stream-format.ts).
pub fn append_search_citations(results: &[DeepSeekSearchResult], model: &str) -> String {
    if results.is_empty() || model.to_lowercase().contains("search-silent") {
        return String::new();
    }
    let mut sorted: Vec<&DeepSeekSearchResult> =
        results.iter().filter(|r| r.cite_index.is_some()).collect();
    sorted.sort_by_key(|r| r.cite_index.unwrap_or(0));
    sorted
        .iter()
        .map(|r| {
            format!(
                "[{}]: [{}]({})",
                r.cite_index.unwrap_or(0),
                r.title.as_deref().unwrap_or(""),
                r.url.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolve modelType/thinking/search flags (deepseek-web.ts resolveModelOptions).
pub fn resolve_model_options(model: &str, body: &Value) -> (String, bool, bool) {
    let m = model.to_lowercase();
    let model_type = if m.contains("pro") || m.contains("expert") {
        "expert"
    } else {
        "default"
    }
    .to_string();
    let thinking = m.contains("r1")
        || m.contains("think")
        || m.contains("reason")
        || body.get("thinking_enabled") == Some(&Value::Bool(true))
        || body.get("thinking") == Some(&Value::Bool(true))
        || body.get("reasoning_effort").is_some();
    let search = m.contains("search")
        || body.get("search_enabled") == Some(&Value::Bool(true))
        || body.get("search") == Some(&Value::Bool(true))
        || body.get("web_search") == Some(&Value::Bool(true));
    (model_type, thinking, search)
}

// ── Tag tokenizer + stack parser ────────────────────────────────────────────

struct TagToken {
    start: usize,
    end: usize,
    closing: bool,
    suffix: String,
    attrs: String,
}

fn is_tag_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-')
}

/// Tokenize `<tool ...>` / `<tool:name ...>` / `<tool_call ...>` opens and closes.
fn tokenize_tool_tags(text: &str) -> Vec<TagToken> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let closing = j < bytes.len() && bytes[j] == b'/';
        if closing {
            j += 1;
        }
        let name_start = j;
        // match tool_call first (longer)
        let tag_len = if text[j..].starts_with("tool_call") {
            9
        } else if text[j..].starts_with("tool") {
            4
        } else {
            i += 1;
            continue;
        };
        // boundary: next char must not be a tag-name char
        let after = j + tag_len;
        if after < bytes.len()
            && (bytes[after].is_ascii_alphanumeric() && bytes[after] != b':')
            && !(bytes[after] == b'_' || bytes[after] == b'-')
        {
            // e.g. <tools> — not a tool tag
            i += 1;
            continue;
        }
        j = after;
        let _ = name_start;
        // optional :suffix
        let mut suffix = String::new();
        if j < bytes.len() && bytes[j] == b':' {
            j += 1;
            let s = j;
            while j < bytes.len() && is_tag_char(bytes[j]) {
                j += 1;
            }
            suffix = text[s..j].to_string();
        }
        // attribute text until '>'
        let attrs_start = j;
        // find closing '>' (no nesting inside a tag)
        let mut k = j;
        let mut self_close = false;
        while k < bytes.len() && bytes[k] != b'>' {
            k += 1;
        }
        if k >= bytes.len() {
            break;
        }
        // detect trailing '/' before '>' (self-closing)
        let mut back = k;
        while back > attrs_start && bytes[back - 1].is_ascii_whitespace() {
            back -= 1;
        }
        if back > attrs_start && bytes[back - 1] == b'/' {
            self_close = true;
        }
        let attrs = text[attrs_start..k].to_string();
        let end = k + 1;
        tokens.push(TagToken {
            start: i,
            end,
            closing,
            suffix,
            attrs,
        });
        if self_close && !closing {
            // A self-closing tag counts as open+close at the same spot.
            tokens.push(TagToken {
                start: i,
                end,
                closing: true,
                suffix: String::new(),
                attrs: String::new(),
            });
        }
        i = end;
    }
    tokens
}

struct ToolBlock {
    open_idx: usize,
    close_idx: usize,
    inner_start: usize,
    inner_end: usize,
}

fn pair_tool_blocks(tokens: &[TagToken], text_len: usize) -> Vec<ToolBlock> {
    let mut blocks = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for (idx, tok) in tokens.iter().enumerate() {
        if !tok.closing {
            stack.push(idx);
            continue;
        }
        if let Some(open_idx) = stack.pop() {
            blocks.push(ToolBlock {
                open_idx,
                close_idx: idx,
                inner_start: tokens[open_idx].end,
                inner_end: tok.start,
            });
        }
    }
    for open_idx in stack {
        blocks.push(ToolBlock {
            open_idx,
            close_idx: usize::MAX,
            inner_start: tokens[open_idx].end,
            inner_end: text_len,
        });
    }
    blocks
}

/// Read an attribute value, tolerating backslash-escaped quotes.
fn get_attr(attrs: &str, name: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // word boundary
        if (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
            && attrs[i..].starts_with(name)
        {
            let mut j = i + name.len();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                    let quote = bytes[j];
                    j += 1;
                    let mut out = String::new();
                    while j < bytes.len() {
                        if bytes[j] == b'\\' && j + 1 < bytes.len() {
                            out.push(bytes[j + 1] as char);
                            j += 2;
                            continue;
                        }
                        if bytes[j] == quote {
                            break;
                        }
                        out.push(bytes[j] as char);
                        j += 1;
                    }
                    return Some(out);
                }
            }
        }
        i += 1;
    }
    None
}

fn get_xml_child(inner: &str, tag: &str) -> Option<String> {
    let lower = inner.to_lowercase();
    let open_pat = format!("<{tag}");
    let mut search = 0;
    loop {
        let rel = lower[search..].find(&open_pat)?;
        let os = search + rel;
        // ensure boundary
        let after = os + open_pat.len();
        if after < inner.len() && (inner.as_bytes()[after].is_ascii_alphanumeric()) {
            search = after;
            continue;
        }
        let Some(gt) = inner[os..].find('>') else {
            return None;
        };
        let content_start = os + gt + 1;
        let close_pat = format!("</{tag}>");
        let cpos = lower[content_start..].find(&close_pat)?;
        return Some(
            inner[content_start..content_start + cpos]
                .trim()
                .to_string(),
        );
    }
}
/// Collect parameter tags into an object. Supports attribute-only tags
/// (name/content attrs, no closing tag) and body tags. The body scan stops
/// at the next parameter open (tempered greedy, JS parity).
fn build_args_from_parameters(inner: &str) -> Option<serde_json::Map<String, Value>> {
    let mut out = serde_json::Map::new();
    let mut found = false;
    let lower = inner.to_lowercase();
    let mut search = 0;
    while let Some(rel) = lower[search..].find(PARAM_OPEN) {
        let os = search + rel;
        let after = os + PARAM_OPEN.len();
        if after < inner.len() && is_param_name_char(inner.as_bytes()[after]) {
            search = after;
            continue;
        }
        let Some(gt) = inner[os..].find('>') else {
            break;
        };
        let tag_end = os + gt + 1;
        let attrs = &inner[os..tag_end];
        let self_closing = attrs.trim_end_matches('>').trim_end().ends_with('/');
        let mut body: Option<String> = None;
        let mut next_search = tag_end;
        if !self_closing {
            let rest_lower = &lower[tag_end..];
            let close_rel = rest_lower.find(PARAM_CLOSE);
            let next_rel = rest_lower.find(PARAM_OPEN);
            match close_rel {
                Some(c) if next_rel.is_none() || next_rel.unwrap() > c => {
                    body = Some(inner[tag_end..tag_end + c].trim().to_string());
                    next_search = tag_end + c + PARAM_CLOSE.len();
                }
                _ => {
                    next_search = tag_end;
                }
            }
        }
        if let Some(name) = get_attr(&attrs[..attrs.len().saturating_sub(1)], "name") {
            let value = get_attr(&attrs[..attrs.len().saturating_sub(1)], "content")
                .or_else(|| body.clone().map(|b| b.trim().to_string()))
                .unwrap_or_default();
            out.insert(name, Value::String(value));
            found = true;
        }
        search = next_search.max(tag_end);
        if search <= os {
            break;
        }
    }
    if found {
        Some(out)
    } else {
        None
    }
}

const PARAM_OPEN: &str = "<parameter";
const PARAM_CLOSE: &str = concat!("</", "parameter>");

fn is_param_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() && b != b'/' && b != b'>' && !b.is_ascii_whitespace()
}

/// Scan for the first balanced `{...}` object (quote/escape aware) and
/// return just that slice — salvages valid calls with trailing garbage.
fn salvage_leading_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut quote: u8 = 0;
    let mut escaped = false;
    let mut i = start;
    while i < bytes.len() {
        let ch = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if quote != 0 {
            if ch == b'\\' {
                escaped = true;
            } else if ch == quote {
                quote = 0;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' {
            quote = ch;
            i += 1;
            continue;
        }
        if ch == b'{' {
            depth += 1;
            i += 1;
            continue;
        }
        if ch == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(text[start..=i].to_string());
            }
        }
        i += 1;
    }
    None
}

/// Tool name to set of parameter-schema keys (nameless-block fallback #5154).
fn build_schema_param_map(tools: &Value) -> HashMap<String, HashSet<String>> {
    let mut map = HashMap::new();
    if let Some(arr) = tools.as_array() {
        for t in arr {
            let (Some(name), params) = (
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str),
                t.get("function").and_then(|f| f.get("parameters")),
            ) else {
                continue;
            };
            let keys: HashSet<String> = params
                .and_then(|p| p.get("properties"))
                .and_then(Value::as_object)
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            map.insert(name.to_string(), keys);
        }
    }
    map
}
struct ExtractedCall {
    name: String,
    arguments: String,
}

fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Turn one tool block (tag name + inner text) into name + JSON-string args.
fn extract_call(
    tag_name: &str,
    inner_raw: &str,
    requested: &[RequestedToolName],
    schema_map: &HashMap<String, HashSet<String>>,
) -> Option<ExtractedCall> {
    let inner = inner_raw.trim();
    let name_child = get_xml_child(inner, "name");
    let args_child =
        get_xml_child(inner, "arguments").or_else(|| get_xml_child(inner, "parameters"));
    let param_obj = if args_child.is_some() {
        None
    } else {
        build_args_from_parameters(inner)
    };
    let has_xml = name_child.is_some() || args_child.is_some() || param_obj.is_some();

    let mut json: Option<serde_json::Map<String, Value>> = if has_xml {
        None
    } else {
        parse_loose_json_object(inner)
    };
    if json.is_none() && !has_xml {
        if let Some(salvaged) = salvage_leading_json_object(inner) {
            json = parse_loose_json_object(&salvaged);
        }
    }
    let json_name = json.as_ref().and_then(|j| {
        as_string(j.get("name").unwrap_or(&Value::Null))
            .or_else(|| as_string(j.get("type").unwrap_or(&Value::Null)))
    });

    let child_resolved = name_child
        .as_deref()
        .and_then(|n| resolve_requested_tool_name(n, requested));
    let json_resolved = json_name
        .as_deref()
        .and_then(|n| resolve_requested_tool_name(n, requested));
    let tag_resolved = if tag_name.is_empty() {
        None
    } else {
        resolve_requested_tool_name(tag_name, requested)
    };

    // JSON body wins over the tag attribute (bogus tag names, #3260).
    let mut name: Option<String> = None;
    let mut name_from_tag = false;
    let mut pick = |val: Option<String>, from_tag: bool| {
        if name.is_none() {
            if let Some(v) = val {
                name = Some(v);
                name_from_tag = from_tag;
            }
        }
    };
    pick(child_resolved, false);
    pick(json_resolved, false);
    pick(tag_resolved, true);
    pick(name_child.clone(), false);
    pick(json_name.clone(), false);
    if !tag_name.is_empty() {
        pick(Some(tag_name.to_string()), true);
    }

    // Shell-style {"command": ...} with no tag name.
    if name.is_none() && tag_name.is_empty() {
        if let Some(json) = json.as_ref() {
            if let Some(cmd) = json.get("command").and_then(Value::as_str) {
                if let Some(resolved) = resolve_requested_tool_name(cmd, requested) {
                    name = Some(resolved);
                    name_from_tag = false;
                }
            }
        }
    }

    // Nameless-block fallback (#5154): exactly one requested tool whose
    // schema keys are a superset of the extracted param names.
    if name.is_none() {
        if let Some(params) = param_obj.as_ref() {
            if !schema_map.is_empty() && !params.is_empty() {
                let keys: Vec<&String> = params.keys().collect();
                let mut candidates = Vec::new();
                for (tool_name, schema_keys) in schema_map {
                    if !schema_keys.is_empty() && keys.iter().all(|k| schema_keys.contains(*k)) {
                        candidates.push(tool_name.clone());
                    }
                }
                if candidates.len() == 1 {
                    name = Some(candidates.remove(0));
                    name_from_tag = false;
                }
            }
        }
    }

    let name = name?;
    let args_value: Value = if let Some(child) = args_child {
        parse_loose_json_object(&child)
            .map(Value::Object)
            .unwrap_or(Value::String(child))
    } else if let Some(params) = param_obj {
        Value::Object(params)
    } else if let Some(json) = json {
        if let Some(a) = json.get("arguments") {
            a.clone()
        } else if let Some(p) = json.get("params") {
            p.clone()
        } else if name_from_tag {
            Value::Object(json)
        } else {
            let mut rest = json;
            for k in [
                "name",
                "type",
                "id",
                "command",
                "arguments",
                "params",
                "_nonce",
            ] {
                rest.remove(k);
            }
            Value::Object(rest)
        }
    } else {
        Value::Object(serde_json::Map::new())
    };
    Some(ExtractedCall {
        name,
        arguments: to_arguments_string(&args_value),
    })
}

/// Parse a DeepSeek-web text reply into OpenAI tool calls.
/// Falls back to the canonical parser for tag-free replies; when tags are
/// present but none parse, returns the text unchanged with None (no fallback,
//-te- re-processing, #9343).
pub fn parse_deepseek_tool_calls(
    text: &str,
    id_seed: &str,
    requested_tools: &Value,
    nonce: &str,
) -> (String, Option<Vec<OpenAIToolCall>>) {
    if text.is_empty() {
        return (text.to_string(), None);
    }
    let tokens = tokenize_tool_tags(text);
    if tokens.is_empty() {
        let (content, calls) = parse_tool_calls_from_text(text, id_seed, requested_tools, nonce);
        return (content, calls);
    }
    let requested = get_requested_tool_names(requested_tools);
    let schema_map = build_schema_param_map(requested_tools);
    let blocks = pair_tool_blocks(&tokens, text.len());
    let mut leaf_idx: Vec<usize> = (0..blocks.len())
        .filter(|&i| {
            !blocks
                .iter()
                .enumerate()
                .any(|(j, o)| j != i && o.contains_block(&blocks[i], &tokens))
        })
        .collect();
    leaf_idx.sort_by_key(|&i| blocks[i].open_start(&tokens));
    let mut calls: Vec<OpenAIToolCall> = Vec::new();
    let mut accepted: Vec<(usize, usize)> = Vec::new();
    for i in leaf_idx {
        let b = &blocks[i];
        let open = &tokens[b.open_idx];
        let tag_name = if !open.suffix.is_empty() {
            open.suffix.clone()
        } else if let Some(n) = get_attr(&open.attrs, "name") {
            n
        } else if let Some(id) = get_attr(&open.attrs, "id") {
            id
        } else {
            String::new()
        };
        let inner = &text[b.inner_start..b.inner_end];
        let Some(call) = extract_call(&tag_name, inner, &requested, &schema_map) else {
            continue;
        };
        // Nonce check: canonical JSON-body blocks with explicit _nonce must match.
        if !nonce.is_empty() {
            if let Some(parsed) = parse_loose_json_object(inner) {
                let named = parsed.get("name").and_then(Value::as_str).is_some();
                let mismatch = parsed
                    .get("_nonce")
                    .is_some_and(|n| n.as_str().unwrap_or("") != nonce);
                if named && mismatch {
                    continue;
                }
            }
        }
        calls.push(OpenAIToolCall {
            id: format!("{id_seed}_{}", calls.len()),
            name: call.name,
            arguments: call.arguments,
        });
        let close_end = if b.close_idx == usize::MAX {
            b.inner_end
        } else {
            tokens[b.close_idx].end
        };
        accepted.push((open.start, close_end));
    }
    if calls.is_empty() {
        return (text.to_string(), None);
    }
    // Strip accepted blocks plus stray tags outside them.
    let mut ranges = accepted.clone();
    for tok in &tokens {
        let inside = accepted.iter().any(|r| tok.start >= r.0 && tok.end <= r.1);
        if !inside {
            ranges.push((tok.start, tok.end));
        }
    }
    (strip_ranges(text, &ranges), Some(calls))
}

impl ToolBlock {
    fn open_start(&self, tokens: &[TagToken]) -> usize {
        tokens[self.open_idx].start
    }
    fn close_end(&self, tokens: &[TagToken]) -> usize {
        if self.close_idx == usize::MAX {
            self.inner_end
        } else {
            tokens[self.close_idx].end
        }
    }
    fn contains_block(&self, other: &ToolBlock, tokens: &[TagToken]) -> bool {
        tokens[other.open_idx].start >= self.inner_start
            && other.close_end(tokens) <= self.inner_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Value {
        serde_json::json!([
            {"type": "function", "function": {"name": "bash", "description": "run shell", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
            {"type": "function", "function": {"name": "read_file", "description": "read", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
        ])
    }

    #[test]
    fn canonical_block_parses() {
        let t = tools();
        let nonce = get_tool_nonce(&t);
        let text = format!(
            "plan <tool>{{\"name\": \"bash\", \"arguments\": {{\"command\": \"ls\"}}, \"_nonce\": \"{nonce}\"}} done"
        );
        // close the tag properly
        let text = text.replace("}} done", "}}</tool> done");
        let (content, calls) = parse_deepseek_tool_calls(&text, "call", &t, &nonce);
        let calls = calls.expect("one call");
        assert_eq!(calls[0].name, "bash");
        assert!(!content.contains("<tool>"));
    }

    #[test]
    fn suffix_and_attr_names_parse() {
        let t = tools();
        let nonce = get_tool_nonce(&t);
        let text = "<tool:bash>{\"command\": \"ls\"}</tool>";
        let (_, calls) = (
            String::new(),
            parse_deepseek_tool_calls(text, "call", &t, "").1,
        );
        let calls = calls.expect("suffix call");
        assert_eq!(calls[0].name, "bash");
        let text2 = "<tool name=\"read_file\">{\"path\": \"x\"}</tool>";
        let (_, calls2) = parse_deepseek_tool_calls(text2, "call", &t, "");
        assert_eq!(calls2.expect("attr call")[0].name, "read_file");
    }

    #[test]
    fn doubled_wrapper_yields_single_call() {
        let t = tools();
        let text =
            "<tool><tool>{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}</tool></tool>";
        let (content, calls) = parse_deepseek_tool_calls(text, "call", &t, "");
        assert_eq!(calls.expect("one call").len(), 1);
        assert!(!content.contains("<tool"));
    }

    #[test]
    fn xml_children_and_params_parse() {
        let t = tools();
        let text = "<tool><name>bash</name><arguments>{\"command\": \"ls\"}</arguments></tool>";
        let (_, calls) = parse_deepseek_tool_calls(text, "call", &t, "");
        assert_eq!(calls.expect("xml call")[0].name, "bash");
        let p1 = "<tool><parameter name=\"command\" content=\"ls\"/></tool>";
        let (_, c1) = parse_deepseek_tool_calls(&p1, "call", &t, "");
        assert_eq!(c1.expect("param call")[0].name, "bash");
        // nameless parameter block -> schema fallback picks bash (command key)
        let p2 = "<tool><parameter name=\"command\" run>ls</parameter></tool>";
        let (_, c2) = parse_deepseek_tool_calls(&p2, "call", &t, "");
        assert_eq!(c2.expect("schema call")[0].name, "bash");
    }

    #[test]
    fn prompt_builders_shape() {
        let t = tools();
        let (prompt, nonce) = serialize_deepseek_tool_prompt(&t).expect("prompt");
        assert!(prompt.contains(&nonce));
        assert!(prompt.contains("bash"));
        let msgs = serde_json::json!([
            {"role": "system", "content": "sys"},
            {"role": "user", "content": "do task"},
        ]);
        let arr = msgs.as_array().cloned().unwrap();
        let p = messages_to_prompt(&arr, 0);
        assert!(p.contains("do task"));
        assert!(p.contains("sys"));
        // multi-turn stitches transcript
        let msgs2 = serde_json::json!([
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "ack"},
            {"role": "user", "content": "second"},
        ]);
        let arr2 = msgs2.as_array().cloned().unwrap();
        let p2 = messages_to_prompt(&arr2, 0);
        assert!(p2.contains("first"));
        assert!(p2.contains("second"));
        assert_eq!(
            resolve_model_options("deepseek-v4-flash-think", &serde_json::json!({})).1,
            true
        );
        assert_eq!(
            resolve_model_options("plain", &serde_json::json!({"search_enabled": true})).2,
            true
        );
        assert_eq!(format_stream_content("FINISHED hi", "m"), " hi");
    }

    #[test]
    fn user_token_extraction() {
        assert_eq!(
            extract_user_token(Some("abc"), None).as_deref(),
            Some("abc")
        );
        assert_eq!(
            extract_user_token(Some("{\"value\": \"v2\"}"), None).as_deref(),
            Some("v2")
        );
        assert_eq!(
            extract_user_token(None, Some("tok")),
            Some("tok".to_string())
        );
        assert!(extract_user_token(None, None).is_none());
    }
}
