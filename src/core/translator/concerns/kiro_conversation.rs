//! Kiro conversation canonicalization — ports of the JavaScript helpers in
//! 9router `open-sse/translator/concerns/kiroConversation.js`.
//!
//! These functions normalize OpenAI/Claude tool definitions into Kiro
//! `toolSpecification` specs and canonicalize an inbound conversation into the
//! strict Kiro wire shape: alternating user/assistant turns, adjacent one-to-one
//! tool-use/tool-result pairs with reserved ids, and tool specs only on the
//! final (current) user message.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

const KIRO_TOOL_NAME_MAX_LENGTH: usize = 64;
const KIRO_TOOL_DESCRIPTION_MAX_LENGTH: usize = 10237;
const KIRO_TOOL_ID_MAX_LENGTH: usize = 64;

/// Tally of structural repairs performed while canonicalizing a conversation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KiroRepairs {
    /// Tool uses that had no matching tool result (flattened to text).
    pub missing_results: usize,
    /// Tool results that had no matching tool use (flattened to text).
    pub orphan_results: usize,
    /// Tool uses dropped because they had no spec and/or a null input.
    pub invalid_tool_uses: usize,
}

// ---------------------------------------------------------------------------
// Low-level value helpers
// ---------------------------------------------------------------------------

/// Deep-clone a `Value` (mirrors `JSON.parse(JSON.stringify(v))`).
fn clone(value: &Value) -> Value {
    value.clone()
}

/// Stringify a value the way JS `text()` does.
fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// JS-truthiness for serde_json values (null / false / "" / 0 are falsy).
fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(false) => true,
        Value::String(s) => s.is_empty(),
        Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

/// JS `!value?.length` check: only non-empty arrays / strings have length.
fn has_truthy_length(v: &Value) -> bool {
    match v {
        Value::Array(a) => !a.is_empty(),
        Value::String(s) => !s.is_empty(),
        _ => false,
    }
}

/// Append text to a turn object's `content` (JS `appendText`).
fn append_text(target: &mut Value, extra: Value) {
    if is_falsy(&extra) {
        return;
    }
    let Some(obj) = target.as_object_mut() else {
        return;
    };
    let existing = obj.get("content").cloned();
    let combined = match existing {
        Some(existing) if !is_falsy(&existing) => {
            Value::String(format!("{}\n\n{}", text(&existing), text(&extra)))
        }
        _ => extra,
    };
    obj.insert("content".to_string(), combined);
}

/// Truncate a string by code points (JS `[...value].slice(0, limit).join("")`).
fn trim_code_points(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

/// Collapse underscore runs and strip leading/trailing underscores.
fn collapse_underscores(s: &str) -> String {
    let collapsed: String = {
        let mut out = String::new();
        let mut prev_underscore = false;
        for c in s.chars() {
            if c == '_' {
                if !prev_underscore {
                    out.push('_');
                }
                prev_underscore = true;
            } else {
                out.push(c);
                prev_underscore = false;
            }
        }
        out
    };
    collapsed.trim_matches('_').to_string()
}

/// JS `uniqueName`: sanitize a raw tool name, dedupe against `used_names`.
fn unique_name(raw_name: &str, index: usize, used_names: &mut HashSet<String>) -> String {
    let cleaned: String = raw_name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = collapse_underscores(&cleaned);
    let fallback = format!("tool_{}", index + 1);
    let base = trim_code_points(
        if cleaned.is_empty() {
            &fallback
        } else {
            &cleaned
        },
        KIRO_TOOL_NAME_MAX_LENGTH,
    );
    let mut candidate = base.clone();
    let mut suffix = 2;
    while used_names.contains(&candidate) {
        let tail = format!("_{}", suffix);
        suffix += 1;
        let keep = KIRO_TOOL_NAME_MAX_LENGTH.saturating_sub(tail.chars().count());
        candidate = format!("{}{}", trim_code_points(&base, keep), tail);
    }
    used_names.insert(candidate.clone());
    candidate
}

/// JS `cleanSchemaValue`: recursively drop `additionalProperties` and empty
/// `required` arrays.
fn clean_schema_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(clean_schema_value).collect()),
        Value::Object(map) => {
            let mut cleaned = Map::new();
            for (key, child) in map {
                if key == "additionalProperties" {
                    continue;
                }
                if key == "required" && child.as_array().is_some_and(|a| a.is_empty()) {
                    continue;
                }
                cleaned.insert(key.clone(), clean_schema_value(child));
            }
            Value::Object(cleaned)
        }
        other => other.clone(),
    }
}

/// JS `normalizeRootSchema`: force `type: object`, `properties`, filtered
/// `required`.
fn normalize_root_schema(schema: &Value) -> Value {
    let mut cleaned = if schema.is_object() {
        clean_schema_value(schema)
    } else {
        json!({})
    };
    let obj = cleaned.as_object_mut().unwrap();
    obj.insert("type".to_string(), Value::String("object".to_string()));
    let props_valid = obj.get("properties").is_some_and(|p| p.is_object());
    if !props_valid {
        obj.insert("properties".to_string(), json!({}));
    }
    let required = obj.get("required").cloned();
    if let Some(Value::Array(required)) = required {
        let props = obj.get("properties").cloned().unwrap_or_else(|| json!({}));
        let mut seen = HashSet::new();
        let mut filtered: Vec<Value> = Vec::new();
        for r in &required {
            if let Some(name) = r.as_str() {
                if props.get(name).is_some() && seen.insert(name.to_string()) {
                    filtered.push(r.clone());
                }
            }
        }
        if filtered.is_empty() {
            obj.remove("required");
        } else {
            obj.insert("required".to_string(), Value::Array(filtered));
        }
    }
    cleaned
}

/// JS `rawId`: a string value, else "".
fn raw_id(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// JS `normalizeToolInput`.
fn normalize_tool_input(input: &Value) -> Option<Value> {
    if let Value::Object(_) = input {
        return Some(clone(input));
    }
    if let Value::String(s) = input {
        match serde_json::from_str::<Value>(s) {
            Ok(parsed) => {
                if matches!(parsed, Value::Object(_)) {
                    return Some(parsed);
                }
                return None;
            }
            Err(_) => return None,
        }
    }
    if input.is_null() {
        return Some(json!({}));
    }
    None
}

/// JS `normalizeToolResult`.
fn normalize_tool_result(result: &Value) -> Value {
    let content: Vec<Value> = match result.get("content") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|part| {
                let p = part.get("text").cloned().unwrap_or_else(|| part.clone());
                json!({ "text": text(&p) })
            })
            .collect(),
        Some(other) => vec![json!({ "text": text(other) })],
        None => vec![json!({ "text": String::new() })],
    };
    let content = if content.is_empty() {
        vec![json!({ "text": String::new() })]
    } else {
        content
    };
    let status = if result.get("status").and_then(|v| v.as_str()) == Some("error") {
        "error"
    } else {
        "success"
    };
    json!({
        "toolUseId": raw_id(result.get("toolUseId").unwrap_or(&Value::Null)),
        "status": status,
        "content": content,
    })
}

/// JS `reserveToolId`.
fn reserve_tool_id(
    value: &str,
    turn_index: usize,
    call_index: usize,
    name: &str,
    used_ids: &mut HashSet<String>,
) -> String {
    let sanitized: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let generated = format!(
        "call_msg{}_tc{}_{}",
        turn_index,
        call_index,
        if name.is_empty() { "tool" } else { name }
    );
    let valid_sanitized = !sanitized.is_empty()
        && sanitized
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let base_source = if valid_sanitized {
        &sanitized
    } else {
        &generated
    };
    let base = trim_code_points(base_source, KIRO_TOOL_ID_MAX_LENGTH);
    let mut candidate = base.clone();
    let mut suffix = 2;
    while used_ids.contains(&candidate) {
        let tail = format!("_{}", suffix);
        suffix += 1;
        let keep = KIRO_TOOL_ID_MAX_LENGTH.saturating_sub(tail.chars().count());
        candidate = format!("{}{}", trim_code_points(&base, keep), tail);
    }
    used_ids.insert(candidate.clone());
    candidate
}

/// JS `toolCallText`.
fn tool_call_text(tool_use: &Value) -> String {
    let name = tool_use
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string();
    let input = match tool_use.get("input") {
        Some(v) if !is_falsy(v) => v.clone(),
        _ => json!({}),
    };
    format!("[Tool call: {}({})]", name, text(&input))
}

/// JS `toolResultText`.
fn tool_result_text(tool_result: &Value) -> String {
    let content = match tool_result.get("content") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|part| {
                let p = part.get("text").cloned().unwrap_or_else(|| part.clone());
                text(&p)
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => text(other),
        None => String::new(),
    };
    let err_suffix = if tool_result.get("status").and_then(|v| v.as_str()) == Some("error") {
        " (error)"
    } else {
        ""
    };
    format!("[Tool result{}: {}]", err_suffix, content)
}

// ---------------------------------------------------------------------------
// Turn / context helpers
// ---------------------------------------------------------------------------

/// JS `mergeUser`: merge a source user turn into a target user turn.
fn merge_user(target: &mut Value, source: &Value) {
    let source_content = source.get("content").cloned();
    if let Some(c) = source_content {
        append_text(target, c);
    }
    let source_images: Vec<Value> = source
        .get("images")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if !source_images.is_empty() {
        if let Some(obj) = target.as_object_mut() {
            let mut merged = obj
                .get("images")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            merged.extend(source_images);
            obj.insert("images".to_string(), Value::Array(merged));
        }
    }
    let source_results: Vec<Value> = source
        .get("userInputMessageContext")
        .and_then(|c| c.get("toolResults"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if !source_results.is_empty() {
        if let Some(obj) = target.as_object_mut() {
            let ctx = obj
                .entry("userInputMessageContext".to_string())
                .or_insert_with(|| json!({}));
            if let Some(ctx_obj) = ctx.as_object_mut() {
                let mut merged = ctx_obj
                    .get("toolResults")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                merged.extend(source_results);
                ctx_obj.insert("toolResults".to_string(), Value::Array(merged));
            }
        }
    }
}

/// JS `mergeAssistant`: merge a source assistant turn into a target one.
fn merge_assistant(target: &mut Value, source: &Value) {
    let source_content = source.get("content").cloned();
    if let Some(c) = source_content {
        append_text(target, c);
    }
    let source_tool_uses: Vec<Value> = source
        .get("toolUses")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if !source_tool_uses.is_empty() {
        if let Some(obj) = target.as_object_mut() {
            let mut merged = obj
                .get("toolUses")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            merged.extend(source_tool_uses);
            obj.insert("toolUses".to_string(), Value::Array(merged));
        }
    }
}

/// JS `normalizeTurns`.
fn normalize_turns(
    history: &[Value],
    current_message: Option<&Value>,
    model_id: &str,
) -> Vec<Value> {
    let mut raw_turns: Vec<&Value> = history.iter().collect();
    if let Some(cm) = current_message {
        raw_turns.push(cm);
    }
    let mut turns: Vec<Value> = Vec::new();

    for raw in raw_turns {
        let is_user = raw.get("userInputMessage").is_some_and(|v| !is_falsy(v));
        let is_assistant = raw
            .get("assistantResponseMessage")
            .is_some_and(|v| !is_falsy(v));
        if is_user == is_assistant {
            continue;
        }
        let turn = if is_user {
            json!({ "userInputMessage": clone(raw.get("userInputMessage").unwrap()) })
        } else {
            json!({
                "assistantResponseMessage": clone(raw.get("assistantResponseMessage").unwrap())
            })
        };

        let can_merge = turns.last().is_some_and(|prev| {
            if is_user {
                prev.get("userInputMessage").is_some_and(|v| !is_falsy(v))
            } else {
                prev.get("assistantResponseMessage")
                    .is_some_and(|v| !is_falsy(v))
            }
        });

        if can_merge {
            let key = if is_user {
                "userInputMessage"
            } else {
                "assistantResponseMessage"
            };
            let source = turn.get(key).cloned().unwrap();
            if let Some(last) = turns.last_mut() {
                if is_user {
                    if let Some(prev_user) = last.get_mut("userInputMessage") {
                        merge_user(prev_user, &source);
                    }
                } else if let Some(prev_assistant) = last.get_mut("assistantResponseMessage") {
                    merge_assistant(prev_assistant, &source);
                }
            }
        } else {
            turns.push(turn);
        }
    }

    let first_is_assistant = turns
        .first()
        .and_then(|t| t.get("assistantResponseMessage"))
        .is_some_and(|v| !is_falsy(v));
    if first_is_assistant {
        turns.insert(
            0,
            json!({ "userInputMessage": { "content": "continue", "modelId": model_id } }),
        );
    }
    let last_is_assistant = turns
        .last()
        .and_then(|t| t.get("assistantResponseMessage"))
        .is_some_and(|v| !is_falsy(v));
    if turns.is_empty() || last_is_assistant {
        turns.push(json!({
            "userInputMessage": { "content": "continue", "modelId": model_id }
        }));
    }

    for turn in turns.iter_mut() {
        if let Some(user) = turn.get_mut("userInputMessage") {
            let content = text(user.get("content").unwrap_or(&Value::Null));
            let content = content.trim().to_string();
            let content = if content.is_empty() {
                "continue".to_string()
            } else {
                content
            };
            let obj = user.as_object_mut().unwrap();
            obj.insert("content".to_string(), Value::String(content));
            if obj.get("modelId").is_none_or(is_falsy) {
                obj.insert("modelId".to_string(), Value::String(model_id.to_string()));
            }
            if let Some(ctx) = obj.get_mut("userInputMessageContext") {
                if let Some(ctx_obj) = ctx.as_object_mut() {
                    if ctx_obj.get("tools").is_some_and(|v| !is_falsy(v)) {
                        ctx_obj.remove("tools");
                    }
                }
            }
        } else if let Some(assistant) = turn.get_mut("assistantResponseMessage") {
            let content = text(assistant.get("content").unwrap_or(&Value::Null));
            let content = content.trim().to_string();
            let content = if content.is_empty() {
                "...".to_string()
            } else {
                content
            };
            if let Some(obj) = assistant.as_object_mut() {
                obj.insert("content".to_string(), Value::String(content));
            }
        }
    }
    turns
}

/// JS `flattenResults`: append each tool result as text to the user message.
fn flatten_results(user_message: &mut Value, results: &[Value]) {
    for result in results {
        append_text(user_message, Value::String(tool_result_text(result)));
    }
}

/// JS `cleanUserContext`: drop empty toolResults/tools and prune an empty
/// context.
fn clean_user_context(user_message: &mut Value) {
    let should_remove_context = {
        let Some(obj) = user_message.as_object_mut() else {
            return;
        };
        let Some(context) = obj.get_mut("userInputMessageContext") else {
            return;
        };
        let Some(ctx_obj) = context.as_object_mut() else {
            return;
        };
        // Mirrors JS `if (!context.toolResults?.length) delete ...`
        if !ctx_obj.get("toolResults").is_some_and(has_truthy_length) {
            ctx_obj.remove("toolResults");
        }
        if !ctx_obj.get("tools").is_some_and(has_truthy_length) {
            ctx_obj.remove("tools");
        }
        // Mirrors JS `if (Object.keys(context).length === 0) delete ...`
        ctx_obj.is_empty()
    };
    if should_remove_context {
        if let Some(obj) = user_message.as_object_mut() {
            obj.remove("userInputMessageContext");
        }
    }
}

// ---------------------------------------------------------------------------
// Pair reconciliation
// ---------------------------------------------------------------------------

/// JS `reconcileToolPair`: match assistant tool uses against the following
/// user turn's tool results, flattening what cannot be kept.
#[allow(clippy::too_many_arguments)]
fn reconcile_tool_pair(
    assistant: &mut Value,
    user: &mut Value,
    turn_index: usize,
    name_map: &HashMap<String, String>,
    spec_names: &HashSet<String>,
    used_ids: &mut HashSet<String>,
    repairs: &mut KiroRepairs,
) {
    struct CallRecord {
        call: Value,
        call_index: usize,
        key: String,
        mapped_name: Option<String>,
        input: Option<Value>,
        result: Option<Value>,
    }

    let calls: Vec<Value> = assistant
        .get("toolUses")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let results: Vec<Value> = user
        .get("userInputMessageContext")
        .and_then(|c| c.get("toolResults"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(normalize_tool_result).collect())
        .unwrap_or_default();

    if calls.is_empty() {
        if !results.is_empty() {
            flatten_results(user, &results);
            repairs.orphan_results += results.len();
        }
        if let Some(ctx) = user.get_mut("userInputMessageContext") {
            if let Some(ctx_obj) = ctx.as_object_mut() {
                ctx_obj.remove("toolResults");
            }
        }
        clean_user_context(user);
        return;
    }

    let mut call_queues: HashMap<String, Vec<usize>> = HashMap::new();
    let mut call_records: Vec<CallRecord> = Vec::new();
    for (call_index, call) in calls.iter().enumerate() {
        let key = raw_id(call.get("toolUseId").unwrap_or(&Value::Null));
        let mapped_name = call
            .get("name")
            .and_then(|n| n.as_str())
            .map(|n| name_map.get(n).cloned().unwrap_or_else(|| n.to_string()));
        let input = match call.get("input") {
            Some(v) => normalize_tool_input(v),
            None => Some(json!({})),
        };
        let queue = call_queues.entry(key.clone()).or_default();
        queue.push(call_records.len());
        call_records.push(CallRecord {
            call: call.clone(),
            call_index,
            key,
            mapped_name,
            input,
            result: None,
        });
    }

    let mut orphan_results: Vec<Value> = Vec::new();
    for result in &results {
        let id = raw_id(result.get("toolUseId").unwrap_or(&Value::Null));
        let mut assigned = false;
        if let Some(queue) = call_queues.get(&id) {
            for &rec_idx in queue {
                if call_records[rec_idx].result.is_none() {
                    call_records[rec_idx].result = Some(result.clone());
                    assigned = true;
                    break;
                }
            }
        }
        if !assigned {
            orphan_results.push(result.clone());
        }
    }

    let mut kept_calls: Vec<Value> = Vec::new();
    let mut kept_results: Vec<Value> = Vec::new();
    for record in &call_records {
        let has_spec = record
            .mapped_name
            .as_ref()
            .is_some_and(|n| spec_names.contains(n));
        let valid = record.result.is_some() && has_spec && record.input.is_some();
        if !valid {
            let name = record.mapped_name.clone().unwrap_or_default();
            let tc_input = record.call.get("input").cloned().unwrap_or(Value::Null);
            let tc = json!({ "name": name, "input": tc_input });
            append_text(assistant, Value::String(tool_call_text(&tc)));
            if record.result.is_none() {
                repairs.missing_results += 1;
            }
            if !(has_spec && record.input.is_some()) {
                repairs.invalid_tool_uses += 1;
            }
            if let Some(r) = &record.result {
                flatten_results(user, std::slice::from_ref(r));
                repairs.orphan_results += 1;
            }
            continue;
        }

        let tool_use_id = reserve_tool_id(
            &record.key,
            turn_index,
            record.call_index,
            record.mapped_name.as_deref().unwrap_or(""),
            used_ids,
        );
        kept_calls.push(json!({
            "toolUseId": tool_use_id.clone(),
            "name": record.mapped_name.clone().unwrap_or_default(),
            "input": record.input.clone().unwrap_or_else(|| json!({})),
        }));
        let mut result = record.result.clone().unwrap();
        if let Some(obj) = result.as_object_mut() {
            obj.insert("toolUseId".to_string(), Value::String(tool_use_id));
        }
        kept_results.push(result);
    }

    if !orphan_results.is_empty() {
        flatten_results(user, &orphan_results);
        repairs.orphan_results += orphan_results.len();
    }

    if kept_calls.is_empty() {
        if let Some(obj) = assistant.as_object_mut() {
            obj.remove("toolUses");
        }
    } else {
        if let Some(obj) = assistant.as_object_mut() {
            obj.insert("toolUses".to_string(), Value::Array(kept_calls));
        }
    }
    {
        let user_obj = user.as_object_mut().unwrap();
        if user_obj
            .get("userInputMessageContext")
            .is_none_or(|c| c.is_null())
        {
            user_obj.insert("userInputMessageContext".to_string(), json!({}));
        }
        if let Some(ctx) = user_obj.get_mut("userInputMessageContext") {
            if let Some(ctx_obj) = ctx.as_object_mut() {
                if kept_results.is_empty() {
                    ctx_obj.remove("toolResults");
                } else {
                    ctx_obj.insert("toolResults".to_string(), Value::Array(kept_results));
                }
            }
        }
    }
    clean_user_context(user);
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate the final Kiro wire conversation without mutating it.
///
/// Returns `(valid, errors)` where `errors` uses the JS error codes
/// (`role:{i}`, `pair:{i}`, `id:{i}`, `spec:{i}`, `orphan:0`, `current`).
pub fn validate_kiro_conversation(
    history: &[Value],
    current_message: &Value,
    tool_specs: &[Value],
) -> (bool, Vec<String>) {
    let mut errors: Vec<String> = Vec::new();
    let mut turns: Vec<&Value> = history.iter().filter(|v| !is_falsy(v)).collect();
    if !is_falsy(current_message) {
        turns.push(current_message);
    }
    let spec_names: HashSet<String> = tool_specs
        .iter()
        .filter_map(|spec| {
            spec.get("toolSpecification")
                .and_then(|ts| ts.get("name"))
                .and_then(|n| n.as_str())
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let mut used_ids: HashSet<String> = HashSet::new();

    for (index, turn) in turns.iter().enumerate() {
        let expected_user = index % 2 == 0;
        let is_user = turn.get("userInputMessage").is_some_and(|v| !is_falsy(v));
        if is_user != expected_user {
            errors.push(format!("role:{}", index));
        }
        if !is_user {
            let calls: Vec<&Value> = turn
                .get("assistantResponseMessage")
                .and_then(|a| a.get("toolUses"))
                .and_then(|t| t.as_array())
                .map(|a| a.iter().collect())
                .unwrap_or_default();
            let results: Vec<&Value> = turns
                .get(index + 1)
                .and_then(|t| t.get("userInputMessage"))
                .and_then(|u| u.get("userInputMessageContext"))
                .and_then(|c| c.get("toolResults"))
                .and_then(|tr| tr.as_array())
                .map(|a| a.iter().collect())
                .unwrap_or_default();
            let call_ids: Vec<String> = calls
                .iter()
                .map(|c| raw_id(c.get("toolUseId").unwrap_or(&Value::Null)))
                .collect();
            let result_ids: Vec<String> = results
                .iter()
                .map(|r| raw_id(r.get("toolUseId").unwrap_or(&Value::Null)))
                .collect();
            if calls.len() != results.len() || call_ids.iter().any(|id| !result_ids.contains(id)) {
                errors.push(format!("pair:{}", index));
            }
            for call in calls {
                let id = raw_id(call.get("toolUseId").unwrap_or(&Value::Null));
                if id.is_empty() || used_ids.contains(&id) {
                    errors.push(format!("id:{}", index));
                }
                used_ids.insert(id);
                let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if !spec_names.contains(name) {
                    errors.push(format!("spec:{}", index));
                }
            }
        } else if index == 0 {
            let results = turn
                .get("userInputMessage")
                .and_then(|u| u.get("userInputMessageContext"))
                .and_then(|c| c.get("toolResults"))
                .and_then(|tr| tr.as_array());
            if results.is_some_and(|a| !a.is_empty()) {
                errors.push("orphan:0".to_string());
            }
        }
    }
    let has_current_content = current_message
        .get("userInputMessage")
        .and_then(|u| u.get("content"))
        .is_some_and(|c| !is_falsy(c));
    if !has_current_content {
        errors.push("current".to_string());
    }
    (errors.is_empty(), errors)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Normalize OpenAI/Claude-shaped tool definitions into Kiro tool specs.
///
/// Returns the specs array and a map from each raw tool name to its unique
/// Kiro name. Empty names and repeated raw names are skipped.
pub fn normalize_kiro_tool_specs(tools: &Value) -> (Vec<Value>, HashMap<String, String>) {
    let mut specs: Vec<Value> = Vec::new();
    let mut name_map: HashMap<String, String> = HashMap::new();
    let mut used_names: HashSet<String> = HashSet::new();

    if let Some(tool_arr) = tools.as_array() {
        for (index, tool) in tool_arr.iter().enumerate() {
            if !tool.is_object() {
                continue;
            }
            let raw_name: Option<String> = tool
                .get("function")
                .and_then(|f| f.get("name"))
                .filter(|v| !v.is_null())
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    tool.get("name")
                        .filter(|v| !v.is_null())
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                });
            let Some(raw_name) = raw_name else {
                continue;
            };
            if raw_name.trim().is_empty() {
                continue;
            }
            // A repeated definition with the same source name describes the
            // same tool.
            if name_map.contains_key(&raw_name) {
                continue;
            }
            let name = unique_name(&raw_name, index, &mut used_names);
            name_map.insert(raw_name.clone(), name.clone());

            let raw_description: Option<Value> = tool
                .get("function")
                .and_then(|f| f.get("description"))
                .filter(|v| !v.is_null())
                .cloned()
                .or_else(|| tool.get("description").filter(|v| !v.is_null()).cloned());
            let description = match &raw_description {
                Some(d) if !is_falsy(d) => {
                    trim_code_points(&text(d), KIRO_TOOL_DESCRIPTION_MAX_LENGTH)
                }
                _ => trim_code_points(
                    &format!("Tool: {}", raw_name),
                    KIRO_TOOL_DESCRIPTION_MAX_LENGTH,
                ),
            };

            let schema: Value = tool
                .get("function")
                .and_then(|f| f.get("parameters"))
                .filter(|v| !v.is_null())
                .cloned()
                .or_else(|| tool.get("parameters").filter(|v| !v.is_null()).cloned())
                .or_else(|| tool.get("input_schema").filter(|v| !v.is_null()).cloned())
                .unwrap_or_else(|| json!({}));

            specs.push(json!({
                "toolSpecification": {
                    "name": name,
                    "description": description,
                    "inputSchema": { "json": normalize_root_schema(&schema) }
                }
            }));
        }
    }
    (specs, name_map)
}

/// JS `flattenAllStructuredTools`: turn every remaining tool use/result into
/// plain text.
fn flatten_all_structured_tools(turns: &mut [Value], repairs: &mut KiroRepairs) {
    for turn in turns.iter_mut() {
        let has_tool_uses = turn
            .get("assistantResponseMessage")
            .and_then(|a| a.get("toolUses"))
            .and_then(|t| t.as_array())
            .is_some_and(|a| !a.is_empty());
        if has_tool_uses {
            let calls = turn
                .get("assistantResponseMessage")
                .and_then(|a| a.get("toolUses"))
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default();
            let assistant = turn.get_mut("assistantResponseMessage").unwrap();
            for call in &calls {
                append_text(assistant, Value::String(tool_call_text(call)));
            }
            repairs.invalid_tool_uses += calls.len();
            let assistant_obj = assistant.as_object_mut().unwrap();
            assistant_obj.remove("toolUses");
        }

        let results = turn
            .get("userInputMessage")
            .and_then(|u| u.get("userInputMessageContext"))
            .and_then(|c| c.get("toolResults"))
            .and_then(|tr| tr.as_array())
            .cloned()
            .unwrap_or_default();
        if !results.is_empty() {
            let user = turn.get_mut("userInputMessage").unwrap();
            flatten_results(user, &results);
            repairs.orphan_results += results.len();
            if let Some(ctx) = user.get_mut("userInputMessageContext") {
                if let Some(ctx_obj) = ctx.as_object_mut() {
                    ctx_obj.remove("toolResults");
                }
            }
            clean_user_context(user);
        }
    }
}

/// Produce a strict Kiro conversation: alternating turns, current user message,
/// adjacent one-to-one tool use/result pairs, and tool specs only on the
/// current message.
///
/// Returns `(history, current_message, repairs, valid)`.
#[allow(clippy::too_many_arguments)]
pub fn canonicalize_kiro_conversation(
    history: &[Value],
    current_message: &Value,
    model_id: &str,
    tool_specs: &[Value],
    name_map: &HashMap<String, String>,
) -> (Vec<Value>, Value, KiroRepairs, bool) {
    let cm = if is_falsy(current_message) {
        None
    } else {
        Some(current_message)
    };
    let mut turns = normalize_turns(history, cm, model_id);
    let mut repairs = KiroRepairs::default();
    let spec_names: HashSet<String> = tool_specs
        .iter()
        .filter_map(|spec| {
            spec.get("toolSpecification")
                .and_then(|ts| ts.get("name"))
                .and_then(|n| n.as_str())
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let mut used_ids: HashSet<String> = HashSet::new();

    let mut index = 0;
    while index < turns.len() {
        if index == 0 {
            let leading_results = turns[0]
                .get("userInputMessage")
                .and_then(|u| u.get("userInputMessageContext"))
                .and_then(|c| c.get("toolResults"))
                .and_then(|tr| tr.as_array())
                .cloned()
                .unwrap_or_default();
            if !leading_results.is_empty() {
                let user = turns[0].get_mut("userInputMessage").unwrap();
                flatten_results(user, &leading_results);
                repairs.orphan_results += leading_results.len();
                if let Some(ctx) = user.get_mut("userInputMessageContext") {
                    if let Some(ctx_obj) = ctx.as_object_mut() {
                        ctx_obj.remove("toolResults");
                    }
                }
                clean_user_context(user);
            }
        }
        if index + 2 < turns.len() {
            let (first, second) = turns.split_at_mut(index + 2);
            let assistant = first
                .get_mut(index + 1)
                .and_then(|t| t.get_mut("assistantResponseMessage"));
            let next_user = second
                .get_mut(0)
                .and_then(|t| t.get_mut("userInputMessage"));
            if let (Some(assistant), Some(next_user)) = (assistant, next_user) {
                reconcile_tool_pair(
                    assistant,
                    next_user,
                    index + 1,
                    name_map,
                    &spec_names,
                    &mut used_ids,
                    &mut repairs,
                );
            }
        }
        index += 2;
    }

    let final_index = turns.len() - 1;

    // finalCurrent.userInputMessage.userInputMessageContext ||= {}
    {
        let final_user = turns[final_index].get_mut("userInputMessage").unwrap();
        let obj = final_user.as_object_mut().unwrap();
        if obj
            .get("userInputMessageContext")
            .is_none_or(|c| c.is_null())
        {
            obj.insert("userInputMessageContext".to_string(), json!({}));
        }
    }
    if !tool_specs.is_empty() {
        let final_user = turns[final_index].get_mut("userInputMessage").unwrap();
        let obj = final_user.as_object_mut().unwrap();
        if let Some(ctx) = obj.get_mut("userInputMessageContext") {
            if let Some(ctx_obj) = ctx.as_object_mut() {
                ctx_obj.insert("tools".to_string(), Value::Array(tool_specs.to_vec()));
            }
        }
    }
    {
        let final_user = turns[final_index].get_mut("userInputMessage").unwrap();
        clean_user_context(final_user);
    }

    let mut final_history = turns[..final_index].to_vec();
    let mut final_current = turns[final_index].clone();
    let mut validation = validate_kiro_conversation(&final_history, &final_current, tool_specs);
    if !validation.0 {
        flatten_all_structured_tools(&mut turns, &mut repairs);
        final_history = turns[..final_index].to_vec();
        final_current = turns[final_index].clone();
        validation = validate_kiro_conversation(&final_history, &final_current, tool_specs);
    }

    (final_history, final_current, repairs, validation.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_tool_specs_sanitizes_and_dedupes_names() {
        let tools = json!([
            {
                "type": "function",
                "function": {
                    "name": "get weather",
                    "description": "Get the weather",
                    "parameters": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                        "required": ["city"],
                        "additionalProperties": false
                    }
                }
            },
            { "name": "get_weather", "description": "Dup" },
            { "name": "get___weather!!!", "description": "Loud" },
            { "name": "get-weather", "description": "Hyphen" },
            { "name": "get weather", "description": "Repeat (skipped)" },
            { "name": "   ", "description": "Blank (skipped)" },
            { "name": "no-desc" }
        ]);
        let (specs, name_map) = normalize_kiro_tool_specs(&tools);
        assert_eq!(specs.len(), 5);
        assert_eq!(name_map.len(), 5);
        assert_eq!(
            name_map.get("get weather"),
            Some(&"get_weather".to_string())
        );
        assert_eq!(
            name_map.get("get_weather"),
            Some(&"get_weather_2".to_string())
        );
        assert_eq!(
            name_map.get("get___weather!!!"),
            Some(&"get_weather_3".to_string())
        );
        assert_eq!(
            name_map.get("get-weather"),
            Some(&"get-weather".to_string())
        );
        assert_eq!(name_map.get("no-desc"), Some(&"no-desc".to_string()));
        assert!(name_map.get("   ").is_none());

        let spec0 = &specs[0]["toolSpecification"];
        assert_eq!(spec0["name"], "get_weather");
        assert_eq!(spec0["description"], "Get the weather");
        let schema = &spec0["inputSchema"]["json"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["city"]["type"], "string");
        assert_eq!(schema["required"], json!(["city"]));
        assert!(schema.get("additionalProperties").is_none());

        let spec4 = &specs[4]["toolSpecification"];
        assert_eq!(spec4["name"], "no-desc");
        assert_eq!(spec4["description"], "Tool: no-desc");
    }

    #[test]
    fn normalize_turns_merges_consecutive_same_role() {
        let history = json!([
            { "userInputMessage": { "content": "first", "modelId": "m" } },
            {
                "userInputMessage": {
                    "content": "second",
                    "images": [{ "format": "png", "source": { "bytes": "abc" } }]
                }
            },
            { "assistantResponseMessage": { "content": "a1" } },
            {
                "assistantResponseMessage": {
                    "content": "a2",
                    "toolUses": [{ "toolUseId": "t1", "name": "x", "input": {} }]
                }
            }
        ]);
        let current = json!({ "userInputMessage": { "content": "current" } });
        let turns = normalize_turns(history.as_array().unwrap(), Some(&current), "model-x");
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0]["userInputMessage"]["content"], "first\n\nsecond");
        assert_eq!(turns[0]["userInputMessage"]["images"][0]["format"], "png");
        assert_eq!(turns[0]["userInputMessage"]["modelId"], "m");
        assert_eq!(turns[1]["assistantResponseMessage"]["content"], "a1\n\na2");
        assert_eq!(
            turns[1]["assistantResponseMessage"]["toolUses"][0]["toolUseId"],
            "t1"
        );
        assert_eq!(turns[2]["userInputMessage"]["content"], "current");
        assert_eq!(turns[2]["userInputMessage"]["modelId"], "model-x");
    }

    #[test]
    fn canonicalize_merges_consecutive_user_turns_with_tool_results() {
        // Guard test from bead .96: consecutive user turns (each carrying
        // toolResults) merge into one turn. Because the single merged turn is
        // the leading turn of the conversation, its toolResults are orphaned
        // (no preceding assistant tool_use) and are flattened to text — this
        // matches the JS reference byte-for-byte (verified against
        // `.tmp/9router` canonicalizeKiroConversation: current content
        // "a\n\nb\n\nnow\n\n[Tool result: one]\n\n[Tool result: two]").
        let history = json!([
            {
                "userInputMessage": {
                    "content": "a",
                    "userInputMessageContext": {
                        "toolResults": [
                            { "toolUseId": "r1", "status": "success", "content": [{ "text": "one" }] }
                        ]
                    }
                }
            },
            {
                "userInputMessage": {
                    "content": "b",
                    "userInputMessageContext": {
                        "toolResults": [
                            { "toolUseId": "r2", "status": "success", "content": [{ "text": "two" }] }
                        ]
                    }
                }
            }
        ]);
        let current = json!({ "userInputMessage": { "content": "now" } });
        let turns = normalize_turns(history.as_array().unwrap(), Some(&current), "model-x");
        // normalizeTurns merges all three user turns into one leading turn.
        assert_eq!(turns.len(), 1);
        let merged = &turns[0]["userInputMessage"];
        // Both toolResults survived the merge, still structured at this stage.
        let results = merged["userInputMessageContext"]["toolResults"]
            .as_array()
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["toolUseId"], "r1");
        assert_eq!(results[1]["toolUseId"], "r2");

        // Full canonicalize then flattens the orphaned leading results to text,
        // matching the JS reference output exactly.
        let specs: Vec<Value> = vec![];
        let name_map: HashMap<String, String> = HashMap::new();
        let (hist, current_msg, repairs, valid) = canonicalize_kiro_conversation(
            history.as_array().unwrap(),
            &current,
            "model-x",
            &specs,
            &name_map,
        );
        assert!(hist.is_empty());
        assert!(valid);
        assert_eq!(
            current_msg["userInputMessage"]["content"],
            "a\n\nb\n\nnow\n\n[Tool result: one]\n\n[Tool result: two]"
        );
        assert_eq!(repairs.orphan_results, 2);
    }

    #[test]
    fn canonicalize_flattens_orphan_results() {
        let history: Vec<Value> = vec![];
        let current = json!({
            "userInputMessage": {
                "content": "what happened",
                "userInputMessageContext": {
                    "toolResults": [
                        { "toolUseId": "r1", "status": "error", "content": [{ "text": "boom" }] }
                    ]
                }
            }
        });
        let tool_specs: Vec<Value> = vec![];
        let name_map: HashMap<String, String> = HashMap::new();
        let (hist, current_msg, repairs, valid) =
            canonicalize_kiro_conversation(&history, &current, "claude-x", &tool_specs, &name_map);
        assert!(hist.is_empty());
        assert!(valid);
        assert_eq!(
            current_msg["userInputMessage"]["content"],
            "what happened\n\n[Tool result (error): boom]"
        );
        assert_eq!(repairs.orphan_results, 1);
        assert_eq!(repairs.missing_results, 0);
        assert_eq!(repairs.invalid_tool_uses, 0);
        assert!(current_msg["userInputMessage"]
            .get("userInputMessageContext")
            .is_none());
    }

    #[test]
    fn reconcile_pairs_and_reserves_ids() {
        let history: Vec<Value> = vec![
            json!({ "userInputMessage": { "content": "hi" } }),
            json!({
                "assistantResponseMessage": {
                    "content": "",
                    "toolUses": [
                        { "toolUseId": "orig_id", "name": "fetch", "input": { "url": "https://x" } }
                    ]
                }
            }),
        ];
        let current = json!({
            "userInputMessage": {
                "content": "here",
                "userInputMessageContext": {
                    "toolResults": [
                        { "toolUseId": "orig_id", "status": "success", "content": [{ "text": "data" }] }
                    ]
                }
            }
        });
        let (tool_specs, name_map) =
            normalize_kiro_tool_specs(&json!([{ "name": "fetch", "description": "Fetch" }]));
        let (hist, current_msg, repairs, valid) =
            canonicalize_kiro_conversation(&history, &current, "m", &tool_specs, &name_map);
        assert!(valid);
        assert_eq!(hist.len(), 2);
        let assistant = &hist[1]["assistantResponseMessage"];
        let calls = assistant["toolUses"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], "fetch");
        assert_eq!(calls[0]["input"]["url"], "https://x");
        assert_eq!(calls[0]["toolUseId"], "orig_id");
        let results = current_msg["userInputMessage"]["userInputMessageContext"]["toolResults"]
            .as_array()
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["toolUseId"], "orig_id");
        assert_eq!(results[0]["status"], "success");
        assert_eq!(results[0]["content"][0]["text"], "data");
        assert_eq!(repairs.missing_results, 0);
        assert_eq!(repairs.orphan_results, 0);
        assert_eq!(repairs.invalid_tool_uses, 0);
    }

    #[test]
    fn calls_without_matching_result_become_text() {
        let history: Vec<Value> = vec![
            json!({ "userInputMessage": { "content": "go" } }),
            json!({
                "assistantResponseMessage": {
                    "content": "",
                    "toolUses": [
                        { "toolUseId": "c1", "name": "no_such_spec", "input": { "q": 1 } }
                    ]
                }
            }),
        ];
        let current = json!({
            "userInputMessage": {
                "content": "result",
                "userInputMessageContext": {
                    "toolResults": [
                        { "toolUseId": "c1", "status": "success", "content": [{ "text": "ok" }] }
                    ]
                }
            }
        });
        let tool_specs: Vec<Value> = vec![json!({
            "toolSpecification": {
                "name": "some_other_spec",
                "description": "d",
                "inputSchema": { "json": {} }
            }
        })];
        let name_map: HashMap<String, String> = HashMap::new();
        let (hist, current_msg, repairs, valid) =
            canonicalize_kiro_conversation(&history, &current, "m", &tool_specs, &name_map);
        assert!(valid);
        let assistant = &hist[1]["assistantResponseMessage"];
        assert!(assistant.get("toolUses").is_none());
        let content = assistant["content"].as_str().unwrap();
        assert!(content.contains("[Tool call: no_such_spec("));
        assert!(content.contains("{\"q\":1}"));
        assert!(repairs.invalid_tool_uses >= 1);
        let cur_content = current_msg["userInputMessage"]["content"].as_str().unwrap();
        assert!(cur_content.contains("[Tool result: ok]"));
        let ctx = current_msg["userInputMessage"]["userInputMessageContext"]
            .as_object()
            .unwrap();
        assert!(ctx.get("toolResults").is_none());
        assert!(ctx.get("tools").is_some());
    }
}
