//! Port of `open-sse/utils/claudeCloaking.js`.
//!
//! Anti-fingerprinting helpers used when forwarding to Anthropic's API
//! through an OAuth Claude Code token. We rename client-supplied tools
//! with a `_ide` suffix so they don't collide with Claude Code's own
//! tool registry, inject a synthetic billing header system block, and
//! plant a fake user-id into the request metadata.

use rand::RngCore;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::core::config::app_constants::CLAUDE_TOOL_SUFFIX;

const CLAUDE_VERSION: &str = "2.1.92";
const CC_ENTRYPOINT: &str = "sdk-cli";

/// Build the synthetic billing header block. Format matches Claude Code
/// 2.1.92+:
/// `x-anthropic-billing-header: cc_version=<ver>.<build>; cc_entrypoint=sdk-cli; cch=<hash>;`
fn generate_billing_header(payload: &Value) -> String {
    let content = serde_json::to_string(payload).unwrap_or_default();
    let cch = {
        let mut h = Sha256::new();
        h.update(content.as_bytes());
        let full = hex::encode(h.finalize());
        full[..5].to_string()
    };
    let mut buf = [0u8; 2];
    rand::thread_rng().fill_bytes(&mut buf);
    let build_hash = hex::encode(buf);
    let build_hash = &build_hash[..3];
    format!(
        "x-anthropic-billing-header: cc_version={CLAUDE_VERSION}.{build_hash}; cc_entrypoint={CC_ENTRYPOINT}; cch={cch};"
    )
}

/// Build the fake user-id JSON blob used for `metadata.user_id`. The
/// `session_id` field is aligned with `X-Claude-Code-Session-Id` if a
/// caller-supplied one is provided.
fn generate_fake_user_id(session_id: Option<&str>) -> String {
    let mut device_buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut device_buf);
    let device_id = hex::encode(device_buf);
    let account_uuid = Uuid::new_v4();
    let session_uuid = match session_id {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => Uuid::new_v4().to_string(),
    };
    format!(
        "{{\"device_id\":\"{device_id}\",\"account_uuid\":\"{account_uuid}\",\"session_id\":\"{session_uuid}\"}}"
    )
}

/// Output of [`cloak_claude_tools`].
#[derive(Debug, Clone)]
pub struct CloakedRequest {
    /// Modified request body (tools renamed, decoys appended).
    pub body: Value,
    /// Map from suffixed tool name back to the original name. Used by
    /// [`decloak_tool_names`] on the response side. `None` if no
    /// renaming was performed.
    pub tool_name_map: Option<BTreeMap<String, String>>,
}

/// Cloak every client-supplied tool name with the `_ide` suffix and append
/// the Claude Code decoy tools afterwards.
///
/// The function is a no-op (returns the body unchanged) if `body.tools` is
/// missing or empty.
pub fn cloak_claude_tools(body: &Value) -> CloakedRequest {
    let Some(tools) = body.get("tools").and_then(|v| v.as_array()) else {
        return CloakedRequest {
            body: body.clone(),
            tool_name_map: None,
        };
    };
    if tools.is_empty() {
        return CloakedRequest {
            body: body.clone(),
            tool_name_map: None,
        };
    }

    let mut tool_name_map: BTreeMap<String, String> = BTreeMap::new();
    let mut renamed_tools: Vec<Value> = Vec::with_capacity(tools.len() + cc_decoy_tools().len());
    for tool in tools {
        let original = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let suffixed = format!("{original}{CLAUDE_TOOL_SUFFIX}");
        tool_name_map.insert(suffixed.clone(), original.to_string());
        let mut renamed = tool.clone();
        if let Some(obj) = renamed.as_object_mut() {
            obj.insert("name".to_string(), Value::String(suffixed));
        }
        renamed_tools.push(renamed);
    }
    renamed_tools.extend(cc_decoy_tools().iter().cloned());

    let mut new_body = body.clone();
    if let Some(obj) = new_body.as_object_mut() {
        obj.insert("tools".to_string(), Value::Array(renamed_tools));
        // Rename `tool_use` blocks in message history so the conversation
        // stays consistent with the renamed tool definitions.
        if let Some(messages) = obj.get_mut("messages").and_then(|v| v.as_array_mut()) {
            for msg in messages.iter_mut() {
                let Some(content) = msg.get_mut("content").and_then(|v| v.as_array_mut()) else {
                    continue;
                };
                for block in content.iter_mut() {
                    if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                        if let Some(b) = block.as_object_mut() {
                            if let Some(name) = b.get("name").and_then(|v| v.as_str()) {
                                let suffixed = format!("{name}{CLAUDE_TOOL_SUFFIX}");
                                b.insert("name".to_string(), Value::String(suffixed));
                            }
                        }
                    }
                }
            }
        }
    }

    CloakedRequest {
        body: new_body,
        tool_name_map: if tool_name_map.is_empty() {
            None
        } else {
            Some(tool_name_map)
        },
    }
}

/// Claude Code's native tool names, exposed as decoys. Mirrors upstream
/// `CC_DEFAULT_TOOLS` (`open-sse/config/appConstants.js`) — the same set
/// backs both the injected decoy tools and the decloak suffix-strip
/// guard (decoy names must reach the client unresolved).
const CC_DEFAULT_TOOL_NAMES: &[&str] = &[
    "Task",
    "TaskOutput",
    "TaskStop",
    "TaskCreate",
    "TaskGet",
    "TaskUpdate",
    "TaskList",
    "Bash",
    "Glob",
    "Grep",
    "Read",
    "Edit",
    "Write",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
    "AskUserQuestion",
    "Skill",
    "EnterPlanMode",
    "ExitPlanMode",
];

fn is_cc_default_tool(name: &str) -> bool {
    CC_DEFAULT_TOOL_NAMES.contains(&name)
}

/// Port of `stripCloakSuffix` (`open-sse/utils/claudeCloaking.js`): strip a
/// trailing [`CLAUDE_TOOL_SUFFIX`] from a cloaked name as a last-resort
/// fallback when the name isn't in `tool_name_map` (e.g. the map was lost
/// across a retry/reconnect). Never strips decoy names — those are meant to
/// reach the client unresolved so it can see "tool unavailable" instead of
/// silently no-oping. Returns `None` when the name is not a cloak leftover.
pub fn strip_cloak_suffix(name: &str) -> Option<String> {
    if !name.ends_with(CLAUDE_TOOL_SUFFIX) || is_cc_default_tool(name) {
        return None;
    }
    let original = &name[..name.len() - CLAUDE_TOOL_SUFFIX.len()];
    if original.is_empty() {
        None
    } else {
        Some(original.to_string())
    }
}

/// Reverse the cloaking: walk a non-streaming Claude response body and
/// strip the `_ide` suffix from every `tool_use` block whose name is in
/// `tool_name_map`.
///
/// When the map has no entry for a name, falls back to
/// [`strip_cloak_suffix`] rather than forwarding an unresolvable
/// `<tool>_ide` name to the client (upstream `decloakToolNames`, b65d2d0).
pub fn decloak_tool_names(body: &Value, tool_name_map: &BTreeMap<String, String>) -> Value {
    let Some(content) = body.get("content").and_then(|v| v.as_array()) else {
        return body.clone();
    };
    let mut new_body = body.clone();
    let new_content: Vec<Value> = content
        .iter()
        .map(|block| {
            if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                return block.clone();
            }
            let Some(name) = block.get("name").and_then(|v| v.as_str()) else {
                return block.clone();
            };
            let resolved = tool_name_map
                .get(name)
                .cloned()
                .or_else(|| strip_cloak_suffix(name));
            let Some(original) = resolved else {
                return block.clone();
            };
            let mut updated = block.clone();
            if let Some(obj) = updated.as_object_mut() {
                obj.insert("name".to_string(), Value::String(original));
            }
            updated
        })
        .collect();
    if let Some(obj) = new_body.as_object_mut() {
        obj.insert("content".to_string(), Value::Array(new_content));
    }
    new_body
}

/// Apply the full Claude cloaking pipeline to an OAuth-authenticated
/// request body: synthetic billing block at `system[0]` and a fake
/// `metadata.user_id`.
///
/// No-op if `api_key` does not contain `sk-ant-oat` (i.e. only OAuth
/// tokens, not first-party API keys, get cloaked).
pub fn apply_cloaking(body: &Value, api_key: &str, session_id: Option<&str>) -> Value {
    if api_key.is_empty() || !api_key.contains("sk-ant-oat") {
        return body.clone();
    }
    let mut result = body.clone();

    let billing_text = generate_billing_header(body);
    let billing_block = json!({"type": "text", "text": billing_text});

    let Some(obj) = result.as_object_mut() else {
        return result;
    };

    match obj.get("system").cloned() {
        Some(Value::Array(mut existing)) => {
            let already_injected = existing
                .first()
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str())
                .map(|s| s.starts_with("x-anthropic-billing-header:"))
                .unwrap_or(false);
            if !already_injected {
                let mut prepended = vec![billing_block.clone()];
                prepended.append(&mut existing);
                obj.insert("system".to_string(), Value::Array(prepended));
            }
        }
        Some(Value::String(s)) => {
            obj.insert(
                "system".to_string(),
                Value::Array(vec![billing_block, json!({"type": "text", "text": s})]),
            );
        }
        _ => {
            obj.insert("system".to_string(), Value::Array(vec![billing_block]));
        }
    }

    // metadata.user_id (only inject if not already present)
    let existing_user_id = obj
        .get("metadata")
        .and_then(|m| m.get("user_id"))
        .and_then(|v| v.as_str());
    if existing_user_id.is_none() {
        let user_id = generate_fake_user_id(session_id);
        let metadata = obj
            .entry("metadata".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(m) = metadata.as_object_mut() {
            m.insert("user_id".to_string(), Value::String(user_id));
        }
    }

    result
}

/// Port of `extractClaudeSessionIdFromUserId` (claudeCloaking.js:33-45).
///
/// `metadata.user_id` is either the cloaked JSON blob
/// `{"device_id":…,"account_uuid":…,"session_id":…}` or a bare
/// `claude:<id>` / plain session string. Returns the session id with any
/// `claude:` prefix stripped, or `None` when absent/empty.
pub fn extract_claude_session_id_from_user_id(user_id: &str) -> Option<String> {
    if user_id.is_empty() {
        return None;
    }
    if user_id.starts_with('{') {
        let parsed = serde_json::from_str::<Value>(user_id).ok()?;
        let sid = parsed.get("session_id")?.as_str()?;
        return clean_session_id(sid);
    }
    clean_session_id(user_id)
}

/// Strip a leading `claude:` prefix (case-insensitive) then trim — the JS
/// `.replace(/^claude:/i, "").trim()` pair, in that order (the regex is
/// anchored, so a leading space defeats the prefix match). Empty → `None`.
fn clean_session_id(s: &str) -> Option<String> {
    let stripped =
        if s.len() >= "claude:".len() && s[.."claude:".len()].eq_ignore_ascii_case("claude:") {
            &s["claude:".len()..]
        } else {
            s
        };
    let cleaned = stripped.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// Static list of Claude Code's native tool names exposed as decoys.
fn cc_decoy_tools() -> &'static [Value] {
    use once_cell::sync::Lazy;
    static DECOYS: Lazy<Vec<Value>> = Lazy::new(|| {
        CC_DEFAULT_TOOL_NAMES
            .iter()
            .map(|n| {
                json!({
                    "name": n,
                    "description": "This tool is currently unavailable.",
                    "input_schema": {"type": "object", "properties": {}}
                })
            })
            .collect()
    });
    &DECOYS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloak_renames_client_tools_and_appends_decoys() {
        let body = json!({
            "tools": [
                {"name": "MyCustomTool", "description": "x"}
            ],
            "messages": []
        });
        let res = cloak_claude_tools(&body);
        let map = res.tool_name_map.expect("map");
        assert!(map.contains_key("MyCustomTool_ide"));
        assert_eq!(map["MyCustomTool_ide"], "MyCustomTool");

        let tools = res.body["tools"].as_array().unwrap();
        // 1 client tool + 20 decoy tools
        assert_eq!(tools.len(), 21);
        assert_eq!(tools[0]["name"], "MyCustomTool_ide");
    }

    #[test]
    fn cloak_renames_tool_use_blocks_in_message_history() {
        let body = json!({
            "tools": [{"name": "WebSearch"}],
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "name": "WebSearch", "id": "tu1"}
                ]}
            ]
        });
        let res = cloak_claude_tools(&body);
        let block = &res.body["messages"][0]["content"][0];
        assert_eq!(block["name"], "WebSearch_ide");
    }

    #[test]
    fn decloak_strips_suffix_in_response() {
        let mut map = BTreeMap::new();
        map.insert("WebSearch_ide".to_string(), "WebSearch".to_string());
        let body = json!({
            "content": [
                {"type": "tool_use", "name": "WebSearch_ide", "id": "x"},
                {"type": "text", "text": "hello"}
            ]
        });
        let res = decloak_tool_names(&body, &map);
        assert_eq!(res["content"][0]["name"], "WebSearch");
        assert_eq!(res["content"][1]["text"], "hello");
    }

    #[test]
    fn decloak_falls_back_to_suffix_strip_when_map_misses() {
        // Map lost across a retry: unmapped `something_ide` still decloaks.
        let map = BTreeMap::new();
        let body = json!({
            "content": [
                {"type": "tool_use", "name": "something_ide", "id": "x"},
                {"type": "tool_use", "name": "plain_name", "id": "y"},
                {"type": "tool_use", "name": "WebSearch", "id": "z"},
            ]
        });
        let res = decloak_tool_names(&body, &map);
        assert_eq!(res["content"][0]["name"], "something");
        // No `_ide` suffix → untouched.
        assert_eq!(res["content"][1]["name"], "plain_name");
        // Decoy names are never suffixed, so they pass through as-is.
        assert_eq!(res["content"][2]["name"], "WebSearch");
    }

    #[test]
    fn strip_cloak_suffix_guards() {
        assert_eq!(strip_cloak_suffix("foo_ide").as_deref(), Some("foo"));
        assert_eq!(strip_cloak_suffix("foo").as_deref(), None);
        assert_eq!(strip_cloak_suffix("_ide").as_deref(), None);
        // A decoy name can never carry the suffix, but the guard mirrors
        // upstream `CC_DEFAULT_TOOLS.has(name)` verbatim.
        assert_eq!(strip_cloak_suffix("WebSearch").as_deref(), None);
    }

    #[test]
    fn apply_cloaking_skips_non_oauth_tokens() {
        let body = json!({"messages": []});
        let res = apply_cloaking(&body, "sk-real-api-key", None);
        assert!(res.get("system").is_none());
    }

    #[test]
    fn apply_cloaking_injects_billing_block_for_oauth() {
        let body = json!({"messages": []});
        let res = apply_cloaking(&body, "sk-ant-oat-foo", None);
        let arr = res["system"].as_array().expect("system array");
        assert_eq!(arr.len(), 1);
        assert!(arr[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("x-anthropic-billing-header:"));
    }

    #[test]
    fn apply_cloaking_preserves_existing_system_string() {
        let body = json!({"messages": [], "system": "be helpful"});
        let res = apply_cloaking(&body, "sk-ant-oat-foo", None);
        let arr = res["system"].as_array().expect("system array");
        assert_eq!(arr.len(), 2);
        assert!(arr[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("x-anthropic-billing-header:"));
        assert_eq!(arr[1]["text"], "be helpful");
    }

    #[test]
    fn extract_session_id_reads_cloaked_json_user_id() {
        let user_id = "{\"device_id\":\"d\",\"account_uuid\":\"a\",\"session_id\":\"sess-abc\"}";
        assert_eq!(
            extract_claude_session_id_from_user_id(user_id).as_deref(),
            Some("sess-abc")
        );
    }

    #[test]
    fn extract_session_id_strips_claude_prefix_and_handles_bare_ids() {
        assert_eq!(
            extract_claude_session_id_from_user_id("claude:sess-1").as_deref(),
            Some("sess-1")
        );
        assert_eq!(
            extract_claude_session_id_from_user_id("CLAUDE: sess-2 ").as_deref(),
            Some("sess-2")
        );
        assert_eq!(
            extract_claude_session_id_from_user_id("plain-id").as_deref(),
            Some("plain-id")
        );
        assert!(extract_claude_session_id_from_user_id("{\"nope\":1}").is_none());
        assert!(extract_claude_session_id_from_user_id("{\"session_id\":\"\"}").is_none());
        assert!(extract_claude_session_id_from_user_id("").is_none());
    }

    #[test]
    fn apply_cloaking_does_not_re_inject_billing_block() {
        let body = json!({
            "messages": [],
            "system": [{"type": "text", "text": "x-anthropic-billing-header: pre-existing"}]
        });
        let res = apply_cloaking(&body, "sk-ant-oat-foo", None);
        assert_eq!(res["system"].as_array().unwrap().len(), 1);
    }
}
