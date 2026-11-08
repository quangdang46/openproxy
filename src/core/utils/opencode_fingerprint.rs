//! OpenCode free-tier client fingerprint helpers.
//!
//! Port of `open-sse/utils/opencodeFingerprint.js` (9router v0.5.111+,
//! commits 5bdcd5e7 / 483c10f5). Two body-side gates are mirrored:
//!
//! - Tools: free-tier requests must declare OpenCode's own `read` and
//!   `shell` tools with their exact descriptions and parameter schemas;
//!   stubbed descriptions, renamed schemas or tool_choice "none" are
//!   answered with 403 FreeTierError. Caller tools are sent untouched. A
//!   tool that already uses an exact fingerprint name with a different
//!   definition adopts OpenCode's definition in place whenever its
//!   arguments can carry them; only a genuinely different schema travels
//!   under an alias, and the response side restores that caller's
//!   original name.
//! - System prompt: the CLI's `<env>` marker must be present. A request
//!   missing either half (marker or tools) answers 429
//!   FreeUsageLimitError regardless of headers, UA or session shape; both
//!   halves together pass.
//!
//! Transport is a third gate (TLS ClientHello) and lives with the
//! executor — see `executor/opencode.rs`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::{json, Value};

/// Names required by the upstream free-tier gate.
pub const OPENCODE_FINGERPRINT_TOOLS: &[&str] = &["read", "shell"];

/// Verbatim from the OpenCode v2.0.26 CLI. Upstream compares description
/// and parameters, so do not reword or reformat these.
fn fingerprint_tool_definition(key: &str) -> (&'static str, Value) {
    match key {
        "read" => (
            "Read the contents of a file or directory. Supports text files, images, and PDFs. Images and PDFs are presented directly to the model. Each text line is prefixed by its 1-based line number as <line>: <content>. The prefix is for reference and is not part of the file content. Directory entries are returned one per line. Use offset and limit to read large files or directories in sections. Prefer one larger read over many small slices, and use grep to find specific content in large files.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File or directory to read" },
                    "offset": { "type": "integer", "minimum": 0, "description": "The line or directory entry to start reading from (1-based)" },
                    "limit": { "type": "integer", "minimum": 0, "description": "The maximum number of lines or directory entries to read (defaults to and capped at 2000)" },
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        ),
        "shell" => (
            "Execute a shell command and return its output. Commands run on Linux using bash. Quote file paths containing spaces or special characters. Prefer dedicated tools over shell commands when possible. When output is large, the full result is saved to a file and a truncated preview is returned. Rely on automatic truncation unless filtering the output is more useful. Commands accept an optional timeout, background commands have no timeout by default. Background commands return immediately, and you will be notified when they complete.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command string to execute" },
                    "workdir": { "type": "string", "description": "Working directory to execute the command in. Defaults to the current working directory. When possible, avoid changing directories in the command and set the working directory here instead." },
                    "timeout": { "type": "integer", "minimum": 0, "description": "Timeout in milliseconds. Set to 0 to disable the timeout. Defaults to 120000 for foreground commands. Background commands have no timeout by default." },
                    "background": { "type": "boolean", "description": "Run the command in the background and return immediately (useful for dev servers and long-running builds). You do not need to use '&' at the end of the command when using this parameter. You will be notified when it completes. DO NOT poll for completion." },
                },
                "required": ["command"],
                "additionalProperties": false,
            }),
        ),
        _ => unreachable!("fingerprint keys are read|shell"),
    }
}

/// The fingerprint name when `name` is exactly one; `None` otherwise.
pub fn fingerprint_tool_key(name: &str) -> Option<&'static str> {
    let trimmed = name.trim();
    OPENCODE_FINGERPRINT_TOOLS
        .iter()
        .copied()
        .find(|k| *k == trimmed)
}

/// Read a tool name from either flat ({name}) or chat ({function:{name}}) shape.
fn tool_name_of(tool: &Value) -> String {
    if !tool.is_object() {
        return String::new();
    }
    if let Some(name) = tool.get("name").and_then(Value::as_str) {
        let t = name.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    tool.get("function")
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn chat_function_of(tool: &Value) -> Option<&serde_json::Map<String, Value>> {
    tool.get("function")
        .and_then(Value::as_object)
        .filter(|_| !tool.get("function").is_some_and(Value::is_array))
}

/// Field accessor that sees through the chat `function` wrapper.
fn def_get<'a>(tool: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(f) = chat_function_of(tool) {
        f.get(key)
    } else {
        tool.get(key)
    }
}

/// True when `tool` already carries OpenCode's exact definition for `key`.
fn is_fingerprint_definition(tool: &Value, key: &str) -> bool {
    let (want_desc, want_params) = fingerprint_tool_definition(key);
    let desc_ok = def_get(tool, "description")
        .and_then(Value::as_str)
        .is_some_and(|d| d == want_desc);
    let params_ok = def_get(tool, "parameters")
        .is_some_and(|p| p == &want_params);
    desc_ok && params_ok
}

fn with_tool_name(tool: &Value, name: &str) -> Value {
    let mut out = tool.clone();
    if let Some(obj) = out.as_object_mut() {
        if obj.contains_key("function") {
            if let Some(f) = obj.get_mut("function").and_then(Value::as_object_mut) {
                f.insert("name".to_string(), Value::String(name.to_string()));
            }
        } else {
            obj.insert("name".to_string(), Value::String(name.to_string()));
        }
    }
    out
}

/// True when `tool` already speaks OpenCode's parameter names for `key`,
/// so the fingerprint definition can replace its own without hiding or
/// adding an argument the caller's handler expects.
///
/// This is the common case, not an edge: OpenCode composes `shell`'s
/// description from `process.platform`, so the very same tool run on
/// Windows reports "Commands run on Windows using pwsh." while the
/// captured definition says "Commands run on Linux using bash." — a
/// two-word difference that an exact string compare rejects.
fn can_normalize_in_place(tool: &Value, key: &str) -> bool {
    let Some(props) = def_get(tool, "parameters")
        .and_then(|p| p.get("properties"))
        .and_then(Value::as_object)
    else {
        return false;
    };
    let (_, want_params) = fingerprint_tool_definition(key);
    let wanted: Vec<&str> = want_params["properties"]
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if props
        .keys()
        .any(|name| !wanted.contains(&name.as_str()))
    {
        return false;
    }
    want_params["required"]
        .as_array()
        .map(|req| {
            req.iter().filter_map(Value::as_str).all(|name| props.contains_key(name))
        })
        .unwrap_or(false)
}

/// Replace `tool`'s definition with OpenCode's while keeping its name, so
/// the wire carries a single, gate-satisfying tool instead of a caller
/// tool plus a placeholder of the same purpose.
fn with_fingerprint_definition(tool: &Value, key: &str) -> Value {
    let (desc, params) = fingerprint_tool_definition(key);
    let mut out = tool.clone();
    let target = if chat_function_of(&out).is_some() {
        out.get_mut("function")
    } else {
        Some(&mut out)
    };
    if let Some(obj) = target.and_then(Value::as_object_mut) {
        obj.insert(
            "description".to_string(),
            Value::String(desc.to_string()),
        );
        obj.insert("parameters".to_string(), params);
    }
    out
}

/// Keep caller tools on the fingerprint names whenever their arguments
/// can carry OpenCode's, and move the remaining ones off them so the
/// exact definitions can be appended without a duplicate name.
/// Everything else, including case variants such as `Read`, is preserved
/// verbatim; upstream accepts them alongside `read`.
///
/// Returns the rename map: sent (alias) name → original name.
pub fn conceal_fingerprint_tool_names(tools: &mut Vec<Value>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if tools.is_empty() {
        return map;
    }

    let taken: std::collections::HashSet<String> = tools
        .iter()
        .map(tool_name_of)
        .filter(|n| !n.is_empty())
        .collect();
    let mut taken = taken;
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Value> = Vec::with_capacity(tools.len());

    for tool in tools.drain(..) {
        if !tool.is_object() {
            out.push(tool);
            continue;
        }
        let name = tool_name_of(&tool);
        let Some(key) = fingerprint_tool_key(&name) else {
            out.push(tool);
            continue;
        };
        // Duplicate declarations of one name are rejected upstream; keep the first.
        if !seen.insert(key.to_string()) {
            continue;
        }
        if is_fingerprint_definition(&tool, key) {
            out.push(tool);
            continue;
        }
        // Same tool, different wording. Adopt OpenCode's definition in
        // place: the caller keeps its own name, so there is no alias to
        // restore, no alias name for the model to collide with, and no
        // placeholder to explain away.
        if can_normalize_in_place(&tool, key) {
            out.push(with_fingerprint_definition(&tool, key));
            continue;
        }
        let mut alias = format!("{key}_client");
        let mut i = 2;
        while taken.contains(&alias) {
            alias = format!("{key}_client{i}");
            i += 1;
        }
        taken.insert(alias.clone());
        map.insert(alias.clone(), key.to_string());
        out.push(with_tool_name(&tool, &alias));
    }
    *tools = out;
    map
}

/// Append OpenCode's definition for each fingerprint tool not already
/// declared. Returns the names that were appended.
pub fn append_missing_fingerprint_tools(tools: &mut Vec<Value>, flat: bool) -> Vec<String> {
    let mut appended = Vec::new();
    for name in OPENCODE_FINGERPRINT_TOOLS {
        let already = tools.iter().any(|tool| {
            fingerprint_tool_key(&tool_name_of(tool)) == Some(*name)
        });
        if already {
            continue;
        }
        let (desc, params) = fingerprint_tool_definition(name);
        if flat {
            tools.push(json!({
                "type": "function",
                "name": name,
                "description": desc,
                "parameters": params,
                "strict": false,
            }));
        } else {
            tools.push(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": desc,
                    "parameters": params,
                },
            }));
        }
        appended.push(name.to_string());
    }
    appended
}

/// Invert a sent → original rename map (alias → original becomes
/// original → alias).
fn aliases_by_original(map: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    map.iter().map(|(alias, original)| (original.clone(), alias.clone())).collect()
}

/// Point an explicit tool_choice at the alias of a concealed caller tool.
pub fn retarget_tool_choice(body: &mut Value, map: &BTreeMap<String, String>) {
    if map.is_empty() {
        return;
    }
    let Some(choice) = body.get("tool_choice") else {
        return;
    };
    if !choice.is_object() {
        return;
    }
    let aliases = aliases_by_original(map);
    let mut new_choice: Option<Value> = None;
    if let Some(name) = choice.get("name").and_then(Value::as_str) {
        if let Some(alias) = aliases.get(name.trim()) {
            let mut c = choice.clone();
            if let Some(obj) = c.as_object_mut() {
                obj.insert("name".to_string(), Value::String(alias.clone()));
            }
            new_choice = Some(c);
        }
    } else if let Some(f) = choice.get("function") {
        if let Some(fname) = f.get("name").and_then(Value::as_str) {
            if let Some(alias) = aliases.get(fname.trim()) {
                let mut c = choice.clone();
                if let Some(fobj) = c.get_mut("function").and_then(Value::as_object_mut) {
                    fobj.insert("name".to_string(), Value::String(alias.clone()));
                }
                new_choice = Some(c);
            }
        }
    }
    if let Some(c) = new_choice {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("tool_choice".to_string(), c);
        }
    }
}

/// Rename prior-turn calls to concealed caller tools as well; otherwise
/// the model reads its own earlier calls as calls to OpenCode's tool of
/// the same name.
pub fn alias_history_tool_calls(body: &mut Value, map: &BTreeMap<String, String>) {
    if map.is_empty() {
        return;
    }
    let aliases = aliases_by_original(map);
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in input.iter_mut() {
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    if let Some(alias) = aliases.get(name) {
                        let alias = alias.clone();
                        if let Some(obj) = item.as_object_mut() {
                            obj.insert("name".to_string(), Value::String(alias));
                        }
                    }
                }
            }
        }
    }
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for msg in messages.iter_mut() {
            let Some(calls) = msg.get_mut("tool_calls").and_then(Value::as_array_mut) else {
                continue;
            };
            for call in calls.iter_mut() {
                let fname = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(orig) = fname.and_then(|n| aliases.get(&n).cloned()) {
                    if let Some(fobj) = call.get_mut("function").and_then(Value::as_object_mut) {
                        fobj.insert("name".to_string(), Value::String(orig));
                    }
                }
            }
        }
    }
}

fn append_system_text(system: &Value, text: &str) -> Value {
    match system {
        Value::String(s) => {
            if s.trim().is_empty() {
                Value::String(text.to_string())
            } else {
                Value::String(format!("{s}\n\n{text}"))
            }
        }
        Value::Array(blocks) => {
            let mut blocks = blocks.clone();
            for i in (0..blocks.len()).rev() {
                let is_text = blocks[i]
                    .get("text")
                    .is_some_and(Value::is_string);
                if is_text {
                    let existing = blocks[i]["text"].as_str().unwrap_or("");
                    let new_text = if existing.trim().is_empty() {
                        text.to_string()
                    } else {
                        format!("{existing}\n\n{text}")
                    };
                    if let Some(obj) = blocks[i].as_object_mut() {
                        obj.insert("text".to_string(), Value::String(new_text));
                    }
                    return Value::Array(blocks);
                }
            }
            blocks.push(json!({ "type": "text", "text": text }));
            Value::Array(blocks)
        }
        _ => json!([{ "type": "text", "text": text }]),
    }
}

/// Without the old "must not be used" descriptions the model does call
/// the appended tools (live probe: `read` on "what files are in /etc?"),
/// so tell it in the system prompt instead. Upstream does not gate on
/// the prompt text. Wording matters: "not available" made the model
/// also refuse the caller's own Read/Bash; "placeholders ... use the
/// other tools" kept those working.
fn note_unavailable_tools(body: &mut Value, flat: bool, names: &[String], claude: bool) {
    let list = names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(" or ");
    let note = if names.len() > 1 {
        format!(
            "Do not call the {list} tools; they are placeholders that do nothing. Use the other tools you have been given instead."
        )
    } else {
        format!(
            "Do not call the {list} tool; it is a placeholder that does nothing. Use the other tools you have been given instead."
        )
    };
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    if flat {
        let instructions = obj.get("instructions").cloned().unwrap_or(Value::Null);
        let new_instructions = match instructions.as_str() {
            Some(s) if !s.trim().is_empty() => format!("{s}\n\n{note}"),
            _ => note,
        };
        obj.insert(
            "instructions".to_string(),
            Value::String(new_instructions),
        );
        return;
    }
    // Claude-shaped bodies carry `system` instead; /messages rejects role:"system".
    if claude || obj.contains_key("system") {
        let system = obj.get("system").cloned().unwrap_or(Value::Null);
        obj.insert("system".to_string(), append_system_text(&system, &note));
        return;
    }
    let messages = match obj.get_mut("messages").and_then(Value::as_array_mut) {
        Some(m) => m,
        None => return,
    };
    let first_is_system_string = messages
        .first()
        .is_some_and(|m| m.get("role").and_then(Value::as_str) == Some("system")
            && m.get("content").is_some_and(Value::is_string));
    if first_is_system_string {
        let first_content = messages[0]["content"].as_str().unwrap_or("").to_string();
        let new_content = format!("{first_content}\n\n{note}");
        if let Some(mobj) = messages[0].as_object_mut() {
            mobj.insert("content".to_string(), Value::String(new_content));
        }
    } else {
        messages.insert(0, json!({ "role": "system", "content": note }));
    }
}

// The marker gate reads text only: a foreign system prompt plus this
// exact block passes, so the marker is appended to the caller's own
// prompt rather than replacing it. Values mirror a real CLI instance on
// this host (JS evaluates os.tmpdir()/cwd/platform once at module load;
// OnceLock gives the same stability here).
const ENV_TAG: &str = "<env>";
const ENV_HEADING: &str =
    "Here is some useful information about the environment you are running in:";

fn env_fingerprint_prompt() -> &'static str {
    static PROMPT: OnceLock<String> = OnceLock::new();
    PROMPT.get_or_init(|| {
        let tmp_dir = std::env::temp_dir().join("opencode");
        let working_dir = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());
        let platform = match std::env::consts::OS {
            "macos" => "darwin",
            other => other,
        };
        [
            ENV_HEADING.to_string(),
            ENV_TAG.to_string(),
            format!("  Working directory: {working_dir}"),
            format!("  Workspace root folder: {working_dir}"),
            "  Is directory a git repo: no".to_string(),
            format!("  Platform: {platform}"),
            format!(
                "  Prefer {} over generic system temporary directories such as /tmp; it is pre-created and approved for external access.",
                tmp_dir.display()
            ),
            "</env>".to_string(),
        ]
        .join("\n")
    })
}

/// True when the marker is already present; a genuine opencode client
/// is passthrough.
fn system_carries_env(system: &Value) -> bool {
    match system {
        Value::String(s) => s.contains(ENV_TAG),
        Value::Array(blocks) => blocks.iter().any(|b| {
            b.get("text")
                .and_then(Value::as_str)
                .is_some_and(|t| t.contains(ENV_TAG))
        }),
        _ => false,
    }
}

/// Append the CLI's environment marker without replacing the caller's
/// content, covering all three request shapes: Responses (`instructions`),
/// chat (`messages[]` system) and claude-top-level (`system`). Fail-open —
/// the marker is an enhancement, so a surprising body is left untouched.
///
/// Returns true when the marker was added.
pub fn apply_env_fingerprint(body: &mut Value, flat: bool, claude: bool) -> bool {
    let prompt = env_fingerprint_prompt();
    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    if flat {
        if obj.contains_key("instructions") && !obj["instructions"].is_string() {
            return false;
        }
        let instructions = obj.get("instructions").cloned().unwrap_or(Value::Null);
        if system_carries_env(&instructions) {
            return false;
        }
        let new_instructions = match instructions.as_str() {
            Some(s) if !s.trim().is_empty() => format!("{s}\n\n{prompt}"),
            _ => prompt.to_string(),
        };
        obj.insert(
            "instructions".to_string(),
            Value::String(new_instructions),
        );
        return true;
    }
    if claude || obj.contains_key("system") {
        let system = obj.get("system").cloned().unwrap_or(Value::Null);
        if system_carries_env(&system) {
            return false;
        }
        obj.insert("system".to_string(), append_system_text(&system, prompt));
        return true;
    }
    let messages = match obj.get("messages").and_then(Value::as_array) {
        Some(m) => m,
        None => return false,
    };
    if let Some(idx) = messages.iter().position(|m| {
        m.get("role").and_then(Value::as_str) == Some("system")
    }) {
        let target = &messages[idx];
        let content = target.get("content").cloned().unwrap_or(Value::Null);
        if system_carries_env(&content) {
            return false;
        }
        let new_content = append_system_text(&content, prompt);
        if let Some(mobj) = obj
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .and_then(|arr| arr.get_mut(idx))
            .and_then(Value::as_object_mut)
        {
            mobj.insert("content".to_string(), new_content);
        }
        return true;
    }
    let msgs = obj
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .expect("messages checked above");
    msgs.insert(0, json!({ "role": "system", "content": prompt }));
    true
}

/// Full request-side pass: normalize caller tools that collide with a
/// fingerprint name, append OpenCode's definitions for any that are
/// missing, and tell the model the appended ones are off-limits.
///
/// Returns the rename map: sent name → original name.
pub fn apply_fingerprint_tools(body: &mut Value, flat: bool, claude: bool) -> BTreeMap<String, String> {
    let Some(obj) = body.as_object_mut() else {
        return BTreeMap::new();
    };
    let mut tools = obj
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let map = conceal_fingerprint_tool_names(&mut tools);
    let appended = append_missing_fingerprint_tools(&mut tools, flat);
    obj.insert("tools".to_string(), Value::Array(tools));
    retarget_tool_choice(body, &map);
    alias_history_tool_calls(body, &map);
    if !appended.is_empty() {
        note_unavailable_tools(body, flat, &appended, claude);
    }
    // The `<env>` marker is required on every free-lane request, with or without tools.
    apply_env_fingerprint(body, flat, claude);
    // Responses keeps its explicit auto default. Never default to "none":
    // upstream answers tool_choice "none" with 403 FreeTierError.
    if flat {
        let has_choice = body.get("tool_choice").is_some();
        if !has_choice {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("tool_choice".to_string(), Value::String("auto".to_string()));
            }
        }
    }
    map
}

// Response side -------------------------------------------------------------

/// Restore caller tool spellings in supported response/event shapes.
/// `map` is sent (alias) name → original name, as produced by
/// [`apply_fingerprint_tools`].
pub fn restore_tool_names(payload: &Value, map: &BTreeMap<String, String>) -> Value {
    if map.is_empty() {
        return payload.clone();
    }
    if let Value::Array(items) = payload {
        return Value::Array(
            items
                .iter()
                .map(|item| restore_tool_names(item, map))
                .collect(),
        );
    }
    let Some(obj) = payload.as_object() else {
        return payload.clone();
    };
    let mut out = obj.clone();

    // Claude streaming content_block_start event.
    if payload.get("type").and_then(Value::as_str) == Some("content_block_start") {
        if let Some(block) = payload.get("content_block") {
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                if let Some(name) = block.get("name").and_then(Value::as_str) {
                    if let Some(orig) = map.get(name) {
                        let mut b = block.clone();
                        if let Some(bobj) = b.as_object_mut() {
                            bobj.insert("name".to_string(), Value::String(orig.clone()));
                        }
                        out.insert("content_block".to_string(), b);
                    }
                }
            }
        }
    }

    // Claude non-streaming message body.
    if let Some(content) = payload.get("content").and_then(Value::as_array) {
        let new_content: Vec<Value> = content
            .iter()
            .map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    if let Some(name) = block.get("name").and_then(Value::as_str) {
                        if let Some(orig) = map.get(name) {
                            let mut b = block.clone();
                            if let Some(bobj) = b.as_object_mut() {
                                bobj.insert("name".to_string(), Value::String(orig.clone()));
                            }
                            return b;
                        }
                    }
                }
                block.clone()
            })
            .collect();
        out.insert("content".to_string(), Value::Array(new_content));
    }

    // OpenAI Chat Completions, both streaming delta and JSON message shapes.
    if let Some(choices) = payload.get("choices").and_then(Value::as_array) {
        let new_choices: Vec<Value> = choices
            .iter()
            .map(|choice| {
                let mut next = choice.clone();
                for holder in ["delta", "message"] {
                    let Some(value) = choice.get(holder) else {
                        continue;
                    };
                    let Some(calls) = value.get("tool_calls").and_then(Value::as_array) else {
                        continue;
                    };
                    if calls.is_empty() {
                        continue;
                    }
                    let new_calls: Vec<Value> = calls
                        .iter()
                        .map(|call| {
                            let name = call
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(Value::as_str);
                            if let Some(orig) = name.and_then(|n| map.get(n)) {
                                let mut c = call.clone();
                                if let Some(fobj) =
                                    c.get_mut("function").and_then(Value::as_object_mut)
                                {
                                    fobj.insert(
                                        "name".to_string(),
                                        Value::String(orig.clone()),
                                    );
                                }
                                c
                            } else {
                                call.clone()
                            }
                        })
                        .collect();
                    if let Some(vobj) = next.get_mut(holder).and_then(Value::as_object_mut) {
                        vobj.insert("tool_calls".to_string(), Value::Array(new_calls));
                    }
                }
                next
            })
            .collect();
        out.insert("choices".to_string(), Value::Array(new_choices));
    }

    // OpenAI Responses final JSON body.
    if let Some(output) = payload.get("output").and_then(Value::as_array) {
        let new_output: Vec<Value> = output
            .iter()
            .map(|item| {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    if let Some(name) = item.get("name").and_then(Value::as_str) {
                        if let Some(orig) = map.get(name) {
                            let mut it = item.clone();
                            if let Some(iobj) = it.as_object_mut() {
                                iobj.insert("name".to_string(), Value::String(orig.clone()));
                            }
                            return it;
                        }
                    }
                }
                item.clone()
            })
            .collect();
        out.insert("output".to_string(), Value::Array(new_output));
    }

    // OpenAI Responses SSE events such as response.output_item.added/done.
    if let Some(item) = payload.get("item") {
        if item.get("type").and_then(Value::as_str) == Some("function_call") {
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                if let Some(orig) = map.get(name) {
                    let mut it = item.clone();
                    if let Some(iobj) = it.as_object_mut() {
                        iobj.insert("name".to_string(), Value::String(orig.clone()));
                    }
                    out.insert("item".to_string(), it);
                }
            }
        }
    }

    Value::Object(out)
}

/// Apply [`restore_tool_names`] to every `data:` JSON payload inside an
/// SSE chunk string (or a bare JSON body). Non-JSON / non-data lines pass
/// through untouched. Mirrors the upstream call sites in
/// `open-sse/utils/stream.js` and `open-sse/translator/index.js`.
pub fn restore_tool_names_in_sse(chunk: &str, map: &BTreeMap<String, String>) -> String {
    if map.is_empty() || chunk.is_empty() {
        return chunk.to_string();
    }
    let mut out = String::with_capacity(chunk.len());
    for line in chunk.split_inclusive('\n') {
        // Split off the line terminator so it is preserved verbatim.
        let (content, ending) = if let Some(stripped) = line.strip_suffix("\r\n") {
            (stripped, "\r\n")
        } else if let Some(stripped) = line.strip_suffix('\n') {
            (stripped, "\n")
        } else {
            (line, "")
        };
        let restored = content
            .strip_prefix("data:")
            .map(str::trim_start)
            .filter(|data| *data != "[DONE]")
            .and_then(|data| serde_json::from_str::<Value>(data).ok())
            .map(|v| {
                let r = restore_tool_names(&v, map);
                if r == v {
                    None
                } else {
                    serde_json::to_string(&r).ok()
                }
            })
            .flatten();
        match restored {
            Some(json) => {
                out.push_str("data: ");
                out.push_str(&json);
                out.push_str(ending);
            }
            None => out.push_str(line),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_tool(name: &str) -> Value {
        json!({
            "type": "function",
            "function": { "name": name, "description": "caller tool", "parameters": {"type":"object","properties":{}} }
        })
    }

    fn flat_tool(name: &str) -> Value {
        json!({
            "type": "function",
            "name": name,
            "description": "caller tool",
            "parameters": {"type":"object","properties":{}}
        })
    }

    #[test]
    fn conceal_aliases_incompatible_schema() {
        // Caller `read` with an extra param can't carry OpenCode's schema
        // in place → renamed to read_client, map sent→original.
        let mut tools = vec![json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "my read",
                "parameters": {"type":"object","properties":{"path":{"type":"string"},"extra":{"type":"string"}},"required":["path"]}
            }
        })];
        let map = conceal_fingerprint_tool_names(&mut tools);
        assert_eq!(map.get("read_client").map(String::as_str), Some("read"));
        assert_eq!(tools[0]["function"]["name"], json!("read_client"));
    }

    #[test]
    fn conceal_normalizes_in_place_when_schema_compatible() {
        // Same param names as the fingerprint def but different wording
        // (e.g. Windows pwsh description) → definition swapped in place,
        // name kept, no alias.
        let (desc, params) = fingerprint_tool_definition("shell");
        let _ = (desc, params);
        let mut tools = vec![json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "Execute a shell command and return its output. Commands run on Windows using pwsh.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "workdir": {"type": "string"},
                        "timeout": {"type": "integer"},
                        "background": {"type": "boolean"}
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        })];
        let map = conceal_fingerprint_tool_names(&mut tools);
        assert!(map.is_empty(), "in-place normalize must not alias");
        assert_eq!(tools[0]["function"]["name"], json!("shell"));
        // Description is now the verbatim Linux fingerprint text.
        let (want_desc, _) = fingerprint_tool_definition("shell");
        assert_eq!(tools[0]["function"]["description"], json!(want_desc));
    }

    #[test]
    fn conceal_keeps_exact_fingerprint_definition_untouched() {
        let (desc, params) = fingerprint_tool_definition("read");
        let mut tools = vec![json!({
            "type": "function",
            "function": { "name": "read", "description": desc, "parameters": params }
        })];
        let map = conceal_fingerprint_tool_names(&mut tools);
        assert!(map.is_empty());
        assert_eq!(tools.len(), 1);
    }

    #[test]
    fn conceal_dedupes_repeated_fingerprint_names() {
        let mut tools = vec![chat_tool("read"), chat_tool("read"), chat_tool("other")];
        let map = conceal_fingerprint_tool_names(&mut tools);
        // First `read` aliases (incompatible schema), second is dropped.
        assert_eq!(tools.len(), 2);
        assert!(map.contains_key("read_client"));
        assert!(tools.iter().any(|t| tool_name_of(t) == "other"));
    }

    #[test]
    fn append_missing_adds_both_tools_once() {
        let mut tools = vec![flat_tool("read")];
        let appended = append_missing_fingerprint_tools(&mut tools, true);
        assert_eq!(appended, vec!["shell".to_string()]);
        // Second call appends nothing.
        assert!(append_missing_fingerprint_tools(&mut tools, true).is_empty());
        let names: Vec<String> = tools.iter().map(|t| tool_name_of(t)).collect();
        assert!(names.contains(&"read".to_string()));
        assert!(names.contains(&"shell".to_string()));
    }

    #[test]
    fn retarget_tool_choice_points_at_alias() {
        let mut body = json!({
            "tool_choice": {"type": "function", "name": "read"}
        });
        let mut map = BTreeMap::new();
        map.insert("read_client".to_string(), "read".to_string());
        retarget_tool_choice(&mut body, &map);
        assert_eq!(body["tool_choice"]["name"], json!("read_client"));
    }

    #[test]
    fn retarget_tool_choice_nested_function_shape() {
        let mut body = json!({
            "tool_choice": {"type": "function", "function": {"name": "shell"}}
        });
        let mut map = BTreeMap::new();
        map.insert("shell_client".to_string(), "shell".to_string());
        retarget_tool_choice(&mut body, &map);
        assert_eq!(body["tool_choice"]["function"]["name"], json!("shell_client"));
    }

    #[test]
    fn alias_history_renames_input_and_message_calls() {
        let mut body = json!({
            "input": [
                {"type": "function_call", "name": "read", "call_id": "c1", "arguments": "{}"},
                {"type": "function_call", "name": "other", "call_id": "c2", "arguments": "{}"}
            ],
            "messages": [
                {"role": "assistant", "tool_calls": [
                    {"id": "c3", "function": {"name": "shell", "arguments": "{}"}}
                ]}
            ]
        });
        let mut map = BTreeMap::new();
        map.insert("read_client".to_string(), "read".to_string());
        map.insert("shell_client".to_string(), "shell".to_string());
        alias_history_tool_calls(&mut body, &map);
        assert_eq!(body["input"][0]["name"], json!("read_client"));
        assert_eq!(body["input"][1]["name"], json!("other"));
        assert_eq!(body["messages"][0]["tool_calls"][0]["function"]["name"], json!("shell_client"));
    }

    #[test]
    fn apply_env_fingerprint_appends_marker_to_system_message() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "be nice"},
                {"role": "user", "content": "hi"}
            ]
        });
        assert!(apply_env_fingerprint(&mut body, false, false));
        let content = body["messages"][0]["content"].as_str().unwrap();
        assert!(content.starts_with("be nice\n\n"));
        assert!(content.contains("<env>"));
        assert!(content.contains("</env>"));
        // Idempotent: second call is a no-op.
        assert!(!apply_env_fingerprint(&mut body, false, false));
    }

    #[test]
    fn apply_env_fingerprint_flat_uses_instructions() {
        let mut body = json!({ "instructions": "do things" });
        assert!(apply_env_fingerprint(&mut body, true, false));
        let s = body["instructions"].as_str().unwrap();
        assert!(s.starts_with("do things\n\n"));
        assert!(s.contains("<env>"));
    }

    #[test]
    fn apply_env_fingerprint_claude_uses_system_field() {
        let mut body = json!({ "system": "sys prompt", "messages": [] });
        assert!(apply_env_fingerprint(&mut body, false, true));
        assert!(body["system"].as_str().unwrap().contains("<env>"));
        // No messages[0] system role was fabricated.
        assert!(body.get("messages").unwrap().as_array().unwrap().is_empty());
    }

    #[test]
    fn apply_fingerprint_tools_full_pass_chat() {
        // Caller has an incompatible `read` and nothing else.
        let mut body = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [json!({
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "mine",
                    "parameters": {"type":"object","properties":{"zzz":{"type":"string"}},"required":[]}
                }
            })]
        });
        let map = apply_fingerprint_tools(&mut body, false, false);
        assert_eq!(map.get("read_client").map(String::as_str), Some("read"));
        let tools = body["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"read_client"));
        assert!(names.contains(&"shell"));
        // <env> marker prepended as a system message (no prior system).
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert!(body["messages"][0]["content"].as_str().unwrap().contains("<env>"));
    }

    #[test]
    fn apply_fingerprint_tools_flat_defaults_tool_choice_auto() {
        let mut body = json!({ "input": [] });
        apply_fingerprint_tools(&mut body, true, false);
        assert_eq!(body["tool_choice"], json!("auto"));
        assert!(body["instructions"].as_str().unwrap().contains("<env>"));
        // tools[] is flat shape.
        let tool = body["tools"].as_array().unwrap()[0].clone();
        assert_eq!(tool["type"], json!("function"));
        assert!(tool.get("function").is_none());
    }

    #[test]
    fn restore_tool_names_chat_delta_and_message() {
        let mut map = BTreeMap::new();
        map.insert("read_client".to_string(), "read".to_string());
        let chunk = json!({
            "choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"name": "read_client", "arguments": "{}"}}]}}]
        });
        let out = restore_tool_names(&chunk, &map);
        assert_eq!(out["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], json!("read"));

        let msg = json!({
            "choices": [{"message": {"tool_calls": [{"function": {"name": "read_client"}}]}}]
        });
        let out = restore_tool_names(&msg, &map);
        assert_eq!(out["choices"][0]["message"]["tool_calls"][0]["function"]["name"], json!("read"));
    }

    #[test]
    fn restore_tool_names_claude_and_responses_shapes() {
        let mut map = BTreeMap::new();
        map.insert("shell_client".to_string(), "shell".to_string());
        // content_block_start event.
        let ev = json!({"type": "content_block_start", "content_block": {"type": "tool_use", "name": "shell_client", "id": "t1"}});
        let out = restore_tool_names(&ev, &map);
        assert_eq!(out["content_block"]["name"], json!("shell"));
        // Claude message content blocks.
        let msg = json!({"content": [{"type": "tool_use", "name": "shell_client"}]});
        let out = restore_tool_names(&msg, &map);
        assert_eq!(out["content"][0]["name"], json!("shell"));
        // Responses output item + SSE item event.
        let resp = json!({"output": [{"type": "function_call", "name": "shell_client"}]});
        let out = restore_tool_names(&resp, &map);
        assert_eq!(out["output"][0]["name"], json!("shell"));
        let ev = json!({"type": "response.output_item.done", "item": {"type": "function_call", "name": "shell_client"}});
        let out = restore_tool_names(&ev, &map);
        assert_eq!(out["item"]["name"], json!("shell"));
    }

    #[test]
    fn restore_tool_names_in_sse_rewrites_data_payloads() {
        let mut map = BTreeMap::new();
        map.insert("read_client".to_string(), "read".to_string());
        let chunk = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"name\":\"read_client\"}}]}}]}\n\ndata: [DONE]\n\n";
        let out = restore_tool_names_in_sse(chunk, &map);
        assert!(out.contains("\"name\":\"read\""), "got: {out}");
        assert!(out.contains("data: [DONE]"));
        // Empty map → passthrough.
        assert_eq!(restore_tool_names_in_sse(chunk, &BTreeMap::new()), chunk);
    }
}
