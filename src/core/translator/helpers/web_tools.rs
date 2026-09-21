//! Tool-call translation for web-cookie providers (DeepSeek Web, ...).
//!
//! Port of OmniRoute `open-sse/translator/webTools.ts`. The web UIs accept
//! only a single plain prompt string and have no native function calling —
//! they reply with tool invocations as raw text. To let agentic clients use
//! these providers we (a) serialize the OpenAI `tools` array into a
//! system-prompt contract on the request side, and (b) parse the upstream
//! `<tool>{...}</tool>` text back into OpenAI `tool_calls` on the response
//! side. (#2820)
//!
//! Security hardening (#9343): bare JSON with name+arguments keys is NEVER
//! promoted to tool_calls — only explicit `<tool>` / `<tool_call>` envelopes
//! are accepted, and a per-request nonce embedded by the serializer must
//! match when present.

use rand::RngCore;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// An OpenAI-format tool call parsed from upstream text.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAIToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl OpenAIToolCall {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "type": "function",
            "function": { "name": self.name, "arguments": self.arguments },
        })
    }
}

/// A requested tool name (original + normalized for fuzzy matching).
#[derive(Debug, Clone)]
pub struct RequestedToolName {
    pub original: String,
    pub normalized: String,
}

// Per-request nonce binding (#9343). JS uses a WeakMap keyed on the tools[]
// array reference; here the caller threads the nonce explicitly (see
// `serialize_tools_to_prompt` returning `(String, String)`), so this map is
// only a fallback registry for callers that pass tools by value twice.
static NONCE_REGISTRY: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn nonce_registry() -> &'static Mutex<HashMap<String, String>> {
    NONCE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn random_nonce() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    let mut out = String::with_capacity(8);
    for _ in 0..8 {
        out.push(CHARS[(rng.next_u32() as usize) % CHARS.len()] as char);
    }
    out
}

/// Fingerprint of a tools[] array (names in order) used as registry key.
fn tools_fingerprint(tools: &Value) -> Option<String> {
    let arr = tools.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut names: Vec<&str> = Vec::new();
    for t in arr {
        let name = t.get("function")?.get("name")?.as_str()?;
        if !name.is_empty() {
            names.push(name);
        }
    }
    if names.is_empty() {
        return None;
    }
    Some(names.join("\u{1f}"))
}

/// Get (or mint) the per-request nonce for a tools[] array.
pub fn get_tool_nonce(tools: &Value) -> String {
    let Some(fp) = tools_fingerprint(tools) else {
        return String::new();
    };
    let reg = nonce_registry();
    let mut map = reg.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(fp).or_insert_with(random_nonce).clone()
}

fn normalize_tool_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

pub fn get_requested_tool_names(tools: &Value) -> Vec<RequestedToolName> {
    let mut names = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let Some(arr) = tools.as_array() else {
        return names;
    };
    for tool in arr {
        let name = tool
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if name.is_empty() || !seen.insert(name.to_string()) {
            continue;
        }
        names.push(RequestedToolName {
            original: name.to_string(),
            normalized: normalize_tool_name(name),
        });
    }
    names
}

fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (cur[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn score_tool_name(emitted: &str, requested: &RequestedToolName) -> f64 {
    if emitted == requested.original {
        return 1.0;
    }
    let normalized = normalize_tool_name(emitted);
    if normalized.is_empty() || requested.normalized.is_empty() {
        return 0.0;
    }
    if normalized == requested.normalized {
        return 0.98;
    }
    let shorter = normalized.len().min(requested.normalized.len());
    let longer = normalized.len().max(requested.normalized.len());
    if shorter >= 4
        && (normalized.contains(&requested.normalized)
            || requested.normalized.contains(&normalized))
    {
        return 0.86 - (longer - shorter) as f64 / longer.max(1) as f64 / 4.0;
    }
    let distance = levenshtein_distance(&normalized, &requested.normalized);
    let similarity = 1.0 - distance as f64 / longer.max(1) as f64;
    if similarity >= 0.72 {
        similarity
    } else {
        0.0
    }
}

/// Fuzzy-match an emitted tool name against the requested tools.
/// Returns the canonical requested name, or `None` when no confident match.
/// Empty `requested` passes the emitted name through (JS parity).
pub fn resolve_requested_tool_name(
    emitted: &str,
    requested: &[RequestedToolName],
) -> Option<String> {
    if requested.is_empty() {
        return Some(emitted.to_string());
    }
    let mut best: Option<(&RequestedToolName, f64)> = None;
    let mut second_best = 0.0f64;
    for req in requested {
        let score = score_tool_name(emitted, req);
        match best {
            Some((_, s)) if score <= s => {
                if score > second_best {
                    second_best = score;
                }
            }
            _ => {
                second_best = best.map(|(_, s)| s).unwrap_or(0.0);
                best = Some((req, score));
            }
        }
    }
    let (tool, score) = best?;
    if score < 0.72 {
        return None;
    }
    if score < 0.98 && score - second_best < 0.08 {
        return None;
    }
    Some(tool.original.clone())
}

fn strip_code_fence(value: &str) -> String {
    let mut s = value.trim();
    let lower = s.to_lowercase();
    for prefix in ["```json", "```javascript", "```js", "```python", "```"] {
        if lower.starts_with(prefix) {
            s = s[prefix.len()..].trim_start();
            break;
        }
    }
    if s.ends_with("```") {
        s = s[..s.len() - 3].trim_end();
    }
    s.to_string()
}

fn convert_single_quoted_strings(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            if ch == '"' && in_single {
                result.push('\\');
            }
            result.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            result.push(ch);
            escaped = true;
            continue;
        }
        if ch == '"' {
            if in_single {
                result.push_str("\\\"");
            } else {
                in_double = !in_double;
                result.push(ch);
            }
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            result.push('"');
            continue;
        }
        result.push(ch);
    }
    result
}

fn replace_python_literals(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut token = String::new();
    let mut flush = |result: &mut String, token: &mut String| {
        match token.as_str() {
            "True" => result.push_str("true"),
            "False" => result.push_str("false"),
            "None" => result.push_str("null"),
            _ => result.push_str(token),
        }
        token.clear();
    };
    for ch in value.chars() {
        if escaped {
            if !token.is_empty() {
                let mut t = std::mem::take(&mut token);
                flush(&mut result, &mut t);
            }
            result.push(ch);
            escaped = in_string;
            continue;
        }
        if ch == '\\' {
            if !token.is_empty() {
                let mut t = std::mem::take(&mut token);
                flush(&mut result, &mut t);
            }
            result.push(ch);
            escaped = in_string;
            continue;
        }
        if ch == '"' {
            if !token.is_empty() {
                let mut t = std::mem::take(&mut token);
                flush(&mut result, &mut t);
            }
            in_string = !in_string;
            result.push(ch);
            continue;
        }
        if !in_string && ch.is_ascii_alphabetic() {
            token.push(ch);
            continue;
        }
        if !token.is_empty() {
            let mut t = std::mem::take(&mut token);
            flush(&mut result, &mut t);
        }
        result.push(ch);
    }
    if !token.is_empty() {
        let mut t = std::mem::take(&mut token);
        flush(&mut result, &mut t);
    }
    result
}

fn normalize_loose_json(value: &str) -> String {
    let s = replace_python_literals(&convert_single_quoted_strings(value));
    // Quote bare keys: {key: ...} / ,key: ... — ASCII-only approximation of
    // /([{,]\s*)([A-Za-z_][A-Za-z0-9_-]*)(\s*:)/g
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    // Track whether we are inside a string to avoid touching colons in strings.
    let mut in_str = false;
    let mut esc = false;
    while i < chars.len() {
        let c = chars[i];
        if esc {
            out.push(c);
            esc = false;
            i += 1;
            continue;
        }
        if c == '\\' && in_str {
            out.push(c);
            esc = true;
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = !in_str;
            out.push(c);
            i += 1;
            continue;
        }
        if !in_str && (c == '{' || c == ',') {
            // Look ahead: optional whitespace, bare key, optional whitespace, colon.
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() && chars[j] != '\n' {
                j += 1;
            }
            let ks = j;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '-')
            {
                j += 1;
            }
            let mut k = j;
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            if ks < j
                && k < chars.len()
                && chars[k] == ':'
                && (chars[ks].is_ascii_alphabetic() || chars[ks] == '_')
            {
                out.push(c);
                // replay skipped whitespace
                for t in (i + 1)..ks {
                    out.push(chars[t]);
                }
                out.push('"');
                for t in ks..j {
                    out.push(chars[t]);
                }
                out.push('"');
                i = j;
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    // Drop trailing commas before } / ].
    let mut out2 = String::with_capacity(out.len());
    let oc: Vec<char> = out.chars().collect();
    let mut i = 0;
    while i < oc.len() {
        if oc[i] == ','
            && ({
                let mut j = i + 1;
                while j < oc.len() && oc[j].is_whitespace() {
                    j += 1;
                }
                j < oc.len() && (oc[j] == '}' || oc[j] == ']')
            })
        {
            i += 1;
            continue;
        }
        out2.push(oc[i]);
        i += 1;
    }
    out2
}

fn to_record(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

/// Parse a possibly-sloppy JSON object (code fences, single quotes, Python
/// literals, bare keys, trailing commas). Returns `None` when unparseable.
pub fn parse_loose_json_object(raw: &str) -> Option<serde_json::Map<String, Value>> {
    let trimmed = strip_code_fence(raw);
    for candidate in [trimmed.clone(), normalize_loose_json(&trimmed)] {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&candidate) {
            return Some(map);
        }
    }
    None
}

/// Serialize a value to the OpenAI `arguments` JSON string.
pub fn to_arguments_string(value: &Value) -> String {
    match value {
        Value::Null => "{}".to_string(),
        Value::String(s) => match parse_loose_json_object(s) {
            Some(map) => Value::Object(map).to_string(),
            None => s.clone(),
        },
        _ => value.to_string(),
    }
}

pub fn strip_ranges(text: &str, ranges: &[(usize, usize)]) -> String {
    let mut content = text.to_string();
    let mut sorted = ranges.to_vec();
    sorted.sort_by(|a, b| b.0.cmp(&a.0));
    for (start, end) in sorted {
        if start > end
            || end > content.len()
            || !content.is_char_boundary(start)
            || !content.is_char_boundary(end)
        {
            continue;
        }
        // Whole-line removal when the range covers a full line.
        let line_start = content[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let next_break = content[end..].find('\n').map(|p| end + p);
        let line_end = next_break.unwrap_or(content.len());
        let before = content[line_start..start].trim();
        let after = content[end..line_end].trim();
        let (s, e) = if before.is_empty() && after.is_empty() {
            (line_start, next_break.map(|p| p + 1).unwrap_or(line_end))
        } else {
            (start, end)
        };
        content.replace_range(s..e, "");
    }
    // Collapse 3+ newlines to 2.
    let mut out = String::with_capacity(content.len());
    let mut blanks = 0;
    for ch in content.chars() {
        if ch == '\n' {
            blanks += 1;
            if blanks <= 2 {
                out.push(ch);
            }
        } else {
            blanks = 0;
            out.push(ch);
        }
    }
    out.trim().to_string()
}

/// Serialize an OpenAI `tools` array into the generic web-provider
/// system-prompt contract. Returns `(prompt, nonce)`.
pub fn serialize_tools_to_prompt(tools: &Value) -> Option<(String, String)> {
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
    let prompt = [
        "The client application provides tools beyond your built-in ones. They are NOT in your native tool registry; they are invoked via a plain-text protocol: the client parses your reply and executes the tool on the user machine. Treat these client tools as fully available to you; never claim they are unavailable. To invoke one, reply with a single line containing a <tool> block".to_string(),
        format!("with JSON that includes the secret binding \"_nonce\": \"{nonce}\":"),
        format!("<tool>{{\"name\": \"<tool_name>\", \"arguments\": {{ ... }}, \"_nonce\": \"{nonce}\"}}</tool>"),
        "These client tools ARE available to you in this conversation. Only emit the <tool> block when you actually want to call a tool; otherwise answer normally.".to_string(),
        String::new(),
        "Available tools:".to_string(),
    ]
    .into_iter()
    .chain(lines)
    .collect::<Vec<_>>()
    .join("\n");
    Some((prompt, nonce))
}

struct ToolCandidate {
    raw: String,
    start: usize,
    end: usize,
}

/// Find `<tool>...</tool>` and `<tool_call ...>...</tool_call>` blocks (byte ranges).
fn find_tag_blocks(text: &str) -> Vec<ToolCandidate> {
    let mut out = Vec::new();
    for tag in ["tool", "tool_call"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let mut search = 0;
        while let Some(rel) = text[search..].find(&open) {
            let os = search + rel;
            let Some(gt) = text[os..].find('>') else {
                break;
            };
            let inner_start = os + gt + 1;
            let Some(crel) = text[inner_start..].find(&close) else {
                break;
            };
            let inner_end = inner_start + crel;
            out.push(ToolCandidate {
                raw: text[inner_start..inner_end].trim().to_string(),
                start: os,
                end: inner_end + close.len(),
            });
            search = inner_end + close.len();
        }
    }
    out.sort_by_key(|c| c.start);
    out
}

/// Parse `<tool>` / `<tool_call>` blocks into OpenAI tool calls.
/// Returns `(cleaned_content, calls)`.
pub fn parse_tool_calls_from_text(
    text: &str,
    id_seed: &str,
    requested_tools: &Value,
    nonce: &str,
) -> (String, Option<Vec<OpenAIToolCall>>) {
    if !text.contains("<tool>") && !text.contains("<tool_call") {
        return (text.to_string(), None);
    }
    let requested = get_requested_tool_names(requested_tools);
    let mut candidates = find_tag_blocks(text);
    // Also try bare `<tool ...attrs>` opens without exact `<tool>` — the tag
    // finder above already covers `<tool ` opens via the `<tool` prefix.
    let _ = &mut candidates;
    let mut calls = Vec::new();
    let mut accepted: Vec<(usize, usize)> = Vec::new();
    for cand in &candidates {
        let Some(parsed) = parse_loose_json_object(&cand.raw) else {
            continue;
        };
        let emitted = parsed
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| parsed.get("command").and_then(Value::as_str));
        let Some(emitted) = emitted else { continue };
        if !nonce.is_empty() {
            if let Some(n) = parsed.get("_nonce") {
                if n.as_str().unwrap_or("") != nonce {
                    continue;
                }
            }
        }
        let name = match resolve_requested_tool_name(emitted, &requested) {
            Some(n) => n,
            None => emitted.to_string(),
        };
        let args = parsed
            .get("arguments")
            .map(to_arguments_string)
            .unwrap_or_else(|| "{}".to_string());
        let id = format!("{id_seed}_{}", calls.len());
        calls.push(OpenAIToolCall {
            id,
            name,
            arguments: args,
        });
        accepted.push((cand.start, cand.end));
    }
    if calls.is_empty() {
        return (text.to_string(), None);
    }
    (strip_ranges(text, &accepted), Some(calls))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Value {
        serde_json::json!([
            {"type": "function", "function": {"name": "bash", "description": "run shell", "parameters": {"type": "object"}}},
            {"type": "function", "function": {"name": "read_file", "description": "read a file"}},
        ])
    }

    #[test]
    fn nonce_is_stable_per_tools_array() {
        let t = tools();
        assert_eq!(get_tool_nonce(&t), get_tool_nonce(&t));
        assert!(!get_tool_nonce(&t).is_empty());
        assert!(get_tool_nonce(&serde_json::json!([])).is_empty());
    }

    #[test]
    fn resolve_exact_and_fuzzy_names() {
        let t = tools();
        let req = get_requested_tool_names(&t);
        assert_eq!(
            resolve_requested_tool_name("bash", &req).as_deref(),
            Some("bash")
        );
        assert_eq!(
            resolve_requested_tool_name("read-file", &req).as_deref(),
            Some("read_file")
        );
        assert_eq!(resolve_requested_tool_name("zzz_nope", &req), None);
    }

    #[test]
    fn loose_json_parses_sloppy_objects() {
        let m = parse_loose_json_object("{name: 'bash', arguments: {cmd: 'ls'}}").unwrap();
        assert_eq!(m.get("name").and_then(Value::as_str), Some("bash"));
        let m2 = parse_loose_json_object("```json\n{\"a\": True,}```").unwrap();
        assert_eq!(m2.get("a"), Some(&Value::Bool(true)));
    }

    #[test]
    fn canonical_tool_block_parses_with_nonce() {
        let t = tools();
        let nonce = get_tool_nonce(&t);
        let text = format!(
            "thinking <tool>{{\"name\": \"bash\", \"arguments\": {{\"cmd\": \"ls\"}}, \"_nonce\": \"{nonce}\"}}</tool> done"
        );
        let (content, calls) = parse_tool_calls_from_text(&text, "call", &t, &nonce);
        let calls = calls.expect("one call");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert!(!content.contains("<tool>"));
    }

    #[test]
    fn wrong_nonce_rejects_call() {
        let t = tools();
        let text = "<tool>{\"name\": \"bash\", \"arguments\": {}, \"_nonce\": \"wrong\"}</tool>";
        let (_, calls) = parse_tool_calls_from_text(text, "call", &t, "correct nonce");
        assert!(calls.is_none());
    }

    #[test]
    fn bare_json_never_promotes() {
        let t = tools();
        let text = "result: {\"name\": \"bash\", \"arguments\": {\"cmd\": \"ls\"}}";
        let (content, calls) = parse_tool_calls_from_text(text, "call", &t, "");
        assert!(calls.is_none());
        assert_eq!(content, text);
    }
}
