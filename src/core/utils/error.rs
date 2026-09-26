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
/// This is hygiene only — 9router has no equivalent, and `buildErrorBody`
/// (open-sse/utils/error.js:9-22) hands the provider's own text to the client
/// untouched. Substituting canned prose for a phrase the upstream chose is what
/// made a 429's organization id, the model that tripped the limit and the
/// provider's own reset hint disappear, and it broke clients that match on the
/// upstream string. So the message is cleaned, never rewritten:
/// - HTML error pages collapse to their `<title>`
/// - whitespace runs collapse
/// - internal prefixes like `Error from provider (Console):` are dropped
/// - status defaults fill in only a genuinely empty/opaque body
/// - the result is clamped so no multi-KB stack dump reaches a client
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

    // A leading "[…]" is left alone. 9router composes two bracketed shapes and
    // passes both through verbatim: `formatProviderError`'s `[<code>]: <msg>`
    // (error.js:139-147) and the all-accounts-cooling-down body
    // `[<provider>/<model>] <lastError> (reset after 4m 12s)`
    // (handlers/chat.js:239-242). Stripping either one deletes the detail the
    // client needs to act on it.

    let lower = msg.to_ascii_lowercase();

    // Empty / opaque → status default. Only a body that says nothing at all
    // falls back; a body that says "Upstream request failed" has said enough.
    if msg.is_empty() || msg == "{}" || msg == "null" || lower == "error" || lower == "failed" {
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

/// Extract the client-facing message from an upstream error body.
///
/// Port of `open-sse/utils/error.js parseUpstreamError`'s message half
/// (error.js:139-160): `error.message` when `error` is an object, else
/// `message`, else `error` stringified, else the raw body. The status half
/// lives in [`parse_upstream_error`]; this is the un-sanitised text a handler
/// prefixes with its own `[n]: ` marker.
pub fn parse_upstream_message(body: &str) -> String {
    let Ok(json) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    let candidate = json
        .pointer("/error/message")
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
        });
    candidate.unwrap_or_else(|| body.to_string())
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

    /// 9router hands the provider's own text to the client
    /// (open-sse/utils/error.js:9-22). The org id and the limit that tripped
    /// are the only thing that tells an operator which account is throttled.
    #[test]
    fn upstream_rate_limit_text_survives_verbatim() {
        let raw = "Rate limit reached for gpt-4 in organization org-abc on tokens per min";
        assert_eq!(friendly_error_message(429, raw), raw);
    }

    #[test]
    fn upstream_quota_text_survives_verbatim() {
        let raw = "You exceeded your current quota, please check your plan and billing details";
        assert_eq!(friendly_error_message(403, raw), raw);
    }

    #[test]
    fn upstream_invalid_key_text_survives_verbatim() {
        let raw = "Invalid API key provided: sk-live-****. You can find your API key at …";
        assert_eq!(friendly_error_message(401, raw), raw);
    }

    /// Both bracketed shapes 9router composes are part of the message the
    /// client is meant to read: `formatProviderError`'s status tag
    /// (error.js:139-147) and the cooling-down body (chat.js:239-242).
    #[test]
    fn bracketed_shapes_survive_the_sanitizer() {
        let cooling = "[kilocode/kimi-k2] Rate limit reached for gpt-4 (reset after 4m 12s)";
        assert_eq!(friendly_error_message(503, cooling), cooling);
        let tagged = "[502]: no upstream configured for provider xai";
        assert_eq!(friendly_error_message(502, tagged), tagged);
    }

    /// A body that says nothing gets the status default; one that says
    /// "Upstream request failed" has said enough and must not be flattened
    /// into a bare "Bad request".
    #[test]
    fn only_a_silent_body_falls_back_to_the_status_default() {
        assert_eq!(friendly_error_message(400, "{}"), "Bad request");
        assert_eq!(friendly_error_message(429, "error"), "Rate limit exceeded");
        assert_eq!(
            friendly_error_message(400, "Upstream request failed"),
            "Upstream request failed"
        );
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

    /// 9router `parseUpstreamError` (error.js:139-160) walks
    /// `error.message` → `message` → `error` → raw body. The extracted text is
    /// what a handler prefixes with `[n]: ` — never the raw JSON dump.
    #[test]
    fn parse_upstream_message_extracts_the_message() {
        assert_eq!(
            parse_upstream_message(
                r#"{"error":{"message":"Incorrect API key provided","type":"x"}}"#
            ),
            "Incorrect API key provided"
        );
        assert_eq!(parse_upstream_message(r#"{"message":"nope"}"#), "nope");
        assert_eq!(
            parse_upstream_message(r#"{"error":"rate limit"}"#),
            "rate limit"
        );
        assert_eq!(
            parse_upstream_message("upstream exploded"),
            "upstream exploded"
        );
        assert_eq!(parse_upstream_message(""), "");
    }

    /// `error` as a non-string (Gemini returns `{"error":{"code":…}}`) is
    /// stringified rather than dropped.
    #[test]
    fn parse_upstream_message_stringifies_non_string_error() {
        let message =
            parse_upstream_message(r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED"}}"#);
        assert!(message.contains("RESOURCE_EXHAUSTED"), "got: {message}");
    }
}
