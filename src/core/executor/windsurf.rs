//! Windsurf executor — Codeium gRPC-web chat.
//!
//! Port of 9router `open-sse/executors/windsurf.js`:
//! OpenAI chat body → protobuf `GetChatMessage` request → gRPC-web framed
//! POST to `https://server.codeium.com` → decode `CompletionChunk` frames →
//! OpenAI SSE chunks.
//!
//! Wire protocol: gRPC-web over HTTPS (`Content-Type: application/grpc-web+proto`).
//! Service `exa.language_server_pb.LanguageServerService`, method `GetChatMessage`
//! (unary request → streamed CompletionChunk frames).
//!
//! Auth: `credentials.accessToken` or `credentials.apiKey` (Codeium apiKey
//! `sk-ws-...` or Firebase-derived) — placed BOTH in the protobuf
//! `Metadata.api_key` field and the `Authorization: Bearer` header. Omitting
//! either breaks auth.
//!
//! CompletionChunk (oneof): field 1 = ContentChunk{field1 text}, field 2 =
//! ToolCallChunk (skipped, unhandled), field 3 = DoneChunk{UsageStats p/c},
//! field 4 = ErrorChunk{field1 message}.

use std::sync::Arc;

use hyper::http;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

const WS_BASE_URL: &str = "https://server.codeium.com";
const WS_SERVICE: &str = "exa.language_server_pb.LanguageServerService";
const WS_METHOD_CHAT: &str = "GetChatMessage";
const WS_CHAT_URL: &str =
    "https://server.codeium.com/exa.language_server_pb.LanguageServerService/GetChatMessage";

const WS_IDE_NAME: &str = "windsurf";
const WS_IDE_VERSION: &str = "3.14.0";
const WS_EXT_VERSION: &str = "3.14.0";
const WS_LOCALE: &str = "en-US";

/// Model alias map (catalog name → Windsurf wire name). Ported verbatim from
/// JS `MODEL_ALIAS_MAP` (windsurf.js:26-119).
pub fn resolve_ws_model_id(model: &str) -> &str {
    match model {
        // ── Cognition SWE ──
        "swe-1.6-fast" => "swe-1-6-fast",
        "swe-1.6" => "swe-1-6",
        "swe-1.5-fast" => "swe-1-5-fast",
        "swe-1.5" => "swe-1-5",
        // ── Claude Opus 4.7 — effort-tiered ──
        "claude-opus-4.7-max" => "claude-opus-4-7-max",
        "claude-opus-4.7-xhigh" => "claude-opus-4-7-xhigh",
        "claude-opus-4.7-high" => "claude-opus-4-7-high",
        "claude-opus-4.7-medium" => "claude-opus-4-7-medium",
        "claude-opus-4.7-low" => "claude-opus-4-7-low",
        "claude-opus-4.7-review" => "opus-4-7-review",
        // ── Claude Opus/Sonnet 4.6 ──
        "claude-sonnet-4.6-thinking-1m" => "claude-sonnet-4-6-thinking-1m",
        "claude-sonnet-4.6-1m" => "claude-sonnet-4-6-1m",
        "claude-sonnet-4.6-thinking" => "claude-sonnet-4-6-thinking",
        "claude-sonnet-4.6" => "claude-sonnet-4-6",
        "claude-opus-4.6-thinking" => "claude-opus-4-6-thinking",
        "claude-opus-4.6" => "claude-opus-4-6",
        // ── Claude 4.5 ──
        "claude-opus-4.5-thinking" => "MODEL_CLAUDE_4_5_OPUS_THINKING",
        "claude-opus-4.5" => "MODEL_CLAUDE_4_5_OPUS",
        "claude-sonnet-4.5-thinking" => "MODEL_PRIVATE_3",
        "claude-sonnet-4.5" => "MODEL_PRIVATE_2",
        "claude-haiku-4.5" => "MODEL_PRIVATE_11",
        // ── GPT-5.5 ──
        "gpt-5.5-xhigh-fast" => "gpt-5-5-xhigh-priority",
        "gpt-5.5-high-fast" => "gpt-5-5-high-priority",
        "gpt-5.5-medium-fast" => "gpt-5-5-medium-priority",
        "gpt-5.5-low-fast" => "gpt-5-5-low-priority",
        "gpt-5.5-none-fast" => "gpt-5-5-none-priority",
        "gpt-5.5-xhigh" => "gpt-5-5-xhigh",
        "gpt-5.5-high" => "gpt-5-5-high",
        "gpt-5.5-medium" => "gpt-5-5-medium",
        "gpt-5.5-low" => "gpt-5-5-low",
        "gpt-5.5-none" => "gpt-5-5-none",
        "gpt-5.5-review" => "gpt-5-5-review",
        "gpt-5.5" => "gpt-5-5-medium",
        // ── GPT-5.4 ──
        "gpt-5.4-xhigh-fast" => "gpt-5-4-xhigh-priority",
        "gpt-5.4-high-fast" => "gpt-5-4-high-priority",
        "gpt-5.4-medium-fast" => "gpt-5-4-medium-priority",
        "gpt-5.4-low-fast" => "gpt-5-4-low-priority",
        "gpt-5.4-none-fast" => "gpt-5-4-none-priority",
        "gpt-5.4-xhigh" => "gpt-5-4-xhigh",
        "gpt-5.4-high" => "gpt-5-4-high",
        "gpt-5.4-medium" => "gpt-5-4-medium",
        "gpt-5.4-low" => "gpt-5-4-low",
        "gpt-5.4-none" => "gpt-5-4-none",
        "gpt-5.4-mini-xhigh" => "gpt-5-4-mini-xhigh",
        "gpt-5.4-mini-high" => "gpt-5-4-mini-high",
        "gpt-5.4-mini-medium" => "gpt-5-4-mini-medium",
        "gpt-5.4-mini-low" => "gpt-5-4-mini-low",
        "gpt-5.4" => "gpt-5-4-medium",
        // ── GPT-5.3-Codex ──
        "gpt-5.3-codex-xhigh-fast" => "gpt-5-3-codex-xhigh-priority",
        "gpt-5.3-codex-high-fast" => "gpt-5-3-codex-high-priority",
        "gpt-5.3-codex-medium-fast" => "gpt-5-3-codex-medium-priority",
        "gpt-5.3-codex-low-fast" => "gpt-5-3-codex-low-priority",
        "gpt-5.3-codex-xhigh" => "gpt-5-3-codex-xhigh",
        "gpt-5.3-codex-high" => "gpt-5-3-codex-high",
        "gpt-5.3-codex-medium" => "gpt-5-3-codex-medium",
        "gpt-5.3-codex-low" => "gpt-5-3-codex-low",
        "gpt-5.3-codex" => "gpt-5-3-codex-medium",
        // ── GPT-5.2 ──
        "gpt-5.2-xhigh" => "MODEL_GPT_5_2_XHIGH",
        "gpt-5.2-high" => "MODEL_GPT_5_2_HIGH",
        "gpt-5.2-medium" => "MODEL_GPT_5_2_MEDIUM",
        "gpt-5.2-low" => "MODEL_GPT_5_2_LOW",
        "gpt-5.2-none" => "MODEL_GPT_5_2_NONE",
        "gpt-5.2" => "MODEL_GPT_5_2_MEDIUM",
        // ── GPT-5 ──
        "gpt-5" => "gpt-5",
        // ── GPT-4.1 / 4o ──
        "gpt-4.1" => "MODEL_CHAT_GPT_4_1_2025_04_14",
        "gpt-4.1-mini" => "gpt-4.1-mini",
        "gpt-4o" => "MODEL_CHAT_GPT_4O_2024_08_06",
        // ── Gemini ──
        "gemini-3.1-pro-high" => "gemini-3-1-pro-high",
        "gemini-3.1-pro-low" => "gemini-3-1-pro-low",
        "gemini-3.1-pro" => "gemini-3-1-pro-high",
        "gemini-3.0-flash-high" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_HIGH",
        "gemini-3.0-flash-medium" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_MEDIUM",
        "gemini-3.0-flash-low" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_LOW",
        "gemini-3.0-flash-minimal" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_MINIMAL",
        "gemini-3.0-flash" => "MODEL_GOOGLE_GEMINI_3_0_FLASH_HIGH",
        "gemini-2.5-pro" => "MODEL_GOOGLE_GEMINI_2_5_PRO",
        // ── Others ──
        "deepseek-v4" => "deepseek-v4",
        "kimi-k2.6" => "kimi-k2-6",
        "kimi-k2.5" => "kimi-k2-5",
        "glm-5.1" => "glm-5-1",
        _ => model,
    }
}

// ---------------------------------------------------------------------------
// Protobuf wire encoder (varint + length-delimited fields)
// ---------------------------------------------------------------------------

/// Encode a protobuf varint.
fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let b = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            out.push(b | 0x80);
        } else {
            out.push(b);
            break;
        }
    }
}

/// Encode a length-delimited field: `tag = (field_num << 3) | 2`, then length,
/// then payload.
fn encode_field(field_num: u64, payload: &[u8], out: &mut Vec<u8>) {
    encode_varint((field_num << 3) | 2, out);
    encode_varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

fn encode_string(field_num: u64, value: &str, out: &mut Vec<u8>) {
    encode_field(field_num, value.as_bytes(), out);
}

fn encode_message(field_num: u64, msg: &[u8], out: &mut Vec<u8>) {
    encode_field(field_num, msg, out);
}

/// Build the `Metadata` protobuf message (field 1 apiKey, 2 ideName, 3
/// ideVersion, 4 extVersion, 5 sessionId, 6 locale).
fn build_metadata(api_key: &str, session_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_string(1, api_key, &mut out);
    encode_string(2, WS_IDE_NAME, &mut out);
    encode_string(3, WS_IDE_VERSION, &mut out);
    encode_string(4, WS_EXT_VERSION, &mut out);
    encode_string(5, session_id, &mut out);
    encode_string(6, WS_LOCALE, &mut out);
    out
}

/// Build the `ModelOrAlias` protobuf message (field 1 model).
fn build_model_or_alias(model: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_string(1, model, &mut out);
    out
}

/// Build a `ChatMessage` protobuf (field 1 role, 2 content, 3 toolCallId).
fn build_chat_message(role: &str, content: &str, tool_call_id: Option<&str>) -> Vec<u8> {
    let mut out = Vec::new();
    encode_string(1, role, &mut out);
    encode_string(2, content, &mut out);
    if let Some(tool_call_id) = tool_call_id {
        encode_string(3, tool_call_id, &mut out);
    }
    out
}

/// Build the `GetChatMessage` request protobuf body.
fn build_get_chat_message_request(api_key: &str, model: &str, messages: &[WsMessage]) -> Vec<u8> {
    let session_id = Uuid::new_v4().to_string();
    let cascade_id = Uuid::new_v4().to_string();

    let mut out = Vec::new();
    // field 1: metadata
    encode_message(1, &build_metadata(api_key, &session_id), &mut out);
    // field 2: cascade_id
    encode_string(2, &cascade_id, &mut out);
    // field 3: model_or_alias
    encode_message(3, &build_model_or_alias(model), &mut out);
    // repeated field 4: messages
    for msg in messages {
        encode_message(
            4,
            &build_chat_message(&msg.role, &msg.content, msg.tool_call_id.as_deref()),
            &mut out,
        );
    }
    out
}

/// Frame the payload as a gRPC-web message: `[0x00][4-byte BE length][payload]`.
fn grpc_web_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(0x00); // no compression
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

// ---------------------------------------------------------------------------
// Protobuf response decoder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum CompletionChunk {
    Content {
        text: String,
    },
    Done {
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    Error {
        message: String,
    },
    Unknown,
}

/// Read a varint at `offset`, returning `(value, new_offset)`.
fn read_varint(buf: &[u8], offset: &mut usize) -> u64 {
    let mut result: u64 = 0;
    let mut shift = 0;
    while *offset < buf.len() {
        let b = buf[*offset];
        *offset += 1;
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    result
}

/// Walk a message's fields; return the string payload of `target_field` (the
/// first length-delimited field with that number), else `None`.
fn decode_string_field(buf: &[u8], target_field: u64) -> Option<String> {
    let mut offset = 0;
    while offset < buf.len() {
        let tag = read_varint(buf, &mut offset);
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;
        match wire_type {
            0 => {
                read_varint(buf, &mut offset);
            }
            1 => {
                offset = offset.saturating_add(8).min(buf.len());
            }
            2 => {
                let len = read_varint(buf, &mut offset) as usize;
                let end = offset.saturating_add(len).min(buf.len());
                let payload = &buf[offset..end];
                offset = end;
                if field_num == target_field {
                    return String::from_utf8(payload.to_vec()).ok();
                }
            }
            5 => {
                offset = offset.saturating_add(4).min(buf.len());
            }
            _ => break,
        }
    }
    None
}

/// Decode a DoneChunk (field 1 = UsageStats{field1 prompt, field2 completion}).
fn decode_done_chunk(buf: &[u8]) -> (u64, u64) {
    // Find the nested UsageStats payload (field 1).
    let mut offset = 0;
    let mut usage: Option<&[u8]> = None;
    while offset < buf.len() {
        let tag = read_varint(buf, &mut offset);
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;
        match wire_type {
            0 => {
                read_varint(buf, &mut offset);
            }
            2 => {
                let len = read_varint(buf, &mut offset) as usize;
                let end = offset.saturating_add(len).min(buf.len());
                if field_num == 1 {
                    usage = Some(&buf[offset..end]);
                }
                offset = end;
            }
            _ => break,
        }
    }
    let Some(usage) = usage else { return (0, 0) };

    let mut prompt = 0;
    let mut completion = 0;
    offset = 0;
    while offset < usage.len() {
        let tag = read_varint(usage, &mut offset);
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;
        match wire_type {
            0 => {
                let v = read_varint(usage, &mut offset);
                if field_num == 1 {
                    prompt = v;
                } else if field_num == 2 {
                    completion = v;
                }
            }
            2 => {
                let len = read_varint(usage, &mut offset) as usize;
                offset = offset.saturating_add(len).min(usage.len());
            }
            _ => break,
        }
    }
    (prompt, completion)
}

/// Decode a `CompletionChunk` protobuf message.
fn decode_completion_chunk(buf: &[u8]) -> CompletionChunk {
    let mut offset = 0;
    while offset < buf.len() {
        let tag = read_varint(buf, &mut offset);
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;
        match wire_type {
            0 => {
                read_varint(buf, &mut offset);
            }
            1 => {
                offset = offset.saturating_add(8).min(buf.len());
            }
            2 => {
                let len = read_varint(buf, &mut offset) as usize;
                let end = offset.saturating_add(len).min(buf.len());
                let payload = &buf[offset..end];
                offset = end;
                match field_num {
                    1 => {
                        // ContentChunk { field 1: string text }
                        if let Some(text) = decode_string_field(payload, 1) {
                            return CompletionChunk::Content { text };
                        }
                    }
                    3 => {
                        // DoneChunk { field 1: UsageStats }
                        let (p, c) = decode_done_chunk(payload);
                        return CompletionChunk::Done {
                            prompt_tokens: p,
                            completion_tokens: c,
                        };
                    }
                    4 => {
                        // ErrorChunk { field 1: string message }
                        let msg = decode_string_field(payload, 1)
                            .unwrap_or_else(|| "unknown windsurf error".to_string());
                        return CompletionChunk::Error { message: msg };
                    }
                    // field 2 = ToolCallChunk — intentionally unhandled (skip)
                    _ => {}
                }
            }
            5 => {
                offset = offset.saturating_add(4).min(buf.len());
            }
            _ => break,
        }
    }
    CompletionChunk::Unknown
}

/// Percent-decode a `grpc-message` trailer value (mirrors JS
/// `decodeURIComponent`).
fn percent_decode(s: &str) -> String {
    // Minimal percent-decoder: %XX → byte (UTF-8 aware). Falls back to the
    // raw string on malformed input.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

// ---------------------------------------------------------------------------
// OpenAI messages → Windsurf wire
// ---------------------------------------------------------------------------

/// A single message ready for the protobuf wire.
struct WsMessage {
    role: String,
    content: String,
    tool_call_id: Option<String>,
}

/// Convert OpenAI-format messages (string or array content) to windsurf wire
/// messages. Mirrors JS `openAIMessagesToWs` (windsurf.js:352-369).
fn openai_messages_to_ws(messages: &[Value]) -> Vec<WsMessage> {
    let mut out = Vec::new();
    for m in messages {
        let role = m
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        let mut content = String::new();
        match m.get("content") {
            Some(Value::String(s)) => content = s.clone(),
            Some(Value::Array(parts)) => {
                for part in parts {
                    if part.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            content.push_str(text);
                        }
                    }
                }
            }
            _ => {}
        }
        let tool_call_id = m
            .get("tool_call_id")
            .and_then(Value::as_str)
            .map(String::from);
        out.push(WsMessage {
            role,
            content,
            tool_call_id,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// SSE emission
// ---------------------------------------------------------------------------

/// Format one OpenAI SSE chunk.
fn sse_chunk(
    cid: &str,
    created: i64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
            }],
        }))
        .unwrap_or_default()
    )
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WindsurfExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug)]
pub enum WindsurfExecutorError {
    MissingCredentials(String),
    Serialize(serde_json::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for WindsurfExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for WindsurfExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<serde_json::Error> for WindsurfExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl std::fmt::Display for WindsurfExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::InvalidHeader(e) => write!(f, "Invalid header: {}", e),
        }
    }
}

impl std::error::Error for WindsurfExecutorError {}

pub struct WindsurfExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for WindsurfExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindsurfExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

pub struct WindsurfExecutor {
    pool: Arc<ClientPool>,
}

impl WindsurfExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    fn build_url(&self) -> String {
        WS_CHAT_URL.to_string()
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, WindsurfExecutorError> {
        let token = credentials
            .access_token
            .as_deref()
            .or(credentials.api_key.as_deref())
            .unwrap_or("");
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/grpc-web+proto"),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/grpc-web+proto"),
        );
        if !token.is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_str(&format!("windsurf/{WS_IDE_VERSION}"))?,
        );
        headers.insert("X-Grpc-Web", HeaderValue::from_static("1"));
        Ok(headers)
    }

    pub async fn execute_request(
        &self,
        request: WindsurfExecutionRequest,
    ) -> Result<WindsurfExecutorResponse, WindsurfExecutorError> {
        let api_key = request
            .credentials
            .access_token
            .as_deref()
            .or(request.credentials.api_key.as_deref())
            .unwrap_or("");
        if api_key.is_empty() {
            return Err(WindsurfExecutorError::MissingCredentials(
                "windsurf".to_string(),
            ));
        }
        let ws_model = resolve_ws_model_id(&request.model).to_string();

        let raw_messages = request
            .body
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut ws_messages = openai_messages_to_ws(&raw_messages);
        if ws_messages.is_empty() {
            ws_messages.push(WsMessage {
                role: "user".into(),
                content: String::new(),
                tool_call_id: None,
            });
        }

        let proto_payload = build_get_chat_message_request(&api_key, &ws_model, &ws_messages);
        let framed_payload = grpc_web_frame(&proto_payload);
        let url = self.build_url();
        let headers = self.build_headers(&request.credentials)?;

        let client = self.pool.get("windsurf", request.proxy.as_ref())?;
        let upstream = client
            .post(&url)
            .headers(headers.clone())
            .body(framed_payload)
            .send()
            .await?;

        let status = upstream.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let bytes = upstream.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&bytes).to_string();
            let error_resp = json_error(status_code, &body_str);
            return Ok(WindsurfExecutorResponse {
                response: error_resp,
                url,
                headers,
                transformed_body: request.body.clone(),
                transport: TransportKind::Reqwest,
            });
        }

        // Collect the full gRPC-web binary stream and convert to OpenAI SSE.
        let bytes = upstream.bytes().await.unwrap_or_default();
        let sse = transform_to_sse(&bytes, &request.model);
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
        let response = UpstreamResponse::Reqwest(reqwest::Response::from(http_resp));

        Ok(WindsurfExecutorResponse {
            response,
            url,
            headers,
            transformed_body: request.body.clone(),
            transport: TransportKind::Reqwest,
        })
    }
}

/// Convert a full gRPC-web response body into OpenAI-compatible SSE text.
/// Mirrors JS `transformToSSE` (windsurf.js:435-580).
fn transform_to_sse(body: &[u8], model: &str) -> String {
    let response_id = format!("chatcmpl-ws-{}", unix_now());
    let created = unix_now();
    let mut sse = String::new();
    let mut role_emitted = false;
    let mut total_text = String::new();
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;
    let mut had_error: Option<String> = None;

    // Drain gRPC-web frames.
    let mut offset = 0;
    while offset + 5 <= body.len() {
        let flag = body[offset];
        let len = u32::from_be_bytes([
            body[offset + 1],
            body[offset + 2],
            body[offset + 3],
            body[offset + 4],
        ]) as usize;
        if offset + 5 + len > body.len() {
            break;
        }
        let payload = &body[offset + 5..offset + 5 + len];
        offset += 5 + len;

        if flag == 0x80 {
            // Trailer frame — grpc-status / grpc-message.
            let trailer = String::from_utf8_lossy(payload);
            if let Some(status_match) = extract_trailer(&trailer, "grpc-status") {
                if status_match != "0" {
                    let msg = extract_trailer(&trailer, "grpc-message")
                        .map(|m| percent_decode(m.trim()))
                        .unwrap_or_else(|| format!("gRPC status {status_match}"));
                    had_error = Some(msg);
                }
            }
            continue;
        }
        if flag != 0x00 {
            continue; // skip unknown flags
        }

        match decode_completion_chunk(payload) {
            CompletionChunk::Content { text } if !text.is_empty() => {
                total_text.push_str(&text);
                if !role_emitted {
                    sse.push_str(&sse_chunk(
                        &response_id,
                        created,
                        model,
                        json!({ "role": "assistant", "content": "" }),
                        None,
                    ));
                    role_emitted = true;
                }
                sse.push_str(&sse_chunk(
                    &response_id,
                    created,
                    model,
                    json!({ "content": text }),
                    None,
                ));
            }
            CompletionChunk::Done {
                prompt_tokens: p,
                completion_tokens: c,
            } => {
                prompt_tokens = p;
                completion_tokens = c;
            }
            CompletionChunk::Error { message } => {
                had_error = Some(message);
            }
            _ => {}
        }
    }

    if let Some(err) = had_error {
        sse.push_str(&format!(
            "data: {}\n\n",
            serde_json::to_string(&json!({
                "error": { "message": err, "type": "windsurf_error", "code": "upstream_error" },
            }))
            .unwrap_or_default()
        ));
        sse.push_str("data: [DONE]\n\n");
        return sse;
    }

    // Unary fallback: nothing streamed but text decoded → emit as one chunk.
    if !role_emitted && !total_text.is_empty() {
        sse.push_str(&sse_chunk(
            &response_id,
            created,
            model,
            json!({ "role": "assistant", "content": "" }),
            None,
        ));
        sse.push_str(&sse_chunk(
            &response_id,
            created,
            model,
            json!({ "content": total_text }),
            None,
        ));
    }

    // Finish chunk + optional usage + [DONE].
    let mut finish = json!({
        "id": response_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
    });
    if prompt_tokens > 0 || completion_tokens > 0 {
        finish["usage"] = json!({
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        });
    }
    sse.push_str(&format!(
        "data: {}\n\n",
        serde_json::to_string(&finish).unwrap_or_default()
    ));
    sse.push_str("data: [DONE]\n\n");
    sse
}

/// Extract a `key: value` trailer line (case-insensitive key).
fn extract_trailer<'a>(trailer: &'a str, key: &str) -> Option<&'a str> {
    for line in trailer.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(key) {
                return Some(v.trim());
            }
        }
    }
    None
}

/// Build a JSON error response body (non-2xx upstream).
fn json_error(status: u16, message: &str) -> UpstreamResponse {
    let body = json!({
        "error": {
            "message": format!("Windsurf upstream returned {status}: {message}"),
            "type": "windsurf_error",
            "code": "upstream_error",
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_model_alias() {
        assert_eq!(resolve_ws_model_id("gpt-5.5"), "gpt-5-5-medium");
        assert_eq!(
            resolve_ws_model_id("claude-opus-4.7-high"),
            "claude-opus-4-7-high"
        );
        assert_eq!(resolve_ws_model_id("unknown-model"), "unknown-model");
    }

    #[test]
    fn test_ws_model_alias_more() {
        assert_eq!(resolve_ws_model_id("swe-1.6-fast"), "swe-1-6-fast");
        assert_eq!(
            resolve_ws_model_id("claude-opus-4.5"),
            "MODEL_CLAUDE_4_5_OPUS"
        );
        assert_eq!(resolve_ws_model_id("gemini-3.1-pro"), "gemini-3-1-pro-high");
        assert_eq!(resolve_ws_model_id("gpt-5.4"), "gpt-5-4-medium");
        assert_eq!(resolve_ws_model_id("gpt-5.2"), "MODEL_GPT_5_2_MEDIUM");
        assert_eq!(
            resolve_ws_model_id("gpt-4o"),
            "MODEL_CHAT_GPT_4O_2024_08_06"
        );
        assert_eq!(resolve_ws_model_id("glm-5.1"), "glm-5-1");
    }

    #[test]
    fn varint_round_trip() {
        let mut out = Vec::new();
        encode_varint(0, &mut out);
        assert_eq!(out, vec![0]);

        out.clear();
        encode_varint(300, &mut out);
        assert_eq!(out, vec![0xAC, 0x02]);

        out.clear();
        encode_varint(150, &mut out);
        assert_eq!(out, vec![0x96, 0x01]);
    }

    #[test]
    fn grpc_frame_layout() {
        let payload = b"hello";
        let frame = grpc_web_frame(payload);
        assert_eq!(frame.len(), 10);
        assert_eq!(frame[0], 0x00); // no compression
                                    // big-endian length
        assert_eq!(&frame[1..5], &[0, 0, 0, 5]);
        assert_eq!(&frame[5..], payload);
    }

    #[test]
    fn decode_completion_content_chunk() {
        // ContentChunk: field 1 = ContentChunk, field 1 = string text.
        let mut buf = Vec::new();
        let text = b"Hello world";
        // inner ContentChunk { field 1: string }
        let mut inner = Vec::new();
        encode_string(1, "Hello world", &mut inner);
        encode_message(1, &inner, &mut buf);
        assert_eq!(
            decode_completion_chunk(&buf),
            CompletionChunk::Content {
                text: text
                    .to_vec()
                    .into_iter()
                    .map(|b| b as char)
                    .collect::<String>()
            }
        );
    }

    #[test]
    fn decode_completion_done_chunk() {
        // DoneChunk: field 3 = DoneChunk { field 1: UsageStats { field1: prompt, field2: completion } }
        let mut usage = Vec::new();
        encode_varint((1 << 3) | 0, &mut usage); // field 1 varint
        encode_varint(12, &mut usage); // prompt_tokens = 12
        encode_varint((2 << 3) | 0, &mut usage); // field 2 varint
        encode_varint(34, &mut usage); // completion_tokens = 34

        let mut done = Vec::new();
        encode_message(1, &usage, &mut done);

        let mut buf = Vec::new();
        encode_message(3, &done, &mut buf);
        assert_eq!(
            decode_completion_chunk(&buf),
            CompletionChunk::Done {
                prompt_tokens: 12,
                completion_tokens: 34
            }
        );
    }

    #[test]
    fn decode_completion_error_chunk() {
        // ErrorChunk: field 4 = ErrorChunk { field 1: string message }
        let mut inner = Vec::new();
        encode_string(1, "boom", &mut inner);
        let mut buf = Vec::new();
        encode_message(4, &inner, &mut buf);
        assert_eq!(
            decode_completion_chunk(&buf),
            CompletionChunk::Error {
                message: "boom".into()
            }
        );
    }

    #[test]
    fn trailer_parse() {
        let trailer = "grpc-status: 14\r\ngrpc-message: upstream%20connect%20error\r\n";
        assert_eq!(extract_trailer(trailer, "grpc-status"), Some("14"));
        assert_eq!(
            extract_trailer(trailer, "grpc-message"),
            Some("upstream%20connect%20error")
        );
        assert_eq!(
            percent_decode("upstream%20connect%20error"),
            "upstream connect error"
        );
    }

    #[test]
    fn percent_decode_handles_utf8() {
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn transform_sse_emits_role_content_done() {
        // Build a synthetic gRPC-web body: one ContentChunk + a trailer.
        let mut content_chunk = Vec::new();
        let mut inner = Vec::new();
        encode_string(1, "Hi there", &mut inner);
        encode_message(1, &inner, &mut content_chunk);

        let mut body = grpc_web_frame(&content_chunk);
        // Append a trailer frame with grpc-status: 0 (success).
        let trailer = b"grpc-status: 0\r\n";
        let mut trailer_frame = Vec::new();
        trailer_frame.push(0x80);
        trailer_frame.extend_from_slice(&(trailer.len() as u32).to_be_bytes());
        trailer_frame.extend_from_slice(trailer);
        body.extend_from_slice(&trailer_frame);

        let sse = transform_to_sse(&body, "gpt-5.5");
        assert!(sse.contains("\"role\":\"assistant\""));
        assert!(sse.contains("Hi there"));
        assert!(sse.contains("\"finish_reason\":\"stop\""));
        assert!(sse.contains("data: [DONE]"));
    }
}
