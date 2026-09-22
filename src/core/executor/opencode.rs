use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::core::utils::session_manager::resolve_session_identity;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const OPENCODE_BASE: &str = "https://opencode.ai";
const OPENCODE_DEFAULT_PATH: &str = "/zen/v1/chat/completions";
const OPENCODE_RESPONSES_PATH: &str = "/zen/v1/responses";

/// Default User-Agent sent upstream when the downstream client isn't a
/// recognizable OpenCode client. 9router PRs #4128/#4131/#4132
/// (decolua/9router, 2026-09-17/18): the Zen free-tier gate
/// (`Authorization: Bearer public`) 403s bare `opencode` or foreign UAs;
/// only `opencode/<major>.<minor>[...]` with major>1 or (major==1 &&
/// minor>=17) is accepted.
const OPENCODE_UA: &str = "opencode/1.18.31 ai-sdk/provider-utils/4.0.40 runtime/bun/1.3.14";

/// Base62 alphabet used by OpenCode's own canonical session/request ids
/// (9router PR #4132 `BASE62_CHARS`).
const BASE62_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Max lengths (9router opencode.js:18-19).
const MAX_SESSION_LENGTH: usize = 256;
const MAX_TOOL_NAME_LEN: usize = 128;

/// File-search tool quartet the Zen free-tier gate fingerprints as proof of
/// an agentic OpenCode client (9router PR #4132, verified live 2026-09-18):
/// 0-3 of {bash, glob, grep, read} present → 403 FreeTierError, regardless
/// of UA/session validity. Callers' own tool declarations are preserved
/// verbatim; only missing fingerprint names are appended as no-op stubs.
const OPENCODE_FINGERPRINT_TOOLS: &[&str] = &["bash", "glob", "grep", "read"];

/// Parse `opencode/<major>.<minor>[.<patch>]` (case-insensitive, optionally
/// followed by other UA tokens) and check major>1 or (major==1 && minor>=17).
/// Mirrors `hasValidOpencodeVersion`/`isGateCompatibleUa` (9router PR #4132).
fn has_valid_opencode_version(ua: &str) -> bool {
    let lower = ua.to_ascii_lowercase();
    let Some(pos) = lower.find("opencode/") else {
        return false;
    };
    let rest = &lower[pos + "opencode/".len()..];
    let mut parts = rest.split(['.', ' ', '\t']);
    let Some(major) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    let Some(minor) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    major > 1 || (major == 1 && minor >= 17)
}

/// A canonical OpenCode session id: `ses_` + 12 lowercase hex chars + 14
/// base62 chars (9router `OPENCODE_SESSION_RE`,
/// `/^ses_[0-9a-f]{12}[0-9A-Za-z]{14}$/`).
fn is_native_opencode_session(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("ses_") else {
        return false;
    };
    if rest.len() != 12 + 14 {
        return false;
    }
    let (hex_part, b62_part) = rest.split_at(12);
    hex_part
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && b62_part.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Deterministically translate an arbitrary session seed into the canonical
/// `ses_` shape the free-tier gate requires, stable across turns of the same
/// conversation. Mirrors `translateSessionId` (9router PR #4132): sha256
/// over a namespaced seed, first 6 bytes as hex (time-shaped prefix), next
/// 14 bytes mapped through the base62 alphabet.
fn translate_opencode_session(seed: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"opencode\0");
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let time_hex: String = digest[0..6].iter().map(|b| format!("{b:02x}")).collect();
    let random_part: String = digest[6..20]
        .iter()
        .map(|b| BASE62_CHARS[(*b as usize) % 62] as char)
        .collect();
    format!("ses_{time_hex}{random_part}")
}

/// Resolve the `x-opencode-session` value the gate requires: pass a native
/// downstream session through unchanged (case sensitivity matters — the
/// shape is lowercase-hex + mixed-case base62), else deterministically
/// translate the resolved conversation-stable seed.
fn resolve_gate_session(downstream_session: Option<&str>, resolved_seed: &str) -> String {
    if let Some(native) = downstream_session {
        if is_native_opencode_session(native) {
            return native.to_string();
        }
    }
    translate_opencode_session(resolved_seed)
}

/// A canonical OpenCode request id: `msg_` + 12 lowercase hex + 14 base62
/// (9router `OPENCODE_REQUEST_RE`, `/^msg_[0-9a-f]{12}[0-9A-Za-z]{14}$/`).
fn is_native_opencode_request(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("msg_") else {
        return false;
    };
    if rest.len() != 12 + 14 {
        return false;
    }
    let (hex_part, b62_part) = rest.split_at(12);
    hex_part
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && b62_part.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Normalize a downstream `x-opencode-request` value (9router
/// `normalizeRequestId`, opencode.js:273-277): trim, length-cap, shape-check.
fn normalize_opencode_request_id(value: &str) -> Option<String> {
    let normalized = value.trim();
    if normalized.is_empty() || normalized.len() > MAX_SESSION_LENGTH {
        return None;
    }
    is_native_opencode_request(normalized).then(|| normalized.to_string())
}

/// Last user text, last 600 chars (9router `lastUserText`, opencode.js:225-254).
fn last_user_text(body: &Value) -> String {
    let arr = body
        .get("messages")
        .and_then(Value::as_array)
        .or_else(|| body.get("input").and_then(Value::as_array));
    if let Some(items) = arr {
        for msg in items.iter().rev() {
            if msg
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|r| r != "user")
            {
                continue;
            }
            let content = msg.get("content");
            match content {
                Some(Value::String(s)) if !s.trim().is_empty() => {
                    return tail_chars(s.trim(), 600);
                }
                Some(Value::Array(parts)) => {
                    let mut text = String::new();
                    for p in parts {
                        match p {
                            Value::String(s) => text.push_str(s),
                            Value::Object(_) => {
                                for k in ["text", "input_text", "content", "output"] {
                                    if let Some(t) = p.get(k).and_then(Value::as_str) {
                                        text.push_str(t);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if !text.trim().is_empty() {
                        return tail_chars(text.trim(), 600);
                    }
                }
                _ => {}
            }
            // Responses-API message items {type:"message", content:[...]}.
            if msg.get("type").and_then(Value::as_str) == Some("message") {
                if let Some(items) = msg.get("content").and_then(Value::as_array) {
                    let mut text = String::new();
                    for p in items {
                        for k in ["text", "input_text"] {
                            if let Some(t) = p.get(k).and_then(Value::as_str) {
                                text.push_str(t);
                            }
                        }
                    }
                    if !text.trim().is_empty() {
                        return tail_chars(text.trim(), 600);
                    }
                }
            }
        }
        return String::new();
    }
    match body.get("input").and_then(Value::as_str) {
        Some(s) => tail_chars(s, 600),
        None => String::new(),
    }
}

fn tail_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    chars[chars.len() - n..].iter().collect()
}

/// Deterministic per-turn request id (9router `deriveRequestId`,
/// opencode.js:256-270): sha256 over `opencode-req\0<session>\0<text>`,
/// first 6 digest bytes as hex + 14 base62 chars. Retries share the id
/// because the session + last user text are stable across retries.
fn derive_opencode_request_id(session_id: &str, body: &Value) -> String {
    let text = last_user_text(body);
    if text.is_empty() {
        return translate_opencode_request_id(&[]);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"opencode-req\0");
    hasher.update(session_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    request_id_from_digest(&digest)
}

fn request_id_from_digest(digest: &[u8]) -> String {
    let time_hex: String = digest[0..6].iter().map(|b| format!("{b:02x}")).collect();
    let random_part: String = digest[6..20]
        .iter()
        .map(|b| BASE62_CHARS[(*b as usize) % 62] as char)
        .collect();
    let id = format!("msg_{time_hex}{random_part}");
    if is_native_opencode_request(&id) {
        return id;
    }
    translate_opencode_request_id(&digest[20..])
}

fn translate_opencode_request_id(seed: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"opencode-req-fallback\0");
    hasher.update(seed);
    let digest = hasher.finalize();
    let bytes: &[u8] = &digest;
    request_id_from_digest_fallback(bytes)
}

fn request_id_from_digest_fallback(digest: &[u8]) -> String {
    let time_hex: String = digest[0..6].iter().map(|b| format!("{b:02x}")).collect();
    let random_part: String = digest[6..20]
        .iter()
        .map(|b| BASE62_CHARS[(*b as usize) % 62] as char)
        .collect();
    format!("msg_{time_hex}{random_part}")
}

/// Resolve the `x-opencode-request` value (9router `resolveOpencodeRequestId`,
/// opencode.js:359-369): normalized downstream value wins, else derive
/// deterministically from session + last user text (stable across retries).
fn resolve_opencode_request_id(downstream: Option<&str>, session_id: &str, body: &Value) -> String {
    if let Some(raw) = downstream {
        if let Some(normalized) = normalize_opencode_request_id(raw) {
            return normalized;
        }
    }
    derive_opencode_request_id(session_id, body)
}

/// Normalize Responses-API tools in place (9router `normalizeResponsesTools`,
/// opencode.js:371-397): drop non-objects/unnamed, coerce flat
/// `{type,name,description,parameters}` shape, truncate names, default
/// `{type:"object",properties:{}}` params, drop invalid tool_choice refs.
fn normalize_responses_tools(body_obj: &mut serde_json::Map<String, Value>) {
    let Some(tools) = body_obj.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    let mut valid_names = std::collections::HashSet::new();
    tools.retain_mut(|tool| {
        // Snapshot the fields we need before mutating (borrow discipline).
        let snapshot: Value = tool.clone();
        let Some(obj) = snapshot.as_object() else {
            return false;
        };
        let func = obj
            .get("function")
            .and_then(Value::as_object)
            .filter(|_| !obj.get("function").is_some_and(|f| f.is_array()));
        let raw_name = obj
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| func.and_then(|f| f.get("name")).and_then(Value::as_str))
            .unwrap_or("");
        let name = raw_name.trim();
        if name.is_empty() {
            return false;
        }
        let description = obj
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| {
                func.and_then(|f| f.get("description"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("");
        let mut parameters = obj
            .get("parameters")
            .filter(|p| p.is_object())
            .or_else(|| {
                func.and_then(|f| f.get("parameters"))
                    .filter(|p| p.is_object())
            })
            .cloned()
            .unwrap_or(json!({"type": "object", "properties": {}}));
        if parameters.get("type").and_then(Value::as_str) == Some("object")
            && parameters.get("properties").is_none()
        {
            if let Some(pobj) = parameters.as_object_mut() {
                pobj.insert("properties".to_string(), json!({}));
            }
        }
        let truncated: String = name.chars().take(MAX_TOOL_NAME_LEN).collect();
        let tool_obj = tool.as_object_mut().expect("checked above");
        tool_obj.clear();
        tool_obj.insert("type".to_string(), Value::String("function".to_string()));
        tool_obj.insert("name".to_string(), Value::String(truncated.clone()));
        if !description.is_empty() {
            tool_obj.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        tool_obj.insert("parameters".to_string(), parameters);
        valid_names.insert(truncated);
        true
    });
    if let Some(choice) = body_obj.get("tool_choice") {
        if let Some(obj) = choice.as_object() {
            if obj.get("type").and_then(Value::as_str) == Some("function") {
                let name = obj.get("name").and_then(Value::as_str).unwrap_or("").trim();
                if name.is_empty() || !valid_names.contains(name) {
                    body_obj.remove("tool_choice");
                }
            }
        }
    }
}

fn tool_name_of(tool: &Value) -> Option<String> {
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            tool.get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
        })?
        .trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Inject the missing fingerprint tools into a Chat Completions body
/// (nested `function: {name, description, parameters}` shape). Mirrors
/// `ensureChatFingerprintTools` (9router PR #4132).
fn ensure_chat_fingerprint_tools(body_obj: &mut serde_json::Map<String, Value>) {
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    let tools = body_obj
        .entry("tools")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(arr) = tools.as_array() {
        for tool in arr {
            if let Some(name) = tool_name_of(tool) {
                present.insert(name);
            }
        }
    } else {
        *tools = Value::Array(Vec::new());
    }
    let arr = tools.as_array_mut().expect("tools coerced to array above");
    for name in OPENCODE_FINGERPRINT_TOOLS {
        if present.contains(*name) {
            continue;
        }
        arr.push(json!({
            "type": "function",
            "function": {
                "name": name,
                "description": format!("OpenCode built-in {name} tool"),
                "parameters": { "type": "object", "properties": {} },
            },
        }));
        present.insert(name.to_string());
    }
}

/// Same fingerprint injection for the Responses flat tool shape (`{type,
/// name, description, parameters}` at the top level, no nested `function`).
/// Mirrors `ensureResponsesFingerprintTools` (9router PR #4132).
fn ensure_responses_fingerprint_tools(body_obj: &mut serde_json::Map<String, Value>) {
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    let tools = body_obj
        .entry("tools")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(arr) = tools.as_array() {
        for tool in arr {
            if let Some(name) = tool_name_of(tool) {
                present.insert(name);
            }
        }
    } else {
        *tools = Value::Array(Vec::new());
    }
    let arr = tools.as_array_mut().expect("tools coerced to array above");
    for name in OPENCODE_FINGERPRINT_TOOLS {
        if present.contains(*name) {
            continue;
        }
        arr.push(json!({
            "type": "function",
            "name": name,
            "description": format!("OpenCode built-in {name} tool"),
            "parameters": { "type": "object", "properties": {} },
        }));
        present.insert(name.to_string());
    }
}

/// Check if a model should be routed through the Responses API instead of chat.
/// Muse Spark models use `/zen/v1/responses`.
/// Mirrors `isResponsesModel` in `open-sse/executors/opencode.js:29-32`.
fn is_responses_model(model: &str) -> bool {
    let base = model.split([':', '@']).next().unwrap_or(model);
    base.contains("muse") && base.contains("spark")
}

/// Valid thinking levels per thinking format.
/// Mirrors `FORMAT_LEVELS` in `open-sse/providers/thinkingLevels.js`
/// (only the formats relevant to the opencode executor path are listed;
/// unknown formats fall back to the openai set, matching Muse Spark's
/// `thinkingFormat: "openai"` capabilities entry).
fn thinking_levels_for_format(format: Option<&str>) -> Option<&'static [&'static str]> {
    match format {
        Some("openai") | None => Some(&["none", "minimal", "low", "medium", "high", "xhigh"]),
        Some("claude-adaptive") | Some("kimi") => Some(&["none", "low", "medium", "high", "max"]),
        Some("claude-budget") => Some(&["none", "low", "medium", "high", "xhigh", "max"]),
        Some("gemini-level") => Some(&["minimal", "low", "medium", "high"]),
        Some("gemini-budget") | Some("qwen") | Some("hunyuan") | Some("step") => {
            Some(&["none", "low", "medium", "high"])
        }
        Some("zai") | Some("minimax") => Some(&["none", "thinking"]),
        Some("deepseek") => Some(&["none", "high", "max"]),
        _ => None,
    }
}

/// Normalize Chat thinking fields into the Responses `reasoning` object.
///
/// Mirrors `normalizeOpencodeReasoning` in `open-sse/executors/opencode.js`
/// (JS ab044e6d): `reasoning_effort` (or an existing `reasoning.effort`)
/// becomes `reasoning: { effort, summary: "auto" }`, and `max`/`ultra`
/// clamp down to the highest level the model accepts (`xhigh` for Muse
/// Spark, whose openai level set has no `max`). No string effort → no-op.
fn normalize_opencode_reasoning(model: &str, body: &mut Value) {
    let Some(body_obj) = body.as_object_mut() else {
        return;
    };
    let current_reasoning = body_obj.get("reasoning").filter(|v| v.is_object()).cloned();
    let requested_effort = body_obj
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            current_reasoning
                .as_ref()
                .and_then(|r| r.get("effort"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(requested) = requested_effort else {
        return;
    };
    // Strip a trailing thinking suffix so capability lookup hits the base id
    // (JS `baseModelId`).
    let (clean_model, _) = crate::core::utils::thinking_suffix::strip_thinking_suffix(model);
    let clean_model = if clean_model.is_empty() {
        model
    } else {
        clean_model
    };
    let format =
        crate::core::combo::capabilities::get_capabilities_for_model("opencode", clean_model)
            .thinking_format;
    let supported_levels = thinking_levels_for_format(format);
    let mut effort = requested.to_lowercase();
    effort = effort.trim().to_string();
    if (effort == "max" || effort == "ultra")
        && supported_levels
            .is_some_and(|levels| !levels.is_empty() && !levels.contains(&effort.as_str()))
    {
        let levels = supported_levels.unwrap_or(&[]);
        if effort == "ultra" && levels.contains(&"max") {
            effort = "max".to_string();
        } else if levels.contains(&"xhigh") {
            effort = "xhigh".to_string();
        }
    }
    let mut reasoning = current_reasoning
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    reasoning.insert("effort".to_string(), Value::String(effort));
    reasoning
        .entry("summary".to_string())
        .or_insert(Value::String("auto".to_string()));
    body_obj.insert("reasoning".to_string(), Value::Object(reasoning));
    body_obj.remove("reasoning_effort");
}

#[derive(Clone)]
pub struct OpenCodeExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum OpenCodeExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for OpenCodeExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for OpenCodeExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for OpenCodeExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for OpenCodeExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for OpenCodeExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct OpenCodeExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
    /// Downstream request headers for passthrough (9router rawHeaders).
    pub raw_headers: std::collections::BTreeMap<String, String>,
}

pub struct OpenCodeExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl OpenCodeExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, OpenCodeExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// 9router `MESSAGES_MODELS` (the `/zen/v1/messages` Claude-format path)
    /// was emptied in commit 67271d85 ("send official client headers on
    /// free-tier requests") — `big-pickle` and every other non-Responses
    /// model now route through `/zen/v1/chat/completions`. Live-verified
    /// 2026-09-18: POSTing the OpenAI Chat body shape to `/zen/v1/messages`
    /// for `big-pickle` 500s (it expects the Anthropic Messages shape we
    /// never send); `/zen/v1/chat/completions` returns 200.
    fn build_url(&self, model: &str) -> String {
        let path = if is_responses_model(model) {
            OPENCODE_RESPONSES_PATH
        } else {
            OPENCODE_DEFAULT_PATH
        };
        format!("{}{}", OPENCODE_BASE, path)
    }

    /// Build headers for the OpenCode request.
    ///
    /// 9router PRs #4128/#4131/#4132 (free-tier gate hardening,
    /// 2026-09-17/18): the Zen free-tier gate (`Authorization: Bearer
    /// public`) fingerprints the official agentic client and 403s
    /// (`FreeTierError`) anything that doesn't match — a bare/foreign
    /// User-Agent, a non-canonical `x-opencode-session` shape, or (handled
    /// in `execute_request`) a request with fewer than 4 of the
    /// {bash,glob,grep,read} tool declarations or `stream:false`. Forward a
    /// genuinely-compatible downstream OpenCode UA/session unchanged;
    /// otherwise emit a known-good default / deterministic translation.
    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
        body: &Value,
        raw_headers: &std::collections::BTreeMap<String, String>,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer public"));

        // Conversation-stable seed (9router resolveOpencodeSession), then
        // translated into the gate's canonical ses_ shape below.
        let resolved_seed = resolve_session_identity(
            Some(&std::collections::HashMap::from_iter(
                raw_headers.iter().map(|(k, v)| (k.clone(), v.clone())),
            )),
            Some(body),
            Some(&credentials.id),
            "opencode",
        )
        .session_id;

        // Pass through downstream OpenCode-specific headers when present,
        // falling back to generated/default values.
        let downstream_ua = raw_headers
            .get("user-agent")
            .or_else(|| raw_headers.get("User-Agent"))
            .map(String::as_str)
            .unwrap_or("");
        let is_opencode_downstream = has_valid_opencode_version(downstream_ua);

        let client = raw_headers
            .get("x-opencode-client")
            .map(String::as_str)
            .unwrap_or("desktop");
        let downstream_session = raw_headers.get("x-opencode-session").map(String::as_str);
        let session = resolve_gate_session(downstream_session, &resolved_seed);
        // Deterministic per-turn id (9router resolveOpencodeRequestId):
        // normalized downstream value wins, else derive from session + last
        // user text so retries share the id.
        let request_id = resolve_opencode_request_id(
            raw_headers.get("x-opencode-request").map(String::as_str),
            &session,
            body,
        );

        // User-Agent: forward downstream only if it's a gate-compatible
        // OpenCode client (opencode/<major>.<minor> with major>1 or
        // major==1&&minor>=17), else use the known-good default.
        let ua = if is_opencode_downstream {
            downstream_ua
        } else {
            OPENCODE_UA
        };
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(ua).unwrap_or_else(|_| HeaderValue::from_static(OPENCODE_UA)),
        );
        headers.insert(
            "x-opencode-client",
            HeaderValue::from_str(client).unwrap_or_else(|_| HeaderValue::from_static("desktop")),
        );
        headers.insert(
            "x-opencode-session",
            HeaderValue::from_str(&session)
                .unwrap_or_else(|_| HeaderValue::from_static("ses_unknown")),
        );
        headers.insert(
            "x-opencode-request",
            HeaderValue::from_str(&request_id)
                .unwrap_or_else(|_| HeaderValue::from_static("global")),
        );
        headers.insert(
            "x-opencode-project",
            HeaderValue::from_str(
                raw_headers
                    .get("x-opencode-project")
                    .map(String::as_str)
                    .unwrap_or("global"),
            )
            .unwrap_or_else(|_| HeaderValue::from_static("global")),
        );

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        headers
    }

    pub async fn execute_request(
        &self,
        mut request: OpenCodeExecutionRequest,
    ) -> Result<OpenCodeExecutorResponse, OpenCodeExecutorError> {
        // Normalize developer→system role (many providers reject role:developer)
        normalize_developer_role(&mut request.body);

        // Responses API models need max_tokens → max_output_tokens normalization
        // plus reasoning_effort → reasoning{effort,summary} normalization.
        // Mirrors opencode.js:76-86 (transformRequest for isResponsesModel).
        let is_responses = is_responses_model(&request.model);
        if is_responses {
            // A request that entered via a native /v1/responses endpoint
            // (e.g. Codex) has source_format == target_format ==
            // OpenAiResponses once Muse Spark's per-model targetFormat
            // override applies, so chat.rs's `needs_translation()` (source
            // != target) skips the translate step — the body is left in
            // the intermediate `messages[]` shape produced by compat.rs's
            // input→messages flattening, which the Zen `/responses`
            // endpoint rejects outright ("unknown parameter `messages`").
            // Convert it here when `input` is absent, mirroring codex.rs's
            // own dual-shape handling (`transform_request_body`) for the
            // identical reason. When translation already ran, `input` is
            // already present and this is a no-op.
            if request.body.get("input").is_none() {
                crate::core::translator::request::openai_responses::chat_to_openai_responses_request(
                    &request.model,
                    &mut request.body,
                    request.stream,
                    None,
                );
            }
            if let Some(body_obj) = request.body.as_object_mut() {
                // Read the value first to avoid borrow conflicts
                let max_val = body_obj
                    .remove("max_tokens")
                    .or_else(|| body_obj.remove("max_completion_tokens"));
                if let Some(val) = max_val {
                    body_obj.insert("max_output_tokens".to_string(), val);
                }
            }
            normalize_opencode_reasoning(&request.model, &mut request.body);
        }

        // 9router PR #4132: the Zen free-tier gate 403s (FreeTierError) any
        // request declaring fewer than 4 of the {bash,glob,grep,read} tools
        // — plain chat callers sending no tools at all is the common case.
        // Inject the missing fingerprint declarations as no-op stubs;
        // caller-supplied tools (including extras) are preserved verbatim.
        if let Some(body_obj) = request.body.as_object_mut() {
            if is_responses {
                ensure_responses_fingerprint_tools(body_obj);
            } else {
                ensure_chat_fingerprint_tools(body_obj);
            }
        }

        let url = self.build_url(&request.model);
        let headers = self.build_headers(
            &request.credentials,
            request.stream,
            &request.body,
            &request.raw_headers,
        );

        let client = self.pool.get("opencode", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&request.body)
            .send()
            .await?;

        Ok(OpenCodeExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body: request.body,
            transport: TransportKind::Reqwest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_effort_becomes_reasoning_object_with_summary_auto() {
        let mut body = json!({"model": "muse-spark-1.2", "reasoning_effort": "high"});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
        assert_eq!(body["reasoning"]["effort"], json!("high"));
        assert_eq!(body["reasoning"]["summary"], json!("auto"));
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn max_and_ultra_clamp_to_xhigh_for_muse_spark() {
        // Muse Spark's openai level set has no max/ultra — clamp to xhigh.
        for level in ["max", "ultra", "MAX", " Ultra "] {
            let mut body = json!({"reasoning_effort": level});
            normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
            assert_eq!(body["reasoning"]["effort"], json!("xhigh"), "level={level}");
        }
    }

    #[test]
    fn existing_reasoning_effort_preserved_and_summary_kept() {
        // Effort falls back to reasoning.effort when reasoning_effort absent.
        let mut body = json!({"reasoning": {"effort": "medium", "summary": "detailed"}});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
        assert_eq!(body["reasoning"]["effort"], json!("medium"));
        assert_eq!(body["reasoning"]["summary"], json!("detailed"));
    }

    #[test]
    fn no_effort_string_is_noop() {
        let mut body = json!({"model": "muse-spark-1.2"});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
        assert!(body.get("reasoning").is_none());
        // Non-object reasoning + no effort string → no-op.
        let mut body = json!({"reasoning": "high"});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
        assert_eq!(body["reasoning"], json!("high"));
    }

    #[test]
    fn thinking_suffix_stripped_for_capability_lookup() {
        let mut body = json!({"reasoning_effort": "ultra"});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free(xhigh)", &mut body);
        assert_eq!(body["reasoning"]["effort"], json!("xhigh"));
    }

    // 9router PR #4132 (free-tier gate): UA version gate.
    #[test]
    fn ua_version_gate_matches_9router_semantics() {
        assert!(has_valid_opencode_version("opencode/1.18.31"));
        assert!(has_valid_opencode_version(
            "opencode/1.18.31 ai-sdk/provider-utils/4.0.40"
        ));
        assert!(has_valid_opencode_version("opencode/2.0"));
        assert!(has_valid_opencode_version("OpenCode/1.17.0"));
        assert!(!has_valid_opencode_version("opencode/1.16.9"));
        assert!(!has_valid_opencode_version("opencode"));
        assert!(!has_valid_opencode_version(""));
        assert!(!has_valid_opencode_version("claude-code/1.0"));
    }

    // 9router PR #4132 `OPENCODE_SESSION_RE`.
    #[test]
    fn native_session_shape_validated_exactly() {
        // ses_ + 12 hex + 14 base62 = ses_0123456789abCDEFGHIJKLMN00
        assert!(is_native_opencode_session("ses_0123456789abCDEFGHIJKLMN00"));
        assert!(!is_native_opencode_session("not-a-session"));
        assert!(!is_native_opencode_session("ses_tooshort"));
        // Uppercase hex in the time segment is rejected (JS regex is
        // [0-9a-f], lowercase only) — same total length (26) as the valid case.
        assert!(!is_native_opencode_session(
            "ses_0123456789ABCDEFGHIJKLMN00"
        ));
    }

    #[test]
    fn translate_session_is_deterministic_and_gate_shaped() {
        let a = translate_opencode_session("conn-42");
        let b = translate_opencode_session("conn-42");
        let c = translate_opencode_session("conn-43");
        assert_eq!(a, b, "same seed must translate to the same session");
        assert_ne!(a, c, "different seeds must not collide trivially");
        assert!(
            is_native_opencode_session(&a),
            "translated session must itself satisfy the gate shape: {a}"
        );
    }

    #[test]
    fn resolve_gate_session_passes_through_native_downstream() {
        let native = "ses_0123456789abCDEFGHIJKLMN00";
        assert_eq!(
            resolve_gate_session(Some(native), "irrelevant-seed"),
            native
        );
        // Non-native downstream session (e.g. a raw UUID) is translated,
        // not forwarded verbatim.
        let translated = resolve_gate_session(Some("not-a-session"), "seed-x");
        assert!(is_native_opencode_session(&translated));
        assert_ne!(translated, "not-a-session");
    }

    // 9router PR #4132 `ensureChatFingerprintTools` / `ensureResponsesFingerprintTools`.
    #[test]
    fn chat_fingerprint_tools_injected_when_missing() {
        let mut body = json!({"messages": []});
        let obj = body.as_object_mut().unwrap();
        ensure_chat_fingerprint_tools(obj);
        let names: Vec<&str> = obj["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        for expected in OPENCODE_FINGERPRINT_TOOLS {
            assert!(names.contains(expected), "missing {expected}: {names:?}");
        }
    }

    #[test]
    fn chat_fingerprint_tools_preserve_caller_tools_and_skip_present_names() {
        let mut body = json!({
            "tools": [
                {"type": "function", "function": {"name": "read", "description": "custom read"}},
                {"type": "function", "function": {"name": "my_custom_tool"}},
            ]
        });
        let obj = body.as_object_mut().unwrap();
        ensure_chat_fingerprint_tools(obj);
        let tools = obj["tools"].as_array().unwrap();
        // Caller's own "read" declaration untouched (not duplicated/overwritten).
        let read_tools: Vec<_> = tools
            .iter()
            .filter(|t| t["function"]["name"] == json!("read"))
            .collect();
        assert_eq!(read_tools.len(), 1);
        assert_eq!(
            read_tools[0]["function"]["description"],
            json!("custom read")
        );
        // Caller's extra tool preserved.
        assert!(tools
            .iter()
            .any(|t| t["function"]["name"] == json!("my_custom_tool")));
        // Missing fingerprint names (bash, glob, grep) appended.
        for expected in ["bash", "glob", "grep"] {
            assert!(tools
                .iter()
                .any(|t| t["function"]["name"] == json!(expected)));
        }
    }

    #[test]
    fn responses_fingerprint_tools_use_flat_shape() {
        let mut body = json!({"input": []});
        let obj = body.as_object_mut().unwrap();
        ensure_responses_fingerprint_tools(obj);
        let tools = obj["tools"].as_array().unwrap();
        for expected in OPENCODE_FINGERPRINT_TOOLS {
            let tool = tools
                .iter()
                .find(|t| t["name"] == json!(*expected))
                .unwrap_or_else(|| panic!("missing {expected}"));
            // Flat shape: no nested "function" key.
            assert!(tool.get("function").is_none());
            assert_eq!(tool["type"], json!("function"));
        }
    }

    // Bead .143: request-id derivation (opencode.js:256-277) — normalized
    // downstream wins, else deterministic from session + last user text.
    #[test]
    fn opencode_request_id_normalize_and_derive() {
        let native = "msg_0123456789abCDEFGHIJKLMN00";
        assert_eq!(
            normalize_opencode_request_id(native).as_deref(),
            Some(native)
        );
        assert!(normalize_opencode_request_id("junk").is_none());
        assert!(normalize_opencode_request_id("").is_none());
        let long = "x".repeat(300);
        assert!(normalize_opencode_request_id(&long).is_none());

        let body = json!({"messages": [{"role": "user", "content": "hello"}]});
        let a = derive_opencode_request_id("ses_abc", &body);
        let b = derive_opencode_request_id("ses_abc", &body);
        assert_eq!(a, b, "retries share the id");
        assert!(is_native_opencode_request(&a), "derived id is gate-shaped");
        let c = derive_opencode_request_id("ses_other", &body);
        assert_ne!(a, c, "different sessions differ");

        // resolve_: downstream normalized wins.
        assert_eq!(
            resolve_opencode_request_id(Some(native), "ses_abc", &body),
            native
        );
        assert_eq!(
            resolve_opencode_request_id(Some("junk"), "ses_abc", &body),
            a
        );
        // last user text only (assistant turns ignored).
        let body2 = json!({"messages": [
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "ack"},
            {"role": "user", "content": "second"},
        ]});
        assert_eq!(last_user_text(&body2), "second");
    }

    // Bead .143: normalize_responses_tools (opencode.js:371-397) — flat
    // shape, name truncation, params default, invalid tool_choice dropped.
    #[test]
    fn opencode_normalize_responses_tools_coerces_shape() {
        let long_name = "n".repeat(200);
        let mut body = json!({"tools": [
            {"name": "ok", "description": "d", "parameters": {"type": "object"}},
            {"function": {"name": "nested", "description": "e"}},
            {"name": "   "},
            {"name": long_name, "parameters": {"type": "object", "properties": {}}},
            "junk",
        ], "tool_choice": {"type": "function", "name": "missing"}});
        let obj = body.as_object_mut().unwrap();
        normalize_responses_tools(obj);
        let tools = obj["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["type"], json!("function"));
        assert_eq!(tools[1]["name"], json!("nested"));
        // nested function description preserved, params defaulted.
        assert_eq!(tools[1]["description"], json!("e"));
        assert_eq!(
            tools[1]["parameters"],
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(tools[2]["name"].as_str().unwrap().len(), MAX_TOOL_NAME_LEN);
        assert!(obj.get("tool_choice").is_none(), "dangling choice dropped");
    }

    // Live bug (2026-09-18): a request entering via a native /v1/responses
    // endpoint has source_format == target_format == OpenAiResponses once
    // Muse Spark's per-model targetFormat override applies, so chat.rs's
    // needs_translation() skips the translate step — the body is left in
    // the intermediate messages[] shape from compat.rs's input→messages
    // flattening, which the Zen /responses endpoint rejects with "unknown
    // parameter `messages`". execute_request must convert it via the
    // generic chat_to_openai_responses_request translator when `input` is
    // absent (mirrors codex.rs's own messages[]/input[] dual handling).
    #[test]
    fn is_responses_gate_converts_stray_messages_shape_to_input() {
        let mut body = json!({
            "model": "muse-spark-1.3-contributor-free",
            "messages": [{"role": "user", "content": "hi"}],
        });
        assert!(body.get("input").is_none(), "precondition: no input yet");
        if body.get("input").is_none() {
            crate::core::translator::request::openai_responses::chat_to_openai_responses_request(
                "muse-spark-1.3-contributor-free",
                &mut body,
                true,
                None,
            );
        }
        assert!(body.get("input").is_some(), "input must be populated");
        assert!(
            body.get("messages").is_none(),
            "messages must not survive into the Responses-shaped body"
        );
        let input = body["input"].as_array().unwrap();
        assert!(input
            .iter()
            .any(|item| item["type"] == json!("message") && item["role"] == json!("user")));
    }

    // A body that already went through translation (input present) must be
    // left untouched by the guard — no double-conversion.
    #[test]
    fn is_responses_gate_is_noop_when_input_already_present() {
        let original = json!({
            "model": "muse-spark-1.3-contributor-free",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        });
        let mut body = original.clone();
        if body.get("input").is_none() {
            crate::core::translator::request::openai_responses::chat_to_openai_responses_request(
                "muse-spark-1.3-contributor-free",
                &mut body,
                true,
                None,
            );
        }
        assert_eq!(body["input"], original["input"]);
    }
}
