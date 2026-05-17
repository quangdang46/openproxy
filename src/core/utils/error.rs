//! Port of `open-sse/utils/error.js`.
//!
//! Builders for OpenAI-compatible client-facing error responses, plus a
//! parser that extracts a usable error message + status from an upstream
//! provider response.

use crate::core::config::error_config::{default_error_message, error_type_for};
use serde_json::{json, Value};

/// Build the OpenAI-shaped error body for a given HTTP status.
pub fn build_error_body(status: u16, message: Option<&str>) -> Value {
    let info = error_type_for(status).unwrap_or({
        if status >= 500 {
            crate::core::config::error_config::ErrorTypeInfo {
                r#type: "server_error",
                code: "internal_server_error",
            }
        } else {
            crate::core::config::error_config::ErrorTypeInfo {
                r#type: "invalid_request_error",
                code: "",
            }
        }
    });

    let msg_owned;
    let msg = match message {
        Some(m) => m,
        None => match default_error_message(status) {
            Some(s) => s,
            None => {
                msg_owned = "An error occurred".to_string();
                msg_owned.as_str()
            }
        },
    };

    json!({
        "error": {
            "message": msg,
            "type": info.r#type,
            "code": info.code,
        }
    })
}

/// Parsed upstream error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamError {
    pub status: u16,
    pub message: String,
    /// Provider-specific cooldown expiry timestamp (ms since epoch).
    /// Some upstreams (e.g. Codex) include a `resets_at` value the
    /// account-fallback path can honour to schedule a retry.
    pub resets_at_ms: Option<u64>,
}

/// Parse an upstream provider error body into [`UpstreamError`]. Walks the
/// usual `{error: {message: ...}}` shape, then `{message}`, then `{error}`,
/// then falls back to the raw body string.
pub fn parse_upstream_error(status: u16, body: &str) -> UpstreamError {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = if let Some(json) = parsed.as_ref() {
        json.pointer("/error/message")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                json.get("message")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .or_else(|| {
                json.get("error").map(|v| match v.as_str() {
                    Some(s) => s.to_string(),
                    None => v.to_string(),
                })
            })
            .unwrap_or_else(|| body.to_string())
    } else {
        body.to_string()
    };

    let final_message = if message.is_empty() {
        default_error_message(status)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Upstream error: {status}"))
    } else {
        message
    };

    UpstreamError {
        status,
        message: final_message,
        resets_at_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_error_body_uses_default_message_when_missing() {
        let body = build_error_body(401, None);
        assert_eq!(body["error"]["type"], "authentication_error");
        assert_eq!(body["error"]["message"], "Invalid API key provided");
    }

    #[test]
    fn build_error_body_uses_explicit_message() {
        let body = build_error_body(429, Some("slow down"));
        assert_eq!(body["error"]["message"], "slow down");
        assert_eq!(body["error"]["type"], "rate_limit_error");
    }

    #[test]
    fn build_error_body_unknown_5xx_is_server_error() {
        let body = build_error_body(599, None);
        assert_eq!(body["error"]["type"], "server_error");
        assert_eq!(body["error"]["code"], "internal_server_error");
    }

    #[test]
    fn parse_walks_error_message_field() {
        let raw = r#"{"error":{"message":"too big","code":"oversized"}}"#;
        let err = parse_upstream_error(400, raw);
        assert_eq!(err.message, "too big");
        assert_eq!(err.status, 400);
    }

    #[test]
    fn parse_falls_back_to_body_text_when_unparseable() {
        let err = parse_upstream_error(503, "Service Unavailable");
        assert_eq!(err.message, "Service Unavailable");
    }
}
