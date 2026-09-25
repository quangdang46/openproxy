//! Grok CLI / Grok Build executor — `cli-chat-proxy.grok.com` Responses API.
//!
//! Distinct from:
//! - [`super::xai`] → `api.x.ai` (API key / OAuth)
//! - [`super::grok_web`] → grok.com web SSO
//!
//! Port of 9router `open-sse/executors/grok-cli.js` + `providers/registry/grok-cli.js`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

pub const GROK_CLI_RESPONSES_URL: &str = "https://cli-chat-proxy.grok.com/v1/responses";
const CLIENT_VERSION: &str = "0.2.93";
const CLIENT_IDENTIFIER: &str = "grok-pager";
const TOKEN_AUTH: &str = "xai-grok-cli";
const COMPACTION_AT: &str = "400000";

/// 9router grok-cli.js EFFORT_LEVELS — xhigh included.
const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh"];

/// Max entries in the in-process turn store before evicting oldest.
const TURN_STORE_MAX_SIZE: usize = 5000;

const HOSTED_TOOL_TYPES: &[&str] = &[
    "web_search",
    "x_search",
    "web_search_preview",
    "file_search",
    "image_generation",
    "code_interpreter",
    "mcp",
    "local_shell",
];

/// Native Grok CLI Responses item ids (`rs_`/`msg_`/`fc_` + UUID) that survive
/// `store=false` round-trips (9router `GROK_CLI_NATIVE_ITEM_ID`).
fn native_item_id_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"^(?:rs|msg|fc)_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
        )
        .expect("native item id regex must compile")
    })
}

/// 9router `isNativeGrokCliItemId`.
fn is_native_grok_cli_item_id(id: &str) -> bool {
    native_item_id_pattern().is_match(id)
}

/// 9router `stringifyGrokCliToolOutput`.
fn stringify_grok_cli_tool_output(output: &Value) -> String {
    match output {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn stringify_grok_cli_tool_output_opt(output: Option<&Value>) -> String {
    match output {
        None => String::new(),
        Some(v) => stringify_grok_cli_tool_output(v),
    }
}

/// 9router `resolveGrokCliSessionId` — stable per-conversation session id.
/// Prefers an explicit id carried on the body, falls back to the connection id.
pub fn resolve_grok_cli_session_id(connection_id: &str, body: &Value) -> Option<String> {
    for key in ["session_id", "conversation_id", "prompt_cache_key"] {
        if let Some(s) = body
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(s.to_string());
        }
    }
    if let Some(meta) = body.get("metadata") {
        for key in ["session_id", "conversation_id"] {
            if let Some(s) = meta
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return Some(s.to_string());
            }
        }
    }
    if connection_id.is_empty() {
        None
    } else {
        Some(connection_id.to_string())
    }
}

/// 9router `normalizeGrokCliInputItem` — returns `None` for items to drop.
fn normalize_grok_cli_input_item(item: &Value) -> Option<Value> {
    let Some(obj) = item.as_object() else {
        // 9router: non-object items pass through unchanged.
        return Some(item.clone());
    };
    let mut clean = item.clone();
    if let Some(map) = clean.as_object_mut() {
        map.remove("internal_chat_message_metadata_passthrough");
    }
    let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");

    if ty == "reasoning" {
        let id_ok = obj
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(is_native_grok_cli_item_id);
        let enc_ok = obj
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_some();
        if !id_ok || !enc_ok {
            return None;
        }
        return Some(clean);
    }

    if ty == "custom_tool_call" {
        let call_id = obj
            .get("call_id")
            .and_then(Value::as_str)
            .or_else(|| obj.get("id").and_then(Value::as_str))
            .unwrap_or("");
        let name = obj.get("name").and_then(Value::as_str).unwrap_or("").trim();
        if call_id.is_empty() || name.is_empty() {
            return None;
        }
        let input = obj.get("input").or_else(|| obj.get("arguments"));
        return Some(json!({
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": json!({"input": stringify_grok_cli_tool_output_opt(input)}).to_string(),
        }));
    }

    if ty == "custom_tool_call_output" || ty == "function_call_output" {
        let call_id = obj
            .get("call_id")
            .and_then(Value::as_str)
            .or_else(|| obj.get("id").and_then(Value::as_str))
            .unwrap_or("");
        if call_id.is_empty() {
            return None;
        }
        return Some(json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": stringify_grok_cli_tool_output_opt(obj.get("output")),
        }));
    }

    if ty == "function_call" {
        let call_id = obj
            .get("call_id")
            .and_then(Value::as_str)
            .or_else(|| obj.get("id").and_then(Value::as_str))
            .unwrap_or("");
        let name = obj.get("name").and_then(Value::as_str).unwrap_or("").trim();
        if call_id.is_empty() || name.is_empty() {
            return None;
        }
        let mut out = json!({
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": match obj.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
                None => "{}".into(),
            },
        });
        if let Some(id) = obj.get("id").and_then(Value::as_str) {
            if is_native_grok_cli_item_id(id) {
                out["id"] = json!(id);
            }
        }
        if let Some(status) = obj.get("status").and_then(Value::as_str) {
            out["status"] = json!(status);
        }
        return Some(out);
    }

    Some(clean)
}

/// 9router `normalizeGrokCliInput` — normalize items, then drop orphan
/// `function_call_output`s with no matching `function_call`.
pub fn normalize_grok_cli_input(body: &mut Value) {
    let Some(arr) = body.get("input").and_then(Value::as_array).cloned() else {
        return;
    };
    let normalized: Vec<Value> = arr
        .iter()
        .filter_map(normalize_grok_cli_input_item)
        .collect();
    let call_ids: std::collections::HashSet<String> = normalized
        .iter()
        .filter(|i| i.get("type").and_then(Value::as_str) == Some("function_call"))
        .filter_map(|i| i.get("call_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let filtered: Vec<Value> = normalized
        .into_iter()
        .filter(|i| {
            if i.get("type").and_then(Value::as_str) != Some("function_call_output") {
                return true;
            }
            i.get("call_id")
                .and_then(Value::as_str)
                .is_some_and(|c| call_ids.contains(c))
        })
        .collect();
    body["input"] = json!(filtered);
}

/// 9router `needsRefresh` → `shouldRefreshCredentials("grok-cli", …)`.
/// Rust: refresh token present and expiry within the lead window.
pub fn grok_cli_needs_refresh(credentials: &ProviderConnection) -> bool {
    if credentials.refresh_token.is_none() {
        return false;
    }
    crate::oauth::needs_refresh(&credentials.expires_at)
}

/// 9router `parseError` — 402 spending-limit surfaces payment/quota detail;
/// 400 and everything else map to a sanitized message.
pub fn parse_grok_cli_error(
    status: u16,
    body_text: &str,
) -> crate::core::utils::error::UpstreamError {
    if status == 402 && !body_text.is_empty() {
        if let Ok(v) = serde_json::from_str::<Value>(body_text) {
            let code = v.get("code").and_then(Value::as_str).unwrap_or("");
            let msg = v
                .get("error")
                .and_then(Value::as_str)
                .or_else(|| v.get("message").and_then(Value::as_str))
                .unwrap_or(body_text);
            return crate::core::utils::error::UpstreamError {
                status: 402,
                message: msg.to_string(),
                resets_at_ms: None,
            };
        }
    }
    crate::core::utils::error::UpstreamError {
        status,
        message: crate::core::utils::error::friendly_error_message(status, body_text),
        resets_at_ms: None,
    }
}

const RESPONSES_ALLOWLIST: &[&str] = &[
    "model",
    "input",
    "instructions",
    "tools",
    "tool_choice",
    "stream",
    "store",
    "reasoning",
    "include",
    "temperature",
    "top_p",
    "max_output_tokens",
    "parallel_tool_calls",
    "text",
    "metadata",
    "prompt_cache_key",
];

fn turn_store() -> &'static Mutex<HashMap<String, (u32, std::time::Instant)>> {
    static STORE: OnceLock<Mutex<HashMap<String, (u32, std::time::Instant)>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// TTL for a session's turn counter (9router uses a WeakMap keyed by request;
/// Rust keeps a per-process monotonic counter and expires stale sessions).
const TURN_STORE_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Count user turns in a Responses `input` array (1-based min 1).
pub fn count_grok_cli_user_turns(input: &Value) -> u32 {
    let Some(arr) = input.as_array() else {
        return 1;
    };
    let mut n = 0u32;
    for item in arr {
        if !item.is_object() {
            continue;
        }
        let role = item.get("role").and_then(Value::as_str).unwrap_or("");
        let ty = item.get("type").and_then(Value::as_str).unwrap_or("");
        if role == "user" && (ty.is_empty() || ty == "message") {
            n += 1;
        }
    }
    n.max(1)
}

/// Monotonic turn index per session (never decreases within process).
///
/// 9router increments per request (`prev + (requestKey ? 1 : 0)`) and keeps a
/// WeakMap keyed by request object; Rust keys on the session id with a
/// per-request increment + TTL + max-size eviction.
pub fn resolve_grok_cli_turn_idx(session_id: Option<&str>, input: &Value) -> u32 {
    let from_input = count_grok_cli_user_turns(input);
    let Some(sid) = session_id.filter(|s| !s.is_empty()) else {
        return from_input;
    };
    let now = std::time::Instant::now();
    let mut store = turn_store().lock().unwrap_or_else(|e| e.into_inner());
    // Expire stale sessions and bound the store size.
    if store.len() >= TURN_STORE_MAX_SIZE {
        store.retain(|_, (_, last)| now.duration_since(*last) < TURN_STORE_TTL);
    }
    let prev = store
        .get(sid)
        .filter(|(_, last)| now.duration_since(*last) < TURN_STORE_TTL)
        .map(|(turn, _)| *turn)
        .unwrap_or(0);
    let turn = from_input.max(prev + 1);
    store.insert(sid.to_string(), (turn, now));
    turn
}

/// Test helper — clear in-memory turn counters.
pub fn reset_grok_cli_turn_store() {
    if let Ok(mut store) = turn_store().lock() {
        store.clear();
    }
}

/// 9router `supportsGrokCliReasoningEffort` — only grok-4.5-family models
/// accept a `reasoning.effort` field.
fn supports_grok_cli_reasoning_effort(model: &str) -> bool {
    let mut re = regex::Regex::new(r"^grok-4\.5(?:$|-)").expect("effort regex must compile");
    re.is_match(model)
}

/// Normalize effort like 9router `normalizeGrokCliEffort`: "max" → "xhigh",
/// unknown → "high".
fn normalize_effort(effort: &str) -> &'static str {
    match effort {
        "max" => "xhigh",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        _ => "high",
    }
}

pub fn resolve_effort_from_model(model_id: &str) -> Option<&'static str> {
    for level in EFFORT_LEVELS {
        if model_id.ends_with(&format!("-{level}")) {
            return Some(*level);
        }
    }
    None
}

/// Format a machine-id fingerprint into the UUID-ish shape 9router builds
/// from `getConsistentMachineId` (16 hex chars):
/// `[mid[0..8], mid[8..12], "5"+mid[13..16], "a"+mid[17..20], mid[0..12].pad(12,"0")].join("-")`.
///
/// The Rust machine id is 64 hex chars; we reuse the same slices so the
/// shape is stable per machine.
fn format_grok_cli_agent_id(mid: &str) -> String {
    let m = if mid.len() < 20 {
        // Pad so the slices below stay in bounds for short ids.
        format!("{mid:0<20}")
    } else {
        mid.to_string()
    };
    let s = |i: usize, j: usize| m[i..j].to_string();
    let p2 = format!("5{}", &m[13..16]);
    let p3 = format!("a{}", &m[17..20]);
    let p4 = format!("{:0<12}", &m[..12]);
    format!("{}-{}-{}-{}-{}", s(0, 8), s(8, 12), p2, p3, p4)
}

fn is_server_id(id: &str) -> bool {
    id.starts_with("rs_")
        || id.starts_with("fc_")
        || id.starts_with("resp_")
        || id.starts_with("msg_")
}

fn strip_stored_item_references(body: &mut Value) {
    let Some(arr) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    arr.retain(|item| {
        if let Some(s) = item.as_str() {
            return !is_server_id(s);
        }
        if item.get("type").and_then(Value::as_str) == Some("item_reference") {
            return false;
        }
        true
    });
    for item in arr.iter_mut() {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            if is_server_id(id) && !is_native_grok_cli_item_id(id) {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("id");
                }
            }
        }
    }
}

fn normalize_grok_cli_tools(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    let mut valid_names = Vec::new();
    let mut out = Vec::new();
    for tool in tools.iter() {
        if !tool.is_object() {
            continue;
        }
        let ty = tool.get("type").and_then(Value::as_str).unwrap_or("");
        if HOSTED_TOOL_TYPES.contains(&ty) {
            out.push(tool.clone());
            continue;
        }
        let is_custom = ty == "custom";
        let fn_obj = tool.get("function").filter(|v| v.is_object());
        let raw_name = tool
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| fn_obj.and_then(|f| f.get("name").and_then(Value::as_str)))
            .unwrap_or("")
            .trim();
        if raw_name.is_empty() && ty != "function" && fn_obj.is_none() {
            continue;
        }
        if raw_name.is_empty() {
            continue;
        }
        let name: String = raw_name.chars().take(128).collect();
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| fn_obj.and_then(|f| f.get("description").and_then(Value::as_str)))
            .unwrap_or("");
        // 9router GROK_CLI_FREEFORM_TOOL_PARAMETERS: custom tools carry a
        // freeform `input` string, not a JSON schema.
        fn is_schema_object(v: Option<&Value>) -> bool {
            matches!(v, Some(Value::Object(_)))
        }
        let parameters = if is_custom {
            json!({
                "type": "object",
                "properties": {"input": {"type": "string"}},
                "required": ["input"],
            })
        } else if is_schema_object(tool.get("parameters")) {
            tool.get("parameters").cloned().unwrap_or_default()
        } else if is_schema_object(fn_obj.and_then(|f| f.get("parameters"))) {
            fn_obj
                .and_then(|f| f.get("parameters").cloned())
                .unwrap_or_default()
        } else {
            json!({"type": "object", "properties": {}})
        };
        let mut flat = json!({
            "type": "function",
            "name": name,
            "parameters": parameters,
        });
        if !description.is_empty() {
            flat["description"] = json!(description);
        }
        valid_names.push(
            flat.get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
        out.push(flat);
    }
    *tools = out;

    if let Some(choice) = body.get_mut("tool_choice") {
        if choice.get("type").and_then(Value::as_str) == Some("function") {
            let n = choice
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if n.is_empty() || !valid_names.iter().any(|v| v == n) {
                body.as_object_mut().map(|o| o.remove("tool_choice"));
            }
        }
    }
}

fn psd_str(credentials: &ProviderConnection, key: &str) -> Option<String> {
    credentials
        .provider_specific_data
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[derive(Clone)]
pub struct GrokCliExecutor {
    pool: Arc<ClientPool>,
    #[allow(dead_code)]
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum GrokCliExecutorError {
    MissingCredentials(String),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
}

impl From<reqwest::Error> for GrokCliExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}
impl From<reqwest::header::InvalidHeaderValue> for GrokCliExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}
impl From<hyper_util::client::legacy::Error> for GrokCliExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}
impl From<std::io::Error> for GrokCliExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}
impl From<serde_json::Error> for GrokCliExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct GrokCliExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct GrokCliExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl GrokCliExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, GrokCliExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    pub fn build_url() -> String {
        GROK_CLI_RESPONSES_URL.to_string()
    }

    fn build_headers(
        credentials: &ProviderConnection,
        stream: bool,
        session_id: &str,
        req_id: &str,
        turn_idx: u32,
        model: &str,
        agent_id: Option<&str>,
    ) -> Result<HeaderMap, GrokCliExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("grok-pager/0.2.93 grok-shell/0.2.93 (linux; x86_64)"),
        );

        let token = credentials
            .access_token
            .as_deref()
            .or(credentials.api_key.as_deref())
            .ok_or_else(|| GrokCliExecutorError::MissingCredentials("grok-cli".into()))?;
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );

        headers.insert("x-xai-token-auth", HeaderValue::from_static(TOKEN_AUTH));
        headers.insert(
            "x-grok-client-identifier",
            HeaderValue::from_static(CLIENT_IDENTIFIER),
        );
        headers.insert(
            "x-grok-client-version",
            HeaderValue::from_static(CLIENT_VERSION),
        );
        headers.insert(
            "x-authenticateresponse",
            HeaderValue::from_static("authenticate-response"),
        );
        headers.insert("x-grok-session-id", HeaderValue::from_str(session_id)?);
        headers.insert("x-grok-conv-id", HeaderValue::from_str(session_id)?);
        headers.insert("x-grok-req-id", HeaderValue::from_str(req_id)?);
        headers.insert(
            "x-grok-turn-idx",
            HeaderValue::from_str(&turn_idx.to_string())?,
        );
        headers.insert("x-compaction-at", HeaderValue::from_static(COMPACTION_AT));
        headers.insert("x-grok-model-override", HeaderValue::from_str(model)?);
        if let Some(aid) = agent_id.filter(|s| !s.is_empty()) {
            headers.insert("x-grok-agent-id", HeaderValue::from_str(aid)?);
        }

        // psd email → top-level credentials.email (9router falls back to the
        // connection's own email when provider-specific data lacks one).
        let email = psd_str(credentials, "email").or_else(|| credentials.email.clone());
        let user_id = psd_str(credentials, "userId")
            .or_else(|| psd_str(credentials, "user_id"))
            .or_else(|| psd_str(credentials, "providerUserId"));
        if let Some(e) = email {
            headers.insert("x-email", HeaderValue::from_str(&e)?);
        }
        if let Some(u) = user_id {
            headers.insert("x-userid", HeaderValue::from_str(&u)?);
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        Ok(headers)
    }

    /// Transform body to cli-chat-proxy Responses shape (9router transformRequest).
    pub fn transform_request_body(model: &str, body: &Value) -> Value {
        let mut body = body.clone();

        // Ensure input[]
        let has_input = body
            .get("input")
            .map(|v| {
                v.as_array()
                    .map(|a| !a.is_empty())
                    .unwrap_or(v.as_str().is_some())
            })
            .unwrap_or(false);
        if !has_input {
            if let Some(messages) = body.get("messages").and_then(Value::as_array) {
                if !messages.is_empty() {
                    let input: Vec<Value> = messages
                        .iter()
                        .map(|m| {
                            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
                            let content = match m.get("content") {
                                Some(Value::String(s)) => s.clone(),
                                Some(other) => other.to_string(),
                                None => String::new(),
                            };
                            json!({
                                "type": "message",
                                "role": role,
                                "content": content,
                            })
                        })
                        .collect();
                    body["input"] = json!(input);
                }
            }
        }
        if body.get("input").is_none()
            || body
                .get("input")
                .and_then(Value::as_array)
                .map(|a| a.is_empty())
                .unwrap_or(false)
        {
            body["input"] = json!([{
                "type": "message",
                "role": "user",
                "content": "..."
            }]);
        }

        normalize_grok_cli_input(&mut body);
        strip_stored_item_references(&mut body);
        normalize_grok_cli_tools(&mut body);

        body["stream"] = json!(true);
        body["store"] = json!(false);

        let mut model_effort =
            resolve_effort_from_model(body.get("model").and_then(Value::as_str).unwrap_or(model));
        let mut resolved = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(model)
            .to_string();
        if let Some(effort) = model_effort {
            let suffix = format!("-{effort}");
            if let Some(stripped) = resolved.strip_suffix(&suffix) {
                resolved = stripped.to_string();
            }
        }
        // model(high) style
        for level in EFFORT_LEVELS {
            let paren = format!("({level})");
            if let Some(idx) = resolved.rfind(&paren) {
                resolved = resolved[..idx].trim_end().to_string();
                model_effort = Some(*level);
                break;
            }
        }
        body["model"] = json!(resolved);

        // 9router: normalize "max" → "xhigh", unknown → "high"; default is
        // "high" (only for models that support reasoning effort).
        let supports_effort = supports_grok_cli_reasoning_effort(
            body.get("model").and_then(Value::as_str).unwrap_or(model),
        );
        let mut reasoning = body.get("reasoning").cloned().unwrap_or_else(|| json!({}));
        if !reasoning.is_object() {
            reasoning = json!({});
        }
        if !supports_effort {
            // 9router supportsGrokCliReasoningEffort gating: non-grok-4.5
            // models must not carry reasoning.effort.
            reasoning.as_object_mut().map(|obj| obj.remove("effort"));
        } else {
            let effort = body
                .pointer("/reasoning/effort")
                .and_then(Value::as_str)
                .map(normalize_effort)
                .or_else(|| {
                    body.get("reasoning_effort")
                        .and_then(Value::as_str)
                        .map(normalize_effort)
                })
                .or_else(|| model_effort.map(normalize_effort))
                .unwrap_or("high");
            if reasoning.get("effort").is_none() {
                reasoning["effort"] = json!(effort);
            }
        }
        if reasoning.get("summary").is_none() {
            reasoning["summary"] = json!("concise");
        }
        body["reasoning"] = reasoning;
        if let Some(obj) = body.as_object_mut() {
            obj.remove("reasoning_effort");
        }

        if body
            .pointer("/reasoning/effort")
            .and_then(Value::as_str)
            .is_some_and(|e| e != "none")
        {
            let mut include = body
                .get("include")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let has = include
                .iter()
                .any(|v| v.as_str() == Some("reasoning.encrypted_content"));
            if !has {
                include.push(json!("reasoning.encrypted_content"));
            }
            body["include"] = json!(include);
        }

        // Drop Chat Completions leftovers
        if let Some(obj) = body.as_object_mut() {
            for k in [
                "messages",
                "max_tokens",
                "max_completion_tokens",
                "n",
                "seed",
                "logprobs",
                "top_logprobs",
                "frequency_penalty",
                "presence_penalty",
                "logit_bias",
                "user",
                "stream_options",
                "prompt_cache_retention",
                "safety_identifier",
                "previous_response_id",
            ] {
                obj.remove(k);
            }
            obj.retain(|k, _| RESPONSES_ALLOWLIST.contains(&k.as_str()));
        }

        body
    }

    pub async fn execute_request(
        &self,
        request: GrokCliExecutionRequest,
    ) -> Result<GrokCliExecutorResponse, GrokCliExecutorError> {
        let url = Self::build_url();
        let transformed = Self::transform_request_body(&request.model, &request.body);

        // 9router resolveGrokCliSessionId: explicit session/conversation id on
        // the body wins; otherwise the connection id; random UUID as fallback.
        let session_id = resolve_grok_cli_session_id(&request.credentials.id, &request.body)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let req_id = Uuid::new_v4().to_string();
        let turn_idx = resolve_grok_cli_turn_idx(
            Some(&session_id),
            transformed.get("input").unwrap_or(&json!([])),
        );
        // 9router: deviceId/agentId from psd wins; otherwise derive a stable
        // machine-id fingerprint (`getConsistentMachineId("grok-cli-agent")`
        // in JS, which is a sha256 of the machine identity). Rust's
        // get_machine_id() returns a 64-char SHA-256 hex string.
        let agent_id = psd_str(&request.credentials, "deviceId")
            .or_else(|| psd_str(&request.credentials, "agentId"))
            .or_else(|| {
                let mid = crate::core::auth::machine_id::get_machine_id();
                if mid.is_empty() {
                    None
                } else {
                    Some(format_grok_cli_agent_id(&mid))
                }
            });
        let model = transformed
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&request.model);

        // forceStream: always stream upstream
        let headers = Self::build_headers(
            &request.credentials,
            true,
            &session_id,
            &req_id,
            turn_idx,
            model,
            agent_id.as_deref(),
        )?;

        tracing::debug!(
            target: "openproxy::executor",
            "EXECUTOR provider=grok-cli mode=responses force_stream=true url={url} turn={turn_idx}"
        );

        let client = self.pool.get("grok-cli", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed)
            .send()
            .await?;

        Ok(GrokCliExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body: transformed,
            transport: TransportKind::Reqwest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_url_is_cli_chat_proxy_responses() {
        assert_eq!(
            GrokCliExecutor::build_url(),
            "https://cli-chat-proxy.grok.com/v1/responses"
        );
    }

    #[test]
    fn effort_from_model_suffix() {
        assert_eq!(resolve_effort_from_model("grok-4.5-high"), Some("high"));
        assert_eq!(resolve_effort_from_model("grok-4.5"), None);
    }

    #[test]
    fn transform_forces_stream_store_and_strips_effort() {
        let body = json!({
            "model": "grok-4.5-high",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.5,
            "max_tokens": 100,
        });
        let out = GrokCliExecutor::transform_request_body("grok-4.5-high", &body);
        assert_eq!(out["stream"], true);
        assert_eq!(out["store"], false);
        assert_eq!(out["model"], "grok-4.5");
        assert_eq!(out["reasoning"]["effort"], "high");
        assert!(out.get("messages").is_none());
        assert!(out.get("max_tokens").is_none());
        assert!(out.get("input").is_some());
    }

    #[test]
    fn transform_normalizes_function_tools() {
        let body = json!({
            "model": "grok-4.5",
            "input": [{"type": "message", "role": "user", "content": "x"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "search",
                    "description": "d",
                    "parameters": {"type": "object", "properties": {}}
                }
            }, {
                "type": "web_search"
            }]
        });
        let out = GrokCliExecutor::transform_request_body("grok-4.5", &body);
        let tools = out["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "search");
        assert_eq!(tools[1]["type"], "web_search");
    }

    static TURN_STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn turn_idx_monotonic() {
        let _g = TURN_STORE_LOCK.lock();
        reset_grok_cli_turn_store();
        let input = json!([
            {"type": "message", "role": "user", "content": "a"},
            {"type": "message", "role": "assistant", "content": "b"},
            {"type": "message", "role": "user", "content": "c"},
        ]);
        assert_eq!(resolve_grok_cli_turn_idx(Some("s1"), &input), 2);
        // per-request increment: fewer users still advances beyond prev
        let input2 = json!([{"type": "message", "role": "user", "content": "x"}]);
        assert_eq!(resolve_grok_cli_turn_idx(Some("s1"), &input2), 3);
        // fresh session starts at its own count
        assert_eq!(resolve_grok_cli_turn_idx(Some("s2"), &input), 2);
    }

    #[test]
    fn turn_idx_without_session_uses_input_count() {
        let _g = TURN_STORE_LOCK.lock();
        reset_grok_cli_turn_store();
        let input = json!([{"type": "message", "role": "user", "content": "x"}]);
        assert_eq!(resolve_grok_cli_turn_idx(None, &input), 1);
        assert_eq!(resolve_grok_cli_turn_idx(Some(""), &input), 1);
    }

    #[test]
    fn effort_normalize_max_to_xhigh() {
        assert_eq!(normalize_effort("max"), "xhigh");
        assert_eq!(normalize_effort("low"), "low");
        assert_eq!(normalize_effort("medium"), "medium");
        assert_eq!(normalize_effort("high"), "high");
        assert_eq!(normalize_effort("xhigh"), "xhigh");
        assert_eq!(normalize_effort("bogus"), "high");
        // resolve_effort_from_model now detects the xhigh suffix too
        assert_eq!(resolve_effort_from_model("grok-4.5-xhigh"), Some("xhigh"));
        assert_eq!(resolve_effort_from_model("grok-4.5-high"), Some("high"));
        assert_eq!(resolve_effort_from_model("grok-4.5"), None);
    }

    #[test]
    fn supports_effort_only_grok_45() {
        assert!(supports_grok_cli_reasoning_effort("grok-4.5"));
        assert!(supports_grok_cli_reasoning_effort("grok-4.5-high"));
        assert!(supports_grok_cli_reasoning_effort("grok-4.5-xhigh"));
        assert!(!supports_grok_cli_reasoning_effort("grok-4.6"));
        assert!(!supports_grok_cli_reasoning_effort("grok-4"));
        assert!(!supports_grok_cli_reasoning_effort("grok-4.6-xhigh"));

        // guard test: transform of model "grok-4.6" yields no reasoning.effort
        let body = json!({
            "model": "grok-4.6",
            "input": [{"type": "message", "role": "user", "content": "hi"}],
        });
        let out = GrokCliExecutor::transform_request_body("grok-4.6", &body);
        assert!(
            out.pointer("/reasoning/effort").is_none(),
            "grok-4.6 must not carry reasoning.effort: {out}"
        );
        // grok-4.5 keeps effort
        let body = json!({
            "model": "grok-4.5",
            "input": [{"type": "message", "role": "user", "content": "hi"}],
        });
        let out = GrokCliExecutor::transform_request_body("grok-4.5", &body);
        assert_eq!(
            out.pointer("/reasoning/effort").and_then(Value::as_str),
            Some("high")
        );
    }

    #[test]
    fn effort_max_normalized_in_transform() {
        let body = json!({
            "model": "grok-4.5",
            "input": [{"type": "message", "role": "user", "content": "hi"}],
            "reasoning_effort": "max",
        });
        let out = GrokCliExecutor::transform_request_body("grok-4.5", &body);
        assert_eq!(
            out.pointer("/reasoning/effort").and_then(Value::as_str),
            Some("xhigh")
        );
        assert!(out.get("reasoning_effort").is_none());
    }

    #[test]
    fn machine_id_fallback_formats_uuid_shape() {
        // 64-char sha256 hex (what get_machine_id returns)
        let mid = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let agent = format_grok_cli_agent_id(mid);
        let parts: Vec<&str> = agent.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], "01234567");
        assert_eq!(parts[1], "89ab");
        assert!(parts[2].starts_with('5'));
        assert!(parts[3].starts_with('a'));
        assert_eq!(parts[4].len(), 12);
        // short ids still format without panicking
        let agent = format_grok_cli_agent_id("abc");
        assert_eq!(agent.split('-').count(), 5);
    }

    #[test]
    fn headers_include_fingerprint() {
        let creds = ProviderConnection {
            id: "conn1".into(),
            provider: "grok-cli".into(),
            access_token: Some("tok".into()),
            ..Default::default()
        };
        let h = GrokCliExecutor::build_headers(
            &creds,
            true,
            "sess",
            "req",
            3,
            "grok-4.5",
            Some("agent-1"),
        )
        .unwrap();
        assert_eq!(
            h.get("x-xai-token-auth").and_then(|v| v.to_str().ok()),
            Some("xai-grok-cli")
        );
        assert_eq!(
            h.get("x-grok-turn-idx").and_then(|v| v.to_str().ok()),
            Some("3")
        );
        assert!(h.get(AUTHORIZATION).is_some());
        // never panic on secrets — just presence
    }

    #[test]
    fn session_id_prefers_body_then_connection() {
        let body = json!({"session_id": "sess-explicit"});
        assert_eq!(
            resolve_grok_cli_session_id("conn1", &body).as_deref(),
            Some("sess-explicit")
        );
        let body = json!({"metadata": {"conversation_id": "conv-9"}});
        assert_eq!(
            resolve_grok_cli_session_id("conn1", &body).as_deref(),
            Some("conv-9")
        );
        let body = json!({});
        assert_eq!(
            resolve_grok_cli_session_id("conn1", &body).as_deref(),
            Some("conn1")
        );
        assert_eq!(resolve_grok_cli_session_id("", &body), None);
    }

    #[test]
    fn native_item_id_detection() {
        assert!(is_native_grok_cli_item_id(
            "rs_12345678-1234-1234-1234-123456789012"
        ));
        assert!(is_native_grok_cli_item_id(
            "msg_abcdef12-3456-7890-abcd-ef1234567890"
        ));
        assert!(!is_native_grok_cli_item_id("rs_abc"));
        assert!(!is_native_grok_cli_item_id("resp_xyz"));
        assert!(!is_native_grok_cli_item_id(""));
    }

    #[test]
    fn stringify_tool_output_shapes() {
        assert_eq!(
            stringify_grok_cli_tool_output(&json!("raw")),
            "raw".to_string()
        );
        assert_eq!(stringify_grok_cli_tool_output(&json!(null)), String::new());
        assert_eq!(
            stringify_grok_cli_tool_output(&json!({"a": 1})),
            r#"{"a":1}"#.to_string()
        );
        assert_eq!(stringify_grok_cli_tool_output_opt(None), String::new());
    }

    #[test]
    fn normalize_input_reasoning_keeps_native_only() {
        let mut body = json!({
            "input": [
                {"type": "reasoning", "id": "rs_12345678-1234-1234-1234-123456789012", "encrypted_content": "enc"},
                {"type": "reasoning", "id": "rs_short", "encrypted_content": "enc"},
                {"type": "reasoning", "encrypted_content": "enc"},
                {"type": "message", "role": "user", "content": "hi", "internal_chat_message_metadata_passthrough": {"x": 1}},
            ]
        });
        normalize_grok_cli_input(&mut body);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[1]["type"], "message");
        assert!(input[1]
            .get("internal_chat_message_metadata_passthrough")
            .is_none());
    }

    #[test]
    fn normalize_input_custom_tool_shapes() {
        let mut body = json!({
            "input": [
                {"type": "custom_tool_call", "call_id": "c1", "name": " grep ", "input": {"q": "x"}},
                {"type": "custom_tool_call", "name": "no-call-id"},
                {"type": "function_call", "call_id": "c2", "name": "run", "arguments": {"a": 1}},
                {"type": "function_call_output", "call_id": "c2", "output": {"ok": true}},
                {"type": "function_call_output", "call_id": "orphan", "output": "x"},
            ]
        });
        normalize_grok_cli_input(&mut body);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[0]["call_id"], "c1");
        assert_eq!(input[0]["name"], "grep");
        assert!(input[1]["arguments"].as_str().is_some());
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "c2");
        assert!(input[2]["output"].as_str().is_some());
    }

    #[test]
    fn normalize_input_non_array_is_noop() {
        let mut body = json!({"input": "oops"});
        normalize_grok_cli_input(&mut body);
        assert_eq!(body["input"], json!("oops"));
    }

    #[test]
    fn native_id_survives_strip_but_server_id_does_not() {
        let native = "rs_12345678-1234-1234-1234-123456789012";
        let body = json!({
            "model": "grok-4.5",
            "input": [
                {"type": "message", "role": "user", "content": "a", "id": native},
                {"type": "message", "role": "user", "content": "b", "id": "rs_short"},
                "resp_abc",
                {"type": "item_reference"},
            ],
        });
        let out = GrokCliExecutor::transform_request_body("grok-4.5", &body);
        let input = out["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["id"], native);
        assert!(input[1].get("id").is_none());
    }

    #[test]
    fn transform_drops_empty_tools_and_bad_choice() {
        let body = json!({
            "model": "grok-4.5",
            "input": [{"type": "message", "role": "user", "content": "hi"}],
            "tools": [{"type": "bogus"}],
            "tool_choice": {"type": "function", "name": "missing"},
        });
        let out = GrokCliExecutor::transform_request_body("grok-4.5", &body);
        assert_eq!(out["tools"].as_array().map(|t| t.len()), Some(0));
        assert!(out.get("tool_choice").is_none());
    }

    #[test]
    fn parse_error_maps_402_and_400() {
        let e = parse_grok_cli_error(402, r#"{"code":"spending-limit","error":"quota hit"}"#);
        assert_eq!(e.status, 402);
        assert_eq!(e.message, "quota hit");
        let e = parse_grok_cli_error(400, "Bad request: input too long");
        assert_eq!(e.status, 400);
        assert!(e.message.contains("Bad request"));
        let e = parse_grok_cli_error(500, "");
        assert_eq!(e.status, 500);
    }

    #[test]
    fn needs_refresh_requires_token_and_expiry() {
        let mut creds = ProviderConnection {
            id: "c".into(),
            provider: "grok-cli".into(),
            ..Default::default()
        };
        assert!(!grok_cli_needs_refresh(&creds));
        // refresh token present but no expiry → not refreshable
        creds.refresh_token = Some("rt".into());
        assert!(!grok_cli_needs_refresh(&creds));
        // past expiry → refresh needed
        creds.expires_at = Some("2000-01-01T00:00:00Z".into());
        assert!(grok_cli_needs_refresh(&creds));
    }
}
