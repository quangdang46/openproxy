use std::sync::Arc;

use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const OPENAI_RESPONSES_API_BASE: &str = "https://api.openai.com/v1";

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
        Ok(Self { pool, provider_node })
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

    /// Build the URL for OpenAI Responses API.
    fn build_url(&self, _model: &str) -> String {
        format!("{}/responses", OPENAI_RESPONSES_API_BASE.trim_end_matches('/'))
    }

    /// Build request headers for OpenAI Responses API.
    fn build_headers(&self, api_key: &str, stream: bool) -> Result<HeaderMap, CodexExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(CodexExecutorError::InvalidHeader)?,
        );

        if stream {
            headers.insert(
                "Accept",
                HeaderValue::from_static("text/event-stream"),
            );
        }

        Ok(headers)
    }

    /// Transform the request body from Chat Completions format to OpenAI Responses API format.
    ///
    /// The Responses API uses `input` instead of `messages`.
    /// Input can be a string or an array of content parts.
    fn transform_request_body(&self, body: &Value, actual_model: &str) -> Result<Value, CodexExecutorError> {
        // Extract the prompt from the messages array
        let input_text = Self::extract_input_from_body(body)?;

        // Build the request body in OpenAI Responses API format
        let mut request_body = json!({
            "model": actual_model,
            "input": input_text,
        });

        // Copy streaming flag
        if let Some(stream) = body.get("stream").and_then(Value::as_bool) {
            request_body["stream"] = json!(stream);
        }

        // Copy temperature if present
        if let Some(temp) = body.get("temperature").and_then(Value::as_f64) {
            request_body["temperature"] = json!(temp);
        }

        // Copy max_tokens / max_completion_tokens if present
        if let Some(max_tokens) = body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .and_then(Value::as_u64)
        {
            request_body["max_tokens"] = json!(max_tokens);
        }

        // Copy top_p if present
        if let Some(top_p) = body.get("top_p").and_then(Value::as_f64) {
            request_body["top_p"] = json!(top_p);
        }

        // Copy stop sequences if present
        if let Some(stop) = body.get("stop").and_then(Value::as_array) {
            let stop_vec: Vec<String> = stop
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            if !stop_vec.is_empty() {
                request_body["stop"] = json!(stop_vec);
            }
        }

        Ok(request_body)
    }

    /// Extract the input text from a Chat Completions style request body.
    fn extract_input_from_body(body: &Value) -> Result<String, CodexExecutorError> {
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| CodexExecutorError::UnsupportedFormat("Missing messages array".to_string()))?;

        let mut input_parts = Vec::new();

        for msg in messages {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = msg.get("content").and_then(Value::as_str).unwrap_or("");

            // Skip system messages as they're handled differently in Responses API
            if role == "system" {
                continue;
            }

            if !content.is_empty() {
                input_parts.push(content.to_string());
            }
        }

        if input_parts.is_empty() {
            return Err(CodexExecutorError::UnsupportedFormat(
                "No valid content found in messages".to_string(),
            ));
        }

        // Join all content parts with newlines
        Ok(input_parts.join("\n"))
    }

    pub async fn execute(
        &self,
        request: CodexExecutionRequest,
    ) -> Result<CodexExecutorResponse, CodexExecutorError> {
        let actual_model = Self::parse_codex_model(&request.model);
        let url = self.build_url(&actual_model);

        // Get API key from credentials
        let api_key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .ok_or_else(|| CodexExecutorError::MissingCredentials("API key required".to_string()))?;

        let headers = self.build_headers(api_key, request.stream)?;
        let transformed_body = self.transform_request_body(&request.body, &actual_model)?;

        let client = self.pool.get("openai", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(CodexExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
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
        assert_eq!(CodexExecutor::parse_codex_model("codex/o4-mini-high"), "o4-mini-high");
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
            "stream": true,
            "temperature": 0.7
        });

        let result = executor.transform_request_body(&chat_body, "o4-mini").unwrap();

        assert_eq!(result["model"], "o4-mini");
        assert_eq!(result["input"], "Hello, world!");
        assert_eq!(result["stream"], true);
        assert_eq!(result["temperature"], 0.7);
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

        let result = executor.transform_request_body(&chat_body, "o4-mini").unwrap();

        assert_eq!(result["input"], "Hello\nHi there!\nHow are you?");
    }

    #[test]
    fn test_codex_request_body_skips_system_messages() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello!"}
            ]
        });

        let result = executor.transform_request_body(&chat_body, "o4-mini").unwrap();

        assert_eq!(result["input"], "Hello!");
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
    fn test_extract_input_from_body_missing_messages() {
        let body = json!({
            "model": "codex/o4-mini"
        });

        let result = CodexExecutor::extract_input_from_body(&body);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_input_from_body_empty_messages() {
        let body = json!({
            "model": "codex/o4-mini",
            "messages": []
        });

        let result = CodexExecutor::extract_input_from_body(&body);
        assert!(result.is_err());
    }
}