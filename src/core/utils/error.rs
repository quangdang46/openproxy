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

/// Sanitize a raw upstream/provider error string for client display.
///
/// Goals (9router parity + UX):
/// - Drop internal prefixes like `Error from provider (Console):`
/// - Map known upstream phrases to short, actionable English
/// - Prefer status-based defaults when the body is empty/opaque
/// - Never return multi-kilobyte HTML/stack dumps to clients
pub fn friendly_error_message(status: u16, raw: &str) -> String {
    let mut msg = raw.trim().to_string();

    // Strip HTML if upstream returned an error page.
    if msg.contains('<')
        && (msg.contains("<html") || msg.contains("<title") || msg.contains("<!DOCTYPE"))
    {
        if let Some(title) = extract_html_title(&msg) {
            msg = title;
        } else {
            msg = strip_html_tags(&msg);
        }
    }

    // Collapse whitespace / newlines from dumps.
    msg = msg.split_whitespace().collect::<Vec<_>>().join(" ");

    // Strip common internal prefixes from free/console proxies (e.g. OpenCode).
    for prefix in [
        "Error from provider (Console):",
        "Error from provider (console):",
        "Error from provider:",
        "Error from provider :",
        "Provider error:",
        "Upstream error:",
        "upstream error:",
    ] {
        if let Some(rest) = msg.strip_prefix(prefix) {
            msg = rest.trim().to_string();
            break;
        }
        // Also handle when prefix appears after a status tag like "[502]: "
        if let Some(idx) = msg.find(prefix) {
            let after = msg[idx + prefix.len()..].trim();
            if !after.is_empty() {
                msg = after.to_string();
                break;
            }
        }
    }

    // Strip leading "[code]: " wrappers we or upstream may add.
    if msg.starts_with('[') {
        if let Some(end) = msg.find(']') {
            let after = msg[end + 1..].trim().trim_start_matches(':').trim();
            if !after.is_empty() {
                msg = after.to_string();
            }
        }
    }

    let lower = msg.to_ascii_lowercase();

    // Known phrase → friendly copy (order matters: more specific first).
    // Order matters: model/quota phrases before auth catch-alls (free proxies
    // often return 401 for "model not supported").
    let mapped = if lower.contains("account balance is insufficient")
        || lower.contains("insufficient balance")
        || lower.contains("insufficient credits")
        || lower.contains("insufficient_quota")
        || lower.contains("you exceeded your current quota")
        || lower.contains("quota exceeded")
    {
        Some("You exceeded your current quota or balance on this provider. Check billing or switch accounts.".to_string())
    } else if lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("rate_limit")
    {
        Some(
            "Rate limit exceeded. Wait a moment and try again, or use another account.".to_string(),
        )
    } else if lower.contains("not supported")
        || lower.contains("model_not_supported")
        || lower.contains("does not support")
    {
        // Preserve "Model <id> is not supported" when present.
        if lower.starts_with("model ") {
            Some(msg.clone())
        } else {
            Some("This model is not supported by the provider.".to_string())
        }
    } else if lower.contains("model not found")
        || lower.contains("does not exist")
        || lower.contains("model_not_found")
    {
        Some("Model not found. Check the model id or enable it on the provider.".to_string())
    } else if lower.contains("upstream request failed")
        || lower == "bad gateway"
        || lower.contains("bad gateway")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("error connecting")
    {
        Some(
            "Upstream provider request failed. The free-tier endpoint may be down or temporarily unavailable — retry or switch models."
                .to_string(),
        )
    } else if lower.contains("invalid api key")
        || lower.contains("invalid_api_key")
        || lower.contains("incorrect api key")
        || lower.contains("unauthorized")
    {
        Some("Invalid API key or credentials. Reconnect the provider with a valid key.".to_string())
    } else if lower.contains("payment required") || lower.contains("billing") {
        Some(
            "Payment required on this provider. Top up the account or use another provider."
                .to_string(),
        )
    } else if lower.contains("overloaded") || lower.contains("capacity") {
        Some("Provider is overloaded. Retry shortly or fall back to another model.".to_string())
    } else if lower.contains("timeout") || lower.contains("timed out") || lower.contains("deadline")
    {
        Some("Provider timed out. Retry with a shorter prompt or another model.".to_string())
    } else {
        None
    };

    if let Some(m) = mapped {
        return m;
    }

    // Empty / opaque → status default.
    if msg.is_empty()
        || msg == "{}"
        || msg == "null"
        || lower == "error"
        || lower == "failed"
        || lower == "upstream request failed"
    {
        return default_error_message(status)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Provider error ({status})"));
    }

    // Clamp length so clients never get multi-KB dumps.
    const MAX: usize = 280;
    if msg.chars().count() > MAX {
        let truncated: String = msg.chars().take(MAX).collect();
        return format!("{truncated}…");
    }

    msg
}

/// Convenience: sanitize + build OpenAI-shaped error body.
pub fn build_friendly_error_body(status: u16, raw_message: Option<&str>) -> Value {
    let friendly = match raw_message {
        Some(m) => friendly_error_message(status, m),
        None => default_error_message(status)
            .map(str::to_string)
            .unwrap_or_else(|| "An error occurred".to_string()),
    };
    let status = infer_status_from_message(status, &friendly);
    build_error_body(status, Some(&friendly))
}

/// When upstream returns a misleading HTTP status (common on free/console
/// proxies: 401 for "model not supported", 400 for upstream outages), pick a
/// more accurate client-facing status from the message text.
pub fn infer_status_from_message(status: u16, message: &str) -> u16 {
    let lower = message.to_ascii_lowercase();
    if lower.contains("rate limit") || lower.contains("too many requests") {
        return 429;
    }
    if lower.contains("insufficient")
        || lower.contains("quota")
        || lower.contains("balance")
        || lower.contains("payment required")
        || lower.contains("billing")
    {
        // Prefer 403 (quota) over 402 unless payment is explicit.
        if lower.contains("payment") || lower.contains("billing") {
            return 402;
        }
        return 403;
    }
    if lower.contains("not supported") || lower.contains("model_not_supported") {
        return 406;
    }
    if lower.contains("model not found") || lower.contains("does not exist") {
        return 404;
    }
    if lower.contains("invalid api key")
        || lower.contains("invalid credentials")
        || lower.contains("unauthorized") && !lower.contains("not supported")
    {
        return 401;
    }
    if lower.contains("upstream")
        || lower.contains("bad gateway")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("provider request failed")
    {
        // Keep 5xx/502 for outages rather than 400.
        if status < 500 {
            return 502;
        }
    }
    status
}

fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title>")? + 7;
    let end_rel = lower[start..].find("</title>")?;
    let title = html[start..start + end_rel].trim();
    let cleaned = strip_html_tags(title);
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.chars().take(160).collect())
    }
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
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
        // Always sanitize for callers that surface this to clients.
        friendly_error_message(status, &message)
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

    #[test]
    fn friendly_strips_console_prefix() {
        let msg = friendly_error_message(
            400,
            "Error from provider (Console): Upstream request failed",
        );
        assert!(
            !msg.to_ascii_lowercase().contains("error from provider"),
            "got: {msg}"
        );
        assert!(
            msg.to_ascii_lowercase().contains("upstream")
                || msg.to_ascii_lowercase().contains("unavailable")
                || msg.to_ascii_lowercase().contains("failed"),
            "got: {msg}"
        );
    }

    #[test]
    fn friendly_maps_insufficient_balance() {
        let msg = friendly_error_message(403, "Sorry, your account balance is insufficient");
        assert!(
            msg.to_ascii_lowercase().contains("quota")
                || msg.to_ascii_lowercase().contains("balance"),
            "got: {msg}"
        );
        assert!(!msg.starts_with("Sorry"), "got: {msg}");
    }

    #[test]
    fn friendly_keeps_model_not_supported_with_name() {
        let msg = friendly_error_message(401, "Model minimax-m3-free is not supported");
        assert!(
            msg.to_ascii_lowercase().contains("minimax-m3-free"),
            "got: {msg}"
        );
        assert_eq!(
            infer_status_from_message(401, &msg),
            406,
            "model-not-supported should map to 406, got status inference for: {msg}"
        );
    }

    #[test]
    fn friendly_maps_rate_limit() {
        let msg = friendly_error_message(429, "Rate limit exceeded for model");
        assert!(msg.to_ascii_lowercase().contains("rate limit"));
    }

    #[test]
    fn friendly_maps_invalid_key() {
        let msg = friendly_error_message(401, "Invalid API key provided by client");
        assert!(
            msg.to_ascii_lowercase().contains("api key")
                || msg.to_ascii_lowercase().contains("credentials")
        );
    }

    #[test]
    fn friendly_strips_html_title() {
        let msg = friendly_error_message(
            502,
            "<html><head><title>502 Bad Gateway</title></head><body>nginx</body></html>",
        );
        assert!(!msg.contains('<'), "got: {msg}");
        assert!(
            msg.contains("502")
                || msg.to_ascii_lowercase().contains("gateway")
                || msg.to_ascii_lowercase().contains("upstream"),
            "got: {msg}"
        );
    }

    #[test]
    fn build_friendly_error_body_uses_sanitized_message() {
        let body = build_friendly_error_body(
            400,
            Some("Error from provider (Console): Upstream request failed"),
        );
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            !message.to_ascii_lowercase().contains("error from provider"),
            "got: {message}"
        );
        // Misleading 400 from free proxies should upgrade toward gateway failure.
        assert_eq!(body["error"]["type"], "server_error");
        assert_eq!(body["error"]["code"], "bad_gateway");
    }
}
