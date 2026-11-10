//! ChatGPT Web executor — `chatgpt.com` session provider (HTTP-first port of
//! 9router `open-sse/executors/chatgpt-web.js` + `chatgpt-web/` at v0.5.112,
//! bead openproxy-4d5h.7).
//!
//! Credential format (upstream `readStorageState`): a Playwright
//! storage-state JSON (`provider_specific_data.storageState` or `api_key`),
//! a `Cookie:` header blob, or a bare `__Secure-next-auth.session-token`
//! value. Only the cookie jar is needed on the HTTP path.
//!
//! Flow: `/api/auth/session` → accessToken → sentinel headers
//! (`sentinel.js`: prepare token + optional proof-of-work; Turnstile is NOT
//! solvable here — upstream falls back to Playwright, which is deferred per
//! bead openproxy-4d5h.8, so enforcement surfaces an upstream-style error) →
//! `/backend-api/f/conversation/prepare` → `/f/conversation` SSE → poll
//! `/backend-api/conversation/{id}` until `is_complete`.
//!
//! Deferred (see bead openproxy-4d5h.8): the Playwright clean-room adapter
//! (`adapter.js`, `browserSession.js`) — the Turnstile fallback.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hyper::http;
use rand::RngCore;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE};
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::web_tools::{
    get_tool_nonce, parse_tool_calls_from_text, serialize_tools_to_prompt,
};
use crate::core::utils::web_cookie::strip_cookie_input_prefix;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

const CHATGPT_WEB_URL: &str = "https://chatgpt.com";
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

const SSE_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(1500);
const POLL_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_SSE_BYTES: usize = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// sentinel.js port — pure-Node Sentinel client for chatgpt.com
// ---------------------------------------------------------------------------

const PREPARE_PREFIX: &str = "gAAAAAC";
const PROOF_PREFIX: &str = "gAAAAAB";
const PROOF_SUFFIX: &str = "~S";
const MAX_POW_ITERATIONS: u64 = 500_000;

pub mod sentinel_headers {
    pub const PREPARE: &str = "OpenAI-Sentinel-Chat-Requirements-Prepare-Token";
    pub const REQUIREMENTS: &str = "OpenAI-Sentinel-Chat-Requirements-Token";
    pub const PROOF: &str = "OpenAI-Sentinel-Proof-Token";
    pub const TURNSTILE: &str = "OpenAI-Sentinel-Turnstile-Token";
}

/// Sentinel's 32-bit hash: FNV-1a over UTF-16 code units (JS `charCodeAt`)
/// with three avalanche rounds, 8 lowercase hex chars.
fn sentinel_hash(input: &str) -> String {
    let mut h: u32 = 0x811c9dc5;
    for unit in input.encode_utf16() {
        h ^= unit as u32;
        h = h.wrapping_mul(0x1000193);
    }
    h ^= h >> 16;
    h = h.wrapping_mul(0x85ebca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^= h >> 16;
    format!("{h:08x}")
}

const SCRIPT_CANDIDATES: [&str; 2] = [
    "https://chatgpt.com/cdn/assets/685809.dec86dcdc1.js",
    "https://chatgpt.com/cdn/assets/399565.a1117ca8b4.js",
];
const DOCUMENT_KEYS: [&str; 7] = [
    "cookie",
    "domain",
    "readyState",
    "title",
    "URL",
    "charset",
    "location",
];
const WINDOW_KEYS: [&str; 6] = [
    "caches",
    "crypto",
    "fetch",
    "indexedDB",
    "localStorage",
    "origin",
];
const NAV_PROTOTYPE: [&str; 3] = ["userAgent", "language", "hardwareConcurrency"];

fn pick<T: Copy>(list: &[T]) -> T {
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut bytes);
    list[(u64::from_le_bytes(bytes) as usize) % list.len()]
}

/// Build the fingerprint slot array the web bundle hashes (sentinel.js
/// `buildFingerprint`). Shape is all the server checks; values are plausible.
fn build_fingerprint(user_agent: &str) -> Vec<Value> {
    let nav_key = pick(&NAV_PROTOTYPE);
    // U+2212 MINUS SIGN between nav key and UA, as in the bundle.
    let nav_ua = format!("{nav_key}\u{2212}{user_agent}");
    vec![
        json!(2080),                            // screen w+h
        json!(chrono::Utc::now().to_rfc2822()), // Date string
        Value::Null,                            // performance.memory
        json!(0),                               // PoW counter
        json!(user_agent),
        json!(pick(&SCRIPT_CANDIDATES)),
        Value::Null, // document build id
        json!("en-US"),
        json!("en-US,en"),
        json!(0), // elapsed ms
        json!(nav_ua),
        json!(pick(&DOCUMENT_KEYS)),
        json!(pick(&WINDOW_KEYS)),
        json!(0), // performance.now()
        json!(Uuid::new_v4().to_string()),
        json!(""), // search-param keys
        json!(8),  // hardwareConcurrency
        json!(0),  // performance.timeOrigin
        json!(0),
        json!(0),
        json!(0),
        json!(0),
        json!(0),
        json!(0),
        json!(0), // feature probes
    ]
}

fn encode_fingerprint(slots: &[Value]) -> String {
    STANDARD.encode(serde_json::to_string(slots).unwrap_or_else(|_| "[]".into()))
}

/// `gAAAAAC…` — the prepare-token header value.
fn build_prepare_token(user_agent: &str) -> String {
    let mut slots = build_fingerprint(user_agent);
    slots[3] = json!(1);
    slots[9] = json!(0);
    format!("{PREPARE_PREFIX}{}", encode_fingerprint(&slots))
}

/// Solve a proof-of-work challenge → `gAAAAAB<base64>~S`.
fn solve_proof_of_work(seed: &str, difficulty: &str, user_agent: &str) -> Result<String, String> {
    if seed.is_empty() || difficulty.is_empty() {
        return Err("ChatGPT sentinel asked for proof of work without seed/difficulty".to_string());
    }
    let started = Instant::now();
    let mut slots = build_fingerprint(user_agent);
    for counter in 0..MAX_POW_ITERATIONS {
        slots[3] = json!(counter);
        slots[9] = json!(started.elapsed().as_millis() as u64);
        let encoded = encode_fingerprint(&slots);
        let hash = sentinel_hash(&format!("{seed}{encoded}"));
        // JS: hash.slice(0, difficulty.length) <= difficulty (lexicographic).
        let prefix: String = hash.chars().take(difficulty.chars().count()).collect();
        if prefix.as_str() <= difficulty {
            return Ok(format!("{PROOF_PREFIX}{encoded}{PROOF_SUFFIX}"));
        }
    }
    Err("ChatGPT sentinel proof of work exhausted its iteration budget".to_string())
}

/// Run prepare → requirements and return the sentinel headers for a
/// `/backend-api` call. Port of `fetchSentinelHeaders` (sentinel.js).
async fn fetch_sentinel_headers(
    client: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    user_agent: &str,
) -> Result<Vec<(String, String)>, ChatGptWebError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}"))
            .map_err(|_| ChatGptWebError::InvalidCredentials("bad access token".into()))?,
    );
    headers.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_str(user_agent)
            .unwrap_or_else(|_| HeaderValue::from_static(DEFAULT_USER_AGENT)),
    );

    // prepare
    headers.insert(
        sentinel_headers::PREPARE,
        HeaderValue::from_str(&build_prepare_token(user_agent))
            .map_err(|_| ChatGptWebError::InvalidCredentials("bad prepare token".into()))?,
    );
    let prepared: Value = client
        .post(format!(
            "{base_url}/backend-api/sentinel/chat-requirements/prepare"
        ))
        .headers(headers.clone())
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| ChatGptWebError::Http {
            status: e.status().map(|s| s.as_u16()).unwrap_or(502),
            message: format!("ChatGPT sentinel prepare failed: {e}"),
        })?
        .json()
        .await?;
    let prepare_token = prepared
        .get("prepare_token")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ChatGptWebError::Upstream("ChatGPT sentinel prepare returned no prepare_token".into())
        })?;

    // requirements
    headers.insert(
        sentinel_headers::PREPARE,
        HeaderValue::from_str(prepare_token)
            .map_err(|_| ChatGptWebError::InvalidCredentials("bad prepare token".into()))?,
    );
    let requirements: Value = client
        .post(format!("{base_url}/backend-api/sentinel/chat-requirements"))
        .headers(headers.clone())
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| ChatGptWebError::Http {
            status: e.status().map(|s| s.as_u16()).unwrap_or(502),
            message: format!("ChatGPT sentinel requirements failed: {e}"),
        })?
        .json()
        .await?;

    // Turnstile VM challenge — browser-only upstream; surface it so the
    // caller can return the deferred-fallback error.
    if requirements
        .get("turnstile")
        .map(|t| !t.is_null())
        .unwrap_or(false)
    {
        return Err(ChatGptWebError::Turnstile);
    }

    let mut tokens = Vec::new();
    if let Some(token) = requirements.get("token").and_then(Value::as_str) {
        tokens.push((
            sentinel_headers::REQUIREMENTS.to_string(),
            token.to_string(),
        ));
    }
    if let Some(pow) = requirements.get("proofofwork") {
        if pow
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let seed = pow.get("seed").and_then(Value::as_str).unwrap_or("");
            let difficulty = pow.get("difficulty").and_then(Value::as_str).unwrap_or("");
            let proof = solve_proof_of_work(seed, difficulty, user_agent)
                .map_err(ChatGptWebError::Upstream)?;
            tokens.push((sentinel_headers::PROOF.to_string(), proof));
        }
    }
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Storage state (adapter.js readStorageState / chatGptWebStorageStateFromCookieHeader)
// ---------------------------------------------------------------------------

/// Minimal storage-state view: only the cookie jar matters on the HTTP path.
struct StorageState {
    cookie_header: String,
}

fn cookie_header_from_cookies(cookies: &[Value]) -> String {
    cookies
        .iter()
        .filter(|c| c.get("name").and_then(Value::as_str).is_some())
        .filter_map(|c| {
            let name = c.get("name").and_then(Value::as_str)?;
            let value = c.get("value").and_then(Value::as_str)?;
            Some(format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `chatGptWebStorageStateFromCookieHeader`: bare session-token value,
/// `name=value` pair, or full `;`-separated cookie blob → normalized state.
fn storage_state_from_cookie_header(raw: &str) -> Result<StorageState, ChatGptWebError> {
    let header = strip_cookie_input_prefix(raw);
    if header.is_empty() {
        return Err(ChatGptWebError::InvalidCredentials(
            "ChatGPT Web Cookie header is empty".into(),
        ));
    }

    // Bare session-token value (JWE payload — no `name=` prefix at all).
    if !header.contains('=') {
        return Ok(StorageState {
            cookie_header: format!("__Secure-next-auth.session-token={header}"),
        });
    }

    // Upstream regex: `__Secure-next-auth.session-token` optionally suffixed
    // `.N` (chunked cookies).
    let has_session = header.split(';').any(|part| {
        let name = part.split('=').next().unwrap_or("").trim();
        name == "__Secure-next-auth.session-token"
            || name
                .strip_prefix("__Secure-next-auth.session-token.")
                .map(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
                .unwrap_or(false)
    });
    if !has_session {
        if header.contains(';') || header.contains('=') {
            return Err(ChatGptWebError::InvalidCredentials(
                "ChatGPT Web Cookie header is missing __Secure-next-auth.session-token".into(),
            ));
        }
        return Ok(StorageState {
            cookie_header: format!("__Secure-next-auth.session-token={header}"),
        });
    }
    Ok(StorageState {
        cookie_header: header,
    })
}

/// `readStorageState`: storage-state JSON string/object, cookie header, or
/// bare token. Returns the cookie header used by every HTTP call.
fn read_storage_state(credentials: &ProviderConnection) -> Result<StorageState, ChatGptWebError> {
    let provider_data = &credentials.provider_specific_data;
    let raw = provider_data
        .get("storageState")
        .or_else(|| provider_data.get("storage_state"))
        .cloned()
        .or_else(|| credentials.api_key.clone().map(Value::String));
    let Some(raw) = raw else {
        return Err(ChatGptWebError::MissingCredentials("chatgpt-web".into()));
    };
    match raw {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') {
                let parsed: Value = serde_json::from_str(trimmed).map_err(|_| {
                    ChatGptWebError::InvalidCredentials(
                        "ChatGPT Web browser storage state JSON is invalid".into(),
                    )
                })?;
                let cookies = parsed
                    .get("cookies")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        ChatGptWebError::InvalidCredentials(
                            "ChatGPT Web browser storage state is invalid".into(),
                        )
                    })?;
                let header = cookie_header_from_cookies(cookies);
                if header.is_empty() {
                    return Err(ChatGptWebError::InvalidCredentials(
                        "ChatGPT Web storage state has no cookies".into(),
                    ));
                }
                Ok(StorageState {
                    cookie_header: header,
                })
            } else {
                storage_state_from_cookie_header(trimmed)
            }
        }
        Value::Object(map) => {
            // storageState as a JSON object (not stringified).
            let cookies = map
                .get("cookies")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ChatGptWebError::InvalidCredentials(
                        "ChatGPT Web browser storage state is invalid".into(),
                    )
                })?;
            let header = cookie_header_from_cookies(cookies);
            if header.is_empty() {
                return Err(ChatGptWebError::InvalidCredentials(
                    "ChatGPT Web storage state has no cookies".into(),
                ));
            }
            Ok(StorageState {
                cookie_header: header,
            })
        }
        _ => Err(ChatGptWebError::InvalidCredentials(
            "ChatGPT Web credentials are not a storage state".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// chatHttp.js port — plain-HTTP chat turn
// ---------------------------------------------------------------------------

const TOOL_ORDER: &str = "Respond with ONLY <tool> blocks, one per line. No prose, no code fences, no explanation before or after the blocks.";
const TOOL_LEAD: &str =
    "Use the client tools provided below. Do not answer with prose when a tool is needed.";

/// The strict order is only safe while the model still has tools to call.
fn with_tool_order(prompt: &str, has_tools: bool, messages: &[Value]) -> String {
    if !has_tools {
        return prompt.to_string();
    }
    let has_tool_results = messages.iter().any(|m| {
        m.get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .eq_ignore_ascii_case("tool")
    });
    if has_tool_results {
        prompt.to_string()
    } else {
        format!("{TOOL_LEAD}\n\n{prompt}\n\n{TOOL_ORDER}")
    }
}

/// `prepareToolMessages` (webTools.js): inject the tool contract as a trailing
/// system message + a one-line reminder on the latest user message.
/// Returns `(has_tools, effective_messages)`.
fn prepare_tool_messages(body: &Value, messages: &[Value]) -> (bool, Vec<Value>) {
    let requested = body.get("tools").cloned().unwrap_or(Value::Null);
    let has_tools = requested.as_array().map(|a| !a.is_empty()).unwrap_or(false);
    if !has_tools {
        return (false, messages.to_vec());
    }
    let Some((tool_prompt, _nonce)) = serialize_tools_to_prompt(&requested) else {
        return (true, messages.to_vec());
    };

    // buildToolReminder: tool names extracted from "- name" lines.
    let names: Vec<String> = tool_prompt
        .lines()
        .filter_map(|l| l.strip_prefix("- "))
        .filter_map(|l| l.split(':').next())
        .map(|s| s.trim().to_string())
        .collect();
    let reminder = format!(
        "\n\n[Client protocol reminder: the client-tool contract in the system instructions is active in this conversation. These client tools ARE available via the <tool> block protocol{}.]",
        if names.is_empty() {
            String::new()
        } else {
            format!(": {}", names.join(", "))
        }
    );

    let mut effective: Vec<Value> = messages.to_vec();
    for msg in effective.iter_mut().rev() {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        match msg.get("content") {
            Some(Value::String(s)) => {
                let mut m = msg.clone();
                m["content"] = json!(format!("{s}{reminder}"));
                *msg = m;
            }
            Some(Value::Array(parts)) => {
                let mut m = msg.clone();
                let mut new_parts = parts.clone();
                new_parts.push(json!({"type": "text", "text": reminder}));
                m["content"] = Value::Array(new_parts);
                *msg = m;
            }
            _ => {}
        }
        break;
    }
    effective.push(json!({"role": "system", "content": tool_prompt}));
    (true, effective)
}

/// Flatten OpenAI messages into one prompt turn (chatHttp.js
/// `flattenMessages`): system lines first, then the conversation.
fn flatten_messages(messages: &[Value]) -> String {
    let mut system_lines: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let mut parts: Vec<String> = Vec::new();
        match message.get("content") {
            Some(Value::String(s)) => parts.push(s.clone()),
            Some(Value::Array(items)) => {
                for part in items {
                    if let Some(s) = part.as_str() {
                        parts.push(s.to_string());
                    } else if let Some(t) = part.get("text").and_then(Value::as_str) {
                        parts.push(t.to_string());
                    }
                }
            }
            _ => {}
        }
        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                let name = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str);
                let Some(name) = name else { continue };
                // Upstream JSON.parses string `arguments` before embedding.
                let args = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let args = args
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or(args);
                parts.push(format!(
                    "<tool>{}</tool>",
                    json!({"name": name, "arguments": args})
                ));
            }
        }
        let text = parts.join("\n").trim().to_string();
        if text.is_empty() {
            continue;
        }
        match role {
            "system" | "developer" => system_lines.push(text),
            "tool" => {
                let tcid = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .map(|id| format!(" ({id})"))
                    .unwrap_or_default();
                lines.push(format!("Tool result{tcid}: {text}"));
            }
            "assistant" => lines.push(format!("Assistant: {text}")),
            _ => lines.push(text),
        }
    }
    let mut out = system_lines;
    out.extend(lines);
    out.join("\n\n")
}

/// `finishedAssistantText`: walk the conversation document's mapping for the
/// completed assistant message. `None` while the turn has not finished.
fn finished_assistant_text(document: &Value) -> Option<(String, String)> {
    let mapping = document.get("mapping")?.as_object()?;
    let mut text = String::new();
    let mut complete = false;
    let mut error_text = String::new();
    for node in mapping.values() {
        let Some(message) = node.get("message") else {
            continue;
        };
        let role = message
            .get("author")
            .and_then(|a| a.get("role"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if role != "assistant" {
            continue;
        }
        if message
            .pointer("/metadata/is_complete")
            .and_then(Value::as_bool)
            == Some(true)
        {
            complete = true;
        }
        if message
            .pointer("/content/content_type")
            .and_then(Value::as_str)
            == Some("text")
        {
            if let Some(parts) = message.pointer("/content/parts").and_then(Value::as_array) {
                for part in parts {
                    if let Some(s) = part.as_str() {
                        text.push_str(s);
                    }
                }
            }
        }
        if message
            .pointer("/metadata/is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if let Some(parts) = message.pointer("/content/parts").and_then(Value::as_array) {
                let joined: String = parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .concat();
                if !joined.is_empty() {
                    error_text = joined;
                }
            }
        }
    }
    if !complete {
        return None;
    }
    Some((text, error_text))
}

struct ChatTurn {
    text: String,
    has_tools: bool,
}

/// `runChatGptWebHttpTurn`: sentinel → prepare → conversation SSE → poll.
async fn run_chat_turn(
    client: &reqwest::Client,
    access_token: &str,
    cookie: &str,
    user_agent: &str,
    body: &Value,
) -> Result<ChatTurn, ChatGptWebError> {
    if access_token.is_empty() {
        return Err(ChatGptWebError::Http {
            status: 401,
            message: "ChatGPT session has no access token".into(),
        });
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if messages.is_empty() {
        return Err(ChatGptWebError::Http {
            status: 400,
            message: "ChatGPT Web requires messages".into(),
        });
    }

    let (has_tools, effective_messages) = prepare_tool_messages(body, &messages);
    let prompt = with_tool_order(&flatten_messages(&effective_messages), has_tools, &messages);
    if prompt.trim().is_empty() {
        return Err(ChatGptWebError::Http {
            status: 400,
            message: "ChatGPT Web requires non-empty text".into(),
        });
    }

    let sentinel =
        fetch_sentinel_headers(client, CHATGPT_WEB_URL, access_token, user_agent).await?;

    let mut headers = HeaderMap::new();
    for (k, v) in &sentinel {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            headers.insert(name, value);
        }
    }
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}"))
            .map_err(|_| ChatGptWebError::InvalidCredentials("bad access token".into()))?,
    );
    headers.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_str(user_agent)
            .unwrap_or_else(|_| HeaderValue::from_static(DEFAULT_USER_AGENT)),
    );
    if !cookie.is_empty() {
        if let Ok(v) = HeaderValue::from_str(cookie) {
            headers.insert(COOKIE, v);
        }
    }

    // /f/conversation/prepare
    let prepare_resp = client
        .post(format!(
            "{CHATGPT_WEB_URL}/backend-api/f/conversation/prepare"
        ))
        .headers(headers.clone())
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "action": "next",
            "parent_message_id": Uuid::new_v4().to_string(),
            "model": "auto",
            "client_prepare_state": "success",
            "timezone_offset_min": 0,
            "timezone": "UTC",
            "conversation_mode": { "kind": "primary_assistant" },
            "partial_query": {
                "id": Uuid::new_v4().to_string(),
                "author": { "role": "user" },
                "content": { "content_type": "text", "parts": [prompt] },
            },
            "supports_buffering": true,
            "supported_encodings": ["v1"],
            "client_contextual_info": { "app_name": "chatgpt.com" },
        }))
        .timeout(Duration::from_secs(60))
        .send()
        .await?;
    if !prepare_resp.status().is_success() {
        let status = prepare_resp.status().as_u16();
        let detail = prepare_resp.text().await.unwrap_or_default();
        let detail: String = detail.chars().take(200).collect();
        return Err(ChatGptWebError::Http {
            status,
            message: format!("ChatGPT prepare failed (HTTP {status}) {detail}"),
        });
    }
    let prepare_json: Value = prepare_resp.json().await.unwrap_or(Value::Null);
    let conduit_token = prepare_json
        .get("conduit_token")
        .and_then(Value::as_str)
        .unwrap_or("");

    // /f/conversation SSE — the stream ends before the turn does; the
    // conversation document is polled until is_complete.
    let conv_resp = client
        .post(format!("{CHATGPT_WEB_URL}/backend-api/f/conversation"))
        .headers(headers.clone())
        .header(ACCEPT, "text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .header("X-Conduit-Token", conduit_token)
        .header("X-Oai-Turn-Trace-Id", Uuid::new_v4().to_string())
        .json(&json!({
            "action": "next",
            "messages": [{
                "id": Uuid::new_v4().to_string(),
                "author": { "role": "user" },
                "create_time": chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
                "content": { "content_type": "text", "parts": [prompt] },
                "metadata": { "serialization_metadata": { "custom_symbol_offsets": [] } },
            }],
            "parent_message_id": Uuid::new_v4().to_string(),
            "model": "auto",
            "client_prepare_state": "sent",
            "timezone_offset_min": 0,
            "timezone": "UTC",
            "conversation_mode": { "kind": "primary_assistant" },
            "enable_message_followups": true,
            "supports_buffering": true,
            "supported_encodings": ["v1"],
            "client_contextual_info": { "app_name": "chatgpt.com" },
            "force_parallel_switch": "auto",
        }))
        .timeout(SSE_DEADLINE + Duration::from_secs(15))
        .send()
        .await?;
    if !conv_resp.status().is_success() {
        return Err(ChatGptWebError::Http {
            status: conv_resp.status().as_u16(),
            message: format!(
                "ChatGPT conversation failed (HTTP {})",
                conv_resp.status().as_u16()
            ),
        });
    }
    let sse_bytes = conv_resp.bytes().await.unwrap_or_default();
    let sse_text = String::from_utf8_lossy(&sse_bytes[..sse_bytes.len().min(MAX_SSE_BYTES)]);
    let capped: String = sse_text.chars().take(MAX_SSE_BYTES).collect();

    // Extract the conversation id from the SSE payload.
    let conversation_id = extract_conversation_id(&capped).ok_or_else(|| {
        ChatGptWebError::Upstream("ChatGPT conversation produced no conversation id".into())
    })?;

    // Poll until the assistant message reports is_complete.
    let poll_started = Instant::now();
    loop {
        if poll_started.elapsed() > POLL_TIMEOUT {
            return Err(ChatGptWebError::Http {
                status: 504,
                message: "ChatGPT chat turn timed out".into(),
            });
        }
        let poll = client
            .get(format!(
                "{CHATGPT_WEB_URL}/backend-api/conversation/{conversation_id}"
            ))
            .headers({
                let mut h = HeaderMap::new();
                h.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {access_token}"))
                        .unwrap_or_else(|_| HeaderValue::from_static("")),
                );
                h.insert(
                    reqwest::header::USER_AGENT,
                    HeaderValue::from_str(user_agent)
                        .unwrap_or_else(|_| HeaderValue::from_static(DEFAULT_USER_AGENT)),
                );
                h.insert(ACCEPT, HeaderValue::from_static("application/json"));
                if !cookie.is_empty() {
                    if let Ok(v) = HeaderValue::from_str(cookie) {
                        h.insert(COOKIE, v);
                    }
                }
                h
            })
            .timeout(Duration::from_secs(30))
            .send()
            .await;
        if let Ok(resp) = poll {
            if resp.status().is_success() {
                if let Ok(document) = resp.json::<Value>().await {
                    if let Some((text, error_text)) = finished_assistant_text(&document) {
                        if text.trim().is_empty() && !error_text.is_empty() {
                            let detail: String = error_text.chars().take(200).collect();
                            return Err(ChatGptWebError::Http {
                                status: 400,
                                message: format!("ChatGPT refused the turn: {detail}"),
                            });
                        }
                        return Ok(ChatTurn { text, has_tools });
                    }
                }
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `"conversation_id":"<uuid>"` anywhere in the SSE payload.
fn extract_conversation_id(sse_text: &str) -> Option<String> {
    let needle = "\"conversation_id\"";
    let idx = sse_text.find(needle)?;
    let rest = &sse_text[idx + needle.len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let quote = after.strip_prefix('"')?;
    let end = quote.find('"')?;
    let id = &quote[..end];
    // Upstream regex requires a 36-char uuid shape.
    if id.len() == 36 && id.chars().filter(|c| *c == '-').count() == 4 {
        Some(id.to_string())
    } else {
        None
    }
}

/// Cookie header for stored ChatGPT Web credentials — reused by the
/// connection-test probe.
pub fn chatgpt_web_cookie_header(
    credentials: &ProviderConnection,
) -> Result<String, ChatGptWebError> {
    read_storage_state(credentials).map(|s| s.cookie_header)
}

/// Resolve the session access token (`fetchAccessToken` in chatgpt-web.js).
async fn fetch_access_token(
    client: &reqwest::Client,
    cookie: &str,
    user_agent: &str,
) -> Result<String, ChatGptWebError> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_str(user_agent)
            .unwrap_or_else(|_| HeaderValue::from_static(DEFAULT_USER_AGENT)),
    );
    if !cookie.is_empty() {
        if let Ok(v) = HeaderValue::from_str(cookie) {
            headers.insert(COOKIE, v);
        }
    }
    let resp = client
        .get(format!("{CHATGPT_WEB_URL}/api/auth/session"))
        .headers(headers)
        .timeout(Duration::from_secs(30))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        return Err(ChatGptWebError::Http {
            status: if status == 403 { 401 } else { status },
            message: format!("ChatGPT session unavailable (HTTP {status})"),
        });
    }
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    body.get("accessToken")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| ChatGptWebError::Http {
            status: 401,
            message: "ChatGPT session is not signed in".into(),
        })
}

// ---------------------------------------------------------------------------
// imageGenHttp.js port — picture_v2 image generation over plain HTTP
// ---------------------------------------------------------------------------

const IMG_MAX_SSE_BYTES: usize = 16 * 1024 * 1024;
const IMG_MAX_CONVERSATION_BYTES: usize = 32 * 1024 * 1024;
const IMG_SSE_DEADLINE: Duration = Duration::from_secs(90);
const IMG_POLL_INITIAL_WAIT: Duration = Duration::from_millis(5000);
const IMG_POLL_INTERVAL: Duration = Duration::from_millis(5000);
const IMG_POLL_TIMEOUT: Duration = Duration::from_secs(240);
const IMG_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(90);

fn content_policy_hit(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "content policy",
        "violat", // violates/violation/violated
        "can't create",
        "cannot create",
        "i can't generate",
        "i cannot generate",
        "not able to create",
        "unable to create",
        "safety",
    ]
    .iter()
    .any(|needle| t.contains(needle))
}

/// `collectIds`: pull `file-service://`, `file_00000000…`, and
/// `sediment://` ids out of SSE/document text.
fn collect_media_ids(text: &str, file_ids: &mut Vec<String>, sediment_ids: &mut Vec<String>) {
    fn push_unique(list: &mut Vec<String>, value: &str) {
        if !value.is_empty() && !list.iter().any(|v| v == value) {
            list.push(value.to_string());
        }
    }
    for m in text.match_indices("file-service://") {
        let rest = &text[m.0 + "file-service://".len()..];
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        push_unique(file_ids, &id);
    }
    // file_00000000 + 24 hex chars (37 chars total)
    const PREFIX: &str = "file_00000000";
    let mut start = 0;
    while let Some(pos) = text[start..].find(PREFIX) {
        let abs = start + pos;
        let after = &text[abs + PREFIX.len()..];
        let tail: String = after.chars().take(24).collect();
        if tail.len() == 24 && tail.chars().all(|c| c.is_ascii_hexdigit()) {
            push_unique(file_ids, &format!("{PREFIX}{tail}"));
        }
        start = abs + PREFIX.len();
    }
    for m in text.match_indices("sediment://") {
        let rest = &text[m.0 + "sediment://".len()..];
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        push_unique(sediment_ids, &id);
    }
}

fn auth_headers(
    access_token: &str,
    user_agent: &str,
    cookie: &str,
    sentinel: &[(String, String)],
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (k, v) in sentinel {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            headers.insert(name, value);
        }
    }
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {access_token}")) {
        headers.insert(AUTHORIZATION, v);
    }
    if let Ok(v) = HeaderValue::from_str(user_agent) {
        headers.insert(reqwest::header::USER_AGENT, v);
    }
    if !cookie.is_empty() {
        if let Ok(v) = HeaderValue::from_str(cookie) {
            headers.insert(COOKIE, v);
        }
    }
    headers
}

struct ImageConversationIds {
    conversation_id: String,
    file_ids: Vec<String>,
    sediment_ids: Vec<String>,
}

/// prepare → conversation SSE for `system_hints: ["picture_v2"]`.
async fn run_image_conversation(
    client: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    cookie: &str,
    user_agent: &str,
    sentinel: &[(String, String)],
    prompt: &str,
    size_hint: Option<&str>,
) -> Result<ImageConversationIds, ChatGptWebError> {
    let headers = auth_headers(access_token, user_agent, cookie, sentinel);
    let text = match size_hint {
        Some(s) if !s.is_empty() => format!("{prompt} (aspect ratio {s})"),
        _ => prompt.to_string(),
    };

    let prepare = client
        .post(format!("{base_url}/backend-api/f/conversation/prepare"))
        .headers({
            let mut h = headers.clone();
            h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            h.insert(ACCEPT, HeaderValue::from_static("application/json"));
            h
        })
        .json(&json!({
            "action": "next",
            "fork_from_shared_post": false,
            "parent_message_id": Uuid::new_v4().to_string(),
            "model": "auto",
            "client_prepare_state": "success",
            "timezone_offset_min": 0,
            "timezone": "UTC",
            "conversation_mode": { "kind": "primary_assistant" },
            "system_hints": ["picture_v2"],
            "partial_query": {
                "id": Uuid::new_v4().to_string(),
                "author": { "role": "user" },
                "content": { "content_type": "text", "parts": [text] },
            },
            "supports_buffering": true,
            "supported_encodings": ["v1"],
            "client_contextual_info": { "app_name": "chatgpt.com" },
        }))
        .timeout(Duration::from_secs(60))
        .send()
        .await?;
    if !prepare.status().is_success() {
        return Err(ChatGptWebError::Http {
            status: prepare.status().as_u16(),
            message: format!(
                "ChatGPT prepare failed (HTTP {})",
                prepare.status().as_u16()
            ),
        });
    }
    let prepare_json: Value = prepare.json().await.unwrap_or(Value::Null);
    let conduit_token = prepare_json
        .get("conduit_token")
        .and_then(Value::as_str)
        .unwrap_or("");

    let conversation = client
        .post(format!("{base_url}/backend-api/f/conversation"))
        .headers({
            let mut h = headers.clone();
            h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            h.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
            if let Ok(v) = HeaderValue::from_str(conduit_token) {
                h.insert("X-Conduit-Token", v);
            }
            if let Ok(v) = HeaderValue::from_str(&Uuid::new_v4().to_string()) {
                h.insert("X-Oai-Turn-Trace-Id", v);
            }
            h
        })
        .json(&json!({
            "action": "next",
            "messages": [{
                "id": Uuid::new_v4().to_string(),
                "author": { "role": "user" },
                "create_time": chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
                "content": { "content_type": "text", "parts": [text] },
                "metadata": {
                    "developer_mode_connector_ids": [],
                    "selected_github_repos": [],
                    "selected_all_github_repos": false,
                    "system_hints": ["picture_v2"],
                    "serialization_metadata": { "custom_symbol_offsets": [] },
                },
            }],
            "parent_message_id": Uuid::new_v4().to_string(),
            "model": "auto",
            "client_prepare_state": "sent",
            "timezone_offset_min": 0,
            "timezone": "UTC",
            "conversation_mode": { "kind": "primary_assistant" },
            "enable_message_followups": true,
            "system_hints": ["picture_v2"],
            "supports_buffering": true,
            "supported_encodings": ["v1"],
            "client_contextual_info": {
                "is_dark_mode": false,
                "time_since_loaded": 1200,
                "page_height": 1072,
                "page_width": 1724,
                "pixel_ratio": 1.2,
                "screen_height": 1440,
                "screen_width": 2560,
                "app_name": "chatgpt.com",
            },
            "paragen_cot_summary_display_override": "allow",
            "force_parallel_switch": "auto",
        }))
        .timeout(IMG_SSE_DEADLINE + Duration::from_secs(15))
        .send()
        .await?;
    if !conversation.status().is_success() {
        return Err(ChatGptWebError::Http {
            status: conversation.status().as_u16(),
            message: format!(
                "ChatGPT conversation failed (HTTP {})",
                conversation.status().as_u16()
            ),
        });
    }
    let bytes = conversation.bytes().await.unwrap_or_default();
    let capped = String::from_utf8_lossy(&bytes[..bytes.len().min(IMG_MAX_SSE_BYTES)]).to_string();

    let conversation_id = extract_conversation_id(&capped).unwrap_or_default();
    let mut file_ids = Vec::new();
    let mut sediment_ids = Vec::new();
    collect_media_ids(&capped, &mut file_ids, &mut sediment_ids);
    Ok(ImageConversationIds {
        conversation_id,
        file_ids,
        sediment_ids,
    })
}

/// Poll the conversation document until an image_gen record shows up.
async fn poll_conversation_images(
    client: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    cookie: &str,
    user_agent: &str,
    sentinel: &[(String, String)],
    conversation_id: &str,
    initial: &ImageConversationIds,
) -> Result<ImageConversationIds, ChatGptWebError> {
    let started = Instant::now();
    let mut file_ids = initial.file_ids.clone();
    let mut sediment_ids = initial.sediment_ids.clone();
    let headers = auth_headers(access_token, user_agent, cookie, sentinel);

    tokio::time::sleep(IMG_POLL_INITIAL_WAIT).await;
    while started.elapsed() <= IMG_POLL_TIMEOUT {
        let resp = client
            .get(format!(
                "{base_url}/backend-api/conversation/{conversation_id}"
            ))
            .headers({
                let mut h = headers.clone();
                h.insert(ACCEPT, HeaderValue::from_static("application/json"));
                h
            })
            .timeout(Duration::from_secs(30))
            .send()
            .await;
        if let Ok(resp) = resp {
            if resp.status().is_success() {
                let declared = resp
                    .headers()
                    .get(reqwest::header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                if declared > IMG_MAX_CONVERSATION_BYTES {
                    return Err(ChatGptWebError::Http {
                        status: 502,
                        message: "ChatGPT conversation document is unexpectedly large".into(),
                    });
                }
                let text = resp.text().await.unwrap_or_default();
                if text.len() > IMG_MAX_CONVERSATION_BYTES {
                    return Err(ChatGptWebError::Http {
                        status: 502,
                        message: "ChatGPT conversation document is unexpectedly large".into(),
                    });
                }
                collect_media_ids(&text, &mut file_ids, &mut sediment_ids);
                file_ids.retain(|id| id != "file_upload");
                if !file_ids.is_empty() || !sediment_ids.is_empty() {
                    return Ok(ImageConversationIds {
                        conversation_id: conversation_id.to_string(),
                        file_ids,
                        sediment_ids,
                    });
                }
                let is_error =
                    text.contains("\"is_error\":true") || text.contains("\"is_error\": true");
                if is_error && content_policy_hit(&text) {
                    return Err(ChatGptWebError::Http {
                        status: 400,
                        message: "ChatGPT refused the image request (content policy)".into(),
                    });
                }
                if text.contains("ChatGPTAgentToolRateLimitException")
                    || text.contains("Free plan limit for image generation")
                {
                    return Err(ChatGptWebError::Http {
                        status: 429,
                        message: "ChatGPT account has hit its image generation quota — add another account or wait for the reset".into(),
                    });
                }
            }
        }
        tokio::time::sleep(IMG_POLL_INTERVAL).await;
    }
    Err(ChatGptWebError::Http {
        status: 504,
        message: "ChatGPT image generation timed out".into(),
    })
}

async fn download_media_url(client: &reqwest::Client, url: &str) -> Option<(String, String)> {
    let resp = client
        .get(url)
        .timeout(IMG_DOWNLOAD_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|ct| ct.split(';').next())
        .unwrap_or("image/png")
        .to_string();
    let bytes = resp.bytes().await.ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some((STANDARD.encode(&bytes), mime))
}

/// Resolve one generated id into `{b64, mime}` via the files/sediment
/// download endpoints (`downloadGeneratedImage`).
async fn download_generated_image(
    client: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    cookie: &str,
    user_agent: &str,
    sentinel: &[(String, String)],
    conversation_id: &str,
    ids: &ImageConversationIds,
) -> Result<(String, String), ChatGptWebError> {
    let json_headers = auth_headers(access_token, user_agent, cookie, sentinel);

    for file_id in &ids.file_ids {
        let resp = client
            .get(format!("{base_url}/backend-api/files/{file_id}/download"))
            .headers({
                let mut h = json_headers.clone();
                h.insert(ACCEPT, HeaderValue::from_static("application/json"));
                h
            })
            .timeout(Duration::from_secs(60))
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        if !resp.status().is_success() {
            continue;
        }
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let url = body
            .get("download_url")
            .or_else(|| body.get("url"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if url.is_empty() {
            continue;
        }
        if let Some(media) = download_media_url(client, url).await {
            return Ok(media);
        }
    }

    for sediment_id in &ids.sediment_ids {
        let resp = client
            .get(format!(
                "{base_url}/backend-api/conversation/{conversation_id}/attachment/{sediment_id}/download"
            ))
            .headers({
                let mut h = json_headers.clone();
                h.insert(ACCEPT, HeaderValue::from_static("application/json"));
                h
            })
            .timeout(Duration::from_secs(60))
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        if !resp.status().is_success() {
            continue;
        }
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let url = body
            .get("download_url")
            .or_else(|| body.get("url"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if url.is_empty() {
            continue;
        }
        if let Some(media) = download_media_url(client, url).await {
            return Ok(media);
        }
    }

    Err(ChatGptWebError::Http {
        status: 502,
        message: "ChatGPT image generation produced no downloadable file".into(),
    })
}

/// One generated image asset.
#[derive(Debug)]
pub struct ChatGptWebMediaAsset {
    pub b64: String,
    pub mime: String,
}

impl ChatGptWebExecutor {
    /// `generateMedia` (executors/chatgpt-web.js:184-201) — image only.
    /// Browser fallback on Turnstile is deferred (bead openproxy-4d5h.8), so
    /// enforcement surfaces the same upstream-style error as the chat path.
    pub async fn generate_image(
        &self,
        prompt: &str,
        size: Option<&str>,
        n: usize,
        credentials: &ProviderConnection,
        proxy: Option<&ProxyTarget>,
    ) -> Result<Vec<ChatGptWebMediaAsset>, ChatGptWebError> {
        if prompt.trim().is_empty() {
            return Err(ChatGptWebError::Http {
                status: 400,
                message: "prompt is required for image generation".into(),
            });
        }
        let storage = read_storage_state(credentials)?;
        let user_agent = credentials
            .provider_specific_data
            .get("customUserAgent")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
        let client = self
            .pool
            .get("chatgpt-web", proxy)
            .map_err(ChatGptWebError::Request)?;
        let access_token = fetch_access_token(&client, &storage.cookie_header, &user_agent).await?;
        let sentinel =
            fetch_sentinel_headers(&client, CHATGPT_WEB_URL, &access_token, &user_agent).await?;

        let mut results = Vec::with_capacity(n.max(1));
        for _ in 0..n.max(1) {
            let initial = run_image_conversation(
                &client,
                CHATGPT_WEB_URL,
                &access_token,
                &storage.cookie_header,
                &user_agent,
                &sentinel,
                prompt,
                size,
            )
            .await?;
            let ids = if !initial.file_ids.is_empty() || !initial.sediment_ids.is_empty() {
                initial
            } else if !initial.conversation_id.is_empty() {
                poll_conversation_images(
                    &client,
                    CHATGPT_WEB_URL,
                    &access_token,
                    &storage.cookie_header,
                    &user_agent,
                    &sentinel,
                    &initial.conversation_id,
                    &initial,
                )
                .await?
            } else {
                return Err(ChatGptWebError::Http {
                    status: 502,
                    message: "ChatGPT image conversation produced no conversation id".into(),
                });
            };
            let (b64, mime) = download_generated_image(
                &client,
                CHATGPT_WEB_URL,
                &access_token,
                &storage.cookie_header,
                &user_agent,
                &sentinel,
                &ids.conversation_id,
                &ids,
            )
            .await?;
            results.push(ChatGptWebMediaAsset { b64, mime });
        }
        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Response shaping (adapter.js buildChatGptWebOpenAiResponse +
// chatgptWebTools.js buildToolModeResponse)
// ---------------------------------------------------------------------------

fn sse_response(chunks: &[String]) -> UpstreamResponse {
    let mut body = String::new();
    for c in chunks {
        body.push_str(&format!(
            "data: {}\n\n",
            serde_json::to_string(c).unwrap_or_else(|_| "{}".into())
        ));
    }
    body.push_str("data: [DONE]\n\n");
    let mut http_resp = http::Response::new(ReqwestBody::from(body));
    *http_resp.status_mut() = reqwest::StatusCode::OK;
    let headers = http_resp.headers_mut();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(
        reqwest::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    headers.insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    UpstreamResponse::Reqwest(reqwest::Response::from(http_resp))
}

fn json_response(status: u16, body: Value) -> UpstreamResponse {
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
    *http_resp.status_mut() =
        reqwest::StatusCode::from_u16(status).unwrap_or(reqwest::StatusCode::BAD_GATEWAY);
    http_resp.headers_mut().insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    UpstreamResponse::Reqwest(reqwest::Response::from(http_resp))
}

fn error_response(status: u16, message: &str) -> UpstreamResponse {
    json_response(status, json!({ "error": { "message": message } }))
}

/// `statusForAdapterError` (chatgpt-web.js): map error text to an HTTP status.
fn status_for_error(message: &str) -> u16 {
    let m = message.to_ascii_lowercase();
    if m.contains("storage state") || m.contains("credentials") || m.contains("connection id") {
        return 401;
    }
    if m.contains("http_429")
        || m.contains("status 429")
        || m.contains("status: 429")
        || m.contains("rate-limit")
        || m.contains("rate limit")
        || m.contains("rate_limit")
        || m.contains("quota exhausted")
        || m.contains("quota reached")
        || m.contains("quota exceeded")
        || m.contains("usage limit")
    {
        return 429;
    }
    if m.contains("request")
        || m.contains("messages")
        || m.contains("prompt")
        || m.contains("model")
        || m.contains("tools")
        || m.contains("text content")
        || m.contains("reasoning effort")
    {
        return 400;
    }
    502
}

/// Build the OpenAI response for a finished turn (no tools): buffered JSON
/// completion or a 3-chunk SSE replay.
fn build_openai_response(model: &str, text: &str, stream: bool) -> UpstreamResponse {
    let id = format!("chatcmpl-{}", Uuid::new_v4());
    let created = chrono::Utc::now().timestamp();
    if !stream {
        return json_response(
            200,
            json!({
                "id": id,
                "object": "chat.completion",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": text },
                    "finish_reason": "stop",
                }],
            }),
        );
    }
    let chunk = |delta: Value, finish: Value| {
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        })
        .to_string()
    };
    sse_response(&[
        chunk(json!({"role": "assistant"}), Value::Null),
        chunk(json!({"content": text}), Value::Null),
        chunk(json!({}), json!("stop")),
    ])
}

/// `buildToolAwareResult` + `buildToolModeResponse`: parse `<tool>` blocks
/// from the finished text into OpenAI `tool_calls`, then emit the JSON
/// completion (non-stream) or an SSE replay (stream).
fn build_tool_mode_response(
    model: &str,
    text: &str,
    requested_tools: &Value,
    stream: bool,
) -> UpstreamResponse {
    let nonce = get_tool_nonce(requested_tools);
    let (content, calls) = parse_tool_calls_from_text(
        text,
        &format!("call-{}", chrono::Utc::now().timestamp_millis()),
        requested_tools,
        &nonce,
    );
    let id = format!("chatcmpl-{}", Uuid::new_v4());
    let created = chrono::Utc::now().timestamp();

    let (message, finish_reason) = match calls {
        Some(calls) if !calls.is_empty() => {
            let tool_calls: Vec<Value> = calls.iter().map(|c| c.to_json()).collect();
            (
                json!({ "role": "assistant", "content": Value::Null, "tool_calls": tool_calls }),
                "tool_calls",
            )
        }
        _ => (json!({ "role": "assistant", "content": content }), "stop"),
    };

    if !stream {
        return json_response(
            200,
            json!({
                "id": id,
                "object": "chat.completion",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": message,
                    "finish_reason": finish_reason,
                }],
            }),
        );
    }

    // toolCompletionToSseStream: role chunk → one delta per tool call (with
    // index) → terminal chunk → [DONE].
    let mut chunks = vec![json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "system_fingerprint": Value::Null,
        "choices": [{ "index": 0, "delta": { "role": "assistant" }, "finish_reason": Value::Null, "logprobs": Value::Null }],
    })
    .to_string()];
    if let Some(tc) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in tc.iter().enumerate() {
            let mut entry = call.clone();
            entry["index"] = json!(index);
            chunks.push(json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "system_fingerprint": Value::Null,
                "choices": [{ "index": 0, "delta": { "tool_calls": [entry] }, "finish_reason": Value::Null, "logprobs": Value::Null }],
            })
            .to_string());
        }
        chunks.push(
            json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "system_fingerprint": Value::Null,
                "choices": [{ "index": 0, "delta": {}, "finish_reason": finish_reason, "logprobs": Value::Null }],
            })
            .to_string(),
        );
    } else {
        chunks.push(json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "system_fingerprint": Value::Null,
            "choices": [{ "index": 0, "delta": { "content": message.get("content").cloned().unwrap_or(Value::Null) }, "finish_reason": finish_reason, "logprobs": Value::Null }],
        })
        .to_string());
    }
    sse_response(&chunks)
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ChatGptWebError {
    MissingCredentials(String),
    InvalidCredentials(String),
    Http { status: u16, message: String },
    Upstream(String),
    Turnstile,
    Serialize(serde_json::Error),
    Request(reqwest::Error),
}

impl From<reqwest::Error> for ChatGptWebError {
    fn from(e: reqwest::Error) -> Self {
        Self::Request(e)
    }
}
impl From<serde_json::Error> for ChatGptWebError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialize(e)
    }
}
impl std::fmt::Display for ChatGptWebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {p}"),
            Self::InvalidCredentials(m) => write!(f, "Invalid credentials: {m}"),
            Self::Http { status, message } => write!(f, "HTTP {status}: {message}"),
            Self::Upstream(m) => write!(f, "{m}"),
            Self::Turnstile => write!(f, "ChatGPT requires the Turnstile challenge"),
            Self::Serialize(e) => write!(f, "Serialization error: {e}"),
            Self::Request(e) => write!(f, "Request error: {e}"),
        }
    }
}
impl std::error::Error for ChatGptWebError {}

pub struct ChatGptWebExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct ChatGptWebExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

pub struct ChatGptWebExecutor {
    pool: Arc<ClientPool>,
}

impl ChatGptWebExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    /// HTTP-first path (`#executeHttp` in chatgpt-web.js). Returns an
    /// `UpstreamResponse` — error responses are already shaped like upstream's
    /// `errorResponse`.
    pub async fn execute_request(
        &self,
        request: ChatGptWebExecutionRequest,
    ) -> Result<ChatGptWebExecutorResponse, ChatGptWebError> {
        let transformed_body = request.body.clone();
        let empty_headers = HeaderMap::new();

        let finish = |response: UpstreamResponse| ChatGptWebExecutorResponse {
            response,
            url: CHATGPT_WEB_URL.to_string(),
            headers: empty_headers.clone(),
            transformed_body: transformed_body.clone(),
            transport: TransportKind::Reqwest,
        };

        // Storage state → cookie jar. Parse failures surface as upstream's
        // 401 credentials error (browser fallback is deferred).
        let storage = match read_storage_state(&request.credentials) {
            Ok(s) => s,
            Err(e) => {
                let status = match &e {
                    ChatGptWebError::MissingCredentials(_) => 401,
                    _ => status_for_error(&e.to_string()),
                };
                return Ok(finish(error_response(status, &e.to_string())));
            }
        };

        let user_agent = request
            .credentials
            .provider_specific_data
            .get("customUserAgent")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());

        let client = match self.pool.get("chatgpt-web", request.proxy.as_ref()) {
            Ok(c) => c,
            Err(e) => {
                return Ok(finish(error_response(
                    502,
                    &format!("ChatGPT Web client init failed: {e}"),
                )));
            }
        };

        let run = async {
            let access_token =
                fetch_access_token(&client, &storage.cookie_header, &user_agent).await?;
            run_chat_turn(
                &client,
                &access_token,
                &storage.cookie_header,
                &user_agent,
                &request.body,
            )
            .await
        };

        match run.await {
            Ok(turn) => {
                let requested_tools = request.body.get("tools").cloned().unwrap_or(Value::Null);
                let has_tools = turn.has_tools
                    && requested_tools
                        .as_array()
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                let response = if has_tools {
                    build_tool_mode_response(
                        &request.model,
                        &turn.text,
                        &requested_tools,
                        request.stream,
                    )
                } else {
                    build_openai_response(&request.model, &turn.text, request.stream)
                };
                Ok(finish(response))
            }
            Err(ChatGptWebError::Turnstile) => Ok(finish(error_response(
                503,
                "ChatGPT requires the Turnstile challenge — the browser fallback is not available in this build (deferred, see bead openproxy-4d5h.8). Re-try later or use a different provider.",
            ))),
            Err(e) => {
                let status = match &e {
                    ChatGptWebError::Http { status, .. } => *status,
                    _ => status_for_error(&e.to_string()),
                };
                Ok(finish(error_response(status, &e.to_string())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_hash_matches_fnv1a_shape() {
        // FNV-1a of "" seeds then avalanches — just assert stable 8-hex output.
        let h = sentinel_hash("test");
        assert_eq!(h.len(), 8);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(sentinel_hash("test"), h);
    }

    #[test]
    fn prepare_token_has_prefix_and_payload() {
        let token = build_prepare_token(DEFAULT_USER_AGENT);
        assert!(token.starts_with("gAAAAAC"));
        let b64 = &token[7..];
        let decoded = STANDARD.decode(b64).expect("base64 payload");
        let slots: Value = serde_json::from_slice(&decoded).expect("json slots");
        let arr = slots.as_array().expect("array");
        assert_eq!(arr[3], json!(1)); // prepare marker
        assert_eq!(arr[4], json!(DEFAULT_USER_AGENT));
    }

    #[test]
    fn storage_state_from_bare_token() {
        let s = storage_state_from_cookie_header("eyJhbGciOiJ9.abc.def").unwrap();
        assert_eq!(
            s.cookie_header,
            "__Secure-next-auth.session-token=eyJhbGciOiJ9.abc.def"
        );
    }

    #[test]
    fn storage_state_from_cookie_blob() {
        let s = storage_state_from_cookie_header("__Secure-next-auth.session-token=tok; other=1")
            .unwrap();
        assert!(s
            .cookie_header
            .contains("__Secure-next-auth.session-token=tok"));
        assert!(s.cookie_header.contains("other=1"));
    }

    #[test]
    fn storage_state_rejects_blob_without_session() {
        assert!(storage_state_from_cookie_header("foo=1; bar=2").is_err());
    }

    #[test]
    fn extract_conversation_id_finds_uuid() {
        let sse = r#"data: {"conversation_id":"123e4567-e89b-42d3-a456-426614174000","x":1}"#;
        assert_eq!(
            extract_conversation_id(sse).as_deref(),
            Some("123e4567-e89b-42d3-a456-426614174000")
        );
        assert!(extract_conversation_id("no id here").is_none());
    }

    #[test]
    fn finished_assistant_text_requires_complete() {
        let incomplete = json!({
            "mapping": {
                "a": {"message": {"author": {"role": "assistant"},
                    "content": {"content_type": "text", "parts": ["hi"]},
                    "metadata": {"is_complete": false}}}
            }
        });
        assert!(finished_assistant_text(&incomplete).is_none());
        let complete = json!({
            "mapping": {
                "a": {"message": {"author": {"role": "assistant"},
                    "content": {"content_type": "text", "parts": ["hi", " there"]},
                    "metadata": {"is_complete": true}}}
            }
        });
        let (text, err) = finished_assistant_text(&complete).unwrap();
        assert_eq!(text, "hi there");
        assert!(err.is_empty());
    }

    #[test]
    fn flatten_messages_orders_system_first() {
        let msgs = vec![
            json!({"role": "user", "content": "u1"}),
            json!({"role": "system", "content": "sys"}),
            json!({"role": "assistant", "content": "a1"}),
        ];
        let flat = flatten_messages(&msgs);
        assert_eq!(flat, "sys\n\nu1\n\nAssistant: a1");
    }
}
