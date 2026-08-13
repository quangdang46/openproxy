use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE};
use reqwest::Body as ReqwestBody;
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

pub struct GrokWebExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug)]
pub enum GrokWebExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidUri(InvalidUri),
    InvalidRequest(hyper::http::Error),
    CookieParse(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for GrokWebExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<InvalidUri> for GrokWebExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for GrokWebExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for GrokWebExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for GrokWebExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for GrokWebExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl std::fmt::Display for GrokWebExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::InvalidCredentials(msg) => write!(f, "Invalid credentials: {}", msg),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
            Self::CookieParse(msg) => write!(f, "Cookie parse error: {}", msg),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::UnsupportedFormat(msg) => write!(f, "Unsupported format: {}", msg),
        }
    }
}

impl std::error::Error for GrokWebExecutorError {}

pub struct GrokWebExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for GrokWebExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrokWebExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

pub struct GrokWebExecutor {
    pool: Arc<ClientPool>,
}

impl GrokWebExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    pub async fn execute_request(
        &self,
        request: GrokWebExecutionRequest,
    ) -> Result<GrokWebExecutorResponse, GrokWebExecutorError> {
        let url = self.build_url();
        let headers = self.build_headers(&request.credentials)?;
        let transformed_body = self.transform_request(&request.body);

        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get("grok-web", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        Ok(GrokWebExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    fn build_url(&self) -> String {
        "https://grok.com/app-chat/conversations/new".to_string()
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, GrokWebExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        if let Some(sso) = credentials.access_token.as_ref() {
            let cookie_value = HeaderValue::from_str(&format!("sso={}", sso))
                .map_err(|_| GrokWebExecutorError::CookieParse("Invalid cookie".to_string()))?;
            headers.insert(COOKIE, cookie_value);
        }

        Ok(headers)
    }

    fn transform_request(&self, body: &Value) -> Value {
        body.clone()
    }
}

pub struct PerplexityWebExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug)]
pub enum PerplexityWebExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidUri(InvalidUri),
    InvalidRequest(hyper::http::Error),
    CookieParse(String),
    Serialize(serde_json::Error),
    SessionCache(String),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for PerplexityWebExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<InvalidUri> for PerplexityWebExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for PerplexityWebExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for PerplexityWebExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for PerplexityWebExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for PerplexityWebExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl std::fmt::Display for PerplexityWebExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::InvalidCredentials(msg) => write!(f, "Invalid credentials: {}", msg),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
            Self::CookieParse(msg) => write!(f, "Cookie parse error: {}", msg),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::SessionCache(msg) => write!(f, "Session cache error: {}", msg),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::UnsupportedFormat(msg) => write!(f, "Unsupported format: {}", msg),
        }
    }
}

impl std::error::Error for PerplexityWebExecutorError {}

pub struct PerplexityWebExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for PerplexityWebExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerplexityWebExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

/// `open-sse/executors/perplexity-web.js` port. The executor performs the
/// full JS pipeline: MODEL_MAP/THINKING_MAP model resolution, message
/// parsing, query building (JSON + 96000 trailing truncation), pplxBody
/// construction (API v2.18), session cache keyed by FNV-1a (UTF-16 code
/// units, JS `charCodeAt` parity), upstream SSE consumption (plan/markdown
/// blocks), and OpenAI chat.completion SSE/JSON response reconstruction.
const PPLX_SSE_ENDPOINT: &str = "https://www.perplexity.ai/rest/sse/perplexity_ask";
const PPLX_API_VERSION: &str = "2.18";
const PPLX_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const SESSION_MAX_AGE_MS: u64 = 3600_000;
const SESSION_MAX_ENTRIES: usize = 200;
const QUERY_MAX_LEN: usize = 96000;

/// 9router MODEL_MAP (perplexity-web.js:8-16): [mode, model_preference].
const MODEL_MAP: [(&str, (&str, &str)); 7] = [
    ("pplx-auto", ("concise", "pplx_pro")),
    ("pplx-sonar", ("copilot", "experimental")),
    ("pplx-gpt", ("copilot", "gpt54")),
    ("pplx-gemini", ("copilot", "gemini31pro_high")),
    ("pplx-sonnet", ("copilot", "claude46sonnet")),
    ("pplx-opus", ("copilot", "claude46opus")),
    ("pplx-nemotron", ("copilot", "nv_nemotron_3_super")),
];

/// 9router THINKING_MAP (perplexity-web.js:18-22) — model_preference used
/// when the request enables thinking. Only these models emit
/// `reasoning_content`; auto/sonar models must NOT.
const THINKING_MAP: [(&str, &str); 3] = [
    ("pplx-gpt", "gpt54_thinking"),
    ("pplx-sonnet", "claude46sonnetthinking"),
    ("pplx-opus", "claude46opusthinking"),
];

/// One session-cache entry: upstream `backend_uuid` + insert timestamp.
struct SessionEntry {
    backend_uuid: String,
    ts_ms: u64,
}

/// In-process session cache (9router sessionCache): keyed by the FNV-1a of
/// the full conversation (`role:content` lines joined by `\n`), TTL 1h,
/// LRU-evicted past 200 entries (oldest ts wins).
static SESSION_CACHE: Mutex<Option<HashMap<String, SessionEntry>>> = Mutex::new(None);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// JS-compatible FNV-1a session key (perplexity-web.js:38-46). The JS hash
/// iterates UTF-16 code units via `charCodeAt` over `role:content` lines
/// joined by `\n` (including the `:` separator and the `\n` join), so Rust
/// must hash each char's u16 value — hashing UTF-8 bytes would break wire
/// parity.
fn session_key(history: &[(String, String)]) -> String {
    let mut hash: u32 = 0x811c9dc5;
    for (i, (role, content)) in history.iter().enumerate() {
        if i > 0 {
            hash ^= '\n' as u32;
            hash = hash.wrapping_mul(0x01000193);
        }
        for unit in role
            .encode_utf16()
            .chain(":".encode_utf16())
            .chain(content.encode_utf16())
        {
            hash ^= unit as u32;
            hash = hash.wrapping_mul(0x01000193);
        }
    }
    format!("{:08x}", hash)
}

fn session_lookup(history: &[(String, String)]) -> Option<String> {
    if history.is_empty() {
        return None;
    }
    let key = session_key(history);
    let mut cache = SESSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = cache.get_or_insert_with(HashMap::new);
    let entry = map.get(&key)?;
    if now_ms().saturating_sub(entry.ts_ms) > SESSION_MAX_AGE_MS {
        map.remove(&key);
        return None;
    }
    Some(entry.backend_uuid.clone())
}

fn session_store(
    history: &[(String, String)],
    current_msg: &str,
    response_text: &str,
    backend_uuid: Option<&str>,
) {
    let Some(backend_uuid) = backend_uuid else {
        return;
    };
    if backend_uuid.is_empty() {
        return;
    }
    let mut full = history.to_vec();
    full.push(("user".to_string(), current_msg.to_string()));
    full.push(("assistant".to_string(), response_text.to_string()));
    let key = session_key(&full);
    let mut cache = SESSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = cache.get_or_insert_with(HashMap::new);
    map.insert(
        key,
        SessionEntry {
            backend_uuid: backend_uuid.to_string(),
            ts_ms: now_ms(),
        },
    );
    if map.len() > SESSION_MAX_ENTRIES {
        let oldest_key = map
            .iter()
            .min_by_key(|(_, entry)| entry.ts_ms)
            .map(|(k, _)| k.clone());
        if let Some(oldest_key) = oldest_key {
            map.remove(&oldest_key);
        }
    }
}

/// 9router cleanResponse (perplexity-web.js:75-88). `strip` collapses
/// multi-space / `\n{3,}` and trims — streaming deltas call with
/// strip=false, the non-streaming answer with strip=true.
fn clean_response(text: &str, strip: bool) -> String {
    let mut t = text.to_string();
    // Strip <?xml...?> declarations (JS XML_DECL_RE /<\?xml[^?]*\?>/g).
    let mut without_xml_decl = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(start) = rest.find("<?xml") {
        without_xml_decl.push_str(&rest[..start]);
        let after = &rest[start + 5..];
        if let Some(end) = after.find("?>") {
            rest = &after[end + 2..];
        } else {
            rest = after;
            break;
        }
    }
    without_xml_decl.push_str(rest);
    let t = without_xml_decl;
    // Strip [n] citations (JS CITATION_RE /\[\d+\]/g) — "[12x]" is not a
    // citation: the "[" and digits must be preserved.
    let mut without_citations = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(start) = rest.find('[') {
        without_citations.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            without_citations.push('[');
            rest = after;
        } else {
            let after_digits = &after[digits.len()..];
            if after_digits.starts_with(']') {
                rest = &after_digits[1..];
            } else {
                without_citations.push('[');
                without_citations.push_str(&after[..digits.len()]);
                rest = after_digits;
            }
        }
    }
    without_citations.push_str(rest);
    let t = without_citations;
    // Strip <grok:...>...</grok:...> blocks (JS GROK_TAG_RE, s flag) and
    // self-closing <grok:.../> (JS GROK_SELF_RE).
    let mut without_grok = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(start) = rest.find("<grok:") {
        without_grok.push_str(&rest[..start]);
        let after = &rest[start..];
        let close = after.find("</grok:");
        if let Some(close) = close {
            let end = after[close..].find('>').map(|i| close + i + 1);
            if let Some(end) = end {
                rest = &after[end..];
                continue;
            }
            rest = after;
            break;
        }
        let gt = after.find('>');
        if let Some(gt) = gt {
            if after[..gt].ends_with('/') {
                rest = &after[gt + 1..];
            } else {
                rest = after;
                break;
            }
        } else {
            rest = after;
            break;
        }
    }
    without_grok.push_str(rest);
    let t = without_grok;
    // Strip </?response...> tags (JS RESPONSE_TAG_RE /<\/?response\b[^>]*>/gi
    // — single pass, left to right; `\b` guards against <responsex>).
    let mut without_response = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(start) = rest.find('<') {
        without_response.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let (tag_after, is_closing) = if after.starts_with('/') {
            (&after[1..], true)
        } else {
            (after, false)
        };
        let lower = tag_after.to_ascii_lowercase();
        let boundary_ok = tag_after
            .chars()
            .nth(8)
            .map(|c| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(false);
        if lower.starts_with("response") && boundary_ok {
            let gt = tag_after.find('>');
            if let Some(gt) = gt {
                rest = &tag_after[gt + 1..];
                continue;
            }
        }
        without_response.push('<');
        rest = after;
        let _ = is_closing;
    }
    without_response.push_str(rest);
    let mut t = without_response;
    if strip {
        // Collapse runs of 2+ spaces to one (JS MULTI_SPACE / {2,}/g).
        let mut collapsed = String::with_capacity(t.len());
        let mut prev_space = false;
        for c in t.chars() {
            if c == ' ' {
                if !prev_space {
                    collapsed.push(' ');
                }
                prev_space = true;
            } else {
                collapsed.push(c);
                prev_space = false;
            }
        }
        t = collapsed;
        // Collapse runs of 3+ newlines to two (JS MULTI_NL /\n{3,}/g).
        let mut collapsed_nl = String::with_capacity(t.len());
        let mut nl_run = 0usize;
        for c in t.chars() {
            if c == '\n' {
                nl_run += 1;
                if nl_run <= 2 {
                    collapsed_nl.push('\n');
                }
            } else {
                nl_run = 0;
                collapsed_nl.push(c);
            }
        }
        t = collapsed_nl;
        t = t.trim().to_string();
    }
    t
}

/// 9router parseOpenAIMessages (perplexity-web.js:136-156): returns
/// (system_msg, history, current_msg). The trailing user message becomes
/// current_msg; the rest of the conversation becomes history.
fn parse_openai_messages(messages: &Value) -> (String, Vec<(String, String)>, String) {
    let mut system_msg = String::new();
    let mut history: Vec<(String, String)> = Vec::new();
    let mut current_msg = String::new();
    if let Some(array) = messages.as_array() {
        for msg in array {
            let mut role = msg
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_string();
            if role == "developer" {
                role = "system".to_string();
            }
            let mut content = String::new();
            if let Some(s) = msg.get("content").and_then(Value::as_str) {
                content = s.to_string();
            } else if let Some(parts) = msg.get("content").and_then(Value::as_array) {
                content = parts
                    .iter()
                    .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                    .map(|c| {
                        c.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            if content.trim().is_empty() {
                continue;
            }
            if role == "system" {
                system_msg.push_str(&content);
                system_msg.push('\n');
            } else if role == "user" || role == "assistant" {
                history.push((role, content));
            }
        }
    }
    if let Some((role, content)) = history.last() {
        if role == "user" {
            current_msg = content.clone();
            history.pop();
        }
    }
    (system_msg, history, current_msg)
}

/// 9router formatToolsHint (perplexity-web.js:182-191).
fn format_tools_hint(tools: Option<&Value>) -> String {
    let Some(array) = tools.and_then(Value::as_array) else {
        return String::new();
    };
    if array.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = array
        .iter()
        .map(|tool| {
            let fn_obj = tool.get("function").unwrap_or(tool);
            let name = fn_obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unnamed")
                .to_string();
            let desc = fn_obj
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .split('\n')
                .next()
                .unwrap_or("")
                .chars()
                .take(200)
                .collect::<String>();
            format!("- {name}: {desc}")
        })
        .collect();
    format!(
        "Available tools (reference only, cannot invoke):\n{}",
        lines.join("\n")
    )
}

/// 9router buildQuery (perplexity-web.js:193-207). Follow-up turns (session
/// backend_uuid present) send the plain current message; first turns send
/// instructions + history + query JSON, truncated to the last 96000 chars.
fn build_query(
    system_msg: &str,
    history: &[(String, String)],
    current_msg: &str,
    follow_up_uuid: Option<&str>,
    tools: Option<&Value>,
) -> String {
    if follow_up_uuid.is_some() {
        return current_msg.to_string();
    }
    let mut instr: Vec<String> = Vec::new();
    if !system_msg.trim().is_empty() {
        instr.push(system_msg.trim().to_string());
    }
    let tools_hint = format_tools_hint(tools);
    if !tools_hint.is_empty() {
        instr.push(tools_hint);
    }
    instr.push(
        "You have built-in web search. Answer questions directly using search results.".to_string(),
    );
    let mut obj = serde_json::Map::new();
    obj.insert(
        "instructions".to_string(),
        Value::Array(instr.into_iter().map(Value::String).collect()),
    );
    if !history.is_empty() {
        obj.insert(
            "history".to_string(),
            Value::Array(
                history
                    .iter()
                    .map(|(role, content)| serde_json::json!({ "role": role, "content": content }))
                    .collect(),
            ),
        );
    }
    if !current_msg.is_empty() {
        obj.insert("query".to_string(), Value::String(current_msg.to_string()));
    } else if history.is_empty() {
        obj.insert("query".to_string(), Value::String(String::new()));
    }
    let json = serde_json::to_string(&Value::Object(obj)).unwrap_or_default();
    // JS slice(-96000) is in UTF-16 code units.
    let units: Vec<u16> = json.encode_utf16().collect();
    if units.len() > QUERY_MAX_LEN {
        String::from_utf16_lossy(&units[units.len() - QUERY_MAX_LEN..])
    } else {
        json
    }
}

/// 9router buildPplxRequestBody (perplexity-web.js:158-180).
fn build_pplx_request_body(
    query: &str,
    mode: &str,
    model_pref: &str,
    follow_up_uuid: Option<&str>,
) -> Value {
    serde_json::json!({
        "query_str": query,
        "params": {
            "query_str": query,
            "search_focus": "internet",
            "mode": mode,
            "model_preference": model_pref,
            "sources": ["web"],
            "attachments": [],
            "frontend_uuid": uuid::Uuid::new_v4().to_string(),
            "frontend_context_uuid": uuid::Uuid::new_v4().to_string(),
            "version": PPLX_API_VERSION,
            "language": "en-US",
            "timezone": "UTC",
            "search_recency_filter": Value::Null,
            "is_incognito": true,
            "use_schematized_api": true,
            "last_backend_uuid": follow_up_uuid,
        }
    })
}

/// 9router model resolution (perplexity-web.js:409-421): THINKING_MAP when
/// thinking is enabled for a mapped model, else MODEL_MAP, else the raw
/// model as preference.
fn resolve_pplx_model(model: &str, thinking: bool) -> (String, String) {
    if thinking {
        if let Some((_, pref)) = THINKING_MAP.iter().find(|(m, _)| *m == model) {
            return ("copilot".to_string(), pref.to_string());
        }
    }
    if let Some((_, (mode, pref))) = MODEL_MAP.iter().find(|(m, _)| *m == model) {
        return (mode.to_string(), pref.to_string());
    }
    ("copilot".to_string(), model.to_string())
}

/// 9router readPplxSseEvents (perplexity-web.js:90-134) as a Vec: the
/// `data:` payloads of each flushed SSE event (blank line), stopping on
/// `event: end_of_stream` or a `[DONE]` payload.
fn read_pplx_sse_events(body_bytes: &[u8]) -> Vec<Value> {
    let text = String::from_utf8_lossy(body_bytes);
    let mut events = Vec::new();
    let mut data_lines: Vec<String> = Vec::new();
    for line in text.split_inclusive('\n') {
        let raw = line.strip_suffix('\n').unwrap_or(line);
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.is_empty() {
            // Blank line → flush pending data lines.
            if !data_lines.is_empty() {
                let payload = data_lines.join("\n");
                data_lines.clear();
                let trimmed = payload.trim();
                if !trimmed.is_empty() {
                    if trimmed == "[DONE]" {
                        return events;
                    }
                    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                        events.push(parsed);
                    }
                }
            }
            continue;
        }
        if raw == "event: end_of_stream" {
            return events;
        }
        if let Some(data) = raw.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }
    // Tail flush for a trailing data line without a final blank line (the
    // JS decoder does the same after the reader loop).
    if !data_lines.is_empty() {
        let payload = data_lines.join("\n");
        let trimmed = payload.trim();
        if !trimmed.is_empty() && trimmed != "[DONE]" {
            if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                events.push(parsed);
            }
        }
    }
    events
}

/// 9router extractContent (perplexity-web.js:209-290). Returns
/// (delta, answer, thinking, backend_uuid, done, error) tuples.
fn extract_content(
    events: &[Value],
) -> Vec<(String, String, String, Option<String>, bool, Option<String>)> {
    let mut out = Vec::new();
    let mut full_answer = String::new();
    let mut backend_uuid: Option<String> = None;
    let mut seen_len = 0usize;
    let mut seen_thinking: Vec<String> = Vec::new();
    for event in events {
        if event.get("error_code").is_some() || event.get("error_message").is_some() {
            let message = event
                .get("error_message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "Perplexity error: {}",
                        event
                            .get("error_code")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                    )
                });
            out.push((
                String::new(),
                String::new(),
                String::new(),
                backend_uuid.clone(),
                true,
                Some(message),
            ));
            return out;
        }
        if let Some(uuid) = event.get("backend_uuid").and_then(Value::as_str) {
            backend_uuid = Some(uuid.to_string());
        }
        let blocks = event
            .get("blocks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for block in &blocks {
            let usage = block
                .get("intended_usage")
                .and_then(Value::as_str)
                .unwrap_or("");
            if usage == "pro_search_steps" {
                if let Some(plan_block) = block.get("plan_block") {
                    if let Some(steps) = plan_block.get("steps").and_then(Value::as_array) {
                        for step in steps {
                            let step_type =
                                step.get("step_type").and_then(Value::as_str).unwrap_or("");
                            if step_type == "SEARCH_WEB" {
                                if let Some(queries) = step
                                    .get("search_web_content")
                                    .and_then(|s| s.get("queries"))
                                    .and_then(Value::as_array)
                                {
                                    for q in queries {
                                        let qr =
                                            q.get("query").and_then(Value::as_str).unwrap_or("");
                                        if !qr.is_empty() && !seen_thinking.iter().any(|s| s == qr)
                                        {
                                            seen_thinking.push(qr.to_string());
                                            out.push((
                                                String::new(),
                                                String::new(),
                                                format!("Searching: {qr}"),
                                                backend_uuid.clone(),
                                                false,
                                                None,
                                            ));
                                        }
                                    }
                                }
                            } else if step_type == "READ_RESULTS" {
                                if let Some(urls) = step
                                    .get("read_results_content")
                                    .and_then(|s| s.get("urls"))
                                    .and_then(Value::as_array)
                                {
                                    for url in urls.iter().take(3) {
                                        let u = url.as_str().unwrap_or("");
                                        if !u.is_empty() && !seen_thinking.iter().any(|s| s == u) {
                                            seen_thinking.push(u.to_string());
                                            out.push((
                                                String::new(),
                                                String::new(),
                                                format!("Reading: {u}"),
                                                backend_uuid.clone(),
                                                false,
                                                None,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if usage == "plan" {
                if let Some(plan_block) = block.get("plan_block") {
                    if let Some(goals) = plan_block.get("goals").and_then(Value::as_array) {
                        for goal in goals {
                            let desc = goal
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            if !desc.is_empty() && !seen_thinking.iter().any(|s| s == desc) {
                                seen_thinking.push(desc.to_string());
                                out.push((
                                    String::new(),
                                    String::new(),
                                    desc.to_string(),
                                    backend_uuid.clone(),
                                    false,
                                    None,
                                ));
                            }
                        }
                    }
                }
            }
            if !usage.contains("markdown") {
                continue;
            }
            let Some(mb) = block.get("markdown_block") else {
                continue;
            };
            let Some(chunks) = mb.get("chunks").and_then(Value::as_array) else {
                continue;
            };
            if chunks.is_empty() {
                continue;
            }
            let chunk_text: String = chunks
                .iter()
                .map(|c| c.as_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("");
            if mb.get("progress").and_then(Value::as_str) == Some("DONE") {
                // JS sets fullAnswer = chunks.join("") — the DONE block
                // carries the full answer, not a delta.
                full_answer = chunk_text;
            } else {
                let cumulative = full_answer.clone() + &chunk_text;
                if cumulative.len() > seen_len {
                    let delta = cumulative[seen_len..].to_string();
                    full_answer = cumulative;
                    seen_len = full_answer.len();
                    out.push((
                        delta,
                        full_answer.clone(),
                        String::new(),
                        backend_uuid.clone(),
                        false,
                        None,
                    ));
                }
            }
        }
        if blocks.is_empty() {
            if let Some(text) = event.get("text").and_then(Value::as_str) {
                let t = text.trim();
                if t.len() > seen_len {
                    let delta = t[seen_len..].to_string();
                    full_answer = t.to_string();
                    seen_len = t.len();
                    out.push((
                        delta,
                        full_answer.clone(),
                        String::new(),
                        backend_uuid.clone(),
                        false,
                        None,
                    ));
                }
            }
        }
        if event.get("final").and_then(Value::as_bool).unwrap_or(false)
            || event.get("status").and_then(Value::as_str) == Some("COMPLETED")
        {
            break;
        }
    }
    out.push((
        String::new(),
        full_answer,
        String::new(),
        backend_uuid,
        true,
        None,
    ));
    out
}

/// OpenAI-compatible SSE chunk frame (9router sseChunk).
fn sse_chunk(
    cid: &str,
    created: i64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&serde_json::json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "system_fingerprint": Value::Null,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
                    .map(|s| Value::String(s.to_string()))
                    .unwrap_or(Value::Null),
                "logprobs": Value::Null,
            }],
        }))
        .unwrap_or_default()
    )
}

fn json_error(status: u16, message: &str, err_type: &str, code: Option<&str>) -> UpstreamResponse {
    let mut body = serde_json::json!({ "error": { "message": message, "type": err_type } });
    if let Some(code) = code {
        body["error"]["code"] = Value::String(code.to_string());
    }
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

pub struct PerplexityWebExecutor {
    pool: Arc<ClientPool>,
}

impl PerplexityWebExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    pub async fn execute_request(
        &self,
        request: PerplexityWebExecutionRequest,
    ) -> Result<PerplexityWebExecutorResponse, PerplexityWebExecutorError> {
        let url = self.build_url();
        let messages = request.body.get("messages").cloned();
        let has_messages = messages
            .as_ref()
            .and_then(Value::as_array)
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if !has_messages {
            return Ok(PerplexityWebExecutorResponse {
                response: json_error(
                    400,
                    "Missing or empty messages array",
                    "invalid_request",
                    None,
                ),
                url,
                headers: HeaderMap::new(),
                transformed_body: request.body.clone(),
                transport: TransportKind::Reqwest,
            });
        }

        // Thinking trigger (9router perplexity-web.js:407): body.thinking ===
        // true OR reasoning_effort present and != "none".
        let thinking = request
            .body
            .get("thinking")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || request
                .body
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .map(|e| e != "none")
                .unwrap_or(false);

        let model = &request.model;
        let (pplx_mode, model_pref) = resolve_pplx_model(model, thinking);

        let (system_msg, history, current_msg) = parse_openai_messages(&messages.unwrap());
        let follow_up_uuid = session_lookup(&history);
        let query = build_query(
            &system_msg,
            &history,
            &current_msg,
            follow_up_uuid.as_deref(),
            request.body.get("tools"),
        );
        if query.trim().is_empty() {
            return Ok(PerplexityWebExecutorResponse {
                response: json_error(400, "Empty query after processing", "invalid_request", None),
                url,
                headers: HeaderMap::new(),
                transformed_body: request.body.clone(),
                transport: TransportKind::Reqwest,
            });
        }

        let transformed_body =
            build_pplx_request_body(&query, &pplx_mode, &model_pref, follow_up_uuid.as_deref());
        let headers = self.build_headers(&request.credentials)?;
        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get("perplexity-web", request.proxy.as_ref())?;
        // 9router perplexity-web.js:458-467 — a connection failure becomes a
        // 502 JSON response, not an executor error.
        let upstream = match client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                return Ok(PerplexityWebExecutorResponse {
                    response: json_error(
                        502,
                        &format!("Perplexity connection failed: {err}"),
                        "upstream_error",
                        None,
                    ),
                    url,
                    headers,
                    transformed_body,
                    transport: TransportKind::Reqwest,
                });
            }
        };

        // 9router perplexity-web.js:469-479 — auth/rate-limit/HTTP errors
        // are surfaced as JSON responses with HTTP_<status> codes.
        let status = upstream.status();
        if !status.is_success() {
            let err_msg = if status.as_u16() == 401 || status.as_u16() == 403 {
                "Perplexity auth failed — session cookie may be expired. Re-paste your __Secure-next-auth.session-token.".to_string()
            } else if status.as_u16() == 429 {
                "Perplexity rate limited. Wait a moment and retry.".to_string()
            } else {
                format!("Perplexity returned HTTP {}", status.as_u16())
            };
            return Ok(PerplexityWebExecutorResponse {
                response: json_error(
                    status.as_u16(),
                    &err_msg,
                    "upstream_error",
                    Some(&format!("HTTP_{}", status.as_u16())),
                ),
                url,
                headers,
                transformed_body,
                transport: TransportKind::Reqwest,
            });
        }

        let body_bytes = upstream.bytes().await?;
        let events = read_pplx_sse_events(&body_bytes);
        let cid = format!("chatcmpl-pplx-{}", &uuid::Uuid::new_v4().to_string()[..12]);
        let created = (now_ms() / 1000) as i64;

        let response = if request.stream {
            // buildStreamingResponse (perplexity-web.js:296-357): OpenAI
            // chat.completion.chunk SSE with reasoning_content deltas.
            let mut sse = String::new();
            sse.push_str(&sse_chunk(
                &cid,
                created,
                model,
                serde_json::json!({ "role": "assistant" }),
                None,
            ));
            let mut full_answer = String::new();
            let mut resp_backend_uuid: Option<String> = None;
            for (delta, answer, thinking_text, backend_uuid, done, error) in
                extract_content(&events)
            {
                if let Some(uuid) = backend_uuid {
                    resp_backend_uuid = Some(uuid);
                }
                if let Some(error) = error {
                    sse.push_str(&sse_chunk(
                        &cid,
                        created,
                        model,
                        serde_json::json!({ "content": format!("[Error: {error}]") }),
                        None,
                    ));
                    break;
                }
                if !thinking_text.is_empty() {
                    sse.push_str(&sse_chunk(
                        &cid,
                        created,
                        model,
                        serde_json::json!({ "reasoning_content": format!("{thinking_text}\n") }),
                        None,
                    ));
                    continue;
                }
                if done {
                    if !answer.is_empty() {
                        full_answer = answer;
                    }
                    break;
                }
                if !delta.is_empty() {
                    let cleaned = clean_response(&delta, false);
                    if !cleaned.is_empty() {
                        sse.push_str(&sse_chunk(
                            &cid,
                            created,
                            model,
                            serde_json::json!({ "content": cleaned }),
                            None,
                        ));
                    }
                }
                if !answer.is_empty() {
                    full_answer = answer;
                }
            }
            sse.push_str(&sse_chunk(
                &cid,
                created,
                model,
                serde_json::json!({}),
                Some("stop"),
            ));
            sse.push_str("data: [DONE]\n\n");
            session_store(
                &history,
                &current_msg,
                &clean_response(&full_answer, true),
                resp_backend_uuid.as_deref(),
            );
            let mut http_resp = http::Response::new(ReqwestBody::from(sse));
            *http_resp.status_mut() = reqwest::StatusCode::OK;
            http_resp.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            http_resp.headers_mut().insert(
                reqwest::header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache"),
            );
            http_resp.headers_mut().insert(
                reqwest::header::HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            );
            UpstreamResponse::Reqwest(reqwest::Response::from(http_resp))
        } else {
            // buildNonStreamingResponse (perplexity-web.js:359-391).
            let mut full_answer = String::new();
            let mut resp_backend_uuid: Option<String> = None;
            let mut thinking_parts: Vec<String> = Vec::new();
            let mut err_response: Option<UpstreamResponse> = None;
            for (delta, answer, thinking_text, backend_uuid, done, error) in
                extract_content(&events)
            {
                if let Some(uuid) = backend_uuid {
                    resp_backend_uuid = Some(uuid);
                }
                if let Some(error) = error {
                    err_response = Some(json_error(
                        502,
                        &error,
                        "upstream_error",
                        Some("PPLX_ERROR"),
                    ));
                    break;
                }
                if !thinking_text.is_empty() {
                    thinking_parts.push(thinking_text);
                    continue;
                }
                if done {
                    if !answer.is_empty() {
                        full_answer = answer;
                    }
                    break;
                }
                if !answer.is_empty() {
                    full_answer = answer;
                }
            }
            if let Some(err_response) = err_response {
                err_response
            } else {
                full_answer = clean_response(&full_answer, true);
                session_store(
                    &history,
                    &current_msg,
                    &full_answer,
                    resp_backend_uuid.as_deref(),
                );
                let mut msg = serde_json::json!({ "role": "assistant", "content": full_answer });
                if !thinking_parts.is_empty() {
                    msg["reasoning_content"] = Value::String(thinking_parts.join("\n"));
                }
                let prompt_tokens = current_msg.encode_utf16().count().div_ceil(4);
                let completion_tokens = full_answer.encode_utf16().count().div_ceil(4);
                let body = serde_json::json!({
                    "id": cid,
                    "object": "chat.completion",
                    "created": created,
                    "model": model,
                    "system_fingerprint": Value::Null,
                    "choices": [{
                        "index": 0,
                        "message": msg,
                        "finish_reason": "stop",
                        "logprobs": Value::Null,
                    }],
                    "usage": {
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "total_tokens": prompt_tokens + completion_tokens,
                    }
                });
                let bytes = serde_json::to_vec(&body)?;
                let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
                *http_resp.status_mut() = reqwest::StatusCode::OK;
                http_resp.headers_mut().insert(
                    reqwest::header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                UpstreamResponse::Reqwest(reqwest::Response::from(http_resp))
            }
        };

        Ok(PerplexityWebExecutorResponse {
            response,
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    fn build_url(&self) -> String {
        PPLX_SSE_ENDPOINT.to_string()
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, PerplexityWebExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(
            reqwest::header::ORIGIN,
            HeaderValue::from_static("https://www.perplexity.ai"),
        );
        headers.insert(
            reqwest::header::REFERER,
            HeaderValue::from_static("https://www.perplexity.ai/"),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(PPLX_USER_AGENT),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-app-apiclient"),
            HeaderValue::from_static("default"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-app-apiversion"),
            HeaderValue::from_static(PPLX_API_VERSION),
        );

        // Auth split (9router perplexity-web.js:447-451): accessToken →
        // Bearer; else apiKey → session-token cookie.
        if let Some(token) = &credentials.access_token {
            let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                PerplexityWebExecutorError::CookieParse("Invalid Bearer token".to_string())
            })?;
            headers.insert(AUTHORIZATION, value);
        } else if let Some(api_key) = &credentials.api_key {
            let value =
                HeaderValue::from_str(&format!("__Secure-next-auth.session-token={api_key}"))
                    .map_err(|_| {
                        PerplexityWebExecutorError::CookieParse("Invalid cookie".to_string())
                    })?;
            headers.insert(COOKIE, value);
        }

        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pplx_session_key_fnv1a() {
        // Guard test: JS FNV-1a (offset 0x811c9dc5, prime 0x01000193, u32
        // wrap, `>>> 0`) over UTF-16 code units of "user:x" → 0x2419d7e0.
        let key = session_key(&[("user".to_string(), "x".to_string())]);
        assert_eq!(key, "2419d7e0");
    }

    #[test]
    fn test_pplx_clean_response_strips_tags() {
        // Guard test: citations, grok tags, xml decl, response tags.
        let input = "<?xml version=\"1.0\"?><response><grok:hidden>secret</grok:hidden>Answer [1] text <grok:br/> done</response>";
        let cleaned = clean_response(input, true);
        assert!(!cleaned.contains("[1]"));
        assert!(!cleaned.contains("grok"));
        assert!(!cleaned.contains("xml"));
        assert!(!cleaned.contains("response"));
        assert!(cleaned.contains("Answer"));
        assert!(cleaned.contains("text"));
        assert!(cleaned.contains("done"));
    }

    #[test]
    fn test_pplx_clean_response_collapses_whitespace_only_when_strip() {
        let stripped = clean_response("a  b\n\n\nc", true);
        assert_eq!(stripped, "a b\n\nc");
        // strip=false must not collapse (streaming deltas).
        let kept = clean_response("a  b\n\n\nc", false);
        assert_eq!(kept, "a  b\n\n\nc");
    }

    #[test]
    fn test_pplx_clean_response_boundary_guard() {
        // JS RESPONSE_TAG_RE has \b — <responsex> must survive.
        let kept = clean_response("<responsex>keep</responsex>", true);
        assert!(kept.contains("responsex"));
        assert!(kept.contains("keep"));
        // Self-closing grok tag removed.
        let kept2 = clean_response("a <grok:br/> b", false);
        assert_eq!(kept2, "a  b");
    }

    #[test]
    fn test_pplx_clean_response_non_citation_brackets_survive() {
        // JS CITATION_RE /\[\d+\]/g — "[12x]" is not a citation.
        let kept = clean_response("keep [12x] here", false);
        assert!(kept.contains("[12x]"));
        let stripped = clean_response("cite [12] end", false);
        assert_eq!(stripped, "cite  end");
    }

    #[test]
    fn test_pplx_model_map() {
        assert_eq!(
            resolve_pplx_model("pplx-auto", false),
            ("concise".to_string(), "pplx_pro".to_string())
        );
        assert_eq!(
            resolve_pplx_model("pplx-sonar", false),
            ("copilot".to_string(), "experimental".to_string())
        );
        assert_eq!(
            resolve_pplx_model("pplx-nemotron", false),
            ("copilot".to_string(), "nv_nemotron_3_super".to_string())
        );
    }

    #[test]
    fn test_pplx_thinking_map() {
        // THINKING_MAP only for gpt/sonnet/opus; auto/sonar must NOT map.
        assert_eq!(
            resolve_pplx_model("pplx-opus", true),
            ("copilot".to_string(), "claude46opusthinking".to_string())
        );
        assert_eq!(
            resolve_pplx_model("pplx-sonnet", true),
            ("copilot".to_string(), "claude46sonnetthinking".to_string())
        );
        assert_eq!(
            resolve_pplx_model("pplx-gpt", true),
            ("copilot".to_string(), "gpt54_thinking".to_string())
        );
        // No THINKING_MAP entry → fall back to MODEL_MAP.
        assert_eq!(
            resolve_pplx_model("pplx-auto", true),
            ("concise".to_string(), "pplx_pro".to_string())
        );
    }

    #[test]
    fn test_pplx_unmapped_model_raw_preference() {
        assert_eq!(
            resolve_pplx_model("some-custom-model", false),
            ("copilot".to_string(), "some-custom-model".to_string())
        );
    }

    #[test]
    fn test_pplx_parse_messages() {
        let messages = serde_json::json!([
            { "role": "system", "content": "Be helpful" },
            { "role": "user", "content": "Q1" },
            { "role": "assistant", "content": "A1" },
            { "role": "user", "content": "Q2" }
        ]);
        let (system_msg, history, current_msg) = parse_openai_messages(&messages);
        assert_eq!(system_msg.trim(), "Be helpful");
        assert_eq!(
            history,
            vec![
                ("user".to_string(), "Q1".to_string()),
                ("assistant".to_string(), "A1".to_string())
            ]
        );
        assert_eq!(current_msg, "Q2");
    }

    #[test]
    fn test_pplx_parse_messages_developer_role_and_multipart() {
        let messages = serde_json::json!([
            { "role": "developer", "content": "Be concise" },
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "part1" },
                    { "type": "text", "text": "part2" },
                    { "type": "image_url", "image_url": { "url": "x" } }
                ]
            }
        ]);
        let (system_msg, history, current_msg) = parse_openai_messages(&messages);
        assert_eq!(system_msg.trim(), "Be concise");
        assert_eq!(current_msg, "part1 part2");
    }

    #[test]
    fn test_pplx_build_query_followup_is_plain_message() {
        let q = build_query("", &[], "Follow up", Some("uuid-abc"), None);
        assert_eq!(q, "Follow up");
    }

    #[test]
    fn test_pplx_build_query_first_turn_json() {
        let history = vec![("user".to_string(), "earlier".to_string())];
        let q = build_query("Be helpful\n", &history, "now", None, None);
        let obj: serde_json::Value = serde_json::from_str(&q).unwrap();
        assert_eq!(obj["query"], "now");
        assert_eq!(obj["history"][0]["content"], "earlier");
        let instr = obj["instructions"].as_array().unwrap();
        assert!(instr
            .iter()
            .any(|s| s.as_str().unwrap().contains("Be helpful")));
        assert!(instr
            .iter()
            .any(|s| s.as_str().unwrap().contains("built-in web search")));
    }

    #[test]
    fn test_pplx_build_query_truncates_to_96000() {
        // JS slice(-96000) — the truncation keeps the tail of the JSON,
        // which ends with the query field: ..."query":"hi"}.
        let big = "x".repeat(100_000);
        let q = build_query(&big, &[], "hi", None, None);
        assert!(q.len() <= 96000);
        assert!(q.ends_with("\"hi\"}"));
        assert!(q.contains("\"query\":\"hi\""));
    }

    #[test]
    fn test_pplx_body_shape() {
        let body = build_pplx_request_body("hello world", "concise", "pplx_pro", Some("uuid-xyz"));
        assert_eq!(body["query_str"], "hello world");
        assert_eq!(body["params"]["query_str"], "hello world");
        assert_eq!(body["params"]["search_focus"], "internet");
        assert_eq!(body["params"]["mode"], "concise");
        assert_eq!(body["params"]["model_preference"], "pplx_pro");
        assert_eq!(body["params"]["sources"], serde_json::json!(["web"]));
        assert_eq!(body["params"]["use_schematized_api"], true);
        assert_eq!(body["params"]["is_incognito"], true);
        assert_eq!(body["params"]["last_backend_uuid"], "uuid-xyz");
        assert_eq!(body["params"]["version"], "2.18");
        assert_eq!(body["params"]["language"], "en-US");
    }

    #[test]
    fn test_perplexity_build_url() {
        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        assert_eq!(
            executor.build_url(),
            "https://www.perplexity.ai/rest/sse/perplexity_ask"
        );
    }

    #[test]
    fn test_grok_build_url() {
        let executor = GrokWebExecutor::new(Arc::new(ClientPool::default()));
        let url = executor.build_url();
        assert!(url.contains("grok.com"));
    }

    #[test]
    fn test_pplx_auth_bearer_from_access_token() {
        use std::collections::BTreeMap;

        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let credentials = ProviderConnection {
            id: "test".to_string(),
            provider: "perplexity".to_string(),
            auth_type: "oauth".to_string(),
            name: None,
            priority: None,
            is_active: Some(true),
            created_at: None,
            updated_at: None,
            display_name: None,
            email: None,
            global_priority: None,
            default_model: None,
            access_token: Some("tok-1".to_string()),
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: None,
            test_status: None,
            last_tested: None,
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: None,
            error_code: None,
            consecutive_use_count: None,
            backoff_level: None,
            consecutive_errors: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            runtime_transport: None,
            provider_specific_data: BTreeMap::new(),
            extra: BTreeMap::new(),
        };
        let headers = executor.build_headers(&credentials).unwrap();
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap(),
            "Bearer tok-1",
            "accessToken must map to Authorization: Bearer"
        );
        assert!(headers.get(COOKIE).is_none());
        assert_eq!(
            headers.get(reqwest::header::ORIGIN).unwrap(),
            "https://www.perplexity.ai"
        );
        assert_eq!(
            headers
                .get(reqwest::header::HeaderName::from_static("x-app-apiversion"))
                .unwrap(),
            "2.18"
        );
    }

    #[test]
    fn test_pplx_auth_cookie_from_api_key() {
        use std::collections::BTreeMap;

        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let credentials = ProviderConnection {
            id: "test".to_string(),
            provider: "perplexity".to_string(),
            auth_type: "cookie".to_string(),
            name: None,
            priority: None,
            is_active: Some(true),
            created_at: None,
            updated_at: None,
            display_name: None,
            email: None,
            global_priority: None,
            default_model: None,
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: Some("my-session-token".to_string()),
            test_status: None,
            last_tested: None,
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: None,
            error_code: None,
            consecutive_use_count: None,
            backoff_level: None,
            consecutive_errors: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            runtime_transport: None,
            provider_specific_data: BTreeMap::new(),
            extra: BTreeMap::new(),
        };
        let headers = executor.build_headers(&credentials).unwrap();
        assert_eq!(
            headers.get(COOKIE).unwrap(),
            "__Secure-next-auth.session-token=my-session-token"
        );
        assert!(headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn test_pplx_extract_content_markdown_and_thinking() {
        let events = serde_json::from_str::<serde_json::Value>(
            r#"[
                {"backend_uuid": "resp-uuid-1", "blocks": [
                    {"intended_usage": "plan", "plan_block": {"goals": [{"description": "Find the answer"}]}},
                    {"intended_usage": "markdown", "markdown_block": {"chunks": ["An"], "progress": null}},
                    {"intended_usage": "markdown", "markdown_block": {"chunks": ["swer"], "progress": "DONE"}}
                ], "status": "COMPLETED"}
            ]"#,
        )
        .unwrap();
        let chunks = extract_content(events.as_array().unwrap());
        // thinking goal first, then the "An" delta, then the DONE block
        // carries the full answer (JS: fullAnswer = chunks.join("")).
        assert_eq!(chunks[0].2, "Find the answer");
        assert_eq!(chunks[1].0, "An");
        assert_eq!(chunks[1].1, "An");
        let last = chunks.last().unwrap();
        assert!(last.4, "final tuple must be done");
        assert_eq!(last.1, "swer");
        assert_eq!(last.3.as_deref(), Some("resp-uuid-1"));
    }

    #[test]
    fn test_pplx_read_sse_events_blank_line_flush() {
        let raw = b"data: {\"a\": 1}\n\nevent: end_of_stream\n";
        let events = read_pplx_sse_events(raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["a"], 1);
    }
}
