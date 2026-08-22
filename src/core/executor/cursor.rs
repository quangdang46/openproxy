use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use uuid::Uuid;

use bytes::Bytes;
use http_body_util::Full;

use crate::core::proxy::ProxyTarget;
use crate::core::utils::cursor_checksum;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

/// 9router registry default baseUrl + chatPath.
const CURSOR_API_ENDPOINT: &str =
    "https://api2.cursor.sh/aiserver.v1.ChatService/StreamUnifiedChatWithTools";
/// Non-privacy agent endpoint (settings/override).
const CURSOR_AGENTN_ENDPOINT: &str =
    "https://agentn.api5.cursor.sh/aiserver.v1.ChatService/StreamUnifiedChatWithTools";

// ==================== COMPRESSION FLAGS ====================

const COMPRESS_FLAG_NONE: u8 = 0x00;
const COMPRESS_FLAG_GZIP: u8 = 0x01;
const COMPRESS_FLAG_TRAILER: u8 = 0x02;
const COMPRESS_FLAG_GZIP_TRAILER: u8 = 0x03;

// ==================== SSE CONSTANTS ====================

const SSE_DONE: &str = "data: [DONE]\n\n";

// ==================== TYPES ====================

#[derive(Clone)]
#[allow(dead_code)]
pub struct CursorExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

pub struct CursorExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct CursorExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for CursorExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

#[derive(Debug)]
pub enum CursorExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    InvalidUri(InvalidUri),
    InvalidRequest(http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    EventStreamDecode(String),
    UnsupportedFormat(String),
    ProtobufEncode(String),
    ProtobufDecode(String),
    ChecksumError(String),
    StreamError(String),
}

impl From<reqwest::Error> for CursorExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for CursorExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for CursorExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<http::Error> for CursorExecutorError {
    fn from(error: http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for CursorExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for CursorExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for CursorExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

// ==================== PROTOBUF FIELD NUMBERS ====================

mod proto_fields {
    pub const WIRE_VARINT: u8 = 0;
    pub const WIRE_FIXED64: u8 = 1;
    pub const WIRE_LEN: u8 = 2;
    pub const WIRE_FIXED32: u8 = 5;

    // Role
    pub const ROLE_USER: u32 = 1;
    pub const ROLE_ASSISTANT: u32 = 2;

    // Unified mode
    pub const UNIFIED_MODE_CHAT: u32 = 1;
    pub const UNIFIED_MODE_AGENT: u32 = 2;

    // Thinking level
    pub const THINKING_UNSPECIFIED: u32 = 0;
    pub const THINKING_MEDIUM: u32 = 1;
    pub const THINKING_HIGH: u32 = 2;

    // ClientSideToolV2
    pub const CLIENT_SIDE_TOOL_V2_MCP: u32 = 19;

    // StreamUnifiedChatRequestWithTools (top level)
    pub const FLD_REQUEST: u32 = 1;

    // StreamUnifiedChatRequest
    pub const FLD_MESSAGES: u32 = 1;
    pub const FLD_UNKNOWN_2: u32 = 2;
    pub const FLD_INSTRUCTION: u32 = 3;
    pub const FLD_UNKNOWN_4: u32 = 4;
    pub const FLD_MODEL: u32 = 5;
    pub const FLD_WEB_TOOL: u32 = 8;
    pub const FLD_UNKNOWN_13: u32 = 13;
    pub const FLD_CURSOR_SETTING: u32 = 15;
    pub const FLD_UNKNOWN_19: u32 = 19;
    pub const FLD_CONVERSATION_ID: u32 = 23;
    pub const FLD_METADATA: u32 = 26;
    pub const FLD_IS_AGENTIC: u32 = 27;
    pub const FLD_SUPPORTED_TOOLS: u32 = 29;
    pub const FLD_MESSAGE_IDS: u32 = 30;
    pub const FLD_MCP_TOOLS: u32 = 34;
    pub const FLD_LARGE_CONTEXT: u32 = 35;
    pub const FLD_UNKNOWN_38: u32 = 38;
    pub const FLD_UNIFIED_MODE: u32 = 46;
    pub const FLD_UNKNOWN_47: u32 = 47;
    pub const FLD_SHOULD_DISABLE_TOOLS: u32 = 48;
    pub const FLD_THINKING_LEVEL: u32 = 49;
    pub const FLD_UNKNOWN_51: u32 = 51;
    pub const FLD_UNKNOWN_53: u32 = 53;
    pub const FLD_UNIFIED_MODE_NAME: u32 = 54;

    // ConversationMessage
    pub const FLD_MSG_CONTENT: u32 = 1;
    pub const FLD_MSG_ROLE: u32 = 2;
    pub const FLD_MSG_ID: u32 = 13;
    pub const FLD_MSG_TOOL_RESULTS: u32 = 18;
    pub const FLD_MSG_IS_AGENTIC: u32 = 29;
    pub const FLD_MSG_SERVER_BUBBLE_ID: u32 = 32;
    pub const FLD_MSG_UNIFIED_MODE: u32 = 47;
    pub const FLD_MSG_SUPPORTED_TOOLS: u32 = 51;

    // Tool result fields
    pub const FLD_TOOL_RESULT_CALL_ID: u32 = 1;
    pub const FLD_TOOL_RESULT_NAME: u32 = 2;
    pub const FLD_TOOL_RESULT_INDEX: u32 = 3;
    pub const FLD_TOOL_RESULT_RAW_ARGS: u32 = 5;
    pub const FLD_TOOL_RESULT_RESULT: u32 = 8;
    pub const FLD_TOOL_RESULT_TOOL_CALL: u32 = 11;
    pub const FLD_TOOL_RESULT_MODEL_CALL_ID: u32 = 12;

    // ClientSideToolV2Result
    pub const FLD_CV2R_TOOL: u32 = 1;
    pub const FLD_CV2R_MCP_RESULT: u32 = 28;
    pub const FLD_CV2R_CALL_ID: u32 = 35;
    pub const FLD_CV2R_MODEL_CALL_ID: u32 = 48;
    pub const FLD_CV2R_TOOL_INDEX: u32 = 49;

    // MCPResult
    pub const FLD_MCPR_SELECTED_TOOL: u32 = 1;
    pub const FLD_MCPR_RESULT: u32 = 2;

    // ClientSideToolV2Call
    pub const FLD_CV2C_TOOL: u32 = 1;
    pub const FLD_CV2C_MCP_PARAMS: u32 = 27;
    pub const FLD_CV2C_CALL_ID: u32 = 3;
    pub const FLD_CV2C_NAME: u32 = 9;
    pub const FLD_CV2C_RAW_ARGS: u32 = 10;
    pub const FLD_CV2C_TOOL_INDEX: u32 = 48;
    pub const FLD_CV2C_MODEL_CALL_ID: u32 = 49;

    // Model
    pub const FLD_MODEL_NAME: u32 = 1;
    pub const FLD_MODEL_EMPTY: u32 = 4;

    // Instruction
    pub const FLD_INSTRUCTION_TEXT: u32 = 1;

    // CursorSetting
    pub const FLD_SETTING_PATH: u32 = 1;
    pub const FLD_SETTING_UNKNOWN_3: u32 = 3;
    pub const FLD_SETTING_UNKNOWN_6: u32 = 6;
    pub const FLD_SETTING_UNKNOWN_8: u32 = 8;
    pub const FLD_SETTING_UNKNOWN_9: u32 = 9;

    // CursorSetting.Unknown6
    pub const FLD_SETTING6_FIELD_1: u32 = 1;
    pub const FLD_SETTING6_FIELD_2: u32 = 2;

    // Metadata
    pub const FLD_META_PLATFORM: u32 = 1;
    pub const FLD_META_ARCH: u32 = 2;
    pub const FLD_META_VERSION: u32 = 3;
    pub const FLD_META_CWD: u32 = 4;
    pub const FLD_META_TIMESTAMP: u32 = 5;

    // MessageId
    pub const FLD_MSGID_ID: u32 = 1;
    pub const FLD_MSGID_SUMMARY: u32 = 2;
    pub const FLD_MSGID_ROLE: u32 = 3;

    // MCPTool
    pub const FLD_MCP_TOOL_NAME: u32 = 1;
    pub const FLD_MCP_TOOL_DESC: u32 = 2;
    pub const FLD_MCP_TOOL_PARAMS: u32 = 3;
    pub const FLD_MCP_TOOL_SERVER: u32 = 4;

    // StreamUnifiedChatResponseWithTools (response)
    pub const FLD_TOOL_CALL: u32 = 1;
    pub const FLD_RESPONSE: u32 = 2;

    // ClientSideToolV2Call
    pub const FLD_TOOL_ID: u32 = 3;
    pub const FLD_TOOL_NAME: u32 = 9;
    pub const FLD_TOOL_RAW_ARGS: u32 = 10;
    pub const FLD_TOOL_IS_LAST: u32 = 11;
    pub const FLD_TOOL_IS_LAST_ALT: u32 = 15;
    pub const FLD_TOOL_MCP_PARAMS: u32 = 27;

    // MCPParams
    pub const FLD_MCP_TOOLS_LIST: u32 = 1;

    // MCPParams.Tool (nested)
    pub const FLD_MCP_NESTED_NAME: u32 = 1;
    pub const FLD_MCP_NESTED_PARAMS: u32 = 3;

    // StreamUnifiedChatResponse
    pub const FLD_RESPONSE_TEXT: u32 = 1;
    pub const FLD_THINKING: u32 = 25;

    // Thinking
    pub const FLD_THINKING_TEXT: u32 = 1;
}

// ==================== PROTOBUF ENCODING ====================

/// Encode a varint value
fn encode_varint(mut value: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    while value >= 0x80 {
        bytes.push((value as u8 & 0x7F) | 0x80);
        value >>= 7;
    }
    bytes.push(value as u8 & 0x7F);
    bytes
}

/// Encode a protobuf field tag
fn encode_tag(field_number: u32, wire_type: u8) -> Vec<u8> {
    let tag = (field_number << 3) | (wire_type as u32);
    encode_varint(tag)
}

/// Encode a length-delimited field
fn encode_field_len(field_num: u32, wire_type: u8, data: &[u8]) -> Vec<u8> {
    let mut result = encode_tag(field_num, wire_type);
    result.extend_from_slice(&encode_varint(data.len() as u32));
    result.extend_from_slice(data);
    result
}

/// Encode a varint field
fn encode_field_varint(field_num: u32, wire_type: u8, value: u32) -> Vec<u8> {
    let mut result = encode_tag(field_num, wire_type);
    result.extend_from_slice(&encode_varint(value));
    result
}

/// Concatenate multiple byte arrays
fn concat_arrays(arrays: &[&[u8]]) -> Vec<u8> {
    let total_len: usize = arrays.iter().map(|a| a.len()).sum();
    let mut result = Vec::with_capacity(total_len);
    for arr in arrays {
        result.extend_from_slice(arr);
    }
    result
}

// ==================== AGENT SERVICE (9router cursor.js executeAgent) ====================
//
// Cursor's AgentService (agent.api5.cursor.sh/agent.v1.AgentService/Run) is an
// HTTP/2-only Connect-RPC endpoint. This increment ports the request-side
// pieces: the `isAgentTextRequest` routing predicate and the protobuf
// `AgentRun` frame builder (with the guard-testable pure logic). The HTTP/2
// duplex transport is a follow-up (see beads pnc34).

/// `agent.v1.AgentService/Run` path (9router cursor.js AGENT_RUN_PATH).
const AGENT_RUN_PATH: &str = "/agent.v1.AgentService/Run";
/// AgentService base URL (9router registry cursor.js agentEndpoint).
const AGENT_BASE_URL: &str = "https://agent.api5.cursor.sh";

/// Current unix time in milliseconds (9router Date.now()).
fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Current unix time in seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// Connect-RPC request frame flags: bit 0 = gzip.
const CONNECT_RPC_FLAG_COMPRESSED: u8 = 0x01;
/// Connect-RPC trailer flag: end-of-stream marker frame (JSON).
const CONNECT_RPC_FLAG_TRAILER: u8 = 0x02;

/// 9router `isAgentTextRequest`: true when every message is text-only — no
/// `tool_calls`, no `role:"tool"`, and content is a string or an array of
/// text-only parts. Tool *schemas* on the request are ignored (AgentService
/// answers text turns fine); a real tool-call/result conversation stays on the
/// legacy ChatService path.
fn is_agent_text_request(body: &Value) -> bool {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return false;
    };
    messages.iter().all(|message| {
        if message
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|c| !c.is_empty())
        {
            return false;
        }
        if message.get("role").and_then(Value::as_str) == Some("tool") {
            return false;
        }
        match message.get("content") {
            Some(Value::String(_)) => true,
            Some(Value::Array(parts)) => parts
                .iter()
                .all(|p| p.get("type").and_then(Value::as_str) == Some("text")),
            _ => false,
        }
    })
}

/// 9router `textFromContent`: string content, or array of `{type:"text"}` parts.
fn text_from_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Encode a protobuf length-delimited field (wire type 2).
fn pb_len(field: u32, data: &[u8]) -> Vec<u8> {
    encode_field_len(field, 2, data)
}

/// Encode a protobuf varint field (wire type 0).
fn pb_varint(field: u32, value: u32) -> Vec<u8> {
    encode_field_varint(field, 0, value)
}

/// 9router `encodeHistoryMessage`: a conversation-history entry.
/// ConversationHistoryMessage.user / .assistant → repeated content → text.
fn encode_history_message(message: &Value) -> Option<Vec<u8>> {
    let content = text_from_content(message.get("content")?);
    if content.is_empty() {
        return None;
    }
    let text = pb_len(1, content.as_bytes());
    let inner = pb_len(1, &text);
    let wrapper = if message.get("role").and_then(Value::as_str) == Some("assistant") {
        pb_len(2, &inner)
    } else {
        pb_len(1, &inner)
    };
    Some(wrapper)
}

/// 9router `buildAgentRunFrame(messages, model)`: encode the
/// `agent.v1.AgentClientMessage.run_request` Connect-RPC frame.
pub fn build_agent_run_frame(messages: &[Value], model: &str) -> Vec<u8> {
    let system: String = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("system"))
        .map(|m| text_from_content(m.get("content").unwrap_or(&Value::Null)))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    let chat_messages: Vec<&Value> = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) != Some("system"))
        .collect();

    let current_index = chat_messages
        .iter()
        .rposition(|m| m.get("role").and_then(Value::as_str) == Some("user"));
    let current = match current_index {
        Some(i) => chat_messages[i],
        None => chat_messages.last().copied().unwrap_or(&Value::Null),
    };

    let history_end = current_index.unwrap_or(chat_messages.len().saturating_sub(1));
    let history: Vec<Vec<u8>> = chat_messages
        .iter()
        .take(history_end)
        .filter_map(|m| encode_history_message(m))
        .collect();

    let user_text = {
        let t = text_from_content(current.get("content").unwrap_or(&Value::Null));
        if t.is_empty() {
            "Continue.".to_string()
        } else {
            t
        }
    };

    // agent.v1.UserMessageAction.user_message (+ optional history).
    let user_message = concat_arrays(&[&pb_len(1, user_text.as_bytes()), &pb_len(2, &uuid_uuid())]);
    let conversation_history = if history.is_empty() {
        Vec::new()
    } else {
        let mut buf = Vec::new();
        for entry in &history {
            buf.extend_from_slice(&pb_len(1, entry));
        }
        buf
    };
    let mut user_action = pb_len(1, &user_message);
    if !conversation_history.is_empty() {
        user_action.extend_from_slice(&pb_len(7, &conversation_history));
    }
    let conversation_action = pb_len(1, &user_action);

    // requestedModel: field 1 model, field 7 bool true (9router agentBool(7,true)).
    let requested_model = concat_arrays(&[&pb_len(1, model.as_bytes()), &pb_varint(7, 1)]);

    let mut run_request = Vec::new();
    run_request.extend_from_slice(&pb_len(1, &[])); // empty ConversationStateStructure
    run_request.extend_from_slice(&pb_len(2, &conversation_action));
    if !system.is_empty() {
        run_request.extend_from_slice(&pb_len(8, system.as_bytes()));
    }
    run_request.extend_from_slice(&pb_len(9, &requested_model));

    // agent.v1.AgentClientMessage.run_request → Connect-RPC frame.
    // Cursor does not support compressed requests, so compress=false.
    wrap_connect_rpc_frame(&pb_len(1, &run_request), false)
}

/// Random UUID for the agent user-message action (9router crypto.randomUUID).
fn uuid_uuid() -> Vec<u8> {
    uuid::Uuid::new_v4().as_bytes().to_vec()
}

/// 9router `extractAgentString`: first occurrence of a length-delimited field.
fn extract_agent_string(message: &[u8], field: u32) -> String {
    match decode_message(message) {
        Ok(fields) => fields
            .get(&field)
            .and_then(|values| values.first())
            .map(|v| String::from_utf8_lossy(v).to_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// 9router `createRequestContextResponse`: an empty RequestContext result
/// wrapped in `execClientMessage` field 10, then a Connect-RPC frame.
/// AgentService asks every run for client context; 9router has no IDE file
/// context, so it acknowledges with an empty RequestContext (cursor.js:160-167).
fn create_request_context_response() -> Vec<u8> {
    let request_context_success = pb_len(1, &[]);
    let request_context_result = pb_len(1, &request_context_success);
    let exec_client_message = pb_len(10, &request_context_result);
    wrap_connect_rpc_frame(&pb_len(2, &exec_client_message), false)
}

/// An event emitted by the agent-stream consumer (9router executeAgent
/// `onEvent`). Reasoning (`thinking`) stays upstream-only — it is never
/// forwarded to clients, matching the reference.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    Text(String),
    /// The server asked for client context — respond with an empty
    /// RequestContext on the request half-stream (9router cursor.js:571-583).
    RequestContext,
    Done,
    /// Upstream error. The bool marks `resource_exhausted` (gRPC code 8) —
    /// JS createErrorResponse maps it to HTTP 429 rate_limit_error so account
    /// rotation fires; everything else is a 400 api_error.
    Error(String, bool),
}

/// Decompress an agent Connect-RPC frame payload when the gzip flag is set
/// (9router decodeAgentFrames cursor.js:152-154). Trailer frames carry JSON
/// (errors/metadata) and are surfaced as `Err` instead.
fn decode_agent_frame_payload(flags: u8, payload: &[u8]) -> Result<Vec<u8>, String> {
    if flags & CONNECT_RPC_FLAG_COMPRESSED != 0 {
        let mut decoder = GzDecoder::new(payload);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|e| format!("agent frame gzip: {e}"))?;
        Ok(out)
    } else {
        Ok(payload.to_vec())
    }
}

/// Incremental agent frame decoder: parse complete Connect-RPC frames from a
/// byte buffer. Returns the leftover partial bytes. Emits an event per frame
/// (9router decodeAgentFrames + the executeAgent per-frame handler).
fn consume_agent_frames(
    buffer: &[u8],
    finished: &mut bool,
    events: &mut Vec<AgentEvent>,
) -> Vec<u8> {
    let mut pending = buffer.to_vec();
    while pending.len() >= 5 {
        let flags = pending[0];
        let length = u32::from_be_bytes([pending[1], pending[2], pending[3], pending[4]]) as usize;
        if pending.len() < 5 + length {
            break;
        }
        let payload = pending[5..5 + length].to_vec();
        pending = pending[5 + length..].to_vec();
        if flags & CONNECT_RPC_FLAG_TRAILER != 0 {
            // End-of-stream marker; error JSON rides here.
            if let Ok(text) = std::str::from_utf8(&payload) {
                if let Ok(value) = serde_json::from_str::<Value>(text) {
                    if value.get("error").is_some() {
                        // JS createErrorResponse (cursor.js:257-276):
                        // error.code == "resource_exhausted" → 429
                        // rate_limit_error; otherwise 400 api_error.
                        let is_rate_limit = value
                            .pointer("/error/code")
                            .and_then(Value::as_str)
                            .is_some_and(|c| c == "resource_exhausted");
                        let msg = value
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("API Error")
                            .to_string();
                        *finished = true;
                        events.push(AgentEvent::Error(msg, is_rate_limit));
                        continue;
                    }
                }
            }
            continue;
        }

        let Ok(decoded) = decode_agent_frame_payload(flags, &payload) else {
            continue;
        };
        let Ok(server) = decode_message(&decoded) else {
            continue;
        };

        // agent.v1.AgentServerMessage.interaction_update (field 1).
        if let Some(updates) = server.get(&1) {
            for update in updates {
                let Ok(update_fields) = decode_message(update) else {
                    continue;
                };
                // Field 1 → text delta.
                if let Some(texts) = update_fields.get(&1) {
                    for t in texts {
                        let delta = extract_agent_string(t, 1);
                        if !delta.is_empty() {
                            events.push(AgentEvent::Text(delta));
                        }
                    }
                }
                // Field 14 → internal reasoning; upstream-only, end of turn.
                if update_fields.contains_key(&14) {
                    *finished = true;
                    events.push(AgentEvent::Done);
                }
            }
        }

        // agent.v1.AgentServerMessage.exec_server_message (field 2).
        if let Some(execs) = server.get(&2) {
            for exec in execs {
                let Ok(exec_fields) = decode_message(exec) else {
                    continue;
                };
                if exec_fields.contains_key(&10) {
                    // RequestContext → acknowledge with an empty response so the
                    // upstream does not stall (9router cursor.js:571-583).
                    events.push(AgentEvent::RequestContext);
                } else {
                    // Every other ExecServerMessage variant is an editor-backed
                    // tool (shell, read, write, …) that 9router cannot service.
                    *finished = true;
                    events.push(AgentEvent::Error(
                        "Cursor AgentService requested an unsupported IDE tool".to_string(),
                        false,
                    ));
                }
            }
        }
    }
    pending
}

/// Consume the agent response body stream, dispatching decoded events. The
/// request half-stream is kept open (for the request-context handshake) until
/// the turn ends; a half-closed request stream before the server streams
/// yields "No exec result" (see 9router openAgentHttp2Stream + shunt/agent.rs).
/// Mirrors 9router executeAgent `consume` (cursor.js:539-591).
async fn consume_agent_stream(
    mut incoming: Incoming,
    response_id: &str,
    created: u64,
    model: &str,
    stream: bool,
) -> Result<Vec<u8>, CursorExecutorError> {
    let mut pending = Vec::new();
    let mut finished = false;
    let mut content = String::new();
    let mut chunks: Vec<String> = Vec::new();
    // Request-context handshake payloads the consumer must write back on the
    // request half-stream. Written immediately (per-frame), keeping the stream
    // open — matching the reference's session.write(createRequestContextResponse()).
    let mut context_responses: Vec<Vec<u8>> = Vec::new();

    while !finished {
        let frame = incoming
            .frame()
            .await
            .transpose()
            .map_err(|e| CursorExecutorError::StreamError(format!("agent frame: {e}")))?;
        match frame {
            Some(frame) => {
                let bytes = frame
                    .into_data()
                    .map_err(|e| {
                        CursorExecutorError::StreamError(format!("agent frame data: {e:?}"))
                    })?
                    .to_vec();
                pending.extend_from_slice(&bytes);
                let mut events = Vec::new();
                pending = consume_agent_frames(&pending, &mut finished, &mut events);
                for event in events {
                    match event {
                        AgentEvent::Text(delta) => {
                            content.push_str(&delta);
                            if stream {
                                let sse = format_chat_chunk_sse(
                                    response_id,
                                    created,
                                    model,
                                    json!({"content": delta}),
                                    None,
                                );
                                chunks.push(sse);
                            }
                        }
                        AgentEvent::RequestContext => {
                            context_responses.push(create_request_context_response());
                        }
                        AgentEvent::Done => {}
                        AgentEvent::Error(msg, is_rate_limit) => {
                            // JS createErrorResponse: resource_exhausted → 429
                            // rate_limit_error so account rotation fires.
                            chunks.push(format!(
                                "data: {}\n\n",
                                json!({"error": {
                                    "message": msg,
                                    "type": if is_rate_limit { "rate_limit_error" } else { "api_error" },
                                    "code": if is_rate_limit { "resource_exhausted" } else { "unknown" },
                                }})
                            ));
                            chunks.push(SSE_DONE.to_string());
                            return Ok(chunks.concat().into_bytes());
                        }
                    }
                }
            }
            None => break,
        }
    }

    if stream {
        chunks.push(format_chat_chunk_sse(
            response_id,
            created,
            model,
            json!({}),
            Some("stop"),
        ));
        chunks.push(SSE_DONE.to_string());
        Ok(chunks.concat().into_bytes())
    } else {
        let usage = serde_json::json!({
            "prompt_tokens": 0,
            "completion_tokens": content.len() / 4,
            "total_tokens": content.len() / 4,
        });
        Ok(serde_json::to_string(&serde_json::json!({
            "id": response_id,
            "object": "chat.completion",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": if content.is_empty() { Value::Null } else { Value::String(content) } },
                "finish_reason": "stop",
            }],
            "usage": usage,
        }))
        .unwrap_or_default()
        .into_bytes())
    }
}

/// 9router cursor.js AGENT_RUN_PATH constant (for tests / diagnostics).
#[allow(dead_code)]
const AGENT_RUN_PATH_STR: &str = AGENT_RUN_PATH;

/// Format tool name: "toolName" → "mcp_custom_toolName"
fn format_tool_name(name: &str) -> String {
    if name.is_empty() {
        return "mcp_custom_tool".to_string();
    }
    if let Some(rest) = name.strip_prefix("mcp__") {
        if let Some(idx) = rest.find("__") {
            let server = &rest[..idx];
            let tool_name = &rest[idx + 2..];
            return format!("mcp_{}_{}", server, tool_name);
        }
        return format!("mcp_custom_{}", rest);
    }
    if name.starts_with("mcp_") {
        return name.to_string();
    }
    format!("mcp_custom_{}", name)
}

/// Parse formatted tool name: "mcp_server_tool" -> (server_name, selected_tool)
fn parse_tool_name(formatted_name: &str) -> (String, String) {
    if let Some(tail) = formatted_name.strip_prefix("mcp_") {
        if let Some(idx) = tail.find('_') {
            (tail[..idx].to_string(), tail[idx + 1..].to_string())
        } else {
            ("custom".to_string(), tail.to_string())
        }
    } else {
        ("custom".to_string(), formatted_name.to_string())
    }
}

/// Parse tool_call_id into { tool_call_id, model_call_id }
fn parse_tool_id(id: &str) -> (String, Option<String>) {
    let delimiter = "\nmc_";
    if let Some(idx) = id.find(delimiter) {
        (
            id[..idx].to_string(),
            Some(id[idx + delimiter.len()..].to_string()),
        )
    } else {
        (id.to_string(), None)
    }
}

/// Encode MCP result
fn encode_mcp_result(selected_tool: &str, result_content: &str) -> Vec<u8> {
    concat_arrays(&[
        &encode_field_len(
            proto_fields::FLD_MCPR_SELECTED_TOOL,
            proto_fields::WIRE_LEN,
            selected_tool.as_bytes(),
        ),
        &encode_field_len(
            proto_fields::FLD_MCPR_RESULT,
            proto_fields::WIRE_LEN,
            result_content.as_bytes(),
        ),
    ])
}

/// Encode ClientSideToolV2Result
fn encode_client_side_tool_v2_result(
    tool_call_id: &str,
    model_call_id: Option<&str>,
    selected_tool: &str,
    result_content: &str,
    tool_index: u32,
) -> Vec<u8> {
    let mut result = Vec::new();

    // Tool type = 19 (MCP)
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_CV2R_TOOL,
        proto_fields::WIRE_VARINT,
        proto_fields::CLIENT_SIDE_TOOL_V2_MCP,
    ));

    // MCP result
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_CV2R_MCP_RESULT,
        proto_fields::WIRE_LEN,
        &encode_mcp_result(selected_tool, result_content),
    ));

    // Call ID
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_CV2R_CALL_ID,
        proto_fields::WIRE_LEN,
        tool_call_id.as_bytes(),
    ));

    // Model call ID (optional)
    if let Some(mcid) = model_call_id {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_CV2R_MODEL_CALL_ID,
            proto_fields::WIRE_LEN,
            mcid.as_bytes(),
        ));
    }

    // Tool index
    if tool_index > 0 {
        result.extend_from_slice(&encode_field_varint(
            proto_fields::FLD_CV2R_TOOL_INDEX,
            proto_fields::WIRE_VARINT,
            tool_index,
        ));
    }

    result
}

/// Encode tool result
fn encode_tool_result(tool_result: &Value) -> Vec<u8> {
    let tool_name = tool_result
        .get("tool_name")
        .and_then(|v| v.as_str())
        .or_else(|| tool_result.get("name").and_then(|v| v.as_str()))
        .unwrap_or("");
    let raw_args = tool_result
        .get("raw_args")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let result_content = tool_result
        .get("result_content")
        .or_else(|| tool_result.get("result"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_call_id = tool_result
        .get("tool_call_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_index = tool_result
        .get("tool_index")
        .or_else(|| tool_result.get("index"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;

    let (tc_id, mc_id) = parse_tool_id(tool_call_id);

    // Parse tool name
    let formatted_name = format_tool_name(tool_name);
    let (server_name, selected_tool) = parse_tool_name(&formatted_name);

    let name_bytes = formatted_name.as_bytes();

    let mut result = Vec::new();

    // Field 1: tool_call_id
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_TOOL_RESULT_CALL_ID,
        proto_fields::WIRE_LEN,
        tc_id.as_bytes(),
    ));

    // Field 2: name
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_TOOL_RESULT_NAME,
        proto_fields::WIRE_LEN,
        name_bytes,
    ));

    // Field 3: index
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_TOOL_RESULT_INDEX,
        proto_fields::WIRE_VARINT,
        tool_index,
    ));

    // Field 12: model_call_id (only when \nmc_ present)
    if let Some(ref mcid) = mc_id {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_TOOL_RESULT_MODEL_CALL_ID,
            proto_fields::WIRE_LEN,
            mcid.as_bytes(),
        ));
    }

    // Field 5: raw_args
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_TOOL_RESULT_RAW_ARGS,
        proto_fields::WIRE_LEN,
        raw_args.as_bytes(),
    ));

    // Field 8: result (ClientSideToolV2Result)
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_TOOL_RESULT_RESULT,
        proto_fields::WIRE_LEN,
        &encode_client_side_tool_v2_result(
            &tc_id,
            mc_id.as_deref(),
            &selected_tool,
            result_content,
            tool_index,
        ),
    ));

    // Field 11: tool_call (ClientSideToolV2Call) — 9router parity
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_TOOL_RESULT_TOOL_CALL,
        proto_fields::WIRE_LEN,
        &encode_client_side_tool_v2_call(
            &tc_id,
            &formatted_name,
            &selected_tool,
            &server_name,
            raw_args,
            mc_id.as_deref(),
            tool_index,
        ),
    ));

    result
}

/// Encode MCP params for tool call
fn encode_mcp_params_for_call(tool_name: &str, raw_args: &str, server_name: &str) -> Vec<u8> {
    let tool = concat_arrays(&[
        &encode_field_len(
            proto_fields::FLD_MCP_TOOL_NAME,
            proto_fields::WIRE_LEN,
            tool_name.as_bytes(),
        ),
        &encode_field_len(
            proto_fields::FLD_MCP_TOOL_PARAMS,
            proto_fields::WIRE_LEN,
            raw_args.as_bytes(),
        ),
        &encode_field_len(
            proto_fields::FLD_MCP_TOOL_SERVER,
            proto_fields::WIRE_LEN,
            server_name.as_bytes(),
        ),
    ]);
    encode_field_len(
        proto_fields::FLD_MCP_TOOLS_LIST,
        proto_fields::WIRE_LEN,
        &tool,
    )
}

/// Encode ClientSideToolV2Call
fn encode_client_side_tool_v2_call(
    tool_call_id: &str,
    tool_name: &str,
    selected_tool: &str,
    server_name: &str,
    raw_args: &str,
    model_call_id: Option<&str>,
    tool_index: u32,
) -> Vec<u8> {
    let mut result = Vec::new();

    // Tool type = 19 (MCP)
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_CV2C_TOOL,
        proto_fields::WIRE_VARINT,
        proto_fields::CLIENT_SIDE_TOOL_V2_MCP,
    ));

    // MCP params
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_CV2C_MCP_PARAMS,
        proto_fields::WIRE_LEN,
        &encode_mcp_params_for_call(selected_tool, raw_args, server_name),
    ));

    // Call ID
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_CV2C_CALL_ID,
        proto_fields::WIRE_LEN,
        tool_call_id.as_bytes(),
    ));

    // Name
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_CV2C_NAME,
        proto_fields::WIRE_LEN,
        tool_name.as_bytes(),
    ));

    // Raw args
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_CV2C_RAW_ARGS,
        proto_fields::WIRE_LEN,
        raw_args.as_bytes(),
    ));

    // Tool index
    if tool_index > 0 {
        result.extend_from_slice(&encode_field_varint(
            proto_fields::FLD_CV2C_TOOL_INDEX,
            proto_fields::WIRE_VARINT,
            tool_index,
        ));
    }

    // Model call ID (optional)
    if let Some(mcid) = model_call_id {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_CV2C_MODEL_CALL_ID,
            proto_fields::WIRE_LEN,
            mcid.as_bytes(),
        ));
    }

    result
}

/// Encode a conversation message
fn encode_message(
    content: &str,
    role: u32,
    message_id: &str,
    is_last: bool,
    has_tools: bool,
    tool_results: &[Value],
) -> Vec<u8> {
    let _has_tool_results = !tool_results.is_empty();
    let mut result = Vec::new();

    // Content
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_MSG_CONTENT,
        proto_fields::WIRE_LEN,
        content.as_bytes(),
    ));

    // Role
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_MSG_ROLE,
        proto_fields::WIRE_VARINT,
        role,
    ));

    // Message ID
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_MSG_ID,
        proto_fields::WIRE_LEN,
        message_id.as_bytes(),
    ));

    // Tool results
    for tr in tool_results {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_MSG_TOOL_RESULTS,
            proto_fields::WIRE_LEN,
            &encode_tool_result(tr),
        ));
    }

    // Is agentic
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_MSG_IS_AGENTIC,
        proto_fields::WIRE_VARINT,
        if has_tools { 1 } else { 0 },
    ));

    // Unified mode
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_MSG_UNIFIED_MODE,
        proto_fields::WIRE_VARINT,
        if has_tools {
            proto_fields::UNIFIED_MODE_AGENT
        } else {
            proto_fields::UNIFIED_MODE_CHAT
        },
    ));

    // Supported tools (only on last message with tools)
    if is_last && has_tools {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_MSG_SUPPORTED_TOOLS,
            proto_fields::WIRE_LEN,
            &encode_varint(1),
        ));
    }

    result
}

/// Encode instruction
fn encode_instruction(text: &str) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    encode_field_len(
        proto_fields::FLD_INSTRUCTION_TEXT,
        proto_fields::WIRE_LEN,
        text.as_bytes(),
    )
}

/// Encode model
fn encode_model(model_name: &str) -> Vec<u8> {
    concat_arrays(&[
        &encode_field_len(
            proto_fields::FLD_MODEL_NAME,
            proto_fields::WIRE_LEN,
            model_name.as_bytes(),
        ),
        &encode_field_len(proto_fields::FLD_MODEL_EMPTY, proto_fields::WIRE_LEN, &[]),
    ])
}

/// Encode cursor setting
fn encode_cursor_setting() -> Vec<u8> {
    let unknown6 = concat_arrays(&[
        &encode_field_len(
            proto_fields::FLD_SETTING6_FIELD_1,
            proto_fields::WIRE_LEN,
            &[],
        ),
        &encode_field_len(
            proto_fields::FLD_SETTING6_FIELD_2,
            proto_fields::WIRE_LEN,
            &[],
        ),
    ]);

    concat_arrays(&[
        &encode_field_len(
            proto_fields::FLD_SETTING_PATH,
            proto_fields::WIRE_LEN,
            b"cursor\\aisettings",
        ),
        &encode_field_len(
            proto_fields::FLD_SETTING_UNKNOWN_3,
            proto_fields::WIRE_LEN,
            &[],
        ),
        &encode_field_len(
            proto_fields::FLD_SETTING_UNKNOWN_6,
            proto_fields::WIRE_LEN,
            &unknown6,
        ),
        &encode_field_varint(
            proto_fields::FLD_SETTING_UNKNOWN_8,
            proto_fields::WIRE_VARINT,
            1,
        ),
        &encode_field_varint(
            proto_fields::FLD_SETTING_UNKNOWN_9,
            proto_fields::WIRE_VARINT,
            1,
        ),
    ])
}

/// Encode metadata
fn encode_metadata() -> Vec<u8> {
    let platform = std::env::consts::OS.as_bytes();
    let arch = std::env::consts::ARCH.as_bytes();

    concat_arrays(&[
        &encode_field_len(
            proto_fields::FLD_META_PLATFORM,
            proto_fields::WIRE_LEN,
            platform,
        ),
        &encode_field_len(proto_fields::FLD_META_ARCH, proto_fields::WIRE_LEN, arch),
        &encode_field_len(
            proto_fields::FLD_META_VERSION,
            proto_fields::WIRE_LEN,
            b"v20.0.0",
        ),
        &encode_field_len(proto_fields::FLD_META_CWD, proto_fields::WIRE_LEN, b"/"),
        &encode_field_len(
            proto_fields::FLD_META_TIMESTAMP,
            proto_fields::WIRE_LEN,
            chrono::Utc::now().to_rfc3339().as_bytes(),
        ),
    ])
}

/// Encode message ID
fn encode_message_id(message_id: &str, role: u32) -> Vec<u8> {
    concat_arrays(&[
        &encode_field_len(
            proto_fields::FLD_MSGID_ID,
            proto_fields::WIRE_LEN,
            message_id.as_bytes(),
        ),
        &encode_field_varint(
            proto_fields::FLD_MSGID_ROLE,
            proto_fields::WIRE_VARINT,
            role,
        ),
    ])
}

/// Encode MCP tool
fn encode_mcp_tool(tool: &Value) -> Vec<u8> {
    let tool_name = tool
        .get("function")
        .and_then(|f| f.get("name"))
        .or_else(|| tool.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_desc = tool
        .get("function")
        .and_then(|f| f.get("description"))
        .or_else(|| tool.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let input_schema = tool
        .get("function")
        .and_then(|f| f.get("parameters"))
        .or_else(|| tool.get("input_schema"))
        .and_then(|v| serde_json::to_string(v).ok())
        .unwrap_or_else(|| "{}".to_string());

    let mut result = Vec::new();

    if !tool_name.is_empty() {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_MCP_TOOL_NAME,
            proto_fields::WIRE_LEN,
            tool_name.as_bytes(),
        ));
    }
    if !tool_desc.is_empty() {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_MCP_TOOL_DESC,
            proto_fields::WIRE_LEN,
            tool_desc.as_bytes(),
        ));
    }
    if input_schema != "{}" {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_MCP_TOOL_PARAMS,
            proto_fields::WIRE_LEN,
            input_schema.as_bytes(),
        ));
    }
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_MCP_TOOL_SERVER,
        proto_fields::WIRE_LEN,
        b"custom",
    ));

    result
}

/// Build the full request payload
fn build_chat_request(
    messages: &[Value],
    model_name: &str,
    tools: &[Value],
    reasoning_effort: Option<&str>,
    force_agent_mode: bool,
) -> Vec<u8> {
    let has_tools = !tools.is_empty();
    let is_agentic = has_tools || force_agent_mode;

    // Normalize messages - split mixed assistant payloads
    let mut normalized_messages: Vec<Value> = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let tool_calls = msg.get("tool_calls").and_then(|v| v.as_array());
        let tool_results = msg.get("tool_results").and_then(|v| v.as_array());

        let has_tc = tool_calls.as_ref().map(|a| !a.is_empty()).unwrap_or(false);
        let has_tr = tool_results
            .as_ref()
            .map(|a| !a.is_empty())
            .unwrap_or(false);

        if role == "assistant" && has_tc && has_tr {
            // Keep assistant tool call without results
            let mut normalized = msg.clone();
            normalized["tool_results"] = json!([]);
            normalized_messages.push(normalized);

            // Always insert tool-result assistant message unless duplicate detected
            // Check if next message has the same tool_call_ids
            let next = messages.get(i + 1);
            let next_has_tr = next
                .and_then(|n| n.get("tool_results"))
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);

            let current_ids: std::collections::BTreeSet<String> = tool_results
                .map(|arr| {
                    arr.iter()
                        .filter_map(|tr| {
                            tr.get("tool_call_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();

            let next_ids: std::collections::BTreeSet<String> = next
                .and_then(|n| n.get("tool_results"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|tr| {
                            tr.get("tool_call_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();

            let same_ids = !current_ids.is_empty()
                && current_ids.len() == next_ids.len()
                && current_ids == next_ids;

            if !(next_has_tr && same_ids) {
                let result_msg = serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_results": tool_results
                });
                normalized_messages.push(result_msg);
            }
        } else {
            normalized_messages.push(msg.clone());
        }
    }

    // Prepare formatted messages and message IDs
    let mut formatted_messages: Vec<(String, u32, bool, Vec<Value>)> = Vec::new();
    let mut message_ids: Vec<(String, u32)> = Vec::new();

    for (i, msg) in normalized_messages.iter().enumerate() {
        let role = if msg.get("role").and_then(|v| v.as_str()) == Some("user") {
            proto_fields::ROLE_USER
        } else {
            proto_fields::ROLE_ASSISTANT
        };
        let message_id = uuid::Uuid::new_v4().to_string();
        let is_last = i == normalized_messages.len() - 1;
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let tool_results = msg
            .get("tool_results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        formatted_messages.push((content.to_string(), role, is_last, tool_results));
        message_ids.push((message_id, role));
    }

    // Map reasoning effort to thinking level
    let thinking_level = match reasoning_effort {
        Some("medium") => proto_fields::THINKING_MEDIUM,
        Some("high") => proto_fields::THINKING_HIGH,
        _ => proto_fields::THINKING_UNSPECIFIED,
    };

    // Build request
    let mut result = Vec::new();

    // Messages
    for (i, (content, role, is_last, tool_results)) in formatted_messages.iter().enumerate() {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_MESSAGES,
            proto_fields::WIRE_LEN,
            &encode_message(
                content,
                *role,
                &message_ids[i].0,
                *is_last,
                has_tools,
                tool_results,
            ),
        ));
    }

    // Static fields
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_UNKNOWN_2,
        proto_fields::WIRE_VARINT,
        1,
    ));
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_INSTRUCTION,
        proto_fields::WIRE_LEN,
        &encode_instruction(""),
    ));
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_UNKNOWN_4,
        proto_fields::WIRE_VARINT,
        1,
    ));
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_MODEL,
        proto_fields::WIRE_LEN,
        &encode_model(model_name),
    ));
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_WEB_TOOL,
        proto_fields::WIRE_LEN,
        b"",
    ));
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_UNKNOWN_13,
        proto_fields::WIRE_VARINT,
        1,
    ));
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_CURSOR_SETTING,
        proto_fields::WIRE_LEN,
        &encode_cursor_setting(),
    ));
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_UNKNOWN_19,
        proto_fields::WIRE_VARINT,
        1,
    ));
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_CONVERSATION_ID,
        proto_fields::WIRE_LEN,
        uuid::Uuid::new_v4().to_string().as_bytes(),
    ));
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_METADATA,
        proto_fields::WIRE_LEN,
        &encode_metadata(),
    ));

    // Tool-related fields
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_IS_AGENTIC,
        proto_fields::WIRE_VARINT,
        if is_agentic { 1 } else { 0 },
    ));
    if is_agentic {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_SUPPORTED_TOOLS,
            proto_fields::WIRE_LEN,
            &encode_varint(1),
        ));
    }

    // Message IDs
    for (mid, role) in &message_ids {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_MESSAGE_IDS,
            proto_fields::WIRE_LEN,
            &encode_message_id(mid, *role),
        ));
    }

    // MCP Tools
    for tool in tools {
        result.extend_from_slice(&encode_field_len(
            proto_fields::FLD_MCP_TOOLS,
            proto_fields::WIRE_LEN,
            &encode_mcp_tool(tool),
        ));
    }

    // Mode fields
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_LARGE_CONTEXT,
        proto_fields::WIRE_VARINT,
        0,
    ));
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_UNKNOWN_38,
        proto_fields::WIRE_VARINT,
        0,
    ));
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_UNIFIED_MODE,
        proto_fields::WIRE_VARINT,
        if is_agentic {
            proto_fields::UNIFIED_MODE_AGENT
        } else {
            proto_fields::UNIFIED_MODE_CHAT
        },
    ));
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_UNKNOWN_47,
        proto_fields::WIRE_LEN,
        b"",
    ));
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_SHOULD_DISABLE_TOOLS,
        proto_fields::WIRE_VARINT,
        if is_agentic { 0 } else { 1 },
    ));
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_THINKING_LEVEL,
        proto_fields::WIRE_VARINT,
        thinking_level,
    ));
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_UNKNOWN_51,
        proto_fields::WIRE_VARINT,
        0,
    ));
    result.extend_from_slice(&encode_field_varint(
        proto_fields::FLD_UNKNOWN_53,
        proto_fields::WIRE_VARINT,
        1,
    ));
    result.extend_from_slice(&encode_field_len(
        proto_fields::FLD_UNIFIED_MODE_NAME,
        proto_fields::WIRE_LEN,
        if is_agentic { b"Agent" } else { b"Ask" },
    ));

    result
}

/// Build the full request payload (wrapped in field 1 REQUEST)
/// Corresponds to 9router buildChatRequest() → encodeField(FLD_REQUEST, WIRE_TYPE.LEN, encodeRequest(...))
fn build_chat_request_wrapper(
    messages: &[Value],
    model_name: &str,
    tools: &[Value],
    reasoning_effort: Option<&str>,
    force_agent_mode: bool,
) -> Vec<u8> {
    let inner = build_chat_request(
        messages,
        model_name,
        tools,
        reasoning_effort,
        force_agent_mode,
    );
    encode_field_len(proto_fields::FLD_REQUEST, proto_fields::WIRE_LEN, &inner)
}

/// Wrap payload in Connect-RPC frame: [1 byte flags][4 bytes length BE][payload]
fn wrap_connect_rpc_frame(payload: &[u8], compress: bool) -> Vec<u8> {
    let flags = if compress { 0x01u8 } else { 0x00u8 };
    let length = payload.len() as u32;

    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(flags);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Use cursor_checksum::generate_cursor_checksum instead (in cursor_checksum.rs).
/// This function is kept as a compatibility shim.
fn generate_cursor_checksum(machine_id: &str) -> String {
    cursor_checksum::generate_cursor_checksum(machine_id)
}

/// See cursor_checksum::generate_hashed64_hex instead.
#[allow(dead_code)]
fn generate_machine_id(token: &str) -> String {
    cursor_checksum::generate_hashed64_hex(token, "machineId")
}

/// See cursor_checksum::generate_session_id instead.
#[allow(dead_code)]
fn generate_session_id(token: &str) -> String {
    cursor_checksum::generate_session_id(token)
}

/// See cursor_checksum::generate_hashed64_hex instead.
#[allow(dead_code)]
fn generate_client_key(token: &str) -> String {
    cursor_checksum::generate_hashed64_hex(token, "")
}

/// Build Cursor API headers using cursor_checksum::build_cursor_headers.
/// Extracts machineId and ghostMode from credentials.provider_specific_data.
/// Returns an error if machineId is missing (matching 9router behavior).
fn build_cursor_headers(
    access_token: &str,
    machine_id: Option<&str>,
    ghost_mode: bool,
) -> Result<HeaderMap, CursorExecutorError> {
    let headers = cursor_checksum::build_cursor_headers(access_token, machine_id, ghost_mode);

    let mut header_map = HeaderMap::new();

    // Authorization
    header_map.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&headers.authorization)
            .map_err(CursorExecutorError::InvalidHeader)?,
    );

    // Static headers
    header_map.insert("connect-accept-encoding", HeaderValue::from_static("gzip"));
    header_map.insert("connect-protocol-version", HeaderValue::from_static("1"));
    header_map.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/connect+proto"),
    );
    header_map.insert("user-agent", HeaderValue::from_static("connect-es/1.6.1"));

    // Dynamic headers
    header_map.insert(
        "x-amzn-trace-id",
        HeaderValue::from_str(&format!("Root={}", uuid::Uuid::new_v4()))
            .map_err(CursorExecutorError::InvalidHeader)?,
    );
    header_map.insert(
        "x-client-key",
        HeaderValue::from_str(&headers.client_key).map_err(CursorExecutorError::InvalidHeader)?,
    );
    header_map.insert(
        "x-cursor-checksum",
        HeaderValue::from_str(&headers.checksum).map_err(CursorExecutorError::InvalidHeader)?,
    );
    header_map.insert(
        "x-cursor-client-version",
        HeaderValue::from_static("3.12.17"),
    );
    header_map.insert(
        "x-cursor-client-commit",
        HeaderValue::from_static("0fb762053c34788bb7760d5673f8a6d4c8589d50"),
    );
    header_map.insert("x-cursor-client-type", HeaderValue::from_static("ide"));
    header_map.insert("x-cursor-client-os", HeaderValue::from_static(headers.os));
    header_map.insert(
        "x-cursor-client-arch",
        HeaderValue::from_static(headers.arch),
    );
    header_map.insert(
        "x-cursor-client-device-type",
        HeaderValue::from_static("desktop"),
    );
    header_map.insert(
        "x-cursor-config-version",
        HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
            .map_err(CursorExecutorError::InvalidHeader)?,
    );
    header_map.insert("x-cursor-timezone", HeaderValue::from_static("UTC"));
    header_map.insert(
        "x-ghost-mode",
        HeaderValue::from_str(if ghost_mode { "true" } else { "false" })
            .map_err(CursorExecutorError::InvalidHeader)?,
    );
    header_map.insert(
        "x-request-id",
        HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
            .map_err(CursorExecutorError::InvalidHeader)?,
    );
    header_map.insert(
        "x-session-id",
        HeaderValue::from_str(&headers.session_id).map_err(CursorExecutorError::InvalidHeader)?,
    );

    Ok(header_map)
}

// ==================== PROTOBUF DECODING ====================

/// Decode a varint from buffer
fn decode_varint(buffer: &[u8], offset: &mut usize) -> Result<u32, CursorExecutorError> {
    let mut result: u32 = 0;
    let mut shift = 0;

    while *offset < buffer.len() {
        let b = buffer[*offset];
        result |= ((b & 0x7F) as u32) << shift;
        *offset += 1;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
    }

    Ok(result)
}

/// Decode a protobuf field
fn decode_field(
    buffer: &[u8],
    offset: &mut usize,
) -> Result<Option<(u32, u8, Vec<u8>)>, CursorExecutorError> {
    if *offset >= buffer.len() {
        return Ok(None);
    }

    let tag = decode_varint(buffer, offset)?;
    let field_num = tag >> 3;
    let wire_type = (tag & 0x07) as u8;

    let value = match wire_type {
        proto_fields::WIRE_VARINT => {
            let val = decode_varint(buffer, offset)?;
            val.to_le_bytes().to_vec()
        }
        proto_fields::WIRE_LEN => {
            let len = decode_varint(buffer, offset)? as usize;
            if *offset + len > buffer.len() {
                return Err(CursorExecutorError::ProtobufDecode(
                    "Unexpected end of buffer".to_string(),
                ));
            }
            let value = buffer[*offset..*offset + len].to_vec();
            *offset += len;
            value
        }
        proto_fields::WIRE_FIXED64 => {
            if *offset + 8 > buffer.len() {
                return Err(CursorExecutorError::ProtobufDecode(
                    "Unexpected end of buffer".to_string(),
                ));
            }
            let value = buffer[*offset..*offset + 8].to_vec();
            *offset += 8;
            value
        }
        proto_fields::WIRE_FIXED32 => {
            if *offset + 4 > buffer.len() {
                return Err(CursorExecutorError::ProtobufDecode(
                    "Unexpected end of buffer".to_string(),
                ));
            }
            let value = buffer[*offset..*offset + 4].to_vec();
            *offset += 4;
            value
        }
        _ => {
            return Err(CursorExecutorError::ProtobufDecode(format!(
                "Unknown wire type: {}",
                wire_type
            )));
        }
    };

    Ok(Some((field_num, wire_type, value)))
}

/// Decode a message into a map of field_num -> list of values
fn decode_message(
    data: &[u8],
) -> Result<std::collections::HashMap<u32, Vec<Vec<u8>>>, CursorExecutorError> {
    let mut fields: std::collections::HashMap<u32, Vec<Vec<u8>>> = std::collections::HashMap::new();
    let mut offset = 0;

    while offset < data.len() {
        match decode_field(data, &mut offset)? {
            Some((field_num, _wire_type, value)) => {
                fields.entry(field_num).or_default().push(value);
            }
            None => break,
        }
    }

    Ok(fields)
}

/// Parse a Connect-RPC frame: [1 byte flags][4 bytes length BE][payload]
fn parse_connect_rpc_frame(buffer: &[u8]) -> Result<Option<(u8, Vec<u8>)>, CursorExecutorError> {
    if buffer.len() < 5 {
        return Ok(None);
    }

    let flags = buffer[0];
    let length = u32::from_be_bytes([buffer[1], buffer[2], buffer[3], buffer[4]]) as usize;

    if buffer.len() < 5 + length {
        return Ok(None);
    }

    let payload = buffer[5..5 + length].to_vec();

    Ok(Some((flags, payload)))
}

// ==================== EXECUTOR IMPLEMENTATION ====================

impl CursorExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, CursorExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// Parse Cursor model string to extract actual model name.
    ///
    /// Examples:
    /// - "cursor/claude-4.6-opus-max" → "claude-4.6-opus-max"
    /// - "cursor/gpt-5.3-codex" → "gpt-5.3-codex"
    /// - "claude-4.6-opus-max" → "claude-4.6-opus-max" (no prefix)
    pub fn parse_cursor_model(model: &str) -> String {
        if let Some(stripped) = model.strip_prefix("cursor/") {
            stripped.to_string()
        } else {
            model.to_string()
        }
    }

    /// Extract messages from the request body
    fn extract_messages(body: &Value) -> Result<Vec<Value>, CursorExecutorError> {
        let messages = body
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                CursorExecutorError::UnsupportedFormat("Missing messages array".to_string())
            })?
            .clone();
        Ok(messages)
    }

    /// Extract tools from the request body
    fn extract_tools(body: &Value) -> Vec<Value> {
        body.get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    /// Extract reasoning effort from the request body
    fn extract_reasoning_effort(body: &Value) -> Option<String> {
        body.get("reasoning_effort")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    /// Transform the request body to Cursor protobuf format.
    /// `force_agent_mode` is true for Claude Code UA (9router cursor.js:178-179).
    fn transform_request_body(
        &self,
        body: &Value,
        actual_model: &str,
        force_agent_mode: bool,
    ) -> Result<Vec<u8>, CursorExecutorError> {
        let messages = Self::extract_messages(body)?;
        let tools = Self::extract_tools(body);
        let reasoning_effort = Self::extract_reasoning_effort(body);

        let protobuf = build_chat_request_wrapper(
            &messages,
            actual_model,
            &tools,
            reasoning_effort.as_deref(),
            force_agent_mode,
        );
        let framed = wrap_connect_rpc_frame(&protobuf, false);

        Ok(framed)
    }

    fn resolve_endpoint(credentials: &ProviderConnection) -> &'static str {
        // Optional override: agentn / non-privacy host
        let host = credentials
            .provider_specific_data
            .get("cursorHost")
            .or_else(|| credentials.provider_specific_data.get("cursor_host"))
            .and_then(|v| v.as_str())
            .unwrap_or("api2");
        if host == "agentn" || host == "agentn.api5" {
            CURSOR_AGENTN_ENDPOINT
        } else {
            CURSOR_API_ENDPOINT
        }
    }

    /// Execute a text-only request via `agent.v1.AgentService/Run` over an
    /// HTTP/2 duplex stream (9router cursor.js executeAgent, P0-K1).
    ///
    /// The agent endpoint is HTTP/2-only (Node undici fails with
    /// HTTPParserError on the h2 preface — cursor.js:385-387). The request
    /// half-stream stays open while reading the response: AgentService asks
    /// for client context mid-stream (request-context handshake) and stalls if
    /// the request side is half-closed before it streams.
    async fn execute_agent(
        &self,
        request: CursorExecutionRequest,
    ) -> Result<CursorExecutorResponse, CursorExecutorError> {
        let actual_model = Self::parse_cursor_model(&request.model);
        let access_token = request.credentials.access_token.as_deref().ok_or_else(|| {
            CursorExecutorError::MissingCredentials("Cursor access token required".to_string())
        })?;
        let machine_id = request
            .credentials
            .provider_specific_data
            .get("machineId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CursorExecutorError::MissingCredentials(
                    "Machine ID is required for Cursor API. Re-import your Cursor account."
                        .to_string(),
                )
            })?;
        let ghost_mode = request
            .credentials
            .provider_specific_data
            .get("ghostMode")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let messages = Self::extract_messages(&request.body)?;
        let frame = build_agent_run_frame(&messages, &actual_model);
        let url = format!("{AGENT_BASE_URL}{AGENT_RUN_PATH}");
        let headers = build_cursor_headers(access_token, Some(machine_id), ghost_mode)?;

        // Hyper-util legacy client with HTTP/2 enabled (client_pool.rs
        // build_hyper_client). The request body is a Connect-RPC frame; we
        // keep the response side streaming and write the request-context
        // handshake back on the request half-stream as frames arrive.
        let client = self.pool.get_hyper_direct("cursor")?;
        // 9router v0.5.50 agent path uses the same buildHeaders as the chat
        // path (cursorChecksum.js buildCursorHeaders — x-cursor-client-type
        // "ide", x-ghost-mode, checksum bundle), spread over the h2 request.
        let request_builder = hyper::Request::builder()
            .method(http::Method::POST)
            .uri(url.clone())
            .header(":authority", "agent.api5.cursor.sh")
            .header(":scheme", "https")
            .header("accept", "application/connect+proto, */*")
            .header("content-type", "application/connect+proto")
            .header("connect-accept-encoding", "gzip")
            .header("connect-protocol-version", "1")
            .header("user-agent", "connect-es/1.6.1")
            .header("x-cursor-client-type", "ide")
            .header("x-ghost-mode", if ghost_mode { "true" } else { "false" })
            .header("x-request-id", uuid::Uuid::new_v4().to_string());

        let mut request_builder = request_builder;
        for (name, value) in headers.iter() {
            request_builder = request_builder.header(name.as_str(), value.as_bytes());
        }
        let req = request_builder
            .body(Full::new(bytes::Bytes::from(frame)))
            .map_err(CursorExecutorError::InvalidRequest)?;
        let response = client
            .request(req)
            .await
            .map_err(CursorExecutorError::Hyper)?;

        let status = response.status();
        if status != 200 {
            // Non-200: read the raw body and surface it as a JSON error
            // (cursor.js:509-528 — errors are returned untransformed so
            // account fallback can read the body).
            let error_text = response
                .into_body()
                .collect()
                .await
                .map(|collected| collected.to_bytes().to_vec())
                .unwrap_or_default();
            let error_text = String::from_utf8_lossy(&error_text).to_string();
            let error_body = serde_json::json!({
                "error": {
                    "message": format!(
                        "Cursor AgentService {}: {}",
                        status,
                        if error_text.is_empty() {
                            "request failed".to_string()
                        } else {
                            error_text
                        }
                    ),
                    "type": "api_error",
                }
            });
            let http_response = http::Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(reqwest::Body::from(
                    serde_json::to_string(&error_body).unwrap_or_default(),
                ))
                .map_err(CursorExecutorError::InvalidRequest)?;
            return Ok(CursorExecutorResponse {
                response: UpstreamResponse::Reqwest(http_response.into()),
                url,
                headers,
                transformed_body: request.body,
                transport: TransportKind::Reqwest,
            });
        }

        let response_id = format!("chatcmpl-msg_{}", now_millis());
        let created = now_secs();
        let body_bytes = consume_agent_stream(
            response.into_body(),
            &response_id,
            created,
            &actual_model,
            request.stream,
        )
        .await?;

        // JS createErrorResponse (cursor.js:257-276): an in-stream
        // resource_exhausted error must surface as HTTP 429 so the caller's
        // account rotation / combo fallback reacts instead of treating the
        // turn as a successful empty completion.
        let is_rate_limit = String::from_utf8_lossy(&body_bytes).contains("\"rate_limit_error\"");
        let content_type = if request.stream {
            "text/event-stream"
        } else {
            "application/json"
        };
        let http_response = http::Response::builder()
            .status(if is_rate_limit { 429 } else { 200 })
            .header("content-type", content_type)
            .header("cache-control", "no-cache")
            .body(reqwest::Body::from(body_bytes))
            .map_err(CursorExecutorError::InvalidRequest)?;

        Ok(CursorExecutorResponse {
            response: UpstreamResponse::Reqwest(http_response.into()),
            url,
            headers,
            transformed_body: request.body,
            transport: TransportKind::Reqwest,
        })
    }

    pub async fn execute(
        &self,
        request: CursorExecutionRequest,
    ) -> Result<CursorExecutorResponse, CursorExecutorError> {
        // 9router execute(): text-only requests route to the AgentService path
        // (agent.v1.AgentService/Run over HTTP/2); tool-call/history
        // conversations stay on the legacy ChatService (cursor.js:666-680).
        if is_agent_text_request(&request.body) {
            return self.execute_agent(request).await;
        }

        let actual_model = Self::parse_cursor_model(&request.model);

        // Get access token from credentials
        let access_token = request.credentials.access_token.as_deref().ok_or_else(|| {
            CursorExecutorError::MissingCredentials("Cursor access token required".to_string())
        })?;

        // Extract machineId from provider_specific_data (must exist after OAuth import)
        let machine_id = request
            .credentials
            .provider_specific_data
            .get("machineId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CursorExecutorError::MissingCredentials(
                    "Machine ID is required for Cursor API. Re-import your Cursor account."
                        .to_string(),
                )
            })?;

        // Extract ghostMode from provider_specific_data (defaults to true)
        let ghost_mode = request
            .credentials
            .provider_specific_data
            .get("ghostMode")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // 9router: forceAgentMode when UA is Claude Code / claude-cli
        let force_agent_mode = request
            .credentials
            .provider_specific_data
            .get("rawHeaders")
            .and_then(|h| h.get("user-agent"))
            .and_then(|v| v.as_str())
            .map(|ua| {
                let u = ua.to_lowercase();
                u.contains("claude-cli") || u.contains("claude-code") || u.contains("claude code")
            })
            .unwrap_or(false);

        let headers = build_cursor_headers(access_token, Some(machine_id), ghost_mode)?;
        let body_bytes =
            self.transform_request_body(&request.body, &actual_model, force_agent_mode)?;
        let endpoint = Self::resolve_endpoint(&request.credentials);

        let client = self.pool.get("cursor", request.proxy.as_ref())?;
        let raw_response = client
            .post(endpoint)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        let status = raw_response.status();
        let is_stream = request.stream;

        if status == 200 {
            let raw_body = raw_response
                .bytes()
                .await
                .map_err(CursorExecutorError::Request)?;
            let body_value = request.body.clone();
            let cursor_model = request.model.clone();

            let (body_string, content_type) = if is_stream {
                let sse = transform_protobuf_to_sse(&raw_body, &cursor_model, &body_value)?;
                (sse, "text/event-stream")
            } else {
                let json = transform_protobuf_to_json(&raw_body, &cursor_model, &body_value)?;
                (json, "application/json")
            };

            let http_response = http::Response::builder()
                .status(200)
                .header("content-type", content_type)
                .header("cache-control", "no-cache")
                .body(reqwest::Body::from(body_string))
                .map_err(CursorExecutorError::InvalidRequest)?;
            let fake_response: reqwest::Response = http_response.into();

            Ok(CursorExecutorResponse {
                response: UpstreamResponse::Reqwest(fake_response),
                url: endpoint.to_string(),
                headers,
                transformed_body: request.body,
                transport: TransportKind::Reqwest,
            })
        } else {
            Ok(CursorExecutorResponse {
                response: UpstreamResponse::Reqwest(raw_response),
                url: endpoint.to_string(),
                headers,
                transformed_body: request.body,
                transport: TransportKind::Reqwest,
            })
        }
    }
}

// ==================== DECODING HELPERS ====================

/// Response extracted from a single protobuf frame.
/// Each frame can contain EITHER a text/thinking update OR a tool-call chunk.
pub struct DecodedFrame {
    pub text: Option<String>,
    pub thinking: Option<String>,
    pub tool_call: Option<Value>,
    pub error: Option<String>,
}

/// Extract text, thinking and tool call from a response payload.
/// Returns (text, thinking, tool_call) — at most one of these is Some per frame.
fn extract_from_response(payload: &[u8]) -> Result<Option<DecodedFrame>, CursorExecutorError> {
    let fields = decode_message(payload)?;

    // Field 1: ClientSideToolV2Call (tool call)
    if let Some(values) = fields.get(&proto_fields::FLD_TOOL_CALL) {
        if let Some(data) = values.first() {
            let tool_call_fields = decode_message(data)?;
            let mut tool_call_json = serde_json::Map::new();

            // Extract tool call ID
            if let Some(ids) = tool_call_fields.get(&proto_fields::FLD_TOOL_ID) {
                if let Some(id_data) = ids.first() {
                    if let Ok(id_str) = std::str::from_utf8(id_data) {
                        tool_call_json.insert(
                            "id".to_string(),
                            serde_json::Value::String(
                                id_str.split('\n').next().unwrap_or("").to_string(),
                            ),
                        );
                    }
                }
            }

            // Extract tool name
            if let Some(names) = tool_call_fields.get(&proto_fields::FLD_TOOL_NAME) {
                if let Some(name_data) = names.first() {
                    if let Ok(name_str) = std::str::from_utf8(name_data) {
                        tool_call_json.insert(
                            "name".to_string(),
                            serde_json::Value::String(name_str.to_string()),
                        );
                    }
                }
            }

            // Extract is_last flag
            if let Some(flags) = tool_call_fields.get(&proto_fields::FLD_TOOL_IS_LAST) {
                if let Some(flag_data) = flags.first() {
                    let is_last = flag_data.first().copied().unwrap_or(0) != 0;
                    tool_call_json.insert("is_last".to_string(), serde_json::Value::Bool(is_last));
                }
            }

            // Also check alternate is_last field (FLD_TOOL_IS_LAST_ALT = 15)
            if !tool_call_json.contains_key("is_last") {
                if let Some(flags) = tool_call_fields.get(&proto_fields::FLD_TOOL_IS_LAST_ALT) {
                    if let Some(flag_data) = flags.first() {
                        let is_last = flag_data.first().copied().unwrap_or(0) != 0;
                        tool_call_json
                            .insert("is_last".to_string(), serde_json::Value::Bool(is_last));
                    }
                }
            }

            // Extract MCP params for nested tool info
            if let Some(params) = tool_call_fields.get(&proto_fields::FLD_TOOL_MCP_PARAMS) {
                if let Some(params_data) = params.first() {
                    if let Ok(mcp_fields) = decode_message(params_data) {
                        if let Some(tools_list) = mcp_fields.get(&proto_fields::FLD_MCP_TOOLS_LIST)
                        {
                            if let Some(tool_data) = tools_list.first() {
                                if let Ok(tool_fields) = decode_message(tool_data) {
                                    if let Some(nested_names) =
                                        tool_fields.get(&proto_fields::FLD_MCP_NESTED_NAME)
                                    {
                                        if let Some(name_data) = nested_names.first() {
                                            if let Ok(name_str) = std::str::from_utf8(name_data) {
                                                tool_call_json.insert(
                                                    "name".to_string(),
                                                    serde_json::Value::String(name_str.to_string()),
                                                );
                                            }
                                        }
                                    }
                                    if let Some(nested_params) =
                                        tool_fields.get(&proto_fields::FLD_MCP_NESTED_PARAMS)
                                    {
                                        if let Some(params_data) = nested_params.first() {
                                            if let Ok(params_str) = std::str::from_utf8(params_data)
                                            {
                                                tool_call_json.insert(
                                                    "arguments".to_string(),
                                                    serde_json::Value::String(
                                                        params_str.to_string(),
                                                    ),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Extract raw args as fallback
            if let Some(args_list) = tool_call_fields.get(&proto_fields::FLD_TOOL_RAW_ARGS) {
                if let Some(args_data) = args_list.first() {
                    if let Ok(args_str) = std::str::from_utf8(args_data) {
                        if !tool_call_json.contains_key("arguments") {
                            tool_call_json.insert(
                                "arguments".to_string(),
                                serde_json::Value::String(args_str.to_string()),
                            );
                        }
                    }
                }
            }

            if tool_call_json.contains_key("id") && tool_call_json.contains_key("name") {
                let function = serde_json::json!({
                    "name": tool_call_json.get("name").unwrap_or(&serde_json::Value::String("".to_string())),
                    "arguments": tool_call_json.get("arguments").unwrap_or(&serde_json::Value::String("{}".to_string())),
                });
                let tool_call = serde_json::json!({
                    "id": tool_call_json.get("id").unwrap_or(&serde_json::Value::String("".to_string())),
                    "type": "function",
                    "function": function,
                });
                return Ok(Some(DecodedFrame {
                    text: None,
                    thinking: None,
                    tool_call: Some(tool_call),
                    error: None,
                }));
            }
        }
    }

    // Field 2: StreamUnifiedChatResponse (text + thinking)
    if let Some(values) = fields.get(&proto_fields::FLD_RESPONSE) {
        if let Some(data) = values.first() {
            let response_fields = decode_message(data)?;

            let mut text: Option<String> = None;
            let mut thinking: Option<String> = None;

            // Extract text (field 1)
            if let Some(text_values) = response_fields.get(&proto_fields::FLD_RESPONSE_TEXT) {
                if let Some(text_data) = text_values.first() {
                    if let Ok(text_str) = std::str::from_utf8(text_data) {
                        text = Some(text_str.to_string());
                    }
                }
            }

            // Extract thinking (field 25) — 9router parity
            if let Some(thinking_values) = response_fields.get(&proto_fields::FLD_THINKING) {
                if let Some(thinking_data) = thinking_values.first() {
                    if let Ok(thinking_fields) = decode_message(thinking_data) {
                        if let Some(thinking_text_values) =
                            thinking_fields.get(&proto_fields::FLD_THINKING_TEXT)
                        {
                            if let Some(thinking_text_data) = thinking_text_values.first() {
                                if let Ok(thinking_str) = std::str::from_utf8(thinking_text_data) {
                                    thinking = Some(thinking_str.to_string());
                                }
                            }
                        }
                    }
                }
            }

            if text.is_some() || thinking.is_some() {
                return Ok(Some(DecodedFrame {
                    text,
                    thinking,
                    tool_call: None,
                    error: None,
                }));
            }
        }
    }

    Ok(None)
}

// ==================== DECOMPRESSION ====================

/// Decompress payload based on Connect-RPC flags.
/// Mirrors cursor.js decompressPayload() lines 59-101.
fn decompress_payload(payload: &[u8], flags: u8) -> Vec<u8> {
    // Check for JSON error response (starts with `{"error`)
    if payload.len() > 10 && payload[0] == 0x7b && payload[1] == 0x22 {
        if let Ok(text) = std::str::from_utf8(payload) {
            if text.starts_with("{\"error") {
                return payload.to_vec();
            }
        }
    }

    match flags {
        COMPRESS_FLAG_GZIP | COMPRESS_FLAG_TRAILER | COMPRESS_FLAG_GZIP_TRAILER => {
            // Try gzip first
            let r = std::io::Cursor::new(payload);
            let mut decoder = GzDecoder::new(r);
            let mut out = Vec::new();
            if decoder.read_to_end(&mut out).is_ok() && !out.is_empty() {
                return out;
            }

            // Try zlib deflate (RFC 1950)
            let r = std::io::Cursor::new(payload);
            let mut decoder = ZlibDecoder::new(r);
            let mut out = Vec::new();
            if decoder.read_to_end(&mut out).is_ok() && !out.is_empty() {
                return out;
            }

            // Try raw deflate (RFC 1951)
            let r = std::io::Cursor::new(payload);
            let mut decoder = DeflateDecoder::new(r);
            let mut out = Vec::new();
            if decoder.read_to_end(&mut out).is_ok() && !out.is_empty() {
                return out;
            }

            payload.to_vec()
        }
        COMPRESS_FLAG_NONE => payload.to_vec(),
        _ => payload.to_vec(),
    }
}

/// Read a Connect-RPC frame with decompression.
/// Returns Ok(None) when no more frames (done),
/// Ok(Some(Vec::new())) for skip,
/// Ok(Some(data)) for a valid payload.
fn read_cursor_frame(
    buffer: &[u8],
    offset: &mut usize,
) -> Result<Option<Vec<u8>>, CursorExecutorError> {
    if *offset + 5 > buffer.len() {
        return Ok(None);
    }

    let flags = buffer[*offset];
    let length = u32::from_be_bytes([
        buffer[*offset + 1],
        buffer[*offset + 2],
        buffer[*offset + 3],
        buffer[*offset + 4],
    ]) as usize;

    if *offset + 5 + length > buffer.len() {
        return Ok(None);
    }

    let raw_payload = &buffer[*offset + 5..*offset + 5 + length];
    *offset += 5 + length;

    let decompressed = decompress_payload(raw_payload, flags);
    if decompressed.is_empty() {
        return Ok(Some(Vec::new())); // skip
    }

    Ok(Some(decompressed))
}

// ==================== RESPONSE TRANSFORM HELPERS ====================

/// Check if model is a composer model (9router cursor.js isComposerModel):
/// anchored on the FINAL path segment — `^composer(?:-|$)` case-insensitive.
fn is_composer_model(model: &str) -> bool {
    let model_id = model.split('/').next_back().unwrap_or(model);
    let lower = model_id.to_lowercase();
    lower == "composer" || lower.starts_with("composer-")
}

/// Extract visible content from thinking for composer models.
/// JS visibleComposerContentFromThinking slices after the LAST `</think>`
/// tag (not `</thinking>`), trimming leading whitespace only.
fn visible_composer_content_from_thinking(thinking: &str) -> Option<String> {
    let end_tag = "</think>";
    let end_idx = thinking.rfind(end_tag)?;
    let after = &thinking[end_idx + end_tag.len()..];
    let trimmed = after.trim_start();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Format a chat.completion.chunk SSE data line.
/// Mirrors chatChunkSse from open-sse/utils/sse.js
fn format_chat_chunk_sse(
    id: &str,
    created: u64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> String {
    let choices = serde_json::json!([{
        "index": 0,
        "delta": delta,
        "finish_reason": finish_reason,
    }]);
    let data = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": choices,
    });
    format!(
        "data: {}\n\n",
        serde_json::to_string(&data).unwrap_or_default()
    )
}

/// Build an OpenAI chat completion response.
/// Mirrors cursorChatResponse from cursor.js lines 318-368.
fn build_chat_completion_response(
    id: &str,
    created: u64,
    model: &str,
    text: Option<&str>,
    thinking: Option<&str>,
    tool_calls: Vec<Value>,
) -> Value {
    let content = match (text, thinking) {
        (Some(t), Some(th)) => Some(format!("{}\n\n{}", th, t)),
        (Some(t), None) => Some(t.to_string()),
        (None, Some(th)) => Some(th.to_string()),
        (None, None) => None,
    };

    let mut message = serde_json::json!({
        "role": "assistant",
        "content": content,
    });

    if !tool_calls.is_empty() {
        message["tool_calls"] = serde_json::Value::Array(tool_calls.clone());
    }

    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else {
        "stop"
    };

    let usage = serde_json::json!({
        "prompt_tokens": 0,
        "completion_tokens": content.as_ref().map(|c| c.len() / 4).unwrap_or(0) as u64,
        "total_tokens": content.as_ref().map(|c| c.len() / 4).unwrap_or(0) as u64,
    });

    serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": usage,
    })
}

// ==================== TRANSFORM PROTOS TO SSE ====================

/// Transform Cursor protobuf response into OpenAI SSE chunks.
/// Mirrors transformProtobufToSSE from cursor.js lines 302-516.
pub fn transform_protobuf_to_sse(
    buffer: &[u8],
    model: &str,
    _body: &Value,
) -> Result<String, CursorExecutorError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let response_id = format!("chatcmpl-cursor-{}", now.as_millis());
    let created = now.as_secs();

    let mut offset = 0;
    let mut chunks: Vec<String> = Vec::new();
    let mut first_chunk = true;
    let mut total_content = String::new();
    let mut total_thinking = String::new();
    let mut tool_call_map: HashMap<String, Value> = HashMap::new();
    let mut has_content = false;

    while offset < buffer.len() {
        let frame = read_cursor_frame(buffer, &mut offset)?;
        let payload = match frame {
            None => break,                       // done
            Some(p) if p.is_empty() => continue, // skip
            Some(p) => p,
        };

        // Check for JSON error response
        if payload.len() > 10 && payload[0] == 0x7b && payload[1] == 0x22 {
            if let Ok(text) = std::str::from_utf8(&payload) {
                if text.contains("\"error\"") {
                    if has_content {
                        break;
                    }
                    // Return error as SSE
                    let error_chunk = format!("data: {}\n\n", text);
                    chunks.push(error_chunk);
                    chunks.push(SSE_DONE.to_string());
                    return Ok(chunks.concat());
                }
            }
        }

        let result = extract_from_response(&payload)?;
        let frame = match result {
            None => continue,
            Some(f) => f,
        };

        // Handle error in frame
        if let Some(err) = frame.error {
            let error_sse = format!("data: {}\n\n", err);
            chunks.push(error_sse);
            chunks.push(SSE_DONE.to_string());
            return Ok(chunks.concat());
        }

        // Handle tool call
        if let Some(tc) = frame.tool_call {
            let tc_id = tc["id"].as_str().unwrap_or("").to_string();
            let tc_args = tc["function"]["arguments"]
                .as_str()
                .unwrap_or("{}")
                .to_string();
            let _tc_name = tc["function"]["name"].as_str().unwrap_or("").to_string();

            // Check is_last in our accumulated map or from the frame
            let is_last_value = tc.get("is_last").and_then(|v| v.as_bool()).unwrap_or(false);

            if let Some(existing) = tool_call_map.get_mut(&tc_id) {
                // Accumulate arguments
                let existing_args = existing["function"]["arguments"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                existing["function"]["arguments"] = json!(existing_args + &tc_args);
                if is_last_value {
                    existing["is_last"] = json!(true);

                    // Emit tool call chunk
                    let delta = json!({
                        "role": "assistant",
                        "content": null,
                    });
                    let sse = format_chat_chunk_sse(&response_id, created, model, delta, None);
                    chunks.push(sse);

                    // Emit tool call delta
                    let tool_delta = json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "index": 0,
                            "id": existing["id"],
                            "type": "function",
                            "function": {
                                "name": existing["function"]["name"],
                                "arguments": existing["function"]["arguments"],
                            }
                        }]
                    });
                    let tool_sse =
                        format_chat_chunk_sse(&response_id, created, model, tool_delta, None);
                    chunks.push(tool_sse);
                }
            } else {
                let mut new_tc = tc.clone();
                if is_last_value {
                    new_tc["is_last"] = json!(true);
                }
                tool_call_map.insert(tc_id, new_tc);

                if is_last_value {
                    // Emit tool call chunk immediately
                    let delta = json!({
                        "role": "assistant",
                        "content": null,
                    });
                    let sse = format_chat_chunk_sse(&response_id, created, model, delta, None);
                    chunks.push(sse);

                    let tool_delta = json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "index": 0,
                            "id": tc.get("id"),
                            "type": "function",
                            "function": {
                                "name": tc.get("function").and_then(|f| f.get("name")),
                                "arguments": tc.get("function").and_then(|f| f.get("arguments")),
                            }
                        }]
                    });
                    let tool_sse =
                        format_chat_chunk_sse(&response_id, created, model, tool_delta, None);
                    chunks.push(tool_sse);
                }
            }

            has_content = true;
        }

        // Handle text
        if let Some(text) = frame.text {
            if !text.is_empty() {
                total_content.push_str(&text);
                has_content = true;

                let delta = if first_chunk {
                    first_chunk = false;
                    json!({"role": "assistant", "content": text})
                } else {
                    json!({"content": text})
                };
                let sse = format_chat_chunk_sse(&response_id, created, model, delta, None);
                chunks.push(sse);
            }
        }

        // Handle thinking
        if let Some(thinking) = frame.thinking {
            if !thinking.is_empty() {
                total_thinking.push_str(&thinking);

                // For composer models, extract visible content from thinking
                if is_composer_model(model) {
                    if let Some(visible) = visible_composer_content_from_thinking(&thinking) {
                        if !visible.is_empty() {
                            total_content.push_str(&visible);
                            has_content = true;
                            let delta = if first_chunk {
                                first_chunk = false;
                                json!({"role": "assistant", "content": visible})
                            } else {
                                json!({"content": visible})
                            };
                            let sse =
                                format_chat_chunk_sse(&response_id, created, model, delta, None);
                            chunks.push(sse);
                        }
                    }
                }
            }
        }
    }

    // Finalize remaining tool calls (those without isLast)
    let mut tool_calls: Vec<Value> = Vec::new();
    for tc in tool_call_map.values() {
        let finalized_tc = serde_json::json!({
            "id": tc.get("id"),
            "type": "function",
            "function": {
                "name": tc.get("function").and_then(|f| f.get("name")),
                "arguments": tc.get("function").and_then(|f| f.get("arguments")),
            }
        });
        tool_calls.push(finalized_tc);
    }

    // If we only got tool calls, emit them
    if !tool_calls.is_empty() && !has_content {
        let delta = json!({"role": "assistant", "content": null});
        let sse = format_chat_chunk_sse(&response_id, created, model, delta, None);
        chunks.push(sse);

        for tc in &tool_calls {
            let tool_delta = json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [tc]
            });
            let tool_sse = format_chat_chunk_sse(&response_id, created, model, tool_delta, None);
            chunks.push(tool_sse);
        }
    }

    // Final chunk with finish_reason and usage estimation
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else {
        "stop"
    };

    let usage = serde_json::json!({
        "prompt_tokens": 0,
        "completion_tokens": total_content.len() / 4,
        "total_tokens": total_content.len() / 4,
    });

    let final_delta = json!({});
    let final_sse = serde_json::json!({
        "id": response_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": final_delta,
            "finish_reason": finish_reason,
        }],
        "usage": usage,
    });
    chunks.push(format!(
        "data: {}\n\n",
        serde_json::to_string(&final_sse).unwrap_or_default()
    ));
    chunks.push(SSE_DONE.to_string());

    Ok(chunks.concat())
}

// ==================== TRANSFORM PROTOS TO JSON ====================

/// Transform Cursor protobuf response into OpenAI chat completion JSON.
/// Mirrors transformProtobufToJSON from cursor.js lines 518-681.
pub fn transform_protobuf_to_json(
    buffer: &[u8],
    model: &str,
    _body: &Value,
) -> Result<String, CursorExecutorError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let response_id = format!("chatcmpl-cursor-{}", now.as_millis());
    let created = now.as_secs();

    let mut offset = 0;
    let mut total_content = String::new();
    let mut total_thinking = String::new();
    let mut tool_call_map: HashMap<String, Value> = HashMap::new();
    let mut finalized_ids: HashSet<String> = HashSet::new();
    let mut has_content = false;

    while offset < buffer.len() {
        let frame = read_cursor_frame(buffer, &mut offset)?;
        let payload = match frame {
            None => break,                       // done
            Some(p) if p.is_empty() => continue, // skip
            Some(p) => p,
        };

        // Check for JSON error response
        if payload.len() > 10 && payload[0] == 0x7b && payload[1] == 0x22 {
            if let Ok(text) = std::str::from_utf8(&payload) {
                if text.contains("\"error\"") {
                    if has_content {
                        break;
                    }
                    // Return error JSON as-is
                    return Ok(text.to_string());
                }
            }
        }

        let result = extract_from_response(&payload)?;
        let frame = match result {
            None => continue,
            Some(f) => f,
        };

        // Handle error
        if let Some(err) = frame.error {
            return Ok(err);
        }

        // Handle tool call
        if let Some(tc) = frame.tool_call {
            let tc_id = tc["id"].as_str().unwrap_or("").to_string();
            let tc_args = tc["function"]["arguments"]
                .as_str()
                .unwrap_or("{}")
                .to_string();
            let is_last = tc.get("is_last").and_then(|v| v.as_bool()).unwrap_or(false);

            if let Some(existing) = tool_call_map.get_mut(&tc_id) {
                // Accumulate arguments
                let existing_args = existing["function"]["arguments"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                existing["function"]["arguments"] = json!(existing_args + &tc_args);
                if is_last {
                    finalized_ids.insert(tc_id);
                }
            } else {
                let mut new_tc = tc.clone();
                if is_last {
                    finalized_ids.insert(tc_id.clone());
                }
                tool_call_map.insert(tc_id, new_tc);
            }

            has_content = true;
        }

        // Handle text
        if let Some(text) = frame.text {
            if !text.is_empty() {
                total_content.push_str(&text);
                has_content = true;
            }
        }

        // Handle thinking
        if let Some(thinking) = frame.thinking {
            if !thinking.is_empty() {
                total_thinking.push_str(&thinking);

                // For composer models, extract visible content from thinking
                if is_composer_model(model) {
                    if let Some(visible) = visible_composer_content_from_thinking(&thinking) {
                        if !visible.is_empty() {
                            total_content.push_str(&visible);
                            has_content = true;
                        }
                    }
                }
            }
        }
    }

    // Build tool_calls array
    let mut tool_calls: Vec<Value> = Vec::new();
    for tc in tool_call_map.values() {
        let finalized_tc = serde_json::json!({
            "id": tc.get("id"),
            "type": "function",
            "function": {
                "name": tc.get("function").and_then(|f| f.get("name")),
                "arguments": tc.get("function").and_then(|f| f.get("arguments")),
            }
        });
        tool_calls.push(finalized_tc);
    }

    // Build the response
    let text_opt = if total_content.is_empty() {
        if total_thinking.is_empty() && tool_calls.is_empty() {
            None
        } else {
            None
        }
    } else {
        Some(total_content.as_str())
    };

    let thinking_opt = if total_thinking.is_empty() {
        None
    } else {
        Some(total_thinking.as_str())
    };

    if text_opt.is_none() && thinking_opt.is_none() && tool_calls.is_empty() {
        return Ok(String::new());
    }

    let response = build_chat_completion_response(
        &response_id,
        created,
        model,
        text_opt,
        thinking_opt,
        tool_calls,
    );

    Ok(serde_json::to_string(&response).unwrap_or_default())
}
pub fn parse_cursor_sse_events(data: &[u8]) -> Result<Vec<SseEvent>, CursorExecutorError> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let mut events = Vec::new();
    let mut offset = 0;

    let mut tool_call_accumulators: HashMap<String, Value> = HashMap::new();
    let mut finalized_ids: HashSet<String> = HashSet::new();

    while offset < data.len() {
        // Try to parse a Connect-RPC frame
        match parse_connect_rpc_frame(&data[offset..])? {
            Some((flags, payload)) => {
                let raw_length = u32::from_be_bytes([
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                    data[offset + 4],
                ]) as usize;
                offset += 5 + raw_length;

                // Decompress if needed
                let decompressed_payload = decompress_payload(&payload, flags);

                if decompressed_payload.is_empty() {
                    continue; // skip
                }

                if let Ok(Some(frame)) = extract_from_response(&decompressed_payload) {
                    if let Some(text) = frame.text {
                        if !text.is_empty() {
                            events.push(SseEvent::Text(text));
                        }
                    }
                    if let Some(thinking) = frame.thinking {
                        if !thinking.is_empty() {
                            events.push(SseEvent::Thinking(thinking));
                        }
                    }
                    if let Some(tc) = frame.tool_call {
                        events.push(SseEvent::ToolCall(tc));
                    }
                }
            }
            None => {
                // Try regular SSE parsing as fallback
                if let Ok(text) = std::str::from_utf8(&data[offset..]) {
                    for line in text.lines() {
                        if line.starts_with("data: ") {
                            let data_content = line.trim_start_matches("data: ");
                            if !data_content.is_empty() && data_content != "[DONE]" {
                                events.push(SseEvent::Raw(data_content.to_string()));
                            }
                        }
                    }
                }
                break;
            }
        }
    }

    Ok(events)
}

#[derive(Debug, Clone)]
pub enum SseEvent {
    Text(String),
    Thinking(String),
    ToolCall(Value),
    Raw(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_varint() {
        // Test basic varint encoding
        assert_eq!(encode_varint(0), vec![0]);
        assert_eq!(encode_varint(127), vec![127]);
        assert_eq!(encode_varint(128), vec![0x80, 0x01]);
        assert_eq!(encode_varint(300), vec![0xAC, 0x02]);
    }

    #[test]
    fn test_encode_field_len() {
        let data = b"hello";
        let encoded = encode_field_len(1, proto_fields::WIRE_LEN, data);
        // Tag 1 << 3 | 2 = 10 = 0x0A, then length 5, then "hello"
        assert_eq!(encoded, vec![0x0A, 0x05, 0x68, 0x65, 0x6C, 0x6C, 0x6F]);
    }

    #[test]
    fn test_encode_field_varint() {
        let encoded = encode_field_varint(1, proto_fields::WIRE_VARINT, 100);
        // Tag 1 << 3 | 0 = 8 = 0x08, then value 100 = 0x64
        assert_eq!(encoded, vec![0x08, 0x64]);
    }

    #[test]
    fn test_wrap_connect_rpc_frame() {
        let payload = b"test payload";
        let framed = wrap_connect_rpc_frame(payload, false);

        assert_eq!(framed.len(), 5 + payload.len());
        assert_eq!(framed[0], 0x00); // flags
        assert_eq!(
            u32::from_be_bytes([framed[1], framed[2], framed[3], framed[4]]),
            payload.len() as u32
        );
        assert_eq!(&framed[5..], payload);
    }

    #[test]
    fn test_wrap_connect_rpc_frame_compressed() {
        let payload = b"compressed data";
        let framed = wrap_connect_rpc_frame(payload, true);

        assert_eq!(framed[0], 0x01); // compressed flags
    }

    #[test]
    fn test_generate_cursor_checksum() {
        let checksum = generate_cursor_checksum("test-machine-id");
        assert!(!checksum.is_empty());
    }

    #[test]
    fn test_generate_machine_id() {
        let machine_id = generate_machine_id("test-token");
        // Should be 64-character hex string (SHA-256)
        assert_eq!(machine_id.len(), 64);
    }

    #[test]
    fn test_generate_session_id() {
        let session_id = generate_session_id("test-token");
        // Now UUIDv5 (hyphenated), which is 36 chars like "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
        assert_eq!(session_id.len(), 36);
    }

    #[test]
    fn test_generate_client_key() {
        let client_key = generate_client_key("test-token");
        // Should be 64-character hex string (SHA-256)
        assert_eq!(client_key.len(), 64);
    }

    #[test]
    fn test_parse_cursor_model_with_prefix() {
        assert_eq!(
            CursorExecutor::parse_cursor_model("cursor/claude-4.6-opus-max"),
            "claude-4.6-opus-max"
        );
        assert_eq!(
            CursorExecutor::parse_cursor_model("cursor/gpt-5.3-codex"),
            "gpt-5.3-codex"
        );
        assert_eq!(
            CursorExecutor::parse_cursor_model("cursor/kimi-k2.5"),
            "kimi-k2.5"
        );
    }

    #[test]
    fn test_parse_cursor_model_without_prefix() {
        assert_eq!(
            CursorExecutor::parse_cursor_model("claude-4.6-opus-max"),
            "claude-4.6-opus-max"
        );
        assert_eq!(
            CursorExecutor::parse_cursor_model("gpt-5.3-codex"),
            "gpt-5.3-codex"
        );
    }

    #[test]
    fn test_format_tool_name() {
        assert_eq!(format_tool_name("Read"), "mcp_custom_Read");
        assert_eq!(format_tool_name("mcp__server__Write"), "mcp_server_Write");
        assert_eq!(format_tool_name("mcp_custom_Tool"), "mcp_custom_Tool");
    }

    #[test]
    fn test_parse_tool_id() {
        let (tc_id, mc_id) = parse_tool_id("call_abc123\nmc_model456");
        assert_eq!(tc_id, "call_abc123");
        assert_eq!(mc_id, Some("model456".to_string()));

        let (tc_id2, mc_id2) = parse_tool_id("call_xyz789");
        assert_eq!(tc_id2, "call_xyz789");
        assert_eq!(mc_id2, None);
    }

    #[test]
    fn test_build_cursor_headers() {
        let headers =
            build_cursor_headers("Bearer test-token", Some("test-machine-id"), true).unwrap();
        assert!(headers.contains_key("authorization"));
        assert!(headers.contains_key("content-type"));
        assert!(headers.contains_key("x-cursor-checksum"));
        assert!(headers.contains_key("x-session-id"));
    }

    #[test]
    fn test_build_cursor_headers_with_prefixed_token() {
        // Token with prefix like "cursor::abc123"
        let headers =
            build_cursor_headers("cursor::abc123", Some("test-machine-id"), true).unwrap();
        let auth = headers.get("authorization").unwrap();
        // Should use the part after "::"
        assert_eq!(auth.to_str().unwrap(), "Bearer abc123");
    }

    #[test]
    fn test_build_cursor_headers_ghost_mode_false() {
        let headers = build_cursor_headers("test-token", Some("test-machine-id"), false).unwrap();
        let ghost = headers.get("x-ghost-mode").unwrap();
        assert_eq!(ghost.to_str().unwrap(), "false");
    }

    #[test]
    fn test_build_cursor_headers_without_machine_id_falls_back() {
        // When machine_id is None, it falls back to token-derived hash
        let headers = build_cursor_headers("test-token", None, true).unwrap();
        assert!(headers.contains_key("x-cursor-checksum"));
    }

    #[test]
    fn test_encode_message_roundtrip() {
        let content = "Hello, world!";
        let role = proto_fields::ROLE_USER;
        let message_id = "test-message-id";
        let is_last = true;
        let has_tools = false;
        let tool_results: Vec<Value> = vec![];

        let encoded = encode_message(content, role, message_id, is_last, has_tools, &tool_results);
        // Verify encoding produces non-empty output
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_concat_arrays() {
        let arrays: &[&[u8]] = &[&[1, 2, 3], &[4, 5, 6], &[7, 8, 9]];
        let result = concat_arrays(arrays);
        assert_eq!(result, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn test_encode_model() {
        let encoded = encode_model("claude-sonnet-4.5");
        // Verify encoding produces non-empty output
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_cursor_setting() {
        let encoded = encode_cursor_setting();
        // Verify encoding produces non-empty output
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_chat_request_structure() {
        let messages = vec![serde_json::json!({"role": "user", "content": "Hello"})];
        let tools: Vec<Value> = vec![];
        let result = build_chat_request(&messages, "claude-3.5-sonnet", &tools, None, false);
        // Verify request building produces output
        assert!(!result.is_empty());
    }

    #[test]
    fn test_parse_connect_rpc_frame() {
        let payload = b"test payload";
        let framed = wrap_connect_rpc_frame(payload, false);

        let result = parse_connect_rpc_frame(&framed).unwrap();
        assert!(result.is_some());

        let (flags, decoded_payload) = result.unwrap();
        assert_eq!(flags, 0x00);
        assert_eq!(decoded_payload, payload);
    }

    #[test]
    fn test_parse_connect_rpc_frame_insufficient_data() {
        let result = parse_connect_rpc_frame(b"short").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_from_response_no_match() {
        // Empty payload should not crash
        let result = extract_from_response(b"");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_parse_cursor_sse_events_empty() {
        let events = parse_cursor_sse_events(b"").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_cursor_executor_new() {
        let executor = CursorExecutor::new(Arc::new(ClientPool::new()), None);
        assert!(executor.is_ok());
    }

    #[test]
    fn test_cursor_executor_pool_accessor() {
        let pool = Arc::new(ClientPool::new());
        let executor = CursorExecutor::new(pool.clone(), None).unwrap();
        assert!(Arc::ptr_eq(executor.pool(), &pool));
    }

    #[test]
    fn test_extract_tools_empty() {
        let body = serde_json::json!({"messages": []});
        let tools = CursorExecutor::extract_tools(&body);
        assert!(tools.is_empty());
    }

    #[test]
    fn test_extract_tools_with_tools() {
        let body = serde_json::json!({
            "messages": [],
            "tools": [
                {"type": "function", "function": {"name": "test", "description": "A test function"}}
            ]
        });
        let tools = CursorExecutor::extract_tools(&body);
        assert_eq!(tools.len(), 1);
    }

    #[test]
    fn test_extract_reasoning_effort() {
        let body_with = serde_json::json!({"reasoning_effort": "high"});
        let body_without = serde_json::json!({});

        assert_eq!(
            CursorExecutor::extract_reasoning_effort(&body_with),
            Some("high".to_string())
        );
        assert_eq!(
            CursorExecutor::extract_reasoning_effort(&body_without),
            None
        );
    }

    #[test]
    fn test_cursor_executor_error_from_io() {
        let io_err = std::io::Error::other("test");
        let executor_err: CursorExecutorError = io_err.into();
        assert!(matches!(
            executor_err,
            CursorExecutorError::HyperClientInit(_)
        ));
    }

    // ---- AgentService increment (bead .34) ----

    #[test]
    fn test_is_agent_text_request() {
        use serde_json::json;
        // All-string messages → true.
        let body = json!({
            "messages": [
                { "role": "user", "content": "hello" },
                { "role": "assistant", "content": "hi" }
            ]
        });
        assert!(is_agent_text_request(&body));

        // Text-only content arrays → true.
        let body2 = json!({
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "hello" },
                    { "type": "text", "text": "world" }
                ] }
            ]
        });
        assert!(is_agent_text_request(&body2));

        // A message with tool_calls → false.
        let body3 = json!({
            "messages": [
                { "role": "user", "content": "hello" },
                { "role": "assistant", "content": "ok", "tool_calls": [{"id":"t1"}] }
            ]
        });
        assert!(!is_agent_text_request(&body3));

        // A message with role "tool" → false.
        let body4 = json!({
            "messages": [
                { "role": "user", "content": "hello" },
                { "role": "tool", "content": "result", "tool_call_id": "t1" }
            ]
        });
        assert!(!is_agent_text_request(&body4));

        // A non-text content part (e.g. image) → false.
        let body5 = json!({
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "hello" },
                    { "type": "image_url", "image_url": {"url": "x"} }
                ] }
            ]
        });
        assert!(!is_agent_text_request(&body5));

        // No messages → false.
        assert!(!is_agent_text_request(&json!({ "model": "gpt" })));
    }

    #[test]
    fn test_build_agent_run_frame_has_request_context_fields() {
        use serde_json::json;
        let messages = vec![
            json!({ "role": "system", "content": "You are a helpful assistant." }),
            json!({ "role": "user", "content": "What is 2+2?" }),
        ];
        let frame = build_agent_run_frame(&messages, "gpt-4o");
        // Connect-RPC frame: 1 flag byte + 4-byte BE length + payload.
        assert!(frame.len() > 5);
        assert_eq!(frame[0], 0); // uncompressed
        let payload_len = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
        assert_eq!(frame.len(), 5 + payload_len);
        let payload = &frame[5..];

        // The payload is agent.v1.AgentClientMessage.run_request (field 1).
        // Field 9 requestedModel encodes a varint bool true (field 7).
        // 9router buildAgentRunFrame sets agentBool(7, true) — the encoded
        // bytes for field 9's inner requestedModel contain tag(7,varint)=0x38.
        assert!(
            payload.windows(2).any(|w| w == [0x38, 0x01]),
            "run_request should contain requestedModel bool true (tag 7 varint): {payload:?}"
        );
        // Field 9 requestedModel must be present (tag 9 length-delimited = 0x4A).
        assert!(
            payload.windows(1).any(|w| w[0] == 0x4A),
            "run_request should contain field 9 (requestedModel): {payload:?}"
        );
    }

    #[test]
    fn test_create_request_context_response_shape() {
        // 9router createRequestContextResponse: execClientMessage (field 2)
        // wrapping field 10 → requestContextResult → field 1 → empty message.
        let frame = create_request_context_response();
        assert!(frame.len() >= 5);
        assert_eq!(frame[0], 0, "uncompressed");
        let len = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
        assert_eq!(frame.len(), 5 + len);
        let payload = &frame[5..];
        // agent.v1.AgentClientMessage.exec_client_message = field 2 (0x12).
        assert!(payload.starts_with(&[0x12]));
        // The nested message must contain field 10 (0x52) → field 1 (0x0A).
        assert!(
            payload.windows(2).any(|w| w == [0x52, 0x02])
                || payload.windows(1).any(|w| w[0] == 0x52),
            "execClientMessage should wrap requestContext (field 10): {payload:?}"
        );
        assert!(
            payload.windows(1).any(|w| w[0] == 0x0A),
            "requestContextResult should wrap empty message (field 1): {payload:?}"
        );
    }

    #[test]
    fn test_agent_frame_decode_text_and_done() {
        use serde_json::json;
        // Craft an agent.v1.AgentServerMessage:
        //   field 1 (interaction_update) → field 1 → field 1 = "hello world"
        //   field 1 → field 14 (empty) = reasoning marker → finished.
        let text_delta = pb_len(1, b"hello world");
        let update = concat_arrays(&[&pb_len(1, &text_delta), &pb_len(14, &[])]);
        let server_message = pb_len(1, &update);
        let frame = wrap_connect_rpc_frame(&server_message, false);

        let mut finished = false;
        let mut events = Vec::new();
        let leftover = consume_agent_frames(&frame, &mut finished, &mut events);
        assert!(leftover.is_empty());
        assert!(finished, "field 14 reasoning must finish the turn");
        assert_eq!(
            events,
            vec![
                AgentEvent::Text("hello world".to_string()),
                AgentEvent::Done
            ],
            "events: {events:?}"
        );
    }

    #[test]
    fn test_agent_frame_decode_gzip_and_request_context() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        // agent.v1.AgentServerMessage.exec_server_message (field 2) → field 10
        // (requestContext) → triggers the RequestContext handshake event.
        let request_context = pb_len(10, &[]);
        let exec_server_message = pb_len(2, &request_context);
        let gz = {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&exec_server_message).unwrap();
            encoder.finish().unwrap()
        };
        let mut frame = vec![CONNECT_RPC_FLAG_COMPRESSED];
        frame.extend_from_slice(&(gz.len() as u32).to_be_bytes());
        frame.extend_from_slice(&gz);

        let mut finished = false;
        let mut events = Vec::new();
        let leftover = consume_agent_frames(&frame, &mut finished, &mut events);
        assert!(leftover.is_empty());
        assert_eq!(events, vec![AgentEvent::RequestContext]);

        // A non-requestContext exec message (e.g. field 3 = editor tool) is an
        // unsupported-IDE-tool error that ends the turn.
        let tool_message = pb_len(3, &[]);
        let exec2 = pb_len(2, &tool_message);
        let frame2 = wrap_connect_rpc_frame(&exec2, false);
        let mut finished2 = false;
        let mut events2 = Vec::new();
        consume_agent_frames(&frame2, &mut finished2, &mut events2);
        assert!(finished2);
        assert_eq!(
            events2,
            vec![AgentEvent::Error(
                "Cursor AgentService requested an unsupported IDE tool".to_string(),
                false,
            )]
        );
    }
}
