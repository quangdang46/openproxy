use std::sync::Arc;
use std::time::Duration;

use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

#[derive(Clone)]
#[allow(dead_code)]
pub struct CodexExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum CodexExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    InvalidUri(InvalidUri),
    InvalidRequest(http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    StreamingResponseFailed(String),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for CodexExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for CodexExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for CodexExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<http::Error> for CodexExecutorError {
    fn from(error: http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for CodexExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for CodexExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for CodexExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

pub struct CodexExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct CodexExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for CodexExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

impl CodexExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, CodexExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// Parse Codex model string to extract actual OpenAI model name.
    ///
    /// Examples:
    /// - "codex/o4-mini" → "o4-mini"
    /// - "codex/o4-mini-high" → "o4-mini-high"
    /// - "codex/o3" → "o3"
    /// - "codex/o3-mini" → "o3-mini"
    /// - "o4-mini" → "o4-mini" (no prefix)
    pub fn parse_codex_model(model: &str) -> String {
        if let Some(stripped) = model.strip_prefix("codex/") {
            stripped.to_string()
        } else {
            model.to_string()
        }
    }

    /// Build the URL for Codex Responses API at chatgpt.com.
    ///
    /// When the model name ends with `_compact` or the `provider_node`
    /// carries a custom field `"_compact": true`, the `/compact` suffix
    /// is appended to reduce response size.
    fn build_url(&self, model: &str) -> String {
        let base = CODEX_RESPONSES_URL.trim_end_matches('/').to_string();
        let is_compact_model = model.ends_with("_compact");
        let is_compact_node = self
            .provider_node
            .as_ref()
            .and_then(|n| n.extra.get("_compact"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_compact_model || is_compact_node {
            format!("{}/compact", base)
        } else {
            base
        }
    }

    /// Build request headers for Codex Responses API.
    fn build_headers(
        &self,
        api_key: &str,
        stream: bool,
        connection_id: Option<&str>,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, CodexExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(CodexExecutorError::InvalidHeader)?,
        );

        // 9router parity: session_id header for request session continuity.
        let session_id = connection_id
            .filter(|&cid| !cid.is_empty())
            .unwrap_or("default");
        headers.insert(
            "session_id",
            HeaderValue::from_str(session_id).map_err(CodexExecutorError::InvalidHeader)?,
        );

        // 9router parity: identify client type to Codex backend.
        headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));

        // 9router parity: workspace binding for account scope + cache affinity.
        {
            let ws_id = credentials
                .provider_specific_data
                .get("workspaceId")
                .or_else(|| credentials.provider_specific_data.get("chatgptAccountId"))
                .and_then(|v| v.as_str())
                .or(connection_id);
            if let Some(ws) = ws_id {
                headers.insert(
                    "chatgpt-account-id",
                    HeaderValue::from_str(ws).map_err(CodexExecutorError::InvalidHeader)?,
                );
            }
        }

        if stream {
            headers.insert("Accept", HeaderValue::from_static("text/event-stream"));
        }

        Ok(headers)
    }

    /// Default instructions injected when none are present in the request body.
    const DEFAULT_CODEX_INSTRUCTIONS: &'static str =
        "You are a highly capable coding agent. Use the tools and instructions provided to fulfill the user's request.";

    /// Transform the request body from Chat Completions format to Codex Responses API format.
    ///
    /// Handles both pre-translated bodies (input[] array from `chat_to_openai_responses_request`)
    /// and untranslated OpenAI bodies (messages[] array) — this avoids double-translation bugs
    /// when the pipeline already ran request translation before calling the executor.
    ///
    /// The Codex Responses API at chatgpt.com uses `input` as an array of message items.
    /// This function:
    /// - Converts messages[] to input[] with type "message", role, and content as input_text blocks
    /// - Converts "system" role to "developer"
    /// - Strips server-generated IDs (rs_, fc_, resp_, msg_ prefixes) to avoid 404s with store:false
    /// 9router codex.js parity:
    /// - Forces stream: true (Codex backend; client JSON via forceStream SSE→JSON)
    /// - Forces store: false
    /// - Strips effort suffixes from model (`-high`, `-medium`, …) into reasoning.effort
    /// - Injects instructions default when missing
    fn transform_request_body(
        &self,
        body: &Value,
        actual_model: &str,
        _stream: bool,
    ) -> Result<Value, CodexExecutorError> {
        // Handle both pre-translated (input[]) and untranslated (messages[]) bodies
        let input_items = if let Some(input) = body.get("input").and_then(Value::as_array) {
            if input.is_empty() {
                return Err(CodexExecutorError::UnsupportedFormat(
                    "Empty input array in request body".to_string(),
                ));
            }
            input.clone()
        } else {
            Self::extract_input_items(body)?
        };

        let instructions = body
            .get("instructions")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(Self::DEFAULT_CODEX_INSTRUCTIONS);

        // Strip effort suffix from model name (9router: none/minimal/low/medium/high/xhigh)
        let effort_levels = ["none", "minimal", "low", "medium", "high", "xhigh"];
        let mut model_id = actual_model.to_string();
        let mut model_effort: Option<&str> = None;
        for level in effort_levels {
            let suffix = format!("-{level}");
            if let Some(stripped) = model_id.strip_suffix(&suffix) {
                model_id = stripped.to_string();
                model_effort = Some(level);
                break;
            }
            // Also support model(high) style
            let paren = format!("({level})");
            if let Some(idx) = model_id.rfind(&paren) {
                model_id = model_id[..idx].trim_end().to_string();
                model_effort = Some(level);
                break;
            }
        }

        // Priority: body.reasoning.effort > reasoning_effort > model suffix > default low
        let effort = body
            .pointer("/reasoning/effort")
            .and_then(Value::as_str)
            .or_else(|| body.get("reasoning_effort").and_then(Value::as_str))
            .or(model_effort)
            .unwrap_or("low");

        let mut request_body = json!({
            "model": model_id,
            "input": input_items,
            "instructions": instructions,
            "stream": true, // 9router always forces stream
            "store": false,
            "reasoning": { "effort": effort, "summary": "auto" },
        });

        if let Some(tools) = body.get("tools") {
            request_body["tools"] = tools.clone();
        }
        if let Some(tool_choice) = body.get("tool_choice") {
            request_body["tool_choice"] = tool_choice.clone();
        }
        if let Some(include) = body.get("include") {
            request_body["include"] = include.clone();
        }
        if let Some(prompt_cache_key) = body.get("prompt_cache_key") {
            request_body["prompt_cache_key"] = prompt_cache_key.clone();
        }

        Ok(request_body)
    }

    /// Extract input items array from a Chat Completions style request body.
    ///
    /// Returns a Vec of Responses API input items, where each message becomes:
    /// ```json
    /// {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Hello"}]}
    /// ```
    ///
    /// - "system" role is converted to "developer"
    /// - Server-generated IDs (prefixes rs_, fc_, resp_, msg_) are stripped
    /// - String content is wrapped in an input_text array
    /// - Content arrays have text parts converted to input_text type
    fn extract_input_items(body: &Value) -> Result<Vec<Value>, CodexExecutorError> {
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CodexExecutorError::UnsupportedFormat("Missing messages array".to_string())
            })?;

        if messages.is_empty() {
            return Err(CodexExecutorError::UnsupportedFormat(
                "No messages found in request body".to_string(),
            ));
        }

        let mut items: Vec<Value> = Vec::new();

        for msg in messages {
            let mut role = msg
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_string();

            // Convert "system" role to "developer" (Responses API convention)
            if role == "system" {
                role = "developer".to_string();
            }

            // Extract and transform content into Responses API format
            let content_arr: Value = match msg.get("content") {
                Some(Value::String(s)) => {
                    if s.is_empty() {
                        continue;
                    }
                    json!([{"type": "input_text", "text": s}])
                }
                Some(Value::Array(arr)) => {
                    if arr.is_empty() {
                        continue;
                    }
                    let mut parts: Vec<Value> = Vec::new();
                    for part in arr {
                        let part_type = part.get("type").and_then(Value::as_str).unwrap_or("text");
                        // Convert "text" type to "input_text" for Responses API
                        if part_type == "text" {
                            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.is_empty() {
                                parts.push(json!({"type": "input_text", "text": text}));
                            }
                        } else {
                            // Pass through other content types (image_url, etc.)
                            parts.push(part.clone());
                        }
                    }
                    json!(parts)
                }
                _ => continue,
            };

            // Build the input item
            let mut item = json!({
                "type": "message",
                "role": role,
                "content": content_arr,
            });

            // Strip server-generated IDs (prefixes rs_, fc_, resp_, msg_)
            // Keep user-provided IDs that don't match these patterns
            if let Some(id) = msg.get("id").and_then(Value::as_str) {
                let is_server_id = id.starts_with("rs_")
                    || id.starts_with("fc_")
                    || id.starts_with("resp_")
                    || id.starts_with("msg_");
                if !is_server_id {
                    item["id"] = json!(id);
                }
            }

            // Preserve "name" field if present
            if let Some(name) = msg.get("name").and_then(Value::as_str) {
                if !name.is_empty() {
                    item["name"] = json!(name);
                }
            }

            items.push(item);
        }

        if items.is_empty() {
            return Err(CodexExecutorError::UnsupportedFormat(
                "No valid content found in messages".to_string(),
            ));
        }

        Ok(items)
    }

    pub async fn execute(
        &self,
        request: CodexExecutionRequest,
    ) -> Result<CodexExecutorResponse, CodexExecutorError> {
        let actual_model = Self::parse_codex_model(&request.model);
        let url = self.build_url(&actual_model);

        // Get API key from credentials (try api_key first, then access_token for OAuth)
        let api_key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .ok_or_else(|| {
                CodexExecutorError::MissingCredentials("API key required".to_string())
            })?;

        let connection_id = request
            .credentials
            .email
            .as_deref()
            .or(request.credentials.id.as_str().into())
            .or(request.credentials.display_name.as_deref());
        // Always stream upstream (9router force stream); client JSON via chat sse_to_json
        let headers =
            self.build_headers(api_key, true, connection_id, &request.credentials)?;
        let transformed_body =
            self.transform_request_body(&request.body, &actual_model, true)?;

        let client = self.pool.get("openai", request.proxy.as_ref())?;

        // Retry up to 3 times with exponential backoff when the response
        // body (first 4096 bytes) contains "server_is_overloaded" or
        // "service_unavailable_error" (transient overload errors inside 200 OK).
        const MAX_RETRIES: usize = 3;
        for attempt in 0..MAX_RETRIES {
            let resp = client
                .post(&url)
                .headers(headers.clone())
                .json(&transformed_body)
                .send()
                .await?;

            // Capture parts before consuming the body.
            let status = resp.status();
            let resp_headers = resp.headers().clone();

            // Read the full body so we can inspect it for overload errors
            // and then reconstruct the response downstream.
            let body_bytes = resp.bytes().await?;

            // Peek at the first 4096 bytes for overload indicators,
            // but only when we still have retries remaining.
            if attempt + 1 < MAX_RETRIES {
                let peek_end = body_bytes.len().min(4096);
                let head_str = String::from_utf8_lossy(&body_bytes[..peek_end]);
                if head_str.contains("server_is_overloaded")
                    || head_str.contains("service_unavailable_error")
                {
                    let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                    tokio::time::sleep(delay).await;
                    continue;
                }
            }

            // Reconstruct the reqwest::Response from its parts so the body
            // remains available for downstream consumers (bytes_stream / text / bytes).
            let mut http_resp = http::Response::new(ReqwestBody::from(body_bytes));
            *http_resp.status_mut() = status;
            *http_resp.headers_mut() = resp_headers;
            let reconstructed = reqwest::Response::from(http_resp);

            return Ok(CodexExecutorResponse {
                response: UpstreamResponse::Reqwest(reconstructed),
                url,
                headers,
                transformed_body,
                transport: TransportKind::Reqwest,
            });
        }

        Err(CodexExecutorError::StreamingResponseFailed(
            "max retries exhausted for overloaded SSE response".into(),
        ))
    }
}

/// Convert OpenAI Responses API SSE format to standard SSE format.
///
/// OpenAI Responses API returns events like:
/// - `event: response.done\ndata: {...}\n\n`
/// - `event: content.delta\ndata: {"type": "content.delta", "delta": {"type": "text_delta", "text": "Hello"}}\n\n`
///
/// We need to convert to standard format:
/// - `data: {"type": "content.delta", ...}\n\n`
pub fn convert_openai_sse_to_standard(input: &[u8]) -> Vec<u8> {
    if input.is_empty() {
        return Vec::new();
    }

    let input_str = String::from_utf8_lossy(input);
    let mut output = Vec::new();

    for line in input_str.lines() {
        // Skip the event: line, keep only data: lines
        if line.starts_with("data: ") {
            // Extract data content
            let data_content = line.trim_start_matches("data: ");
            // Output in standard SSE format
            output.extend_from_slice(b"data: ");
            output.extend_from_slice(data_content.as_bytes());
            output.extend_from_slice(b"\n\n");
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_codex_model_with_prefix() {
        assert_eq!(CodexExecutor::parse_codex_model("codex/o4-mini"), "o4-mini");
        assert_eq!(
            CodexExecutor::parse_codex_model("codex/o4-mini-high"),
            "o4-mini-high"
        );
        assert_eq!(CodexExecutor::parse_codex_model("codex/o3"), "o3");
        assert_eq!(CodexExecutor::parse_codex_model("codex/o3-mini"), "o3-mini");
    }

    #[test]
    fn test_parse_codex_model_without_prefix() {
        assert_eq!(CodexExecutor::parse_codex_model("o4-mini"), "o4-mini");
        assert_eq!(CodexExecutor::parse_codex_model("gpt-4"), "gpt-4");
    }

    #[test]
    fn test_codex_request_body_format() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "user", "content": "Hello, world!"}
            ],
            "stream": false,
            "temperature": 0.7
        });

        let result = executor
            .transform_request_body(&chat_body, "o4-mini-high", false)
            .unwrap();

        assert_eq!(result["model"], "o4-mini"); // suffix stripped
        assert_eq!(result["stream"], true); // forced
        assert_eq!(result["store"], false);
        assert_eq!(result["reasoning"]["effort"], "high");
        assert!(
            result.get("temperature").is_none(),
            "temperature should be stripped by allowlist"
        );

        // input should be an array of Response API items
        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Hello, world!");

        // instructions should be injected
        assert_eq!(
            result["instructions"],
            CodexExecutor::DEFAULT_CODEX_INSTRUCTIONS
        );
    }

    #[test]
    fn test_codex_request_body_multiple_messages() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"},
                {"role": "user", "content": "How are you?"}
            ]
        });

        let result = executor
            .transform_request_body(&chat_body, "o4-mini", true)
            .unwrap();

        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "Hello");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["text"], "Hi there!");
        assert_eq!(input[2]["role"], "user");
        assert_eq!(input[2]["content"][0]["text"], "How are you?");
    }

    #[test]
    fn test_codex_request_body_converts_system_to_developer() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello!"}
            ]
        });

        let result = executor
            .transform_request_body(&chat_body, "o4-mini", true)
            .unwrap();

        let input = result["input"].as_array().unwrap();
        // "system" should now be "developer"
        assert_eq!(input[0]["role"], "developer");
        assert_eq!(
            input[0]["content"][0]["text"],
            "You are a helpful assistant."
        );
        assert_eq!(input[1]["role"], "user");
        assert_eq!(input[1]["content"][0]["text"], "Hello!");
        assert_eq!(input.len(), 2);
    }

    #[test]
    fn test_codex_sse_conversion() {
        let openai_sse = b"event: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\nevent: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}\n\nevent: response.done\ndata: {\"type\":\"response.done\"}\n";

        let result = convert_openai_sse_to_standard(openai_sse);
        let result_str = String::from_utf8(result).unwrap();

        assert!(result_str.contains("data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}"));
        assert!(result_str.contains("data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}"));
        assert!(result_str.contains("data: {\"type\":\"response.done\"}"));
    }

    #[test]
    fn test_codex_sse_conversion_empty() {
        let result = convert_openai_sse_to_standard(b"");
        assert!(result.is_empty());
    }

    #[test]
    fn test_codex_sse_conversion_standard_format_unchanged() {
        let standard_sse = b"data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n";
        let result = convert_openai_sse_to_standard(standard_sse);
        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.contains("data: {\"type\":\"content.delta\""));
    }

    #[test]
    fn test_extract_input_items_missing_messages() {
        let body = json!({
            "model": "codex/o4-mini"
        });

        let result = CodexExecutor::extract_input_items(&body);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_input_items_empty_messages() {
        let body = json!({
            "model": "codex/o4-mini",
            "messages": []
        });

        let result = CodexExecutor::extract_input_items(&body);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_input_items_server_ids_stripped() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "Hello", "id": "msg_abc123"},
                {"role": "user", "content": "World", "id": "my-custom-id"}
            ]
        });

        let items = CodexExecutor::extract_input_items(&body).unwrap();
        assert_eq!(items.len(), 2);
        // First item had "msg_" prefix -> stripped, no id field expected
        assert!(
            items[0].get("id").is_none(),
            "server-generated msg_ id should be stripped"
        );
        // Second item had custom ID -> preserved
        assert_eq!(items[1]["id"], "my-custom-id");
    }

    #[test]
    fn test_extract_input_items_content_array_with_text_type() {
        let body = json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Hello "},
                        {"type": "text", "text": "world"}
                    ]
                }
            ]
        });

        let items = CodexExecutor::extract_input_items(&body).unwrap();
        assert_eq!(items.len(), 1);
        let content = items[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Hello ");
        assert_eq!(content[1]["type"], "input_text");
        assert_eq!(content[1]["text"], "world");
    }

    #[test]
    fn test_build_url_base() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let url = executor.build_url("o4-mini");
        assert_eq!(url, "https://chatgpt.com/backend-api/codex/responses");
    }

    #[test]
    fn test_build_url_compact_suffix() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let url = executor.build_url("o4-mini_compact");
        assert_eq!(
            url,
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );
    }
}
