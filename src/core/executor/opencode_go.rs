use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

// Fields that Fireworks AI / OCg upstream reject as "Extra inputs not permitted"
const FORBIDDEN_FIELDS: &[&str] = &[
    "client_metadata",
    "client_meta_data",
    "include",   // Responses API field
    "reasoning", // Responses API field
];

// Tool types that Fireworks AI / OCg upstream accepts (only "function")
const ALLOWED_TOOL_TYPES: &[&str] = &["function"];

// Tool-level fields that Fireworks AI / OCg upstream rejects
const TOOL_FORBIDDEN_FIELDS: &[&str] = &["strict"];

const OPENCODE_GO_BASE: &str = "https://opencode.ai/zen/go/v1";
const OPENCODE_GO_CLAUDE_PATH: &str = "/messages";
const OPENCODE_GO_DEFAULT_PATH: &str = "/chat/completions";

/// The gate header the Zen gateway rejects a request without
/// (`MissingSessionID`). 9router `open-sse/executors/opencode-go.js:12`.
const SESSION_HEADER: &str = "x-opencode-session";
/// 9router `opencode-go.js:14` — `MAX_SESSION_LENGTH`.
const MAX_SESSION_LENGTH: usize = 256;

// Models that use /zen/go/v1/messages (Anthropic/Claude format + x-api-key auth)
// Matches 9router open-sse/executors/opencode-go.js
const CLAUDE_FORMAT_MODELS: [&str; 6] = [
    "minimax-m3",
    "minimax-m2.7",
    "minimax-m2.5",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.6-plus",
];

/// 9router `normalizeSession` (opencode-go.js:18-22): trim, then reject
/// anything empty or over the cap. Note this is NOT the `ses_*` shape check
/// `opencode.rs` applies — the Zen gateway accepts any bounded string here, so
/// an over-strict check would needlessly discard a caller-supplied session.
fn normalize_session(value: &str) -> Option<String> {
    let normalized = value.trim();
    if normalized.is_empty() || normalized.len() > MAX_SESSION_LENGTH {
        return None;
    }
    Some(normalized.to_string())
}

/// 9router `nativeSession` (opencode-go.js:26-30): a case-insensitive lookup of
/// the caller's own session, passed through unchanged so a real OpenCode client
/// keeps one stable session across a conversation.
fn native_session(raw_headers: &std::collections::BTreeMap<String, String>) -> Option<String> {
    raw_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(SESSION_HEADER))
        .and_then(|(_, v)| normalize_session(v))
}

/// 9router `translatedSession` (opencode-go.js:34-39).
///
/// The seed and the digest shape are BOTH specific to this executor and differ
/// from the sibling `opencode.rs` gate: opencode hashes `opencode\0{seed}` and
/// emits `ses_` + 12 hex + 14 base62, while opencode-go hashes
/// `opencode-go\0{clientTool}\0{sessionId}` and emits `ses_` + 32 hex. Copying
/// the opencode.rs constants here would produce a well-formed-looking but
/// wrong session, which the gateway rejects rather than mis-routes — so the
/// difference is deliberate and the tests pin it.
fn translated_session(session_id: &str, client_tool: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"opencode-go\0");
    hasher.update(client_tool.unwrap_or("generic").as_bytes());
    hasher.update(b"\0");
    hasher.update(session_id.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("ses_{hex}")
}

#[derive(Clone)]
pub struct OpenCodeGoExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum OpenCodeGoExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for OpenCodeGoExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for OpenCodeGoExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for OpenCodeGoExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for OpenCodeGoExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for OpenCodeGoExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct OpenCodeGoExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
    /// Downstream request headers for passthrough (9router rawHeaders).
    pub raw_headers: std::collections::BTreeMap<String, String>,
}

pub struct OpenCodeGoExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl OpenCodeGoExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, OpenCodeGoExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn is_claude_format(model: &str) -> bool {
        CLAUDE_FORMAT_MODELS.contains(&model)
    }

    fn build_url(&self, model: &str) -> String {
        let path = if Self::is_claude_format(model) {
            OPENCODE_GO_CLAUDE_PATH
        } else {
            OPENCODE_GO_DEFAULT_PATH
        };
        format!("{}{}", OPENCODE_GO_BASE, path)
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
        model: &str,
        gate_session: &str,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let key = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .unwrap_or("");

        if Self::is_claude_format(model) {
            headers.insert(
                "x-api-key",
                HeaderValue::from_str(key).unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        } else {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", key))
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }

        // The gateway rejects the request outright without this header
        // (`MissingSessionID`), so it is set on EVERY path, not only the
        // streaming one. 9router falls back to preparing a session from bare
        // credentials for the same reason (opencode-go.js:147-150).
        headers.insert(
            SESSION_HEADER,
            HeaderValue::from_str(gate_session)
                .unwrap_or_else(|_| HeaderValue::from_static("ses_")),
        );

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        headers
    }

    /// Resolve the `x-opencode-session` value this request will carry.
    ///
    /// 9router `prepareRequestCredentials` (opencode-go.js:120-133): the
    /// caller's own session wins, else a deterministic one derived from the
    /// conversation-stable session id and the detected client tool. The
    /// determinism is load-bearing — a new value every turn would defeat the
    /// gateway's conversation routing, which is the header's entire purpose.
    fn resolve_gate_session(
        raw_headers: &std::collections::BTreeMap<String, String>,
        body: &Value,
        connection_id: &str,
        client_tool: Option<&str>,
    ) -> String {
        if let Some(native) = native_session(raw_headers) {
            return native;
        }
        let resolved = crate::core::utils::session_manager::resolve_session_identity(
            Some(
                &raw_headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
            Some(body),
            Some(connection_id),
            "opencode-go",
        )
        .session_id;
        let resolved = normalize_session(&resolved).unwrap_or_else(|| "opencode-go".to_string());
        translated_session(&resolved, client_tool)
    }

    pub async fn execute_request(
        &self,
        mut request: OpenCodeGoExecutionRequest,
    ) -> Result<OpenCodeGoExecutorResponse, OpenCodeGoExecutorError> {
        // DECISION (bead openproxy-dgzj, JS e74db4d0): no
        // `normalizeResponsesTools` equivalent is ported here, deliberately.
        // The JS helper flattens Chat tool declarations into the Responses
        // flat shape because the JS opencode-go executor POSTs Muse Spark
        // bodies to `/zen/go/v1/responses`. This executor instead converts
        // Responses `input` back to Chat `messages` (see below) and always
        // POSTs to the Chat endpoints (`/messages` or `/chat/completions`),
        // so a Chat→Responses tool flattening would be dead code on this
        // path — the request never carries a Responses-shaped body upstream.
        // The Chat→Responses tools[] mapping itself (nameless-skip +
        // 128-char clamp) lives in the translator
        // (`chat_to_openai_responses_request` in
        // `core/translator/request/openai_responses.rs`, bead
        // openproxy-ybo0) and is covered by its unit tests.
        let url = self.build_url(&request.model);
        // The client tool feeds the session seed, so it must be detected from
        // the same headers the caller sent (9router passes `clientTool`
        // through from its request pipeline).
        let header_map: std::collections::HashMap<String, String> = request
            .raw_headers
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
        let client_tool =
            crate::core::utils::client_detector::detect_client_tool(&header_map, &request.body)
                .map(|t| t.as_str());
        let gate_session = Self::resolve_gate_session(
            &request.raw_headers,
            &request.body,
            &request.credentials.id,
            client_tool,
        );
        let headers = self.build_headers(
            &request.credentials,
            request.stream,
            &request.model,
            &gate_session,
        );

        // Normalize developer→system role for providers that reject role:developer (DeepSeek, etc.)
        normalize_developer_role(&mut request.body);

        // Strip forbidden fields that Cloudflare Workers AI / Fireworks reject
        let mut needs_chat_format = false;
        if let Some(obj) = request.body.as_object_mut() {
            for field in FORBIDDEN_FIELDS {
                obj.remove(*field);
            }
            // Also strip from nested assistant tool_calls and messages
            if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
                for msg in messages.iter_mut() {
                    if let Some(msg_obj) = msg.as_object_mut() {
                        for field in FORBIDDEN_FIELDS {
                            msg_obj.remove(*field);
                        }
                    }
                }
            }

            // Convert Responses API format (input) to chat format (messages)
            // Codex CLI sends { model, input, client_metadata, ... }
            // OpenCode Go /zen/go/v1/chat/completions expects { model, messages, ... }
            if obj.contains_key("input") && !obj.contains_key("messages") {
                needs_chat_format = true;
                if let Some(input) = obj.remove("input") {
                    let messages = responses_input_to_messages(input);
                    obj.insert("messages".to_string(), messages);
                }
                // Responses API uses `previous_response_id` for multi-turn
                obj.remove("previous_response_id");
                // Responses API uses `instructions` — map to system message if present
                if let Some(instructions) = obj.remove("instructions") {
                    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
                        let sys_msg = serde_json::json!({
                            "role": "system",
                            "content": instructions
                        });
                        messages.insert(0, sys_msg);
                    }
                }
            }

            // Remove Responses API fields that aren't needed for chat
            obj.remove("tool_choice"); // OCg chat endpoint doesn't use Responses' tool_choice format
        }

        // Re-normalize developer→system after potential input→messages conversion
        if needs_chat_format {
            normalize_developer_role(&mut request.body);
        }

        // --- Fix: strip unsupported tools for Fireworks AI ---
        if let Some(obj) = request.body.as_object_mut() {
            // Fireworks rejects: tools with type != "function", and any top-level
            // tool fields besides "type" and "function" (no name/description/parameters at tool level).
            // Codex may send tools in flat format (name/desc/params at tool level) or
            // nested format (inside function:{}). Both need to be normalized.
            if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
                tools.retain(|tool| {
                    let t = tool.get("type").and_then(Value::as_str).unwrap_or("");
                    ALLOWED_TOOL_TYPES.contains(&t)
                });
                for tool in tools.iter_mut() {
                    let mut cleaned = serde_json::Map::new();
                    // Keep type
                    if let Some(t) = tool.get("type").and_then(Value::as_str) {
                        cleaned.insert("type".into(), Value::String(t.into()));
                    }
                    // Extract function name from either nested function:{} or top-level name
                    let fname = tool
                        .get("function")
                        .and_then(|f| f.get("name").and_then(Value::as_str))
                        .or_else(|| tool.get("name").and_then(Value::as_str))
                        .unwrap_or("")
                        .to_string();
                    if fname.is_empty() {
                        continue;
                    }
                    cleaned.insert(
                        "function".into(),
                        serde_json::json!({
                            "name": fname
                        }),
                    );
                    *tool = Value::Object(cleaned);
                }
            }
        }

        // Log if we had to convert format, so debugging is easier
        if needs_chat_format {
            tracing::debug!("Converted Responses API format to chat format for opencode-go");
        }

        let client = self.pool.get("opencode-go", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&request.body)
            .send()
            .await?;

        Ok(OpenCodeGoExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body: request.body,
            transport: TransportKind::Reqwest,
        })
    }
}

/// Convert Responses API `input` (array or string) to chat `messages` array.
///
/// OpenAI Responses API format:
/// ```json
/// {"input": [{"role": "user", "content": "..."}, {"role": "assistant", "content": "..."}]}
/// ```
/// or a plain string `"input": "hello"`.
///
/// Chat format expects:
/// ```json
/// {"messages": [{"role": "user", "content": "..."}]}
/// ```
fn responses_input_to_messages(input: Value) -> Value {
    match input {
        Value::Array(items) => {
            let messages: Vec<Value> = items
                .into_iter()
                .filter_map(|item| {
                    let item_obj = match item {
                        Value::Object(m) => m,
                        _ => return None,
                    };
                    // Only keep items that have a "role" field
                    if item_obj.contains_key("role") {
                        Some(Value::Object(item_obj))
                    } else {
                        None
                    }
                })
                .collect();
            Value::Array(messages)
        }
        Value::String(text) => {
            json!([{"role": "user", "content": text}])
        }
        _ => {
            json!([{"role": "user", "content": input.to_string()}])
        }
    }
}

impl std::fmt::Display for OpenCodeGoExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Transport and encoding failures are opaque wrappers around
            // reqwest/hyper internals: their `Debug` carries the upstream url
            // and the OS error, which must never reach a client. Report the
            // kind only, matching 9router's terse `connection failed`.
            Self::Request(_)
            | Self::Hyper(_)
            | Self::HyperClientInit(_)
            | Self::InvalidHeader(_)
            | Self::Serialize(_) => f.write_str("upstream request failed"),
            // Every remaining variant carries a plain `String` message that
            // is already client-safe, so its `Debug` is fine to surface.
            other => write!(f, "{other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn executor() -> OpenCodeGoExecutor {
        OpenCodeGoExecutor::new(Arc::new(ClientPool::new()), None).expect("executor")
    }

    fn connection() -> ProviderConnection {
        ProviderConnection {
            id: "conn-1".into(),
            provider: "opencode-go".into(),
            auth_type: "apikey".into(),
            api_key: Some("sk-test".into()),
            ..Default::default()
        }
    }

    fn raw(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// THE REGRESSION. `build_headers` returned a map with no
    /// `x-opencode-session` on any path, and the Zen gateway rejects such a
    /// request with `MissingSessionID` before inference — so every `ocg/`
    /// request failed. This asserts on the HeaderMap that is handed to the
    /// transport, which is the last point before the wire.
    ///
    /// All four combinations are covered because the previous code shared one
    /// builder across them, and a fix that sets the header only when `stream`
    /// is true would pass a single-case test while leaving non-streaming
    /// requests broken.
    #[test]
    fn every_request_carries_the_gate_session_header() {
        let ex = executor();
        let conn = connection();
        for stream in [true, false] {
            for model in ["glm-5.1", "qwen3.6-plus"] {
                let headers = ex.build_headers(&conn, stream, model, "ses_abc");
                let got = headers
                    .get(SESSION_HEADER)
                    .expect("gate session header must be present")
                    .to_str()
                    .expect("ascii");
                assert_eq!(got, "ses_abc", "model={model} stream={stream}");
            }
        }
    }

    /// A real OpenCode client's own session is passed through unchanged, so one
    /// conversation keeps one session instead of being re-seeded every turn.
    #[test]
    fn a_native_session_is_passed_through_unchanged() {
        let native = "ses_0123456789abZ9xYQ1wErTy";
        let got = OpenCodeGoExecutor::resolve_gate_session(
            &raw(&[("x-opencode-session", native)]),
            &json!({"messages": []}),
            "conn-1",
            Some("codex"),
        );
        assert_eq!(got, native);
    }

    /// The lookup is case-insensitive (9router lowercases every header key
    /// before comparing).
    #[test]
    fn the_session_lookup_ignores_header_case() {
        let got = OpenCodeGoExecutor::resolve_gate_session(
            &raw(&[("X-OpenCode-Session", "ses_0123456789abZ9xYQ1wErTy")]),
            &json!({"messages": []}),
            "conn-1",
            None,
        );
        assert_eq!(got, "ses_0123456789abZ9xYQ1wErTy");
    }

    /// Deliberately NOT the `opencode.rs` shape. That executor emits
    /// `ses_` + 12 hex + 14 base62; this one emits `ses_` + 32 hex. A fix that
    /// copied the sibling's constants would produce a plausible-looking value
    /// the gateway rejects, so the difference is pinned here.
    #[test]
    fn the_translated_session_uses_the_opencode_go_shape() {
        let session = translated_session("seed-abc", Some("codex"));
        let hex = session
            .strip_prefix("ses_")
            .expect("9router prefixes ses_")
            .to_string();
        assert_eq!(hex.len(), 32, "32 hex chars, not opencode.rs's 12+14");
        assert!(
            hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "must be pure hex, got {hex}"
        );
    }

    /// The seed is executor-specific: `opencode-go\0{tool}\0{id}`, not
    /// opencode.rs's `opencode\0{id}`. Two executors sharing a seed would make
    /// one client's session collide with another's.
    #[test]
    fn the_seed_is_scoped_to_this_executor_and_client_tool() {
        let a = translated_session("seed", Some("codex"));
        let b = translated_session("seed", Some("claude"));
        let c = translated_session("seed", None);
        assert_ne!(a, b, "client tool must change the seed");
        assert_ne!(a, c, "an absent tool must not equal a present one");
        // Absent client tool falls back to 9router's literal "generic".
        assert_eq!(c, translated_session("seed", Some("generic")));
        assert_eq!(translated_session("seed", Some("codex")).len(), 36);
    }

    /// The header's purpose is conversation routing, so the same conversation
    /// must not produce a new session every turn. This is the property a
    /// naive "generate a random id" fix would break.
    #[test]
    fn the_session_is_stable_across_turns_of_one_conversation() {
        let first = OpenCodeGoExecutor::resolve_gate_session(
            &raw(&[]),
            &json!({"messages": [{"role": "user", "content": "turn one"}]}),
            "conn-1",
            Some("codex"),
        );
        let second = OpenCodeGoExecutor::resolve_gate_session(
            &raw(&[]),
            &json!({"messages": [{"role": "user", "content": "turn one"}]}),
            "conn-1",
            Some("codex"),
        );
        assert_eq!(first, second, "same conversation must reuse its session");
        assert!(first.starts_with("ses_"), "got {first}");
    }

    /// A caller-supplied session that is empty or past the cap is not
    /// forwarded; the gateway rejects a malformed one, so we fall back to a
    /// derived value (9router `normalizeSession` returns null).
    #[test]
    fn an_unusable_caller_session_falls_back_to_a_derived_one() {
        for bad in ["", "   ", &"x".repeat(MAX_SESSION_LENGTH + 1)] {
            let got = OpenCodeGoExecutor::resolve_gate_session(
                &raw(&[("x-opencode-session", bad)]),
                &json!({"messages": []}),
                "conn-1",
                None,
            );
            assert!(
                got.starts_with("ses_") && got != bad,
                "bad input {bad:?} must not be forwarded, got {got}"
            );
        }
    }

    /// A session of exactly the cap length is still valid.
    #[test]
    fn a_session_at_the_cap_is_accepted() {
        let at_cap = "x".repeat(MAX_SESSION_LENGTH);
        assert_eq!(normalize_session(&at_cap), Some(at_cap.clone()));
        assert_eq!(
            normalize_session(&at_cap).map(|s| s.len()),
            Some(MAX_SESSION_LENGTH)
        );
    }
}
