use std::sync::Arc;

use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const CURSOR_API_ENDPOINT: &str =
    "https://agentn.api5.cursor.sh/aiserver.v1.ChatService/StreamUnifiedChatWithTools";

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

/// Format tool name: "toolName" → "mcp_custom_toolName"
fn format_tool_name(name: &str) -> String {
    if name.is_empty() {
        return "mcp_custom_tool".to_string();
    }
    if name.starts_with("mcp__") {
        let rest = &name[4..];
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

    // Parse tool name to get selected tool (for MCP result)
    let formatted_name = format_tool_name(tool_name);
    let selected_tool = if let Some(tail) = formatted_name.strip_prefix("mcp_") {
        if let Some(idx) = tail.find('_') {
            tail[idx + 1..].to_string()
        } else {
            tail.to_string()
        }
    } else {
        formatted_name.clone()
    };

    let name_bytes = formatted_name.as_bytes();

    concat_arrays(&[
        &encode_field_len(
            proto_fields::FLD_TOOL_RESULT_CALL_ID,
            proto_fields::WIRE_LEN,
            tc_id.as_bytes(),
        ),
        &encode_field_len(
            proto_fields::FLD_TOOL_RESULT_NAME,
            proto_fields::WIRE_LEN,
            name_bytes,
        ),
        &encode_field_varint(
            proto_fields::FLD_TOOL_RESULT_INDEX,
            proto_fields::WIRE_VARINT,
            tool_index,
        ),
        &encode_field_len(
            proto_fields::FLD_TOOL_RESULT_RAW_ARGS,
            proto_fields::WIRE_LEN,
            raw_args.as_bytes(),
        ),
        &encode_field_len(
            proto_fields::FLD_TOOL_RESULT_RESULT,
            proto_fields::WIRE_LEN,
            &encode_client_side_tool_v2_result(
                &tc_id,
                mc_id.as_deref(),
                &selected_tool,
                result_content,
                tool_index,
            ),
        ),
    ])
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

            // Avoid inserting duplicate if next message already has same results
            if let Some(next) = messages.get(i + 1) {
                let next_tr = next.get("tool_results").and_then(|v| v.as_array());
                let next_has_tr = next_tr.map(|a| !a.is_empty()).unwrap_or(false);
                if !next_has_tr {
                    let result_msg = serde_json::json!({
                        "role": "assistant",
                        "content": "",
                        "tool_results": tool_results
                    });
                    normalized_messages.push(result_msg);
                }
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

/// Generate cursor checksum (JYH cipher)
fn generate_cursor_checksum(machine_id: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Get Unix timestamp in microseconds
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;

    // Create byte array from timestamp (6 bytes, big-endian)
    let mut byte_array = [
        ((timestamp >> 40) & 0xFF) as u8,
        ((timestamp >> 32) & 0xFF) as u8,
        ((timestamp >> 24) & 0xFF) as u8,
        ((timestamp >> 16) & 0xFF) as u8,
        ((timestamp >> 8) & 0xFF) as u8,
        (timestamp & 0xFF) as u8,
    ];

    // Jyh cipher obfuscation
    let mut t: u8 = 165;
    for i in 0..byte_array.len() {
        byte_array[i] = (byte_array[i] ^ t).wrapping_add(i as u8);
        t = byte_array[i];
    }

    // URL-safe base64 encode (without padding)
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::new();

    for chunk in byte_array.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);

        encoded.push(ALPHABET[(a >> 2) as usize] as char);
        encoded.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);

        if chunk.len() > 1 {
            encoded.push(ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        }
        if chunk.len() > 2 {
            encoded.push(ALPHABET[(c & 63) as usize] as char);
        }
    }

    format!("{}{}", encoded, machine_id)
}

/// Generate machine ID from token using SHA-256
fn generate_machine_id(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.update(b"machineId");
    hex::encode(hasher.finalize())
}

/// Generate session ID using UUID v5
fn generate_session_id(token: &str) -> String {
    // Use simple deterministic ID based on token hash
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(token.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}

/// Generate client key using SHA-256
fn generate_client_key(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Build Cursor API headers
fn build_cursor_headers(access_token: &str) -> Result<HeaderMap, CursorExecutorError> {
    use sha2::Digest;

    // Clean token if it has prefix
    let clean_token = if access_token.contains("::") {
        access_token.split("::").nth(1).unwrap_or(access_token)
    } else {
        access_token
    };

    // Generate derived values
    let machine_id = generate_machine_id(clean_token);
    let session_id = generate_session_id(clean_token);
    let client_key = generate_client_key(clean_token);
    let checksum = generate_cursor_checksum(&machine_id);

    // Detect OS
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        _ => "linux",
    };

    // Detect architecture
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x64",
    };

    let mut headers = HeaderMap::new();

    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", clean_token))
            .map_err(CursorExecutorError::InvalidHeader)?,
    );
    headers.insert("connect-accept-encoding", HeaderValue::from_static("gzip"));
    headers.insert("connect-protocol-version", HeaderValue::from_static("1"));
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/connect+proto"),
    );
    headers.insert("user-agent", HeaderValue::from_static("connect-es/1.6.1"));
    headers.insert(
        "x-amzn-trace-id",
        HeaderValue::from_str(&format!("Root={}", uuid::Uuid::new_v4()))
            .map_err(CursorExecutorError::InvalidHeader)?,
    );
    headers.insert(
        "x-client-key",
        HeaderValue::from_str(&client_key).map_err(CursorExecutorError::InvalidHeader)?,
    );
    headers.insert(
        "x-cursor-checksum",
        HeaderValue::from_str(&checksum).map_err(CursorExecutorError::InvalidHeader)?,
    );
    headers.insert("x-cursor-client-version", HeaderValue::from_static("3.1.0"));
    headers.insert("x-cursor-client-type", HeaderValue::from_static("ide"));
    headers.insert("x-cursor-client-os", HeaderValue::from_static(os));
    headers.insert("x-cursor-client-arch", HeaderValue::from_static(arch));
    headers.insert(
        "x-cursor-client-device-type",
        HeaderValue::from_static("desktop"),
    );
    headers.insert(
        "x-cursor-config-version",
        HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
            .map_err(CursorExecutorError::InvalidHeader)?,
    );
    headers.insert("x-cursor-timezone", HeaderValue::from_static("UTC"));
    headers.insert("x-ghost-mode", HeaderValue::from_static("true"));
    headers.insert(
        "x-request-id",
        HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
            .map_err(CursorExecutorError::InvalidHeader)?,
    );
    headers.insert(
        "x-session-id",
        HeaderValue::from_str(&session_id).map_err(CursorExecutorError::InvalidHeader)?,
    );

    Ok(headers)
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
            buffer[*offset..*offset + len].to_vec()
        }
        proto_fields::WIRE_FIXED64 => {
            if *offset + 8 > buffer.len() {
                return Err(CursorExecutorError::ProtobufDecode(
                    "Unexpected end of buffer".to_string(),
                ));
            }
            buffer[*offset..*offset + 8].to_vec()
        }
        proto_fields::WIRE_FIXED32 => {
            if *offset + 4 > buffer.len() {
                return Err(CursorExecutorError::ProtobufDecode(
                    "Unexpected end of buffer".to_string(),
                ));
            }
            buffer[*offset..*offset + 4].to_vec()
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

    /// Transform the request body to Cursor protobuf format
    fn transform_request_body(
        &self,
        body: &Value,
        actual_model: &str,
    ) -> Result<Vec<u8>, CursorExecutorError> {
        let messages = Self::extract_messages(body)?;
        let tools = Self::extract_tools(body);
        let reasoning_effort = Self::extract_reasoning_effort(body);

        let protobuf = build_chat_request(
            &messages,
            actual_model,
            &tools,
            reasoning_effort.as_deref(),
            false,
        );
        let framed = wrap_connect_rpc_frame(&protobuf, false);

        Ok(framed)
    }

    pub async fn execute(
        &self,
        request: CursorExecutionRequest,
    ) -> Result<CursorExecutorResponse, CursorExecutorError> {
        let actual_model = Self::parse_cursor_model(&request.model);

        // Get access token from credentials
        let access_token = request.credentials.access_token.as_deref().ok_or_else(|| {
            CursorExecutorError::MissingCredentials("Cursor access token required".to_string())
        })?;

        let headers = build_cursor_headers(access_token)?;
        let body_bytes = self.transform_request_body(&request.body, &actual_model)?;

        let client = self.pool.get("cursor", request.proxy.as_ref())?;
        let response = client
            .post(CURSOR_API_ENDPOINT)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        Ok(CursorExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url: CURSOR_API_ENDPOINT.to_string(),
            headers,
            transformed_body: request.body,
            transport: TransportKind::Reqwest,
        })
    }
}

// ==================== DECODING HELPERS ====================

/// Extract text and tool call from a response payload
fn extract_from_response(
    payload: &[u8],
) -> Result<Option<(String, Option<Value>)>, CursorExecutorError> {
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
                return Ok(Some((String::new(), Some(tool_call))));
            }
        }
    }

    // Field 2: StreamUnifiedChatResponse (text)
    if let Some(values) = fields.get(&proto_fields::FLD_RESPONSE) {
        if let Some(data) = values.first() {
            let response_fields = decode_message(data)?;

            // Extract text
            if let Some(text_values) = response_fields.get(&proto_fields::FLD_RESPONSE_TEXT) {
                if let Some(text_data) = text_values.first() {
                    if let Ok(text_str) = std::str::from_utf8(text_data) {
                        return Ok(Some((text_str.to_string(), None)));
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Parse SSE events from Cursor streaming response
pub fn parse_cursor_sse_events(data: &[u8]) -> Result<Vec<SseEvent>, CursorExecutorError> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let mut events = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        // Try to parse a Connect-RPC frame
        match parse_connect_rpc_frame(&data[offset..])? {
            Some((_flags, payload)) => {
                if let Ok(Some((text, tool_call))) = extract_from_response(&payload) {
                    if !text.is_empty() {
                        events.push(SseEvent::Text(text));
                    }
                    if let Some(tc) = tool_call {
                        events.push(SseEvent::ToolCall(tc));
                    }
                }
                offset += 5 + u32::from_be_bytes([
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                    data[offset + 4],
                ]) as usize;
            }
            None => {
                // Try regular SSE parsing
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
        // Should be a non-empty hex string
        assert!(!session_id.is_empty());
        assert!(session_id.chars().all(|c| c.is_ascii_hexdigit()));
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
        assert_eq!(format_tool_name("mcp__server__Write"), "mcp__server_Write");
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
        let headers = build_cursor_headers("Bearer test-token").unwrap();
        assert!(headers.contains_key("authorization"));
        assert!(headers.contains_key("content-type"));
        assert!(headers.contains_key("x-cursor-checksum"));
        assert!(headers.contains_key("x-session-id"));
    }

    #[test]
    fn test_build_cursor_headers_with_prefixed_token() {
        // Token with prefix like "cursor::abc123"
        let headers = build_cursor_headers("cursor::abc123").unwrap();
        let auth = headers.get("authorization").unwrap();
        // Should use the part after "::"
        assert_eq!(auth.to_str().unwrap(), "Bearer abc123");
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
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "test");
        let executor_err: CursorExecutorError = io_err.into();
        assert!(matches!(
            executor_err,
            CursorExecutorError::HyperClientInit(_)
        ));
    }
}
