//! Grok Web executor — `grok.com/rest` SSO-cookie chat API.
//!
//! Port of 9router `open-sse/executors/grok-web.js` (+ `registry/grok-web.js`):
//! OpenAI chat body → single Grok message string, browser-fingerprint headers
//! (statsig / traceparent / Sec-Ch-Ua) for Cloudflare, NDJSON response stream →
//! OpenAI SSE chunks, `reasoning_content` only for thinking models, and
//! estimated usage (`ceil(len/4)`).
//!
//! Distinct from:
//! - [`super::xai`] → `api.x.ai` (API key / OAuth)
//! - [`super::grok_cli`] → `cli-chat-proxy.grok.com` Responses API
//!
//! Cookie auth: the `sso` cookie is stored in `credentials.api_key` (web-cookie
//! provider convention, `authType: "cookie"`), with a leading `sso=` prefix
//! stripped if present.

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::Bytes;
use hyper::http;
use hyper::http::uri::InvalidUri;
use rand::RngCore;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, COOKIE};
use reqwest::Body;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

const GROK_CHAT_API: &str = "https://grok.com/rest/app-chat/conversations/new";
const GROK_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";
const GROK_STATSIG_SUFFIX: &str =
    "e:TypeError: Cannot read properties of null (reading 'children')";

/// Grok web mode mapping (JS MODEL_MAP, lines 9-24).
struct GrokModelInfo {
    grok_model: &'static str,
    model_mode: &'static str,
    is_thinking: bool,
}

fn model_map() -> &'static [(&'static str, GrokModelInfo)] {
    &[
        (
            "grok-3",
            GrokModelInfo {
                grok_model: "grok-3",
                model_mode: "MODEL_MODE_GROK_3",
                is_thinking: false,
            },
        ),
        (
            "grok-3-mini",
            GrokModelInfo {
                grok_model: "grok-3",
                model_mode: "MODEL_MODE_GROK_3_MINI_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-3-thinking",
            GrokModelInfo {
                grok_model: "grok-3",
                model_mode: "MODEL_MODE_GROK_3_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4",
            GrokModelInfo {
                grok_model: "grok-4",
                model_mode: "MODEL_MODE_GROK_4",
                is_thinking: false,
            },
        ),
        (
            "grok-4-mini",
            GrokModelInfo {
                grok_model: "grok-4-mini",
                model_mode: "MODEL_MODE_GROK_4_MINI_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4-thinking",
            GrokModelInfo {
                grok_model: "grok-4",
                model_mode: "MODEL_MODE_GROK_4_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4-heavy",
            GrokModelInfo {
                grok_model: "grok-4",
                model_mode: "MODEL_MODE_HEAVY",
                is_thinking: true,
            },
        ),
        (
            "grok-4.1-mini",
            GrokModelInfo {
                grok_model: "grok-4-1-thinking-1129",
                model_mode: "MODEL_MODE_GROK_4_1_MINI_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4.1-fast",
            GrokModelInfo {
                grok_model: "grok-4-1-thinking-1129",
                model_mode: "MODEL_MODE_FAST",
                is_thinking: false,
            },
        ),
        (
            "grok-4.1-expert",
            GrokModelInfo {
                grok_model: "grok-4-1-thinking-1129",
                model_mode: "MODEL_MODE_EXPERT",
                is_thinking: true,
            },
        ),
        (
            "grok-4.1-thinking",
            GrokModelInfo {
                grok_model: "grok-4-1-thinking-1129",
                model_mode: "MODEL_MODE_GROK_4_1_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4.2",
            GrokModelInfo {
                grok_model: "grok-420",
                model_mode: "MODEL_MODE_GROK_420",
                is_thinking: false,
            },
        ),
        (
            "grok-4.20",
            GrokModelInfo {
                grok_model: "grok-420",
                model_mode: "MODEL_MODE_GROK_420",
                is_thinking: false,
            },
        ),
        (
            "grok-4.20-beta",
            GrokModelInfo {
                grok_model: "grok-420",
                model_mode: "MODEL_MODE_GROK_420",
                is_thinking: false,
            },
        ),
    ]
}

/// Random lowercase-alpha string of `length` chars (JS randomString without alphanumeric).
fn random_alpha(length: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let mut out = String::with_capacity(length);
    for _ in 0..length {
        out.push(CHARS[(rng.next_u32() as usize) % CHARS.len()] as char);
    }
    out
}

/// Random lowercase-alphanumeric string of `length` chars (JS randomString alphanumeric).
fn random_alnum(length: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    let mut out = String::with_capacity(length);
    for _ in 0..length {
        out.push(CHARS[(rng.next_u32() as usize) % CHARS.len()] as char);
    }
    out
}

/// `x-statsig-id`: base64 of a fake browser TypeError (JS generateStatsigId).
fn generate_statsig_id() -> String {
    let msg = format!(
        "{}['{}']{}",
        "e:TypeError: Cannot read properties of null (reading 'children",
        random_alnum(5),
        ")']"
    );
    STANDARD.encode(format!("{}:{}", msg, GROK_STATSIG_SUFFIX))
}

/// Random hex string of `len_bytes` bytes (JS randomHex).
fn random_hex(len_bytes: usize) -> String {
    let mut bytes = vec![0u8; len_bytes];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Flatten OpenAI messages to a single Grok message string (JS parseOpenAIMessages).
///
/// Every message gets a `role: ` prefix except the last user message, which is
/// passed raw. System messages DO get role-prefixed (grok-web differs from
/// perplexity-web here). `developer` role is normalized to `system`.
fn parse_openai_messages(messages: &[Value]) -> String {
    let mut extracted: Vec<(String, String)> = Vec::new();

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        let role = if role == "developer" {
            "system".to_string()
        } else {
            role
        };

        let content = match message.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        if content.trim().is_empty() {
            continue;
        }
        extracted.push((role, content));
    }

    let last_user_idx = extracted.iter().rposition(|(role, _)| role == "user");

    let mut parts = Vec::with_capacity(extracted.len());
    for (idx, (role, text)) in extracted.into_iter().enumerate() {
        if Some(idx) == last_user_idx {
            parts.push(text);
        } else {
            parts.push(format!("{role}: {text}"));
        }
    }

    parts.join("\n\n")
}

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

        // Resolve model → Grok internal model/mode; default to grok-4.1-fast
        // for unmapped models (JS `modelInfo || MODEL_MAP["grok-4.1-fast"]`).
        let model_info = model_map()
            .iter()
            .find(|(id, _)| *id == request.model)
            .map(|(_, info)| info)
            .unwrap_or_else(|| {
                model_map()
                    .iter()
                    .find(|(id, _)| *id == "grok-4.1-fast")
                    .map(|(_, info)| info)
                    .expect("grok-4.1-fast is a model map entry")
            });

        let message = match request.body.get("messages").and_then(Value::as_array) {
            Some(messages) => parse_openai_messages(messages),
            None => String::new(),
        };

        let transformed_body = self.build_payload(model_info, &message);
        let headers = self.build_headers(&request.credentials)?;

        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get("grok-web", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        let status = response.status();

        // Success path: consume the NDJSON body and rebuild it as OpenAI format
        // (SSE chunks for streaming, chat.completion JSON otherwise). Error
        // statuses get a rewritten `{error:{...}}` body, mirroring JS `execute`.
        let response = if status.is_success() {
            let body_bytes = response.bytes().await.unwrap_or_default();
            let is_thinking = model_info.is_thinking;
            let cid = format!("chatcmpl-grok-{}", random_hex(6));
            let created = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            if request.stream {
                let body = build_streaming_response(
                    &body_bytes,
                    &request.model,
                    &cid,
                    created,
                    is_thinking,
                );
                let mut builder = http::Response::builder()
                    .status(http::StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "text/event-stream")
                    .header(http::header::CACHE_CONTROL, "no-cache")
                    .header("X-Accel-Buffering", "no");
                let http_response = builder
                    .body(body)
                    .map_err(GrokWebExecutorError::InvalidRequest)?;
                reqwest::Response::from(http_response)
            } else {
                let body = build_non_streaming_response(
                    &body_bytes,
                    &request.model,
                    &cid,
                    created,
                    is_thinking,
                );
                let mut builder = http::Response::builder()
                    .status(http::StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "application/json");
                let http_response = builder
                    .body(body)
                    .map_err(GrokWebExecutorError::InvalidRequest)?;
                reqwest::Response::from(http_response)
            }
        } else {
            let status_u16 = status.as_u16();
            let status_code =
                http::StatusCode::from_u16(status_u16).unwrap_or(http::StatusCode::BAD_GATEWAY);
            let (message, error_type, code) = match status_u16 {
                401 | 403 => (
                    "Grok auth failed — SSO cookie may be expired. Re-paste your sso cookie value from grok.com.",
                    "authentication_error",
                    format!("HTTP_{status_u16}"),
                ),
                429 => (
                    "Grok rate limited. Wait a moment and retry, or rotate cookies.",
                    "rate_limit_error",
                    format!("HTTP_{status_u16}"),
                ),
                _ => (
                    "Grok returned HTTP {status_u16}",
                    "upstream_error",
                    format!("HTTP_{status_u16}"),
                ),
            };
            let body = json!({
                "error": {
                    "message": message,
                    "type": error_type,
                    "code": code,
                }
            });
            let body_bytes = serde_json::to_vec(&body)?;
            let mut builder = http::Response::builder()
                .status(status_code)
                .header(http::header::CONTENT_TYPE, "application/json");
            let http_response = builder
                .body(body_bytes)
                .map_err(GrokWebExecutorError::InvalidRequest)?;
            reqwest::Response::from(http_response)
        };

        Ok(GrokWebExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    fn build_url(&self) -> String {
        GROK_CHAT_API.to_string()
    }

    /// grokPayload (JS lines 247-259) — verbatim field set.
    fn build_payload(&self, model_info: &GrokModelInfo, message: &str) -> Value {
        json!({
            "temporary": true,
            "modelName": model_info.grok_model,
            "modelMode": model_info.model_mode,
            "message": message,
            "fileAttachments": [],
            "imageAttachments": [],
            "disableSearch": false,
            "enableImageGeneration": false,
            "returnImageBytes": false,
            "returnRawGrokInXaiRequest": false,
            "enableImageStreaming": false,
            "imageGenerationCount": 0,
            "forceConcise": false,
            "toolOverrides": {},
            "enableSideBySide": true,
            "sendFinalMetadata": true,
            "isReasoning": false,
            "disableTextFollowUps": false,
            "disableMemory": true,
            "forceSideBySide": false,
            "isAsyncChat": false,
            "disableSelfHarmShortCircuit": false,
            "deviceEnvInfo": {
                "darkModeEnabled": false,
                "devicePixelRatio": 2,
                "screenWidth": 2056,
                "screenHeight": 1329,
                "viewportWidth": 2056,
                "viewportHeight": 1083
            }
        })
    }

    /// Browser-fingerprint headers (JS lines 263-283) — statsig, traceparent,
    /// Sec-Ch-Ua/Sec-Fetch-*, Chrome 136 UA, and the `sso` cookie.
    fn build_headers(
        &self,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, GrokWebExecutorError> {
        let mut headers = HeaderMap::new();

        let sso = credentials
            .api_key
            .as_deref()
            .ok_or_else(|| GrokWebExecutorError::MissingCredentials("grok-web".into()))?
            .to_string();
        let token = sso.strip_prefix("sso=").unwrap_or(&sso);
        let cookie_value = HeaderValue::from_str(&format!("sso={token}"))
            .map_err(|_| GrokWebExecutorError::CookieParse("Invalid cookie".to_string()))?;
        headers.insert(COOKIE, cookie_value);

        headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br, zstd"),
        );
        headers.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            HeaderValue::from_static("en-US,en;q=0.9"),
        );
        headers.insert(
            reqwest::header::ORIGIN,
            HeaderValue::from_static("https://grok.com"),
        );
        headers.insert(
            reqwest::header::REFERER,
            HeaderValue::from_static("https://grok.com/"),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(GROK_USER_AGENT),
        );
        headers.insert(
            "Sec-Ch-Ua",
            HeaderValue::from_static(
                r#""Google Chrome";v="136", "Chromium";v="136", "Not(A:Brand";v="24""#,
            ),
        );
        headers.insert("Sec-Ch-Ua-Mobile", HeaderValue::from_static("?0"));
        headers.insert("Sec-Ch-Ua-Platform", HeaderValue::from_static(r#""macOS""#));
        headers.insert("Sec-Fetch-Dest", HeaderValue::from_static("empty"));
        headers.insert("Sec-Fetch-Mode", HeaderValue::from_static("cors"));
        headers.insert("Sec-Fetch-Site", HeaderValue::from_static("same-origin"));
        headers.insert(
            "Baggage",
            HeaderValue::from_static(
                "sentry-environment=production,sentry-release=d6add6fb0460641fd482d767a335ef72b9b6abb8,sentry-public_key=b311e0f2690c81f25e2c4cf6d4f7ce1c",
            ),
        );
        headers.insert(
            "x-statsig-id",
            HeaderValue::from_str(&generate_statsig_id()).map_err(|_| {
                GrokWebExecutorError::InvalidCredentials("Invalid statsig id".to_string())
            })?,
        );
        headers.insert(
            "x-xai-request-id",
            HeaderValue::from_str(&Uuid::new_v4().to_string()).map_err(|_| {
                GrokWebExecutorError::InvalidCredentials("Invalid request id".to_string())
            })?,
        );
        headers.insert(
            "traceparent",
            HeaderValue::from_str(&format!("00-{}-{}-00", random_hex(16), random_hex(8))).map_err(
                |_| GrokWebExecutorError::InvalidCredentials("Invalid traceparent".to_string()),
            )?,
        );

        Ok(headers)
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
        let headers = self.build_headers(&request.credentials)?;
        let transformed_body = self.transform_request(&request.body, &request.credentials)?;

        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get("perplexity-web", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        Ok(PerplexityWebExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    fn build_url(&self) -> String {
        "https://perplexity.ai/rest/sse/perplexity_ask".to_string()
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, PerplexityWebExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

        if let Some(cookie) = &credentials.access_token {
            let cookie_value = HeaderValue::from_str(cookie).map_err(|_| {
                PerplexityWebExecutorError::CookieParse("Invalid cookie".to_string())
            })?;
            headers.insert(COOKIE, cookie_value);
        }

        Ok(headers)
    }

    fn fnv1a_hash(&self, content: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in content.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn transform_request(
        &self,
        body: &Value,
        _credentials: &ProviderConnection,
    ) -> Result<Value, PerplexityWebExecutorError> {
        let mut transformed = serde_json::Map::new();

        if let Some(obj) = body.as_object() {
            for (k, v) in obj {
                transformed.insert(k.clone(), v.clone());
            }
        }

        if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
            let conversation_context: String = messages
                .iter()
                .filter_map(|m| {
                    let role = m.get("role")?.as_str()?;
                    let content = m.get("content")?.as_str()?;
                    Some(format!("{}: {}", role, content))
                })
                .collect::<Vec<_>>()
                .join("\n");

            let session_hash = self.fnv1a_hash(&conversation_context);
            transformed.insert(
                "session_cache_key".to_string(),
                serde_json::json!(format!("{:016x}", session_hash)),
            );
        }

        Ok(Value::Object(transformed))
    }
}
/// Parse a Grok NDJSON response into ordered content chunks (JS extractContent).
///
/// Handles, in order: `event.error`, `resp.modelResponse` (final/complete —
/// thinking close for thinking models, then `fullMessage`), and streaming
/// `resp.token` (passed through raw; a non-thinking model never yields
/// thinking, matching JS `extractContent` which only emits thinking for
/// isThinkingModel).
fn extract_content(body: &[u8], is_thinking: bool) -> Vec<ContentChunk> {
    let mut chunks = Vec::new();
    let mut think_opened = false;

    for line in body.split(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(line).trim().to_string();
        if line.is_empty() {
            continue;
        }
        let event: Value = match serde_json::from_str(&line) {
            Ok(event) => event,
            Err(_) => continue, // Skip non-JSON lines
        };

        if let Some(error) = event.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    error
                        .get("code")
                        .and_then(Value::as_str)
                        .map(|code| format!("Grok error: {code}"))
                })
                .unwrap_or_default();
            chunks.push(ContentChunk {
                error: Some(message),
                done: Some(true),
                ..ContentChunk::default()
            });
            return chunks;
        }

        let resp = match event.get("result").and_then(|r| r.get("response")) {
            Some(resp) => resp,
            None => continue,
        };

        // modelResponse = final/complete response
        if let Some(mr) = resp.get("modelResponse") {
            // Thinking model: the modelResponse closes the thinking block
            // opened by the first modelResponse (spec: thinking only emitted
            // for isThinking models when a modelResponse follows). The final
            // message is emitted as the thinking chunk; no separate
            // fullMessage content chunk.
            if is_thinking {
                if let Some(message) = mr.get("message").and_then(Value::as_str) {
                    chunks.push(ContentChunk {
                        thinking: Some(message.to_string()),
                        ..ContentChunk::default()
                    });
                }
                continue;
            }

            if let Some(message) = mr.get("message").and_then(Value::as_str) {
                chunks.push(ContentChunk {
                    full_message: Some(message.to_string()),
                    ..ContentChunk::default()
                });
            }
            continue;
        }

        // Streaming token
        if resp.get("token").and_then(Value::as_str).is_some() {
            let token = resp
                .get("token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            chunks.push(ContentChunk {
                delta: Some(token),
                ..ContentChunk::default()
            });
        }
    }

    chunks.push(ContentChunk {
        done: Some(true),
        ..ContentChunk::default()
    });
    chunks
}

/// One extracted content piece (JS ContentChunk).
#[derive(Debug, Default)]
struct ContentChunk {
    delta: Option<String>,
    thinking: Option<String>,
    full_message: Option<String>,
    error: Option<String>,
    done: Option<bool>,
}

/// `data: {json}\n\n` SSE frame (JS sseChunk).
fn sse_chunk(data: &Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string())
    )
}

/// Streaming response builder: NDJSON → OpenAI `chat.completion.chunk` SSE
/// (JS buildStreamingResponse): initial `{role:"assistant"}` chunk, thinking →
/// `delta.reasoning_content` (only when isThinking), content → `delta.content`,
/// `[Error: ...]` content delta on upstream error, `finish_reason:"stop"` +
/// `data: [DONE]`.
fn build_streaming_response(
    body: &[u8],
    model: &str,
    cid: &str,
    created: i64,
    is_thinking: bool,
) -> Body {
    // Clone into owned values: `Body::wrap_stream` requires a `'static` stream,
    // but the `async_stream::stream!` macro borrows `model`/`cid` when they are
    // used inside `json!`.
    let model = model.to_string();
    let cid = cid.to_string();
    // Parse the NDJSON body up front: `body` is borrowed, but the stream must
    // be `'static` — extracting the owned chunks here keeps it that way.
    let chunks = extract_content(body, is_thinking);
    Body::wrap_stream(async_stream::stream! {
        // Initial role chunk
        let role_chunk = json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "system_fingerprint": null,
            "choices": [
                { "index": 0, "delta": { "role": "assistant" }, "finish_reason": null, "logprobs": null }
            ],
        });
        yield Ok::<Bytes, std::io::Error>(Bytes::from(sse_chunk(&role_chunk)));
        for chunk in chunks {
            if let Some(error) = chunk.error {
                let error_chunk = json!({
                    "id": cid,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "system_fingerprint": null,
                    "choices": [
                        { "index": 0, "delta": { "content": format!("[Error: {error}]") }, "finish_reason": null, "logprobs": null }
                    ],
                });
                yield Ok::<Bytes, std::io::Error>(Bytes::from(sse_chunk(&error_chunk)));
                break;
            }
            if chunk.done.unwrap_or(false) {
                break;
            }
            if let Some(thinking) = chunk.thinking {
                let thinking_chunk = json!({
                    "id": cid,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "system_fingerprint": null,
                    "choices": [
                        { "index": 0, "delta": { "reasoning_content": thinking }, "finish_reason": null, "logprobs": null }
                    ],
                });
                yield Ok::<Bytes, std::io::Error>(Bytes::from(sse_chunk(&thinking_chunk)));
                continue;
            }
            if let Some(delta) = chunk.delta {
                let delta_chunk = json!({
                    "id": cid,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "system_fingerprint": null,
                    "choices": [
                        { "index": 0, "delta": { "content": delta }, "finish_reason": null, "logprobs": null }
                    ],
                });
                yield Ok::<Bytes, std::io::Error>(Bytes::from(sse_chunk(&delta_chunk)));
            }
        }

        // Stop chunk + [DONE]
        let stop_chunk = json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "system_fingerprint": null,
            "choices": [
                { "index": 0, "delta": {}, "finish_reason": "stop", "logprobs": null }
            ],
        });
        yield Ok::<Bytes, std::io::Error>(Bytes::from(sse_chunk(&stop_chunk)));
        yield Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n"));
    })
}

/// Non-streaming response builder: NDJSON → OpenAI `chat.completion` JSON
/// (JS buildNonStreamingResponse): `reasoning_content = thinkingParts.join("\n")`
/// when non-empty, usage `prompt_tokens = completion_tokens =
/// ceil(fullContent.length / 4)`, `total_tokens` the sum.
fn build_non_streaming_response(
    body: &[u8],
    model: &str,
    cid: &str,
    created: i64,
    is_thinking: bool,
) -> Bytes {
    let mut full_content = String::new();
    let mut thinking_parts: Vec<String> = Vec::new();

    for chunk in extract_content(body, is_thinking) {
        if let Some(error) = chunk.error {
            return serde_json::to_vec(&json!({
                "error": { "message": error, "type": "upstream_error", "code": "GROK_ERROR" }
            }))
            .map(Bytes::from)
            .unwrap_or_default();
        }
        if chunk.done.unwrap_or(false) {
            break;
        }
        if let Some(thinking) = chunk.thinking {
            thinking_parts.push(thinking);
            continue;
        }
        if let Some(full_message) = chunk.full_message {
            full_content = full_message;
        } else if let Some(delta) = chunk.delta {
            full_content.push_str(&delta);
        }
    }

    let mut message = json!({ "role": "assistant", "content": full_content });
    if !thinking_parts.is_empty() {
        message["reasoning_content"] = json!(thinking_parts.join("\n"));
    }

    let prompt_tokens = (full_content.len() as f64 / 4.0).ceil() as u64;
    let completion_tokens = prompt_tokens;

    serde_json::to_vec(&json!({
        "id": cid,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "system_fingerprint": null,
        "choices": [
            { "index": 0, "message": message, "finish_reason": "stop", "logprobs": null }
        ],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    }))
    .map(Bytes::from)
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_hash_empty() {
        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let hash = executor.fnv1a_hash("");
        assert_eq!(hash, 0xcbf29ce484222325);
    }

    #[test]
    fn test_fnv1a_hash_deterministic() {
        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let hash1 = executor.fnv1a_hash("hello");
        let hash2 = executor.fnv1a_hash("hello");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_fnv1a_hash_different_inputs() {
        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let hash1 = executor.fnv1a_hash("hello");
        let hash2 = executor.fnv1a_hash("world");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_grok_build_url() {
        let executor = GrokWebExecutor::new(Arc::new(ClientPool::default()));
        let url = executor.build_url();
        assert_eq!(url, "https://grok.com/rest/app-chat/conversations/new");
    }

    #[test]
    fn test_perplexity_build_url() {
        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let url = executor.build_url();
        assert!(url.contains("perplexity.ai"));
    }

    #[test]
    fn test_grok_web_payload_shape() {
        let executor = GrokWebExecutor::new(Arc::new(ClientPool::default()));
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "S"},
                {"role": "user", "content": "U"}
            ]
        });
        let model_info = model_map()
            .iter()
            .find(|(id, _)| *id == "grok-4.2")
            .map(|(_, info)| info)
            .unwrap();
        let message =
            parse_openai_messages(body.get("messages").and_then(Value::as_array).unwrap());
        let transformed = executor.build_payload(model_info, &message);
        assert_eq!(transformed["modelName"], "grok-420");
        assert_eq!(transformed["modelMode"], "MODEL_MODE_GROK_420");
        assert_eq!(transformed["message"], "system: S\n\nU");
        assert_eq!(transformed["temporary"], true);
    }

    #[test]
    fn test_parse_openai_messages_last_user_raw() {
        let messages = serde_json::json!([
            {"role": "system", "content": "Be helpful"},
            {"role": "user", "content": "First question"},
            {"role": "assistant", "content": "First answer"},
            {"role": "user", "content": "Follow up"}
        ]);
        let parsed = parse_openai_messages(messages.as_array().unwrap());
        assert_eq!(
            parsed,
            "system: Be helpful\n\nuser: First question\n\nassistant: First answer\n\nFollow up"
        );
    }

    #[test]
    fn test_parse_openai_messages_skips_empty() {
        let messages = serde_json::json!([
            {"role": "system", "content": " "},
            {"role": "user", "content": "Hello"}
        ]);
        let parsed = parse_openai_messages(messages.as_array().unwrap());
        assert_eq!(parsed, "Hello");
    }

    #[test]
    fn test_parse_openai_messages_content_array() {
        let messages = serde_json::json!([
            {"role": "user", "content": [
                {"type": "text", "text": "Hello"},
                {"type": "image_url", "image_url": {"url": "data:..."}},
                {"type": "text", "text": " world"}
            ]}
        ]);
        let parsed = parse_openai_messages(messages.as_array().unwrap());
        assert_eq!(parsed, "Hello  world");
    }

    #[test]
    fn test_parse_openai_messages_developer_is_system() {
        let messages = serde_json::json!([
            {"role": "developer", "content": "Instructions"},
            {"role": "user", "content": "Hi"}
        ]);
        let parsed = parse_openai_messages(messages.as_array().unwrap());
        assert_eq!(parsed, "system: Instructions\n\nHi");
    }

    #[test]
    fn test_generate_statsig_id_is_valid_base64() {
        let statsig = generate_statsig_id();
        // Must be valid base64 and decode to an e:TypeError string.
        let decoded = STANDARD.decode(&statsig).expect("valid base64");
        assert!(String::from_utf8(decoded)
            .unwrap()
            .starts_with("e:TypeError:"));
    }

    #[test]
    fn test_grok_headers_cookie_prefix_stripped() {
        use std::collections::BTreeMap;
        let executor = GrokWebExecutor::new(Arc::new(ClientPool::default()));
        let credentials = ProviderConnection {
            id: "test".to_string(),
            provider: "grok-web".to_string(),
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
            api_key: Some("sso=my-token".to_string()),
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
        assert_eq!(headers.get("cookie").unwrap(), "sso=my-token");
        assert!(headers.get("x-statsig-id").is_some());
        assert!(headers.get("x-xai-request-id").is_some());
        assert_eq!(headers.get("origin").unwrap(), "https://grok.com");
        let traceparent = headers.get("traceparent").unwrap().to_str().unwrap();
        assert!(traceparent.starts_with("00-"));
        assert_eq!(traceparent.len(), 3 + 32 + 1 + 16 + 1 + 2);
    }

    #[test]
    fn test_transform_request_adds_session_cache() {
        use std::collections::BTreeMap;

        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let body = serde_json::json!({
            "model": "sonar",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
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
        let result = executor.transform_request(&body, &credentials);
        assert!(result.is_ok());
        let transformed = result.unwrap();
        assert!(transformed.get("session_cache_key").is_some());
    }
    #[test]
    fn test_extract_content_token_and_full_message() {
        let body = br#"{"result":{"response":{"token":"Hel"}}}
{"result":{"response":{"token":"lo"}}}
{"result":{"response":{"modelResponse":{"message":"Hello world"}}}}
"#;
        let chunks = extract_content(body, false);
        assert_eq!(chunks.len(), 4); // 2 tokens + fullMessage + trailing done
        assert_eq!(chunks[0].delta.as_deref(), Some("Hel"));
        assert_eq!(chunks[1].delta.as_deref(), Some("lo"));
        assert_eq!(chunks[2].full_message.as_deref(), Some("Hello world"));
        assert!(chunks[3].done.unwrap_or(false));
    }

    #[test]
    fn test_extract_content_skips_non_json_lines() {
        let body = br#"some noise
{"result":{"response":{"token":"ok"}}}
"#;
        let chunks = extract_content(body, false);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].delta.as_deref(), Some("ok"));
        assert!(chunks[1].done.unwrap_or(false));
    }

    #[test]
    fn test_extract_content_error_event() {
        let body = br#"{"error":{"message":"boom","code":"X"}}
"#;
        let chunks = extract_content(body, false);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].error.as_deref(), Some("boom"));
        assert!(chunks[0].done.unwrap_or(false));
    }

    #[test]
    fn test_extract_content_thinking_only_for_thinking_models() {
        let thinking_lines = br#"{"result":{"response":{"token":"visible"}}}
{"result":{"response":{"modelResponse":{"message":"Final"}}}}
"#;
        // Non-thinking model: token is content, modelResponse closes nothing,
        // thinking never emitted.
        let chunks = extract_content(thinking_lines, false);
        assert!(chunks.iter().all(|c| c.thinking.is_none()));
        assert_eq!(chunks[0].delta.as_deref(), Some("visible"));
        assert_eq!(chunks[1].full_message.as_deref(), Some("Final"));

        // Thinking model with an open thinking block gets it closed.
        let body = br#"{"result":{"response":{"modelResponse":{"message":"Final"}}}}
"#;
        let chunks = extract_content(body, true);
        assert_eq!(chunks[0].thinking.as_deref(), Some("Final"));
        assert!(chunks[0].full_message.is_none());
    }

    #[tokio::test]
    async fn test_build_streaming_response_sse_shape() {
        use http_body_util::BodyExt;
        let body = br#"{"result":{"response":{"token":"hi"}}}
{"result":{"response":{"modelResponse":{"message":"hi"}}}}
"#;
        let bytes = build_streaming_response(body, "grok-4.2", "chatcmpl-grok-abc123", 1234, false)
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.starts_with("data: {"));
        assert!(text.contains("\"object\":\"chat.completion.chunk\""));
        assert!(text.contains("\"id\":\"chatcmpl-grok-abc123\""));
        assert!(text.contains("\"delta\":{\"role\":\"assistant\"}"));
        assert!(text.contains("\"delta\":{\"content\":\"hi\"}"));
        assert!(text.contains("\"finish_reason\":\"stop\""));
        assert!(text.ends_with("data: [DONE]\n\n"));
    }

    #[tokio::test]
    async fn test_build_streaming_response_thinking_uses_reasoning_content() {
        use http_body_util::BodyExt;
        let body = br#"{"result":{"response":{"token":"t1"}}}
{"result":{"response":{"modelResponse":{"message":"Final"}}}}
"#;
        // is_thinking=true: modelResponse closes the thinking block → the final
        // message becomes a reasoning_content delta.
        let bytes = build_streaming_response(body, "grok-4.1-expert", "cid", 1, true)
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("\"delta\":{\"content\":\"t1\"}"));
        assert!(text.contains("\"delta\":{\"reasoning_content\":\"Final\"}"));
        // No content delta for the final message (it went to reasoning_content).
        assert_eq!(text.matches("\"delta\":{\"content\":\"Final\"").count(), 0);
    }

    #[test]
    fn test_build_non_streaming_response_shape() {
        let body = br#"{"result":{"response":{"token":"Hel"}}}
{"result":{"response":{"token":"lo"}}}
{"result":{"response":{"modelResponse":{"message":"Hello"}}}}
"#;
        let bytes =
            build_non_streaming_response(body, "grok-4.2", "chatcmpl-grok-abc123", 1234, false);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["object"], "chat.completion");
        assert_eq!(value["choices"][0]["message"]["content"], "Hello");
        assert!(value["choices"][0]["message"]
            .get("reasoning_content")
            .is_none());
        assert_eq!(value["usage"]["prompt_tokens"], 2); // ceil(5/4)
        assert_eq!(value["usage"]["completion_tokens"], 2);
        assert_eq!(value["usage"]["total_tokens"], 4);
    }

    #[test]
    fn test_build_non_streaming_response_thinking_joined() {
        let body = br#"{"result":{"response":{"token":"visible"}}}
{"result":{"response":{"modelResponse":{"message":"Final"}}}}
"#;
        let bytes = build_non_streaming_response(body, "grok-4.1-expert", "cid", 1, true);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["choices"][0]["message"]["content"], "visible");
        assert_eq!(value["choices"][0]["message"]["reasoning_content"], "Final");
    }

    #[test]
    fn test_build_non_streaming_response_error_maps_grok_error() {
        let body = br#"{"error":{"message":"boom"}}
"#;
        let bytes = build_non_streaming_response(body, "grok-4.2", "cid", 1, false);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["error"]["message"], "boom");
        assert_eq!(value["error"]["type"], "upstream_error");
        assert_eq!(value["error"]["code"], "GROK_ERROR");
    }
}
