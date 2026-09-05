use std::sync::Arc;

use hyper::http::Response as HttpResponse;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const COMMANDCODE_URL: &str = "https://api.commandcode.ai/alpha/generate";

/// Parsed error from a CommandCode NDJSON `{"type":"error", ...}` event.
#[derive(Debug, Clone)]
pub struct CommandCodeParsedError {
    pub status_code: u16,
    pub message: String,
}

/// Parse a CommandCode NDJSON error event into status code and message.
///
/// Port of 9router `parseCommandCodeError()` (executors/commandcode.js:66-124).
/// The upstream sends errors as `{"type":"error", "error": {"statusCode": N, "message": "..."}}`.
/// Falls back to heuristic message matching when statusCode is absent or out of range.
pub fn parse_command_code_error(event: &Value) -> CommandCodeParsedError {
    let err_val = event.get("error").or_else(|| event.get("message"));
    let mut message = String::new();
    let mut status_code: Option<u16> = None;

    if let Some(obj) = err_val.and_then(Value::as_object) {
        // error is an object: extract message, statusCode/status, type
        message = obj
            .get("message")
            .or_else(|| obj.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| serde_json::to_string(obj).unwrap_or_default());

        status_code = obj
            .get("statusCode")
            .or_else(|| obj.get("status"))
            .and_then(|v| v.as_u64())
            .filter(|s| (400..=599).contains(s))
            .map(|s| s as u16);
    } else if let Some(s) = err_val.and_then(Value::as_str) {
        message = s.to_string();
    } else if let Some(v) = err_val {
        message = serde_json::to_string(v).unwrap_or_default();
    }

    // Check top-level statusCode as well (JS: event.statusCode)
    if status_code.is_none() {
        status_code = event
            .get("statusCode")
            .and_then(|v| v.as_u64())
            .filter(|s| (400..=599).contains(s))
            .map(|s| s as u16);
    }

    // Fallback: heuristic from message text when status is missing or out of range
    if status_code.is_none() {
        let lower = message.to_lowercase();
        status_code = if lower.contains("rate limit") || lower.contains("too many requests") {
            Some(429)
        } else if lower.contains("unauthorized")
            || lower.contains("invalid api key")
            || lower.contains("authentication")
        {
            Some(401)
        } else if lower.contains("payment required") || lower.contains("billing") {
            Some(402)
        } else if lower.contains("quota")
            || lower.contains("forbidden")
            || lower.contains("permission")
        {
            Some(403)
        } else if lower.contains("not found") {
            Some(404)
        } else if lower.contains("unavailable")
            || lower.contains("overloaded")
            || lower.contains("server error")
        {
            Some(503)
        } else {
            Some(503)
        };
    }

    if message.is_empty() {
        message = "CommandCode upstream error".to_string();
    }

    CommandCodeParsedError {
        status_code: status_code.unwrap_or(503),
        message,
    }
}

/// Check if a single NDJSON line (already trimmed, `data:` prefix stripped) is
/// a `{"type":"error", ...}` event. Returns `Some(parsed_error)` if so.
pub fn check_ndjson_line_for_error(line: &str) -> Option<CommandCodeParsedError> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return None;
    }
    let json_str = trimmed
        .strip_prefix("data:")
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let parsed = serde_json::from_str::<Value>(json_str).ok()?;
    if parsed.get("type").and_then(Value::as_str) == Some("error") {
        Some(parse_command_code_error(&parsed))
    } else {
        None
    }
}

/// Wrap NDJSON lines into SSE `data:` frames.
///
/// Each non-empty line is wrapped as `data: {line}\n\n`. The result ends with
/// `data: [DONE]\n\n`.
fn wrap_ndjson_as_sse(body: &[u8]) -> String {
    let mut sse = String::new();
    for raw_line in body.split(|&b| b == b'\n') {
        let line = match std::str::from_utf8(raw_line) {
            Ok(s) => s.trim(),
            Err(_) => continue,
        };
        if line.is_empty() {
            continue;
        }
        // Skip [DONE] sentinel — we'll append our own at the end.
        if line == "[DONE]" {
            continue;
        }
        // Already has data: prefix? Strip it to avoid double-wrapping.
        let payload = line
            .strip_prefix("data:")
            .map(str::trim_start)
            .unwrap_or(line);
        sse.push_str("data: ");
        sse.push_str(payload);
        sse.push('\n');
        sse.push('\n');
    }
    sse.push_str("data: [DONE]\n\n");
    sse
}

/// Inspect a CommandCode upstream response: detect mid-stream NDJSON errors
/// and convert NDJSON to SSE format.
///
/// Port of 9router `inspectAndWrapCommandCodeResponse()`.
///
/// - **Streaming (2xx + event-stream accept)**: reads the full body, scans
///   NDJSON lines for `{"type":"error"}` events. On error: returns a synthetic
///   HTTP error response so the combo/fallback layer can retry. On success:
///   wraps each line in `data: {line}\n\n` SSE format.
/// - **Non-error (non-2xx)**: returned as-is for upstream error handling.
pub async fn inspect_and_wrap_response(
    response: reqwest::Response,
    _model: &str,
) -> UpstreamResponse {
    let status = response.status();

    // Non-success: pass through to upstream error handling (retry-after, etc.)
    if !status.is_success() {
        return UpstreamResponse::Reqwest(response);
    }

    // Read the full body. CommandCode responses are typically small (code
    // completions), so buffering the entire body is acceptable.
    let body_bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "openproxy::executor::commandcode",
                "Failed to read CommandCode upstream body: {e}"
            );
            return build_error_response(503, "Failed to read upstream response");
        }
    };

    let body_text = match std::str::from_utf8(&body_bytes) {
        Ok(s) => s,
        Err(_) => {
            return build_error_response(500, "CommandCode upstream returned invalid UTF-8");
        }
    };

    // Scan NDJSON lines for error events
    for line in body_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(err) = check_ndjson_line_for_error(trimmed) {
            tracing::warn!(
                target: "openproxy::executor::commandcode",
                "Intercepted mid-stream error: status={} message={}",
                err.status_code,
                err.message
            );
            return build_error_response(
                err.status_code,
                &format!("[CommandCode error: {}]", err.message),
            );
        }
    }

    // No error found — wrap NDJSON in SSE format
    let sse_body = wrap_ndjson_as_sse(&body_bytes);
    build_sse_response(status, sse_body)
}

/// Build a synthetic error response that the combo/fallback layer can handle.
fn build_error_response(status_code: u16, message: &str) -> UpstreamResponse {
    let error_body = json!({
        "error": {
            "message": message,
            "type": "server_error",
            "code": status_code,
        }
    });
    let body = error_body.to_string();
    let status = hyper::http::StatusCode::from_u16(status_code)
        .unwrap_or(hyper::http::StatusCode::BAD_GATEWAY);
    let response = HttpResponse::builder()
        .status(status)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .header("Access-Control-Allow-Origin", HeaderValue::from_static("*"))
        .body(reqwest::Body::from(body))
        .unwrap();
    UpstreamResponse::Reqwest(reqwest::Response::from(response))
}

/// Build an SSE response wrapping the given body string.
fn build_sse_response(status: reqwest::StatusCode, body: String) -> UpstreamResponse {
    let response = HttpResponse::builder()
        .status(status)
        .header(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .header("Cache-Control", HeaderValue::from_static("no-cache"))
        .header("Connection", HeaderValue::from_static("keep-alive"))
        .body(reqwest::Body::from(body))
        .unwrap();
    UpstreamResponse::Reqwest(reqwest::Response::from(response))
}

#[derive(Clone)]
pub struct CommandCodeExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum CommandCodeExecutorError {
    MissingCredentials(String),
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for CommandCodeExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for CommandCodeExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for CommandCodeExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for CommandCodeExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for CommandCodeExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct CommandCodeExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct CommandCodeExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl CommandCodeExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, CommandCodeExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(&self) -> String {
        COMMANDCODE_URL.to_string()
    }

    fn build_headers(&self, credentials: &ProviderConnection, stream: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-session-id",
            HeaderValue::from_str(&Uuid::new_v4().to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );

        let token = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref());
        if let Some(t) = token {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", t))
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        headers
    }

    pub async fn execute_request(
        &self,
        request: CommandCodeExecutionRequest,
    ) -> Result<CommandCodeExecutorResponse, CommandCodeExecutorError> {
        let url = self.build_url();
        let headers = self.build_headers(&request.credentials, request.stream);

        let client = self.pool.get("commandcode", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&request.body)
            .send()
            .await?;

        // Inspect NDJSON stream for mid-stream errors and convert to SSE format.
        // Port of 9router inspectAndWrapCommandCodeResponse().
        let response = inspect_and_wrap_response(response, &request.model).await;

        Ok(CommandCodeExecutorResponse {
            response,
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
    fn parse_error_with_status_code_and_message() {
        let event = json!({
            "type": "error",
            "error": {
                "statusCode": 429,
                "message": "Rate limit exceeded"
            }
        });
        let parsed = parse_command_code_error(&event);
        assert_eq!(parsed.status_code, 429);
        assert_eq!(parsed.message, "Rate limit exceeded");
    }

    #[test]
    fn parse_error_with_top_level_status_code() {
        let event = json!({
            "type": "error",
            "statusCode": 401,
            "error": {
                "message": "Unauthorized"
            }
        });
        let parsed = parse_command_code_error(&event);
        assert_eq!(parsed.status_code, 401);
        assert_eq!(parsed.message, "Unauthorized");
    }

    #[test]
    fn parse_error_heuristic_rate_limit() {
        let event = json!({
            "type": "error",
            "error": {
                "message": "Too many requests, slow down"
            }
        });
        let parsed = parse_command_code_error(&event);
        assert_eq!(parsed.status_code, 429);
    }

    #[test]
    fn parse_error_heuristic_auth() {
        let event = json!({
            "type": "error",
            "error": {
                "message": "Invalid API key provided"
            }
        });
        let parsed = parse_command_code_error(&event);
        assert_eq!(parsed.status_code, 401);
    }

    #[test]
    fn parse_error_heuristic_billing() {
        let event = json!({
            "type": "error",
            "error": {
                "message": "Payment required to continue"
            }
        });
        let parsed = parse_command_code_error(&event);
        assert_eq!(parsed.status_code, 402);
    }

    #[test]
    fn parse_error_heuristic_forbidden() {
        let event = json!({
            "type": "error",
            "error": {
                "message": "Quota exceeded for this model"
            }
        });
        let parsed = parse_command_code_error(&event);
        assert_eq!(parsed.status_code, 403);
    }

    #[test]
    fn parse_error_heuristic_unavailable() {
        let event = json!({
            "type": "error",
            "error": {
                "message": "Service is currently overloaded"
            }
        });
        let parsed = parse_command_code_error(&event);
        assert_eq!(parsed.status_code, 503);
    }

    #[test]
    fn parse_error_fallback_to_503() {
        let event = json!({
            "type": "error",
            "error": {
                "message": "something went wrong"
            }
        });
        let parsed = parse_command_code_error(&event);
        assert_eq!(parsed.status_code, 503);
    }

    #[test]
    fn check_ndjson_line_detects_error() {
        let line = r#"{"type":"error","error":{"statusCode":429,"message":"Rate limited"}}"#;
        let result = check_ndjson_line_for_error(line);
        assert!(result.is_some());
        let err = result.unwrap();
        assert_eq!(err.status_code, 429);
        assert_eq!(err.message, "Rate limited");
    }

    #[test]
    fn check_ndjson_line_ignores_non_error() {
        let line = r#"{"type":"text-delta","delta":"hello"}"#;
        assert!(check_ndjson_line_for_error(line).is_none());
    }

    #[test]
    fn check_ndjson_line_ignores_empty() {
        assert!(check_ndjson_line_for_error("").is_none());
        assert!(check_ndjson_line_for_error("   ").is_none());
    }

    #[test]
    fn check_ndjson_line_ignores_done() {
        assert!(check_ndjson_line_for_error("[DONE]").is_none());
    }

    #[test]
    fn check_ndjson_line_strips_data_prefix() {
        let line = r#"data: {"type":"error","error":{"statusCode":503,"message":"Down"}}"#;
        let result = check_ndjson_line_for_error(line);
        assert!(result.is_some());
        assert_eq!(result.unwrap().status_code, 503);
    }

    #[test]
    fn check_ndjson_line_handles_invalid_json() {
        let line = "this is not json";
        assert!(check_ndjson_line_for_error(line).is_none());
    }

    #[test]
    fn wrap_ndjson_as_sse_basic() {
        let body = br#"{"type":"text-delta","delta":"hello"}"#;
        let sse = wrap_ndjson_as_sse(body);
        assert!(sse.starts_with("data: "));
        assert!(sse.contains("data: {\"type\":\"text-delta\",\"delta\":\"hello\"}\n\n"));
        assert!(sse.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn wrap_ndjson_as_sse_multiple_lines() {
        let body = b"line1\nline2\nline3\n";
        let sse = wrap_ndjson_as_sse(body);
        assert!(sse.contains("data: line1\n\n"));
        assert!(sse.contains("data: line2\n\n"));
        assert!(sse.contains("data: line3\n\n"));
        assert!(sse.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn wrap_ndjson_as_sse_strips_data_prefix() {
        let body = b"data: {\"foo\":1}\n";
        let sse = wrap_ndjson_as_sse(body);
        assert!(sse.contains("data: {\"foo\":1}\n\n"));
        // Should not double-wrap
        assert!(!sse.contains("data: data:"));
    }

    #[test]
    fn wrap_ndjson_as_sse_skips_done_and_empty() {
        let body = b"line1\n[DONE]\n\nline2\n";
        let sse = wrap_ndjson_as_sse(body);
        assert!(sse.contains("data: line1\n\n"));
        assert!(sse.contains("data: line2\n\n"));
        // Only the trailing [DONE] should be present
        let done_count = sse.matches("data: [DONE]\n\n").count();
        assert_eq!(done_count, 1);
    }

    #[test]
    fn inspect_and_wrap_error_response_passes_through() {
        // Non-200 responses should be passed through untouched
        let response = HttpResponse::builder()
            .status(429)
            .body(reqwest::Body::from("rate limited"))
            .unwrap();
        let response = reqwest::Response::from(response);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(inspect_and_wrap_response(response, "test-model"));
        match result {
            UpstreamResponse::Reqwest(r) => {
                assert_eq!(r.status().as_u16(), 429);
            }
            _ => panic!("expected Reqwest variant"),
        }
    }

    #[test]
    fn inspect_and_wrap_ndjson_error_returns_error_response() {
        let body = r#"{"type":"text-delta","delta":"partial"}
{"type":"error","error":{"statusCode":429,"message":"Rate limit exceeded"}}"#;
        let response = HttpResponse::builder()
            .status(200)
            .body(reqwest::Body::from(body))
            .unwrap();
        let response = reqwest::Response::from(response);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(inspect_and_wrap_response(response, "test-model"));
        match result {
            UpstreamResponse::Reqwest(r) => {
                assert_eq!(r.status().as_u16(), 429);
                let ct = r
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap();
                assert_eq!(ct, "application/json");
            }
            _ => panic!("expected Reqwest variant"),
        }
    }

    #[test]
    fn inspect_and_wrap_clean_ndjson_returns_sse() {
        let body = r#"{"type":"text-delta","delta":"hello"}
{"type":"text-delta","delta":" world"}
{"type":"finish"}"#;
        let response = HttpResponse::builder()
            .status(200)
            .body(reqwest::Body::from(body))
            .unwrap();
        let response = reqwest::Response::from(response);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(inspect_and_wrap_response(response, "test-model"));
        match result {
            UpstreamResponse::Reqwest(r) => {
                assert_eq!(r.status().as_u16(), 200);
                let ct = r
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap();
                assert_eq!(ct, "text/event-stream");
            }
            _ => panic!("expected Reqwest variant"),
        }
    }

    #[test]
    fn inspect_and_wrap_first_line_error_returns_error() {
        // Error on the very first line
        let body = r#"{"type":"error","error":{"statusCode":401,"message":"Invalid API key"}}"#;
        let response = HttpResponse::builder()
            .status(200)
            .body(reqwest::Body::from(body))
            .unwrap();
        let response = reqwest::Response::from(response);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(inspect_and_wrap_response(response, "test-model"));
        match result {
            UpstreamResponse::Reqwest(r) => {
                assert_eq!(r.status().as_u16(), 401);
            }
            _ => panic!("expected Reqwest variant"),
        }
    }

    #[test]
    fn inspect_and_wrap_auth_error_returns_401() {
        let body = r#"{"type":"error","error":{"statusCode":401,"message":"Unauthorized"}}"#;
        let response = HttpResponse::builder()
            .status(200)
            .body(reqwest::Body::from(body))
            .unwrap();
        let response = reqwest::Response::from(response);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(inspect_and_wrap_response(response, "test-model"));
        match result {
            UpstreamResponse::Reqwest(r) => {
                assert_eq!(r.status().as_u16(), 401);
            }
            _ => panic!("expected Reqwest variant"),
        }
    }

    #[test]
    fn inspect_and_wrap_billing_error_returns_402() {
        let body = r#"{"type":"error","error":{"statusCode":402,"message":"Payment required"}}"#;
        let response = HttpResponse::builder()
            .status(200)
            .body(reqwest::Body::from(body))
            .unwrap();
        let response = reqwest::Response::from(response);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(inspect_and_wrap_response(response, "test-model"));
        match result {
            UpstreamResponse::Reqwest(r) => {
                assert_eq!(r.status().as_u16(), 402);
            }
            _ => panic!("expected Reqwest variant"),
        }
    }

    #[test]
    fn inspect_and_wrap_forbidden_error_returns_403() {
        let body = r#"{"type":"error","error":{"statusCode":403,"message":"Forbidden"}}"#;
        let response = HttpResponse::builder()
            .status(200)
            .body(reqwest::Body::from(body))
            .unwrap();
        let response = reqwest::Response::from(response);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(inspect_and_wrap_response(response, "test-model"));
        match result {
            UpstreamResponse::Reqwest(r) => {
                assert_eq!(r.status().as_u16(), 403);
            }
            _ => panic!("expected Reqwest variant"),
        }
    }

    #[test]
    fn inspect_and_wrap_unavailable_error_returns_503() {
        let body = r#"{"type":"error","error":{"statusCode":503,"message":"Service unavailable"}}"#;
        let response = HttpResponse::builder()
            .status(200)
            .body(reqwest::Body::from(body))
            .unwrap();
        let response = reqwest::Response::from(response);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(inspect_and_wrap_response(response, "test-model"));
        match result {
            UpstreamResponse::Reqwest(r) => {
                assert_eq!(r.status().as_u16(), 503);
            }
            _ => panic!("expected Reqwest variant"),
        }
    }

    #[test]
    fn inspect_and_wrap_heuristic_error_from_message() {
        // No statusCode in error object, but message contains "rate limit"
        let body = r#"{"type":"error","error":{"message":"rate limit exceeded"}}"#;
        let response = HttpResponse::builder()
            .status(200)
            .body(reqwest::Body::from(body))
            .unwrap();
        let response = reqwest::Response::from(response);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(inspect_and_wrap_response(response, "test-model"));
        match result {
            UpstreamResponse::Reqwest(r) => {
                assert_eq!(r.status().as_u16(), 429);
            }
            _ => panic!("expected Reqwest variant"),
        }
    }
}
