//! OpenCode Zen executor — port of `open-sse/executors/opencode-zen.js`
//! (9router v0.5.112, bead openproxy-9router-parity-v5104-sync-eidj.51).
//!
//! Zen is the keyed PAYG lane (`ocz` alias): three transports on
//! `https://opencode.ai/zen/v1/{chat/completions,messages,responses}`.
//! Unlike the `opencode` free lane (`/inference/v1`, always `Bearer public`)
//! and `opencode-go` (`/zen/go/v1`), Zen keeps the connection key for paid
//! models and only switches to the attested free-tier identity
//! (`Bearer public` + `x-opencode-request`) for `*-free` model ids —
//! upstream `isOpencodeFreeTierModel`.
//!
//! Request shaping (upstream `transformRequest`):
//! - `stream` is forced `true` on every body (the free-tier gate 403s
//!   `stream:false`); non-streaming clients are covered by the
//!   forceStream/sse_to_json plan (`stream_flags.rs`).
//! - Chat lane: the bash/glob/grep/read fingerprint quartet is appended.
//! - Responses lane (Muse Spark): input normalization, `max_output_tokens`
//!   mapping, `reasoning{effort,summary}`, `store:false`, flat fingerprint
//!   tools, tool/item sanitation — shared helpers live in `opencode.rs`.

use std::collections::BTreeMap;
use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const CHAT_COMPLETIONS_URL: &str = "https://opencode.ai/zen/v1/chat/completions";
const RESPONSES_URL: &str = "https://opencode.ai/zen/v1/responses";

/// 9router `opencode-zen.js` OPENCODE_UA — the v1 CLI shape the Zen gateway
/// accepts (`hasValidOpencodeVersion` requires major>1 or 1.17+).
const OPENCODE_ZEN_UA: &str = "opencode/1.18.31";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const SESSION_HEADER: &str = "x-opencode-session";
const MAX_SESSION_LENGTH: usize = 256;

/// Free-tier fingerprint quartet (opencode-zen.js
/// `OPENCODE_FINGERPRINT_TOOLS`) — upstream 403s requests missing them.
const FINGERPRINT_TOOLS: &[&str] = &["bash", "glob", "grep", "read"];

const BASE62_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

// ---------------------------------------------------------------------------
// Model classification
// ---------------------------------------------------------------------------

/// Strip a trailing thinking suffix `model(level)` and take the
/// vendor-prefix base — same normalization `isMuseSparkModel` applies.
fn base_model_id(model: &str) -> &str {
    let mut clean = model.trim();
    if let Some(open) = clean.rfind('(') {
        if clean.ends_with(')') && !clean[open + 1..clean.len() - 1].contains(['(', ')']) {
            clean = clean[..open].trim_end();
        }
    }
    clean.rsplit('/').next().unwrap_or(clean)
}

/// `isMuseSparkModel` (providers/models/helpers.js) — local copy to keep
/// the executor free of a core::chat dependency (same as opencode_go.rs).
fn is_muse_spark_model(model_id: &str) -> bool {
    let base = base_model_id(model_id);
    let lower = base.to_lowercase();
    let Some(after_muse) = lower.strip_prefix("muse") else {
        return false;
    };
    let after_sep = after_muse.strip_prefix(['-', '_']).unwrap_or(after_muse);
    let Some(after_spark) = after_sep.strip_prefix("spark") else {
        return false;
    };
    after_spark.is_empty() || after_spark.starts_with(['-', '_', ':', '.', ' ', '\t'])
}

/// `isResponsesModel` (opencode-zen.js): Muse Spark lives on /responses
/// even when a stale runtimeTransport leaks in.
fn is_responses_model(model: &str) -> bool {
    is_muse_spark_model(model)
}

/// `isOpencodeFreeTierModel` (providers/models/helpers.js:44-49): the
/// `-free` suffix gates the attested free-tier identity on the keyed lane.
fn is_free_tier_model(model: &str) -> bool {
    let base = base_model_id(model);
    base.to_lowercase().ends_with("-free")
}

// ---------------------------------------------------------------------------
// Session + request ids (opencode-zen.js:22-83)
// ---------------------------------------------------------------------------

const OPENCODE_SESSION_RE_LEN: usize = 12 + 14;

fn is_native_opencode_session(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("ses_") else {
        return false;
    };
    if rest.len() != OPENCODE_SESSION_RE_LEN {
        return false;
    }
    let (hex_part, b62_part) = rest.split_at(12);
    hex_part
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && b62_part.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn normalize_session(value: &str) -> Option<String> {
    let normalized = value.trim();
    if normalized.is_empty() || normalized.len() > MAX_SESSION_LENGTH {
        return None;
    }
    Some(normalized.to_string())
}

/// `nativeSession` (opencode-zen.js:127-139): case-insensitive
/// `x-opencode-session` lookup; only `ses_*`-shaped values pass through.
fn native_session(raw_headers: &BTreeMap<String, String>) -> Option<String> {
    raw_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(SESSION_HEADER))
        .and_then(|(_, v)| normalize_session(v))
        .filter(|v| is_native_opencode_session(v))
}

fn unstable_random() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..14)
        .map(|_| {
            let b: u8 = rng.gen_range(0..=255);
            BASE62_CHARS[(b as usize) % 62] as char
        })
        .collect()
}

const SESSION_PREFIX_MASK: u64 = (1u64 << 48) - 1;
const SESSION_TIME_MASK: u64 = (1u64 << 36) - 1;

/// `descendingTimeHex` (opencode-zen.js:45-48): `~(t*0x1000 + c)` low 48
/// bits as 12 lowercase hex digits.
fn descending_time_hex(time: u64, counter: u64) -> String {
    let value = !(time.wrapping_mul(0x1000).wrapping_add(counter)) & SESSION_PREFIX_MASK;
    format!("{value:012x}")
}

/// `generateSessionId` (opencode-zen.js:50-53).
#[allow(dead_code)]
fn generate_session_id(timestamp_ms: i64) -> String {
    format!(
        "ses_{}{}",
        descending_time_hex(timestamp_ms.max(0) as u64, 1),
        unstable_random()
    )
}

/// `translateSessionId` (opencode-zen.js:56-83): native `ses_` ids pass
/// through; anything else is deterministically translated — sha256 of
/// `opencode\0{clientTool}\0{sessionId}`, time field from the first 5
/// digest bytes (36-bit mask), random part from bytes 6..20 mod 62.
fn translate_session_id(session_id: &str, client_tool: &str) -> String {
    if is_native_opencode_session(session_id.trim()) {
        return session_id.trim().to_string();
    }
    let mut hasher = Sha256::new();
    hasher.update(b"opencode\0");
    hasher.update(client_tool.as_bytes());
    hasher.update(b"\0");
    hasher.update(session_id.as_bytes());
    let digest = hasher.finalize();
    // digest[0..5] big-endian → 40 bits, masked to 36.
    let mut time_bytes = [0u8; 8];
    time_bytes[3..8].copy_from_slice(&digest[0..5]);
    let time = (u64::from_be_bytes(time_bytes)) & SESSION_TIME_MASK;
    let time_hex = descending_time_hex(time, 1);
    let mut random_part = String::with_capacity(14);
    for i in 6..20 {
        random_part.push(BASE62_CHARS[(digest[i] as usize) % 62] as char);
    }
    format!("ses_{time_hex}{random_part}")
}

/// `generateRequestId` (opencode-zen.js:55-63... actually :55): `msg_` +
/// 6-byte big-endian hex of `timestamp*0x1000+1` + 14 base62 random chars.
fn generate_request_id(timestamp_ms: i64) -> String {
    let value = (timestamp_ms.max(0) as u128) * 0x1000 + 1;
    let time_hex = format!("{value:012x}");
    // Keep the low 48 bits → last 12 hex chars (6 bytes), matching JS
    // which extracts bytes 40..88 of the bigint (always 6 bytes here).
    let time_hex = &time_hex[time_hex.len().saturating_sub(12)..];
    format!("msg_{time_hex}{}", unstable_random())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// UA gate (opencode-zen.js:24-30 `hasValidOpencodeVersion`)
// ---------------------------------------------------------------------------

/// v1 UA shape `opencode/<major>.<minor>[.<patch>]` — major>1 or 1.17+.
/// (Distinct from the v2 `opencode/<channel>/<version>/<client>` check in
/// `opencode.rs`.)
fn has_valid_opencode_version(ua: &str) -> bool {
    let Some(rest) = ua.trim().to_lowercase().strip_prefix("opencode/") else {
        return false;
    };
    let ver = rest.split('/').next().unwrap_or("");
    let mut parts = ver.split('.');
    let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
        return false;
    };
    let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
        return false;
    };
    major > 1 || (major == 1 && minor >= 17)
}

// ---------------------------------------------------------------------------
// Fingerprint tools (opencode-zen.js:95-127)
// ---------------------------------------------------------------------------

fn tool_name_of(tool: &Value) -> String {
    if !tool.is_object() || tool.is_array() {
        return String::new();
    }
    let direct = tool.get("name").and_then(Value::as_str);
    let nested = tool
        .get("function")
        .filter(|f| f.is_object() && !f.is_array())
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str);
    direct.or(nested).unwrap_or("").trim().to_string()
}

fn fingerprint_tool_present(tools: &[Value], name: &str) -> bool {
    tools.iter().any(|t| tool_name_of(t) == name)
}

/// `ensureChatFingerprintTools`: append the quartet in Chat Completions
/// `{type:"function", function:{...}}` shape when missing.
fn ensure_chat_fingerprint_tools(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    if !obj.get("tools").is_some_and(Value::is_array) {
        obj.insert("tools".to_string(), Value::Array(Vec::new()));
    }
    let tools = obj
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .expect("tools array just ensured");
    for name in FINGERPRINT_TOOLS {
        if fingerprint_tool_present(tools, name) {
            continue;
        }
        tools.push(json!({
            "type": "function",
            "function": {
                "name": name,
                "description": format!("OpenCode built-in {name} tool"),
                "parameters": { "type": "object", "properties": {} },
            }
        }));
    }
}

/// `ensureResponsesFingerprintTools`: same quartet in flat Responses
/// `{type:"function", name, ...}` shape.
fn ensure_responses_fingerprint_tools(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    if !obj.get("tools").is_some_and(Value::is_array) {
        obj.insert("tools".to_string(), Value::Array(Vec::new()));
    }
    let tools = obj
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .expect("tools array just ensured");
    for name in FINGERPRINT_TOOLS {
        if fingerprint_tool_present(tools, name) {
            continue;
        }
        tools.push(json!({
            "type": "function",
            "name": name,
            "description": format!("OpenCode built-in {name} tool"),
            "parameters": { "type": "object", "properties": {} },
        }));
    }
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OpenCodeZenExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum OpenCodeZenExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for OpenCodeZenExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for OpenCodeZenExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for OpenCodeZenExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for OpenCodeZenExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for OpenCodeZenExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl std::fmt::Display for OpenCodeZenExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Transport/encoding wrappers can carry upstream urls and OS
            // errors — report the kind only (opencode-go.rs parity).
            Self::Request(_)
            | Self::Hyper(_)
            | Self::HyperClientInit(_)
            | Self::InvalidHeader(_)
            | Self::Serialize(_) => f.write_str("upstream request failed"),
            other => write!(f, "{other:?}"),
        }
    }
}

pub struct OpenCodeZenExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
    /// Downstream request headers for passthrough (9router rawHeaders).
    pub raw_headers: BTreeMap<String, String>,
}

pub struct OpenCodeZenExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl OpenCodeZenExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, OpenCodeZenExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// `buildUrl` (opencode-zen.js): Muse Spark → /responses unconditionally;
    /// otherwise the transport-resolved base URL (runtime_transport set by
    /// chat.rs from the source-format-matched leg) or the default
    /// /chat/completions.
    fn build_url(credentials: &ProviderConnection, model: &str) -> String {
        if is_responses_model(model) {
            return RESPONSES_URL.to_string();
        }
        if let Some(rt) = &credentials.runtime_transport {
            if let Some(base) = rt.base_url.as_deref() {
                if !base.is_empty() {
                    return base.to_string();
                }
            }
        }
        CHAT_COMPLETIONS_URL.to_string()
    }

    /// `transformRequest` responses branch — same shape as the opencode-go
    /// lane (opencode-zen.js:296-322 ≈ opencode-go.js:158-185).
    fn transform_responses_body(model: &str, body: &mut Value) {
        super::strip_unsupported::strip_unsupported_params("opencode-zen", model, body);
        let Some(obj) = body.as_object_mut() else {
            return;
        };
        if let Some(input) = obj.get("input").cloned() {
            if let Some(normalized) = super::opencode::normalize_responses_input(&input) {
                obj.insert("input".to_string(), Value::Array(normalized));
            }
        }
        let input_ok = obj
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty());
        if !input_ok {
            obj.insert(
                "input".to_string(),
                json!([{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "..." }] }]),
            );
        }
        // Responses names the output cap max_output_tokens, not max_tokens.
        if !obj.contains_key("max_output_tokens") {
            let v = obj
                .remove("max_completion_tokens")
                .or_else(|| obj.remove("max_tokens"));
            if let Some(v) = v {
                obj.insert("max_output_tokens".to_string(), v);
            }
        } else {
            obj.remove("max_tokens");
            obj.remove("max_completion_tokens");
        }
        // reasoning_effort → reasoning{effort, summary:"auto"}.
        let effort = obj.remove("reasoning_effort");
        let reasoning_absent = obj.get("reasoning").is_none();
        if let (Some(e), true) = (effort, reasoning_absent) {
            obj.insert(
                "reasoning".to_string(),
                json!({ "effort": e, "summary": "auto" }),
            );
        }
        if let Some(r) = obj.get_mut("reasoning").and_then(Value::as_object_mut) {
            r.entry("summary".to_string())
                .or_insert(Value::String("auto".to_string()));
        }
        obj.insert("stream".to_string(), Value::Bool(true));
        obj.insert("store".to_string(), Value::Bool(false));
        super::opencode::normalize_responses_tools(obj);
        super::opencode::sanitize_responses_items(obj);
        ensure_responses_fingerprint_tools(body);
    }

    /// `prepareRequestCredentials` + `buildHeaders` session resolution:
    /// caller's native `ses_*` session wins, else a deterministic
    /// digest-derived session from the conversation-stable identity.
    fn resolve_session(
        raw_headers: &BTreeMap<String, String>,
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
            "opencode-zen",
        )
        .session_id;
        let resolved = normalize_session(&resolved).unwrap_or_else(|| "opencode-zen".to_string());
        translate_session_id(&resolved, client_tool.unwrap_or("generic"))
    }

    /// `buildHeaders` (opencode-zen.js:258-296): DefaultExecutor auth first
    /// (x-api-key on the /messages leg, Bearer elsewhere), then the Zen
    /// client fingerprint headers, then the free-tier identity override.
    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
        url: &str,
        model: &str,
        session: &str,
        raw_headers: &BTreeMap<String, String>,
    ) -> HeaderMap {
        let lower: std::collections::HashMap<String, &str> = raw_headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str()))
            .collect();

        let key = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .unwrap_or("");

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        // Transport auth: the /messages leg authenticates with x-api-key +
        // anthropic-version (registry transports[1].auth); every other leg
        // uses Bearer.
        if url.ends_with("/messages") {
            headers.insert(
                "x-api-key",
                HeaderValue::from_str(key).unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            headers.insert(
                "anthropic-version",
                HeaderValue::from_static(ANTHROPIC_API_VERSION),
            );
        } else {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }

        // Zen client fingerprint: spoof the official v1 CLI UA unless the
        // downstream client already speaks a valid opencode UA.
        let downstream_ua = lower.get("user-agent").copied().unwrap_or("");
        let ua = if has_valid_opencode_version(downstream_ua) {
            downstream_ua
        } else {
            OPENCODE_ZEN_UA
        };
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(ua).unwrap_or_else(|_| HeaderValue::from_static(OPENCODE_ZEN_UA)),
        );
        headers.insert(
            "x-opencode-client",
            HeaderValue::from_str(lower.get("x-opencode-client").copied().unwrap_or("desktop"))
                .unwrap_or_else(|_| HeaderValue::from_static("desktop")),
        );
        headers.insert(
            "x-opencode-project",
            HeaderValue::from_str(lower.get("x-opencode-project").copied().unwrap_or("global"))
                .unwrap_or_else(|_| HeaderValue::from_static("global")),
        );
        headers.insert(
            SESSION_HEADER,
            HeaderValue::from_str(session).unwrap_or_else(|_| HeaderValue::from_static("ses_")),
        );

        if is_free_tier_model(model) {
            // Free-tier models are gated behind the attested free-tier
            // identity even on the keyed lane: `Bearer public` + session +
            // request id. Paid models keep the connection key untouched.
            headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer public"));
            headers.insert(
                "x-opencode-request",
                HeaderValue::from_str(&generate_request_id(now_ms()))
                    .unwrap_or_else(|_| HeaderValue::from_static("msg_")),
            );
        }

        // Zen forces stream:true upstream; the response is always SSE.
        let _ = stream;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers
    }

    pub async fn execute_request(
        &self,
        mut request: OpenCodeZenExecutionRequest,
    ) -> Result<OpenCodeZenExecutorResponse, OpenCodeZenExecutorError> {
        let effective_model = request
            .body
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .unwrap_or(request.model.as_str())
            .to_string();
        let responses_lane = is_responses_model(&effective_model);

        normalize_developer_role(&mut request.body);

        // `transformRequest` (opencode-zen.js:324-330): stream forced true
        // on every lane, then the fingerprint tools.
        if let Some(obj) = request.body.as_object_mut() {
            obj.insert("stream".to_string(), Value::Bool(true));
        }

        if responses_lane {
            Self::transform_responses_body(&effective_model, &mut request.body);
        } else {
            // Chat lane fingerprint quartet. Applied only to the
            // chat-completions shape — the /messages leg carries Claude
            // tools (`input_schema`), where OpenAI-shaped entries would be
            // rejected; upstream applies it unconditionally, which is a
            // latent bug for the claude leg (free-tier models never take it
            // anyway, so the gate is unaffected).
            let on_messages_leg =
                Self::build_url(&request.credentials, &effective_model).ends_with("/messages");
            if !on_messages_leg {
                ensure_chat_fingerprint_tools(&mut request.body);
            }
        }

        crate::core::utils::reasoning_content_injector::inject_reasoning_content(
            "opencode-zen",
            &effective_model,
            &mut request.body,
        );

        let url = Self::build_url(&request.credentials, &effective_model);
        let header_map: std::collections::HashMap<String, String> = request
            .raw_headers
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
        let client_tool =
            crate::core::utils::client_detector::detect_client_tool(&header_map, &request.body)
                .map(|t| t.as_str());
        let session = Self::resolve_session(
            &request.raw_headers,
            &request.body,
            &request.credentials.id,
            client_tool,
        );
        let headers = self.build_headers(
            &request.credentials,
            request.stream,
            &url,
            &effective_model,
            &session,
            &request.raw_headers,
        );

        let client = self.pool.get("opencode-zen", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&request.body)
            .send()
            .await?;

        Ok(OpenCodeZenExecutorResponse {
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

    #[test]
    fn free_tier_detection_matches_upstream_suffix_rule() {
        assert!(is_free_tier_model("mimo-v2.6-flash-free"));
        assert!(is_free_tier_model("muse-spark-1.3-contributor-free"));
        assert!(is_free_tier_model("big-pickle-free(max)"));
        assert!(!is_free_tier_model("big-pickle"));
        assert!(!is_free_tier_model("gpt-5.5"));
        // "-free" must be a suffix on the base id, not anywhere.
        assert!(!is_free_tier_model("free-wheel-model"));
    }

    #[test]
    fn responses_model_is_muse_spark_only() {
        assert!(is_responses_model("muse-spark-1.3"));
        assert!(is_responses_model("muse-spark-1.2-contributor-free"));
        assert!(!is_responses_model("gpt-5.5"));
        assert!(!is_responses_model("claude-opus-5"));
    }

    #[test]
    fn native_session_requires_ses_shape() {
        let mut h = BTreeMap::new();
        h.insert("x-opencode-session".to_string(), "ses_abc".to_string());
        assert!(native_session(&h).is_none());
        // Valid shape: ses_ + 12 lowercase hex + 14 base62.
        let valid = format!("ses_{}{}", "0123456789ab", "aB3xY9zQ2wErTy");
        assert_eq!(valid.len(), 4 + 26);
        h.insert("x-opencode-session".to_string(), valid.clone());
        assert_eq!(native_session(&h).as_deref(), Some(valid.as_str()));
    }

    #[test]
    fn translate_session_id_is_deterministic_and_native_passthrough() {
        let a = translate_session_id("conv-1", "claude");
        let b = translate_session_id("conv-1", "claude");
        assert_eq!(a, b);
        assert!(a.starts_with("ses_"));
        assert_eq!(a.len(), 4 + 26);
        // Different client tool → different session.
        let c = translate_session_id("conv-1", "codex");
        assert_ne!(a, c);
        // Native id passes through untouched.
        let n = format!("ses_{}{}", "0123456789ab", "aB3xY9zQ2wErTy");
        assert_eq!(translate_session_id(&n, "claude"), n);
    }

    #[test]
    fn generate_request_id_has_msg_shape() {
        let id = generate_request_id(1_700_000_000_000);
        assert!(id.starts_with("msg_"));
        // msg_ + 12 hex + 14 base62 = 4+26.
        assert_eq!(id.len(), 30);
        let (hex, b62) = id[4..].split_at(12);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(b62.bytes().all(|b| b.is_ascii_alphanumeric()));
    }

    #[test]
    fn ua_gate_accepts_v1_shape_only() {
        assert!(has_valid_opencode_version("opencode/1.18.31"));
        assert!(has_valid_opencode_version("opencode/2.0.0"));
        assert!(!has_valid_opencode_version("opencode/1.16.0"));
        assert!(!has_valid_opencode_version("opencode/latest/2.0.26/cli"));
        assert!(!has_valid_opencode_version("curl/8.0"));
    }

    #[test]
    fn chat_fingerprint_tools_appended_once() {
        let mut body = json!({ "model": "big-pickle", "messages": [] });
        ensure_chat_fingerprint_tools(&mut body);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        assert_eq!(tools[0]["function"]["name"], "bash");
        // Second pass does not duplicate.
        ensure_chat_fingerprint_tools(&mut body);
        assert_eq!(body["tools"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn responses_fingerprint_tools_use_flat_shape() {
        let mut body = json!({ "model": "muse-spark-1.3", "input": [] });
        ensure_responses_fingerprint_tools(&mut body);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        assert_eq!(tools[0]["name"], "bash");
        assert!(tools[0].get("function").is_none());
    }

    #[test]
    fn build_url_prefers_runtime_transport_then_chat_default() {
        let mut creds = ProviderConnection::default();
        assert_eq!(
            OpenCodeZenExecutor::build_url(&creds, "big-pickle"),
            CHAT_COMPLETIONS_URL
        );
        // Claude leg via transport override.
        creds.runtime_transport = Some(crate::types::RuntimeTransport {
            base_url: Some("https://opencode.ai/zen/v1/messages".to_string()),
        });
        assert_eq!(
            OpenCodeZenExecutor::build_url(&creds, "big-pickle"),
            "https://opencode.ai/zen/v1/messages"
        );
        // Muse Spark ignores the transport and pins /responses.
        assert_eq!(
            OpenCodeZenExecutor::build_url(&creds, "muse-spark-1.3"),
            RESPONSES_URL
        );
    }
}
