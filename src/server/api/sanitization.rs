//! Upstream request/response sanitization (parity: 9router `sanitizeInput` /
//! `sanitizeResponse`).
//!
//! - **Input**: strip C0/C1 control characters from every string in the request
//!   body except the whitespace controls that carry meaning (`\t`, `\n`, `\r`).
//!   Providers reject or mangle payloads containing raw control bytes, and some
//!   clients send them from copied terminal output.
//! - **Response**: strip provider-specific top-level fields that break
//!   OpenAI-compatible clients (`service_tier`, `x_groq*`, `system_fingerprint`)
//!   and provider-specific usage sub-objects (`usage.usage_breakdown`). The
//!   documented OpenAI envelope keys are preserved.

use serde_json::Value;

/// Strip control characters from a string, keeping `\t`, `\n`, `\r`.
fn strip_control_chars(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            // Keep tab/newline/carriage return; drop every other C0 control,
            // DEL (U+007F), and the C1 range (U+0080..=U+009F).
            if matches!(c, '\t' | '\n' | '\r') {
                return true;
            }
            let code = *c as u32;
            !(code <= 0x1F || code == 0x7F || (0x80..=0x9F).contains(&code))
        })
        .collect()
}

/// Recursively sanitize request-body string values.
fn walk_strings_mut(value: &mut Value) {
    match value {
        Value::String(s) => {
            let cleaned = strip_control_chars(s);
            if cleaned != *s {
                *s = cleaned;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(walk_strings_mut),
        Value::Object(map) => map.iter_mut().for_each(|(_, v)| walk_strings_mut(v)),
        _ => {}
    }
}

/// Sanitize a request body before it is forwarded upstream.
///
/// Returns the same `Value` when nothing changed (no clone on the hot path for
/// clean bodies? — we mutate in place and return the same reference).
pub fn sanitize_request_body(body: &mut Value) {
    walk_strings_mut(body);
}

/// Top-level response keys stripped from provider payloads. `service_tier` is
/// an OpenAI param echoed by some gateways; `x_groq*` and
/// `system_fingerprint` are provider-specific and break strict clients.
const STRIP_TOP_LEVEL: &[&str] = &["service_tier", "system_fingerprint"];

/// Usage sub-keys stripped from the `usage` object.
const STRIP_USAGE: &[&str] = &["usage_breakdown"];

/// Sanitize a single non-streaming provider response object in place.
pub fn sanitize_response_object(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        for key in STRIP_TOP_LEVEL {
            obj.remove(*key);
        }
        // Provider-prefixed extras (`x_groq`, `x_google`, …) are not part of
        // the OpenAI envelope.
        obj.retain(|k, _| !k.starts_with("x_"));
        if let Some(usage) = obj.get_mut("usage").and_then(Value::as_object_mut) {
            for key in STRIP_USAGE {
                usage.remove(*key);
            }
        }
    }
}

/// Sanitize every `data:` frame of an SSE body, preserving framing bytes
/// (`data: ` prefix, blank-line separators, and the `[DONE]` sentinel).
///
/// Non-JSON frames pass through untouched — a provider that emits comments or
/// custom events must not break the stream.
pub fn sanitize_sse_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some(payload) = trimmed.strip_prefix("data:") {
            let payload = payload.trim();
            if payload.is_empty() || payload == "[DONE]" {
                out.push_str(line);
                continue;
            }
            match serde_json::from_str::<Value>(payload) {
                Ok(mut value) => {
                    sanitize_response_object(&mut value);
                    out.push_str("data: ");
                    out.push_str(&serde_json::to_string(&value).unwrap_or_else(|_| "{}".into()));
                    if line.ends_with('\n') {
                        out.push('\n');
                    }
                }
                Err(_) => out.push_str(line),
            }
        } else {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_control_chars_but_keeps_whitespace_controls() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "tab\there\nnl\rcr\u{0000}nul\u{007F}del\u{0085}nel"}],
            "n": 1,
            "nested": {"deep": ["a\u{0001}b"]},
        });
        sanitize_request_body(&mut body);
        let content = body["messages"][0]["content"].as_str().unwrap();
        assert_eq!(content, "tab\there\nnl\rcrnuldelnel");
        assert_eq!(body["nested"]["deep"][0], "ab");
        assert_eq!(body["n"], 1);
    }

    #[test]
    fn strips_response_breaking_fields() {
        let mut v = json!({
            "choices": [{"delta": {"content": "ok"}}],
            "service_tier": "priority",
            "system_fingerprint": "fp_x",
            "x_groq": {"usage_tree": true},
            "usage": {"prompt_tokens": 1, "usage_breakdown": {"wasted": 9}},
        });
        sanitize_response_object(&mut v);
        assert!(v.get("service_tier").is_none());
        assert!(v.get("system_fingerprint").is_none());
        assert!(v.get("x_groq").is_none());
        assert!(v["usage"].get("usage_breakdown").is_none());
        assert_eq!(v["usage"]["prompt_tokens"], 1);
        assert_eq!(v["choices"][0]["delta"]["content"], "ok");
    }

    #[test]
    fn sse_sanitizer_preserves_framing_and_done() {
        let input = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}],\"service_tier\":\"x\"}\n\n",
            ": keep-alive comment\n\n",
            "data: not-json\n\n",
            "data: {\"usage\":{\"usage_breakdown\":{}}}\n\n",
            "data: [DONE]\n\n",
        );
        let out = sanitize_sse_body(input);
        assert!(!out.contains("service_tier"), "{out}");
        assert!(!out.contains("usage_breakdown"), "{out}");
        assert!(out.contains("data: [DONE]\n\n"), "{out}");
        assert!(out.contains(": keep-alive comment\n\n"), "{out}");
        assert!(out.contains("data: not-json\n\n"), "{out}");
        assert!(out.contains("\"content\":\"a\""), "{out}");
    }
}
