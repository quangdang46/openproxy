//! OpenCode free-tier executor — port of `open-sse/executors/opencode.js`
//! at 9router v0.5.111+ (bead openproxy-4d5h.21).
//!
//! The free lane moved from `/zen/v1` to `/inference/v1` (OpenCode v2) in
//! upstream commit 5bdcd5e7: `/zen/v1` answers every free request with 403
//! FreeTierError. The lane is gated three ways:
//!
//! 1. **TLS ClientHello** — the free gate keys on the cipher list, not
//!    headers (upstream `services/opencodeTlsClient.js`, wreq-js). Rust has
//!    no wreq equivalent in this codebase (reqwest/rustls cannot pin a
//!    BoringSSL cipher list), so requests go out on the normal reqwest
//!    transport — the same fail-open posture upstream takes when wreq-js is
//!    absent. See `OPENCODE_CIPHER_LIST` note below.
//! 2. **Body fingerprint** — `core::utils::opencode_fingerprint` ports
//!    `opencodeFingerprint.js`: verbatim {read,shell} tool definitions,
//!    in-place normalization / aliasing, `<env>` system marker.
//! 3. **Session + headers** — current-time `ses_` ids (inverted
//!    timestamp prefix, small counter), stable per-identity sessions,
//!    v2 UA shape.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::core::config::runtime_config::memory_config;
use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::core::utils::opencode_fingerprint;
use crate::core::utils::session_manager::resolve_session_identity;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const OPENCODE_BASE: &str = "https://opencode.ai";
const OPENCODE_CHAT_PATH: &str = "/inference/v1/chat/completions";
const OPENCODE_RESPONSES_PATH: &str = "/inference/v1/responses";
const OPENCODE_MESSAGES_PATH: &str = "/inference/v1/messages";

/// OpenCode v2 UA shape: `opencode/<channel>/<version>/<client>`. Upstream
/// rejects the v1 "opencode/1.18.x" form with 403 FreeTierError since
/// 2026-10-14 (opencode.js:19-22).
const OPENCODE_UA: &str = "opencode/latest/2.0.26/cli";
const OPENCODE_CLIENT: &str = "cli";
const MAX_SESSION_LENGTH: usize = 256;
const MAX_TOOL_NAME_LEN: usize = 128;
const SESSION_HEADER: &str = "x-opencode-session";
/// Upstream `ANTHROPIC_API_VERSION` (providers/shared.js).
const ANTHROPIC_API_VERSION: &str = "2023-06-01";

/// Models served by `/inference/v1/responses`; every other model stays on
/// `/inference/v1/chat/completions` (opencode.js:35-39).
const RESPONSES_MODELS: &[&str] = &[
    "muse-spark-1.2-contributor-free",
    "muse-spark-1.3-contributor-free",
];
/// Models served by `/inference/v1/messages` (Anthropic shape) —
/// opencode.js:39 `MESSAGES_MODELS`.
const MESSAGES_MODELS: &[&str] = &["union-alpha"];

/// Registry quirk `forceAutoToolChoiceModels` (providers/registry/
/// opencode.js:30): these models reject any tool_choice but "auto".
const FORCE_AUTO_TOOL_CHOICE_MODELS: &[&str] = &["muse-spark-1.3-contributor-free"];

/// Base62 alphabet used by OpenCode's own canonical session ids
/// (opencode.js `BASE62_CHARS`).
const BASE62_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// BoringSSL/Chrome-family cipher list the upstream TLS gate accepts
/// (services/opencodeTlsClient.js `OPENCODE_CIPHER_LIST`). Recorded for
/// parity documentation: reqwest/rustls cannot pin this list, so the
/// ClientHello fingerprint gate is NOT reproduced on this transport —
/// the same position upstream takes when wreq-js is absent (fail-open).
#[allow(dead_code)]
const OPENCODE_CIPHER_LIST_NOTE: &str = "TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:ECDHE-ECDSA-AES128-SHA:ECDHE-RSA-AES128-SHA:ECDHE-ECDSA-AES256-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA:AES256-SHA";

/// `hasValidOpencodeVersion` (opencode.js:30-33): v2 shape only —
/// `opencode/<channel>/<major>.<minor>.<patch>/<client>`, anchored, major>=2.
fn has_valid_opencode_version(ua: &str) -> bool {
    let lower = ua.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let Some(rest) = lower.strip_prefix("opencode/") else {
        return false;
    };
    let _ = bytes;
    // <channel>/<major.minor.patch>/<client> — channel and client are
    // [a-z0-9-]+, version is three dot-separated numbers.
    let mut parts = rest.split('/');
    let Some(channel) = parts.next() else {
        return false;
    };
    let Some(version) = parts.next() else {
        return false;
    };
    let Some(client) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let valid_token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if !valid_token(channel) || !valid_token(client) {
        return false;
    }
    let mut vparts = version.split('.');
    let (Some(major), Some(_minor), Some(_patch), None) =
        (vparts.next(), vparts.next(), vparts.next(), vparts.next())
    else {
        return false;
    };
    let Ok(major) = major.parse::<u32>() else {
        return false;
    };
    _minor.parse::<u32>().is_ok() && _patch.parse::<u32>().is_ok() && major >= 2
}

/// A canonical OpenCode session id: `ses_` + 12 lowercase hex chars + 14
/// base62 chars (opencode.js `OPENCODE_SESSION_RE`).
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

fn normalize_session(value: &str) -> Option<String> {
    let normalized = value.trim();
    if normalized.is_empty() || normalized.len() > MAX_SESSION_LENGTH {
        return None;
    }
    Some(normalized.to_string())
}

// ---------------------------------------------------------------------------
// Session ids — opencode.js:41-172. The prefix is 12 hex of
// `~(timeMs * 0x1000 + counter)` (low 48 bits), like the CLI; upstream
// decodes it and rejects a non-small counter or a future/expired
// timestamp, so mint with the current time and memoize per conversation.
// ---------------------------------------------------------------------------

const SESSION_PREFIX_MASK: u64 = (1u64 << 48) - 1;

fn descending_time_hex(time_ms: u64, counter: u64) -> String {
    // ~(t*0x1000 + c) & (2^48-1), 12 lowercase hex digits.
    let value = !(time_ms.wrapping_mul(0x1000).wrapping_add(counter)) & SESSION_PREFIX_MASK;
    format!("{value:012x}")
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

struct SessionClock {
    last_timestamp: i64,
    counter: u64,
}

static SESSION_CLOCK: LazyLock<Mutex<SessionClock>> = LazyLock::new(|| {
    Mutex::new(SessionClock {
        last_timestamp: -1,
        counter: 0,
    })
});

/// `generateSessionId` (opencode.js:64-71): counter resets when the
/// millisecond changes; upstream rejects counters >= ~100, so it must
/// stay a real per-ms counter.
pub fn generate_session_id() -> String {
    generate_session_id_at(now_ms())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn generate_session_id_at(timestamp: i64) -> String {
    let mut clock = SESSION_CLOCK.lock().expect("session clock");
    if timestamp != clock.last_timestamp {
        clock.last_timestamp = timestamp;
        clock.counter = 0;
    }
    clock.counter += 1;
    let counter = clock.counter;
    drop(clock);
    let prefix = descending_time_hex(timestamp.max(0) as u64, counter);
    format!("ses_{prefix}{}", unstable_random())
}

struct SessionCacheEntry {
    session_id: String,
    last_used: Instant,
}

/// Translated (non-native) sessions memoized per `clientTool\0sessionId`
/// so a conversation keeps one stable, valid session (opencode.js:79-107).
static TRANSLATED_SESSIONS: LazyLock<Mutex<HashMap<String, SessionCacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const MAX_TRANSLATED_SESSIONS: usize = 5000;

/// Stable canonical sessions per downstream identity
/// (opencode.js:127-172) — upstream free-tier quota is accounted per
/// session, so minting a fresh one per request burns through it.
static STABLE_SESSIONS: LazyLock<Mutex<HashMap<String, SessionCacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const MAX_STABLE_SESSIONS: usize = 1000;

fn cache_get(map: &Mutex<HashMap<String, SessionCacheEntry>>, key: &str) -> Option<String> {
    let mut guard = map.lock().expect("session cache");
    guard.get_mut(key).map(|entry| {
        entry.last_used = Instant::now();
        entry.session_id.clone()
    })
}

fn cache_insert(
    map: &Mutex<HashMap<String, SessionCacheEntry>>,
    key: String,
    session_id: String,
    max: usize,
) {
    let mut guard = map.lock().expect("session cache");
    if guard.len() >= max {
        // Evict the least-recently-used entry (JS deletes the oldest Map key).
        if let Some(victim) = guard
            .iter()
            .min_by_key(|(_, e)| e.last_used)
            .map(|(k, _)| k.clone())
        {
            guard.remove(&victim);
        }
    }
    guard.insert(
        key,
        SessionCacheEntry {
            session_id,
            last_used: Instant::now(),
        },
    );
}

fn sweep_expired(map: &Mutex<HashMap<String, SessionCacheEntry>>) {
    let ttl = memory_config::SESSION_TTL;
    let cutoff = Instant::now() - ttl;
    map.lock()
        .expect("session cache")
        .retain(|_, e| e.last_used >= cutoff);
}

/// `translateSessionId` (opencode.js:91-107): a native `ses_` id passes
/// through; anything else is translated to a fresh current-time id,
/// memoized per `clientTool\0sessionId`.
fn translate_session_id(session_id: &str, client_tool: &str) -> String {
    if is_native_opencode_session(session_id.trim()) {
        return session_id.trim().to_string();
    }
    let key = format!("{}\u{0}{}", client_tool, session_id);
    if let Some(existing) = cache_get(&TRANSLATED_SESSIONS, &key) {
        return existing;
    }
    // Mint directly (counter=1) rather than via generate_session_id so the
    // shared clock isn't disturbed; the shape is identical.
    let translated = format!(
        "ses_{}{}",
        descending_time_hex(now_ms().max(0) as u64, 1),
        unstable_random()
    );
    sweep_expired(&TRANSLATED_SESSIONS);
    cache_insert(
        &TRANSLATED_SESSIONS,
        key,
        translated.clone(),
        MAX_TRANSLATED_SESSIONS,
    );
    translated
}

/// `identityKey` (opencode.js:145-155): connection id wins, else a hash
/// of the downstream auth header, else the shared default bucket.
fn identity_key(connection: &ProviderConnection, raw_headers: &BTreeMap<String, String>) -> String {
    let connection_id = connection.id.as_str();
    if !connection_id.is_empty() {
        return format!(
            "opencode:conn:{}",
            &connection_id[..connection_id.len().min(128)]
        );
    }
    let auth = raw_headers
        .iter()
        .find(|(k, _)| {
            let lk = k.to_ascii_lowercase();
            lk == "authorization" || lk == "x-api-key"
        })
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    if !auth.is_empty() {
        let digest = Sha256::digest(auth.as_bytes());
        let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        return format!("opencode:auth:{hex}");
    }
    "opencode:default".to_string()
}

/// `stableSessionId` (opencode.js:157-172): one stable canonical session
/// per downstream identity.
fn stable_session_id(
    connection: &ProviderConnection,
    raw_headers: &BTreeMap<String, String>,
) -> String {
    let key = identity_key(connection, raw_headers);
    if let Some(existing) = cache_get(&STABLE_SESSIONS, &key) {
        return existing;
    }
    sweep_expired(&STABLE_SESSIONS);
    let session_id = generate_session_id();
    cache_insert(
        &STABLE_SESSIONS,
        key,
        session_id.clone(),
        MAX_STABLE_SESSIONS,
    );
    session_id
}

/// `nativeSession` (opencode.js:116-125): a valid `ses_` id from the
/// downstream `x-opencode-session` header passes through unchanged.
fn native_session(raw_headers: &BTreeMap<String, String>) -> Option<String> {
    raw_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(SESSION_HEADER))
        .and_then(|(_, v)| normalize_session(v))
        .filter(|v| is_native_opencode_session(v))
}

/// `bodyHasSessionHints` (opencode.js:174-204).
fn body_has_session_hints(body: &Value) -> bool {
    let Some(obj) = body.as_object() else {
        return false;
    };
    for key in ["session_id", "conversation_id", "prompt_cache_key"] {
        if obj
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
        {
            return true;
        }
    }
    if obj
        .get("metadata")
        .and_then(|m| m.get("user_id"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
    {
        return true;
    }
    if obj
        .get("request")
        .and_then(|r| r.get("sessionId"))
        .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
    {
        return true;
    }
    let arr = obj
        .get("messages")
        .and_then(Value::as_array)
        .or_else(|| obj.get("input").and_then(Value::as_array));
    if let Some(items) = arr {
        let mut assistant_text = String::new();
        for msg in items {
            if msg.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            match msg.get("content") {
                Some(Value::String(s)) => assistant_text.push_str(s),
                Some(Value::Array(parts)) => {
                    for part in parts {
                        if let Some(t) = part.as_str() {
                            assistant_text.push_str(t);
                        } else if let Some(obj) = part.as_object() {
                            for k in ["text", "output", "content"] {
                                if let Some(t) = obj.get(k).and_then(Value::as_str) {
                                    assistant_text.push_str(t);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            if assistant_text.len() >= 50 {
                return true;
            }
            // Responses message items carry type:"message" + content parts.
            if msg.get("type").and_then(Value::as_str) == Some("message") {
                if let Some(parts) = msg.get("content").and_then(Value::as_array) {
                    for part in parts {
                        for k in ["text", "input_text", "output"] {
                            if let Some(t) = part.get(k).and_then(Value::as_str) {
                                assistant_text.push_str(t);
                            }
                        }
                    }
                    if assistant_text.len() >= 50 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// `resolveOpencodeSession` (opencode.js:220-252): native header → hinted
/// (raw header value or resolved conversation id) translated → session
/// manager → stable per-identity.
fn resolve_opencode_session(
    body: &Value,
    connection: &ProviderConnection,
    raw_headers: &BTreeMap<String, String>,
    client_tool: &str,
) -> String {
    if let Some(native) = native_session(raw_headers) {
        return native;
    }
    let incoming = raw_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(SESSION_HEADER))
        .and_then(|(_, v)| normalize_session(v));
    if let Some(hinted) = incoming {
        return translate_session_id(&hinted, client_tool);
    }
    let has_connection = !connection.id.is_empty();
    if has_connection || body_has_session_hints(body) {
        let via_manager = resolve_session_identity(
            Some(
                &raw_headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<HashMap<_, _>>(),
            ),
            Some(body),
            Some(&connection.id),
            "opencode",
        )
        .session_id;
        if let Some(normalized) = normalize_session(&via_manager) {
            return translate_session_id(&normalized, client_tool);
        }
    }
    stable_session_id(connection, raw_headers)
}

// ---------------------------------------------------------------------------
// Model-id helpers
// ---------------------------------------------------------------------------

/// Strip the thinking suffix "model(level)" so registry lookups hit the
/// base id (opencode.js `baseModelId`).
fn base_model_id(model: &str) -> &str {
    let trimmed = model.trim();
    if let Some(open) = trimmed.rfind('(') {
        if trimmed.ends_with(')') && !trimmed[open + 1..trimmed.len() - 1].contains(['(', ')']) {
            return trimmed[..open].trim_end();
        }
    }
    trimmed
}

/// `isMuseSparkModel` (providers/models/helpers.js): strip thinking
/// suffix, take the vendor-prefix base, match `muse[-_]?spark` followed
/// by end or a `-_: .\s` separator (case-insensitive).
fn is_muse_spark_model(model_id: &str) -> bool {
    let base = base_model_id(model_id);
    let base = base.rsplit('/').next().unwrap_or(base);
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

fn is_responses_model(model: &str) -> bool {
    let base = base_model_id(model);
    RESPONSES_MODELS.contains(&base) || is_muse_spark_model(base)
}

fn is_messages_model(model: &str) -> bool {
    MESSAGES_MODELS.contains(&base_model_id(model))
}

fn force_auto_tool_choice(model: &str) -> bool {
    FORCE_AUTO_TOOL_CHOICE_MODELS.contains(&base_model_id(model))
}

fn model_for_body<'a>(model: &'a str, body: &'a Value) -> &'a str {
    body.get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .unwrap_or(model)
}

// ---------------------------------------------------------------------------
// Request transforms
// ---------------------------------------------------------------------------

/// `normalizeResponsesInput` (responsesApi.js:11-25): string → single
/// user message item (blank → "..."), empty array → same placeholder,
/// non-string/non-array → None (caller decides).
pub(crate) fn normalize_responses_input(input: &Value) -> Option<Vec<Value>> {
    match input {
        Value::String(text) => {
            let text = if text.trim().is_empty() {
                "...".to_string()
            } else {
                text.clone()
            };
            Some(vec![json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            })])
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                Some(vec![json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "..." }]
                })])
            } else {
                Some(arr.clone())
            }
        }
        _ => None,
    }
}

/// `sanitizeResponsesItems` (opencode.js:284-315): strip prior-turn
/// reasoning items (encrypted_content cannot be validated across the
/// pooled free-tier accounts → 400), coerce function_call /
/// function_call_output payloads.
pub(crate) fn sanitize_responses_items(body_obj: &mut serde_json::Map<String, Value>) {
    let Some(items) = body_obj.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    items.retain_mut(|item| {
        let Some(obj) = item.as_object_mut() else {
            return true;
        };
        if obj.get("type").and_then(Value::as_str) == Some("reasoning") {
            return false;
        }
        obj.remove("encrypted_content");
        obj.remove("reasoning_encrypted_content");
        match obj.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
                if name.trim().is_empty() {
                    return false;
                }
                let truncated: String = name.trim().chars().take(MAX_TOOL_NAME_LEN).collect();
                obj.insert("name".to_string(), Value::String(truncated));
                let call_id = crate::core::translator::request::openai_responses::clamp_call_id(
                    obj.get("call_id").and_then(Value::as_str),
                );
                obj.insert("call_id".to_string(), Value::String(call_id));
                let args =
                    crate::core::translator::request::openai_responses::coerce_responses_arguments(
                        obj.get("arguments").unwrap_or(&Value::Null),
                    );
                obj.insert("arguments".to_string(), Value::String(args));
                true
            }
            Some("function_call_output") => {
                let call_id = crate::core::translator::request::openai_responses::clamp_call_id(
                    obj.get("call_id").and_then(Value::as_str),
                );
                obj.insert("call_id".to_string(), Value::String(call_id));
                let output =
                    crate::core::translator::request::openai_responses::coerce_responses_output(
                        obj.get("output").unwrap_or(&Value::Null),
                    );
                obj.insert("output".to_string(), Value::String(output));
                true
            }
            _ => true,
        }
    });
}

/// `normalizeResponsesTools` — see the retained helper below (ported
/// earlier, opencode.js:254-282).
pub(crate) fn normalize_responses_tools(body_obj: &mut serde_json::Map<String, Value>) {
    let Some(tools) = body_obj.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    let mut valid_names = HashSet::new();
    tools.retain_mut(|tool| {
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

// ---------------------------------------------------------------------------
// Thinking levels — `getThinkingLevels("opencode", model)`
// (open-sse/providers/thinkingLevels.js). Only the pattern rows that can
// match the opencode free lane are listed, in upstream order (first match
// wins); provider-qualified rows for other providers are skipped exactly
// like the JS `entry.provider === provider` filter.
// ---------------------------------------------------------------------------

const OPENAI_LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh"];
const OPENAI_MAX_LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
const BUDGET_X_LEVELS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
const BASE_LEVELS: &[&str] = &["none", "low", "medium", "high"];
const GEMINI_LEVELS: &[&str] = &["minimal", "low", "medium", "high"];
const ON_OFF_LEVELS: &[&str] = &["none", "thinking"];
const LEVEL_MAX_LEVELS: &[&str] = &["none", "low", "medium", "high", "max"];
const HI_MAX_LEVELS: &[&str] = &["none", "high", "max"];

fn format_levels(format: Option<&str>) -> &'static [&'static str] {
    match format {
        Some("claude-adaptive") | Some("claude-budget") => BUDGET_X_LEVELS,
        Some("gemini-level") => GEMINI_LEVELS,
        Some("gemini-budget") | Some("qwen") | Some("hunyuan") | Some("step") => BASE_LEVELS,
        Some("zai") | Some("minimax") => ON_OFF_LEVELS,
        Some("kimi") => LEVEL_MAX_LEVELS,
        Some("deepseek") => HI_MAX_LEVELS,
        Some("commandcode") => BUDGET_X_LEVELS,
        // openai + unknown/null → L.openai? No: JS falls back to
        // FORMAT_LEVELS[format] || L.base for missing formats, but "openai"
        // maps to L.openai. Unknown formats → L.base.
        Some("openai") => OPENAI_LEVELS,
        _ => BASE_LEVELS,
    }
}

/// PATTERN_THINKING rows relevant to provider "opencode", in upstream
/// order. `(provider_filter, pattern, levels)` — provider_filter None =
/// unqualified (matches any provider).
const OPENCODE_THINKING_PATTERNS: &[(Option<&str>, &str, &'static [&'static str])] = &[
    // OpenCode Free MiMo rides the OpenRouter-backed route and takes the
    // full OpenAI effort enum verbatim (upstream PATTERN_THINKING row
    // ahead of the unqualified *mimo*v2.6* one).
    (Some("opencode"), "*mimo*v2.6*", OPENAI_MAX_LEVELS),
    (
        None,
        "*mimo*v2.6*",
        &["none", "low", "medium", "high", "xhigh"],
    ),
    (
        None,
        "*mimo*v2.5-pro*",
        &["none", "low", "medium", "high", "xhigh"],
    ),
    // OpenCode Zen free tier deepseek-v4.1-flash-free publishes
    // low|high|max; the upstream forwards the level verbatim.
    (None, "*deepseek-v4.1-flash-free*", &["low", "high", "max"]),
    (
        None,
        "*deepseek-v4.*",
        &["none", "low", "medium", "high", "xhigh", "max"],
    ),
];

/// `getThinkingLevels("opencode", model)` reduced to the level set, or
/// `None` when the model has no reasoning capability.
fn opencode_thinking_levels(model: &str) -> Option<Vec<&'static str>> {
    let clean = base_model_id(model);
    let caps = crate::core::combo::capabilities::get_capabilities_for_model("opencode", clean);
    if !caps.reasoning {
        return None;
    }
    let mut levels: Vec<&'static str> = OPENCODE_THINKING_PATTERNS
        .iter()
        .find(|(provider, pattern, _)| {
            provider.is_none_or(|p| p == "opencode")
                && crate::core::combo::capabilities::match_pattern(pattern, clean)
        })
        .map(|(_, _, levels)| levels.to_vec())
        .unwrap_or_else(|| format_levels(caps.thinking_format).to_vec());
    if !caps.thinking_can_disable {
        levels.retain(|l| *l != "none");
    }
    Some(levels)
}

/// `normalizeOpencodeReasoning` (opencode.js:317-338): `reasoning_effort`
/// (or an existing `reasoning.effort`) becomes
/// `reasoning: { effort, summary: "auto" }`, with `max`/`ultra` clamped
/// down to the highest level the model accepts. Always writes the
/// reasoning object when an effort string is present — even when the
/// model declares no reasoning capability (upstream only skips the
/// *clamp*, not the write).
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
    let clean_model = base_model_id(model);
    let supported_levels = opencode_thinking_levels(clean_model);
    let mut effort = requested.to_lowercase();
    effort = effort.trim().to_string();
    if (effort == "max" || effort == "ultra")
        && supported_levels
            .as_ref()
            .is_some_and(|levels| !levels.is_empty() && !levels.contains(&effort.as_str()))
    {
        let levels = supported_levels.as_deref().unwrap_or(&[]);
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

/// `dropAdaptiveReasoningEffort` (opencode.js:346-348): the Chat
/// Completions lane validates reasoning_effort against the plain OpenAI
/// enum and 400s on "auto" (Claude Code adaptive thinking); omitting the
/// field lets the upstream pick its own default.
fn drop_adaptive_reasoning_effort(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        if obj.get("reasoning_effort").and_then(Value::as_str) == Some("auto") {
            obj.remove("reasoning_effort");
        }
    }
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

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
    pub raw_headers: BTreeMap<String, String>,
}

pub struct OpenCodeExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
    /// sent (alias) name → original name, from the fingerprint tool
    /// concealment pass. Empty when no caller tool collided with a
    /// fingerprint name. 9router `takeRenamedToolNames`.
    pub tool_name_map: BTreeMap<String, String>,
}

/// Which upstream lane a model takes (opencode.js `buildUrl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Responses,
    Messages,
    Chat,
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

    fn lane_for(&self, model: &str) -> Lane {
        if is_responses_model(model) {
            Lane::Responses
        } else if is_messages_model(model) {
            Lane::Messages
        } else {
            Lane::Chat
        }
    }

    /// Free tier moved from /zen/v1 to /inference/v1 (OpenCode v2);
    /// /zen/v1 now answers every free request with 403 FreeTierError
    /// (opencode.js:425-430).
    fn build_url(&self, model: &str) -> String {
        let path = match self.lane_for(model) {
            Lane::Responses => OPENCODE_RESPONSES_PATH,
            Lane::Messages => OPENCODE_MESSAGES_PATH,
            Lane::Chat => OPENCODE_CHAT_PATH,
        };
        format!("{OPENCODE_BASE}{path}")
    }

    /// `transformRequest` (opencode.js:365-403).
    fn transform_request(&self, model: &str, body: &mut Value) -> BTreeMap<String, String> {
        if let Some(obj) = body.as_object_mut() {
            obj.entry("model".to_string())
                .or_insert_with(|| Value::String(model.to_string()));
            // Zen rejects non-streaming requests on free models with 403
            // FreeTierError; always stream upstream and let the handler
            // layer aggregate for non-stream clients.
            obj.insert("stream".to_string(), Value::Bool(true));
        }
        let effective_model = model_for_body(model, body).to_string();
        let lane = self.lane_for(&effective_model);
        let mut tool_name_map = BTreeMap::new();

        match lane {
            Lane::Responses => {
                // ponytail: only models confirmed auto-only; widen the
                // allowlist with evidence (registry quirks port).
                if force_auto_tool_choice(&effective_model) {
                    let has_tc = body.get("tool_choice").is_some();
                    let is_auto =
                        body.get("tool_choice") == Some(&Value::String("auto".to_string()));
                    if has_tc && !is_auto {
                        if let Some(obj) = body.as_object_mut() {
                            obj.insert(
                                "tool_choice".to_string(),
                                Value::String("auto".to_string()),
                            );
                        }
                    }
                }
                // A request that entered via a native /v1/responses
                // endpoint (source == target) skips translation and arrives
                // in the intermediate messages[] shape, which /responses
                // rejects ("unknown parameter `messages`") — convert when
                // `input` is absent (mirrors codex.rs dual-shape handling).
                if body.get("input").is_none() {
                    crate::core::translator::request::openai_responses::chat_to_openai_responses_request(
                        &effective_model,
                        body,
                        true,
                        None,
                    );
                }
                // Responses API names the output cap max_output_tokens and
                // takes thinking as reasoning:{effort,summary} — normalize
                // the Chat fields at this boundary (opencode.js:381-388).
                if let Some(obj) = body.as_object_mut() {
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
                }
                normalize_opencode_reasoning(&effective_model, body);
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("stream".to_string(), Value::Bool(true));
                    obj.insert("store".to_string(), Value::Bool(false));
                    // Input normalization + item sanitation.
                    if let Some(input) = obj.get("input").cloned() {
                        if let Some(normalized) = normalize_responses_input(&input) {
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
                }
                if let Some(obj) = body.as_object_mut() {
                    normalize_responses_tools(obj);
                    sanitize_responses_items(obj);
                }
                // Free-tier fingerprint tools are required even when an
                // agent client already supplied tools; skipping cloaking
                // triggers 403 FreeTierError (opencode.js:394-397).
                tool_name_map = opencode_fingerprint::apply_fingerprint_tools(body, true, false);
            }
            _ => {
                drop_adaptive_reasoning_effort(body);
                tool_name_map = opencode_fingerprint::apply_fingerprint_tools(
                    body,
                    false,
                    lane == Lane::Messages,
                );
            }
        }
        crate::core::utils::reasoning_content_injector::inject_reasoning_content(
            "opencode",
            &effective_model,
            body,
        );
        tool_name_map
    }

    /// `buildHeaders` (opencode.js:432-458): mirrors the v2 CLI. Upstream
    /// needs `x-opencode-session-id` next to `x-opencode-session`, and
    /// rejects any `x-opencode-request` (v1 only) with 403 — that header
    /// is deliberately absent here.
    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
        session: &str,
        lane: Lane,
        raw_headers: &BTreeMap<String, String>,
    ) -> HeaderMap {
        let lower: HashMap<String, &str> = raw_headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str()))
            .collect();

        let downstream_ua = lower.get("user-agent").copied().unwrap_or("");
        let is_opencode_downstream = has_valid_opencode_version(downstream_ua);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer public"));
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
            HeaderValue::from_str(
                lower
                    .get("x-opencode-client")
                    .copied()
                    .unwrap_or(OPENCODE_CLIENT),
            )
            .unwrap_or_else(|_| HeaderValue::from_static(OPENCODE_CLIENT)),
        );
        let session_hv =
            HeaderValue::from_str(session).unwrap_or_else(|_| HeaderValue::from_static("ses_"));
        for name in [
            "x-opencode-session",
            "x-opencode-session-id",
            "x-session-affinity",
            "x-session-id",
        ] {
            headers.insert(name, session_hv.clone());
        }
        headers.insert(
            "x-opencode-project",
            HeaderValue::from_str(lower.get("x-opencode-project").copied().unwrap_or("global"))
                .unwrap_or_else(|_| HeaderValue::from_static("global")),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(if stream { "text/event-stream" } else { "*/*" }),
        );
        if lane == Lane::Messages {
            headers.insert(
                "anthropic-version",
                HeaderValue::from_static(ANTHROPIC_API_VERSION),
            );
        }
        let _ = credentials;
        headers
    }

    pub async fn execute_request(
        &self,
        mut request: OpenCodeExecutionRequest,
    ) -> Result<OpenCodeExecutorResponse, OpenCodeExecutorError> {
        // Normalize developer→system role (many providers reject role:developer)
        normalize_developer_role(&mut request.body);

        let tool_name_map = self.transform_request(&request.model, &mut request.body);

        let url = self.build_url(model_for_body(&request.model, &request.body));
        let lane = self.lane_for(model_for_body(&request.model, &request.body));

        // Session resolution needs the transformed body (hints scan
        // messages/input) — 9router prepareRequestCredentials order.
        let header_map: HashMap<String, String> = request
            .raw_headers
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
        let client_tool =
            crate::core::utils::client_detector::detect_client_tool(&header_map, &request.body)
                .map(|t| t.as_str().to_string())
                .unwrap_or_else(|| "generic".to_string());
        let session = resolve_opencode_session(
            &request.body,
            &request.credentials,
            &request.raw_headers,
            &client_tool,
        );
        let headers = self.build_headers(
            &request.credentials,
            request.stream,
            &session,
            lane,
            &request.raw_headers,
        );

        // TLS ClientHello fingerprint (upstream opencodeTlsClient.js):
        // reqwest/rustls cannot pin the BoringSSL cipher list wreq-js
        // uses, so this stays on the standard transport — documented
        // fail-open gap, same as upstream without wreq-js.
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
            tool_name_map,
        })
    }
}

impl std::fmt::Display for OpenCodeExecutorError {
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
    use serde_json::json;

    #[test]
    fn ua_version_gate_requires_v2_shape() {
        // v2 shape: opencode/<channel>/<major.minor.patch>/<client>
        assert!(has_valid_opencode_version("opencode/latest/2.0.26/cli"));
        assert!(has_valid_opencode_version("opencode/beta/2.1.0/desktop"));
        // v1 forms are rejected upstream (403 since 2026-10-14).
        assert!(!has_valid_opencode_version("opencode/1.18.31"));
        assert!(!has_valid_opencode_version(
            "opencode/1.18.31 ai-sdk/provider-utils/4.0.40"
        ));
        assert!(!has_valid_opencode_version("opencode/latest/1.9.9/cli"));
        assert!(!has_valid_opencode_version("opencode"));
        assert!(!has_valid_opencode_version(""));
        assert!(!has_valid_opencode_version("claude-code/1.0"));
        // Anchored: trailing tokens fail like the JS regex.
        assert!(!has_valid_opencode_version(
            "opencode/latest/2.0.26/cli extra"
        ));
    }

    #[test]
    fn session_id_shape_and_prefix_decode() {
        let id = generate_session_id();
        assert!(is_native_opencode_session(&id), "id={id}");
        // Prefix must be the inverted ~(t*0x1000+counter) encoding.
        let hex = &id[4..16];
        let prefix = u64::from_str_radix(hex, 16).unwrap();
        let inverted = !prefix & SESSION_PREFIX_MASK;
        // counter is small (<= a handful per ms in tests).
        let counter = inverted & 0xFFF;
        assert!(counter <= 0x100, "counter too large: {counter}");
    }

    #[test]
    fn translate_session_passes_native_and_memoizes() {
        let native = "ses_0123456789abCDEFGHIJKLMN00";
        assert_eq!(translate_session_id(native, "generic"), native);
        let a = translate_session_id("conv-1", "generic");
        let b = translate_session_id("conv-1", "generic");
        assert_eq!(a, b, "same seed → same translated session");
        assert!(is_native_opencode_session(&a));
        let c = translate_session_id("conv-2", "generic");
        assert_ne!(a, c);
    }

    #[test]
    fn stable_session_is_per_identity() {
        let conn = ProviderConnection {
            id: "conn-1".into(),
            provider: "opencode".into(),
            ..Default::default()
        };
        let headers = BTreeMap::new();
        let a = stable_session_id(&conn, &headers);
        let b = stable_session_id(&conn, &headers);
        assert_eq!(a, b);
        let conn2 = ProviderConnection {
            id: "conn-2".into(),
            ..conn.clone()
        };
        let c = stable_session_id(&conn2, &headers);
        assert_ne!(a, c, "different identities get different sessions");
    }

    #[test]
    fn resolve_session_prefers_native_header() {
        let conn = ProviderConnection {
            id: "conn-x".into(),
            ..Default::default()
        };
        let native = "ses_0123456789abCDEFGHIJKLMN00";
        let mut headers = BTreeMap::new();
        headers.insert("x-opencode-session".to_string(), native.to_string());
        let body = json!({"messages": []});
        assert_eq!(
            resolve_opencode_session(&body, &conn, &headers, "generic"),
            native
        );
    }

    #[test]
    fn resolve_session_translates_non_native_header_hint() {
        let conn = ProviderConnection {
            id: "conn-y".into(),
            ..Default::default()
        };
        let mut headers = BTreeMap::new();
        headers.insert("x-opencode-session".to_string(), "plain-seed".to_string());
        let body = json!({"messages": []});
        let a = resolve_opencode_session(&body, &conn, &headers, "generic");
        assert!(is_native_opencode_session(&a));
        let b = resolve_opencode_session(&body, &conn, &headers, "generic");
        assert_eq!(a, b, "hinted sessions are memoized");
    }

    #[test]
    fn lane_routing_matches_upstream() {
        let ex =
            OpenCodeExecutor::new(std::sync::Arc::new(ClientPool::new()), None).expect("executor");
        assert_eq!(
            ex.lane_for("muse-spark-1.3-contributor-free"),
            Lane::Responses
        );
        assert_eq!(ex.lane_for("muse-spark-9-future"), Lane::Responses);
        assert_eq!(ex.lane_for("union-alpha"), Lane::Messages);
        assert_eq!(ex.lane_for("big-pickle"), Lane::Chat);
        assert_eq!(ex.lane_for("deepseek-v4.1-flash-free"), Lane::Chat);
        assert!(is_responses_model(
            "oc/muse-spark-1.2-contributor-free(xhigh)"
        ));
    }

    #[test]
    fn urls_use_inference_v1() {
        let ex =
            OpenCodeExecutor::new(std::sync::Arc::new(ClientPool::new()), None).expect("executor");
        assert_eq!(
            ex.build_url("muse-spark-1.3-contributor-free"),
            "https://opencode.ai/inference/v1/responses"
        );
        assert_eq!(
            ex.build_url("union-alpha"),
            "https://opencode.ai/inference/v1/messages"
        );
        assert_eq!(
            ex.build_url("big-pickle"),
            "https://opencode.ai/inference/v1/chat/completions"
        );
    }

    #[test]
    fn headers_mirror_v2_cli() {
        let ex =
            OpenCodeExecutor::new(std::sync::Arc::new(ClientPool::new()), None).expect("executor");
        let conn = ProviderConnection::default();
        let headers = ex.build_headers(
            &conn,
            true,
            "ses_0123456789abCDEFGHIJKLMN00",
            Lane::Chat,
            &BTreeMap::new(),
        );
        assert_eq!(
            headers.get("user-agent").unwrap(),
            "opencode/latest/2.0.26/cli"
        );
        assert_eq!(headers.get("authorization").unwrap(), "Bearer public");
        for name in [
            "x-opencode-session",
            "x-opencode-session-id",
            "x-session-affinity",
            "x-session-id",
        ] {
            assert_eq!(headers.get(name).unwrap(), "ses_0123456789abCDEFGHIJKLMN00");
        }
        // v1 request header must NOT be sent (upstream 403s it).
        assert!(headers.get("x-opencode-request").is_none());
        assert_eq!(headers.get("x-opencode-client").unwrap(), "cli");
        assert_eq!(headers.get("x-opencode-project").unwrap(), "global");
        // /messages lane adds anthropic-version.
        let mh = ex.build_headers(
            &conn,
            true,
            "ses_0123456789abCDEFGHIJKLMN00",
            Lane::Messages,
            &BTreeMap::new(),
        );
        assert_eq!(mh.get("anthropic-version").unwrap(), ANTHROPIC_API_VERSION);
    }

    #[test]
    fn transform_chat_lane_applies_fingerprint_and_env() {
        let ex =
            OpenCodeExecutor::new(std::sync::Arc::new(ClientPool::new()), None).expect("executor");
        let mut body = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type":"function","function":{"name":"read","description":"mine","parameters":{"type":"object","properties":{"zzz":{"type":"string"}},"required":[]}}}]
        });
        let map = ex.transform_request("big-pickle", &mut body);
        assert_eq!(map.get("read_client").map(String::as_str), Some("read"));
        assert_eq!(body["stream"], json!(true));
        let tools = body["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"shell"));
        // The <env> marker + "do not call placeholders" note land on a
        // prepended system message (first message was user).
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("<env>"));
    }

    #[test]
    fn transform_responses_lane_store_false_and_sanitize() {
        let ex =
            OpenCodeExecutor::new(std::sync::Arc::new(ClientPool::new()), None).expect("executor");
        let mut body = json!({
            "input": [
                {"type": "reasoning", "encrypted_content": "abc"},
                {"type": "function_call", "name": "read", "call_id": "c".repeat(100), "arguments": {"a":1}},
                {"type": "function_call_output", "call_id": "c2", "output": [{"text":"ok"}]}
            ],
            "max_tokens": 100
        });
        ex.transform_request("muse-spark-1.3-contributor-free", &mut body);
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["max_output_tokens"], json!(100));
        assert!(body.get("max_tokens").is_none());
        let input = body["input"].as_array().unwrap();
        // reasoning item dropped, encrypted_content stripped.
        assert_eq!(input.len(), 2);
        assert!(input
            .iter()
            .all(|i| i.get("type") != Some(&json!("reasoning"))));
        // call_id clamped to 64 chars.
        assert_eq!(input[0]["call_id"].as_str().unwrap().len(), 64);
        // arguments object → JSON string.
        assert_eq!(input[0]["arguments"], json!("{\"a\":1}"));
        // output array → joined string.
        assert_eq!(input[1]["output"], json!("ok"));
        // Fingerprint tools appended in flat shape + tool_choice auto.
        assert_eq!(body["tool_choice"], json!("auto"));
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"read") && names.contains(&"shell"));
        assert!(body["instructions"].as_str().unwrap().contains("<env>"));
    }

    #[test]
    fn normalize_reasoning_levels_for_free_models() {
        // mimo-v2.6-flash-free: full OpenAI enum incl. max (provider pattern).
        let mut body = json!({"reasoning_effort": "max"});
        normalize_opencode_reasoning("mimo-v2.6-flash-free", &mut body);
        assert_eq!(body["reasoning"]["effort"], json!("max"));
        // deepseek-v4.1-flash-free: levels are low|high|max; "medium" is not
        // clamped (only max/ultra are), but "ultra"→"max".
        let mut body = json!({"reasoning_effort": "ultra"});
        normalize_opencode_reasoning("deepseek-v4.1-flash-free", &mut body);
        assert_eq!(body["reasoning"]["effort"], json!("max"));
        // glm-5.3-flash-free: openai format minus "none" (canDisable=false),
        // max/ultra clamp to xhigh.
        let mut body = json!({"reasoning_effort": "max"});
        normalize_opencode_reasoning("glm-5.3-flash-free", &mut body);
        assert_eq!(body["reasoning"]["effort"], json!("xhigh"));
    }

    #[test]
    fn drop_adaptive_reasoning_effort_only_auto() {
        let mut body = json!({"reasoning_effort": "auto", "max_tokens": 5});
        drop_adaptive_reasoning_effort(&mut body);
        assert!(body.get("reasoning_effort").is_none());
        let mut body = json!({"reasoning_effort": "high"});
        drop_adaptive_reasoning_effort(&mut body);
        assert_eq!(body["reasoning_effort"], json!("high"));
    }

    #[test]
    fn body_session_hints_detected() {
        assert!(body_has_session_hints(&json!({"session_id": "abc"})));
        assert!(body_has_session_hints(&json!({"prompt_cache_key": "k"})));
        assert!(!body_has_session_hints(&json!({
            "messages": [
                {"role": "user", "content": "x".repeat(60)}
            ]
        }))); // hints need assistant text ≥50, not user
        assert!(body_has_session_hints(&json!({
            "messages": [
                {"role": "assistant", "content": "x".repeat(60)}
            ]
        })));
        assert!(!body_has_session_hints(&json!({"messages": []})));
    }
}
