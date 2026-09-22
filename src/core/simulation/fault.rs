//! Fault injection: spec parsing + injector (beads sim-12/13/14).
//!
//! Headers (namespace `x-openproxy-sim-*`, plan §5.1):
//! - `x-openproxy-sim-status`: 400|429|500|503 → provider-correct error
//!   envelope. Other values IGNORED + warn-logged, never crash.
//! - `x-openproxy-sim-latency-ms`: u64, saturate at 60_000 (bead sim-13).
//! - `x-openproxy-sim-response`: raw override string (bead sim-14).
//! - `x-openproxy-sim-disconnect-after-chunks`: usize (bead sim-14).
//!
//! [`FaultInjector`] is execution-result middleware (plan §2.4): it wraps
//! BOTH the mock and real branches and transforms complete responses AND
//! streaming bodies. This bead implements status fault on complete
//! (non-stream) responses; latency/disconnect arrive in sim-13/14.

use reqwest::header::HeaderMap;

use crate::core::executor::ProviderFormat;

/// Parsed fault controls for one request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FaultSpec {
    /// Forced HTTP status (allowlist 400/429/500/503; else None).
    pub status: Option<u16>,
    /// First-byte delay in ms (bead sim-13; parsed here).
    pub latency_ms: u64,
    /// Raw content override (bead sim-14; parsed here).
    pub response_override: Option<String>,
    /// SSE cut after N chunks (bead sim-14; parsed here).
    pub disconnect_after_chunks: Option<usize>,
}

/// Header names (single source of truth).
pub const HDR_STATUS: &str = "x-openproxy-sim-status";
pub const HDR_LATENCY_MS: &str = "x-openproxy-sim-latency-ms";
pub const HDR_RESPONSE: &str = "x-openproxy-sim-response";
pub const HDR_DISCONNECT: &str = "x-openproxy-sim-disconnect-after-chunks";

/// Statuses the injector may force. Anything else is ignored + warn-logged.
const ALLOWED_STATUS: &[u16] = &[400, 429, 500, 503];

/// Upper bound for injected latency (60s; prevents accidental hangs).
pub const MAX_LATENCY_MS: u64 = 60_000;

impl FaultSpec {
    /// Parse from incoming request headers. Never fails: invalid values are
    /// ignored (status/disconnect) or saturated (latency).
    pub fn parse(headers: &HeaderMap) -> Self {
        let mut spec = FaultSpec::default();
        if let Some(v) = headers.get(HDR_STATUS).and_then(|h| h.to_str().ok()) {
            match v.trim().parse::<u16>() {
                Ok(code) if ALLOWED_STATUS.contains(&code) => spec.status = Some(code),
                _ => tracing::warn!(
                    target: "openproxy::simulation",
                    "ignoring unsupported {HDR_STATUS} value {v:?} (allowlist 400/429/500/503)"
                ),
            }
        }
        if let Some(v) = headers.get(HDR_LATENCY_MS).and_then(|h| h.to_str().ok()) {
            if let Ok(ms) = v.trim().parse::<u64>() {
                spec.latency_ms = ms.min(MAX_LATENCY_MS);
            } else {
                tracing::warn!(
                    target: "openproxy::simulation",
                    "ignoring invalid {HDR_LATENCY_MS} value {v:?}"
                );
            }
        }
        if let Some(v) = headers.get(HDR_RESPONSE).and_then(|h| h.to_str().ok()) {
            spec.response_override = Some(v.to_string());
        }
        if let Some(v) = headers.get(HDR_DISCONNECT).and_then(|h| h.to_str().ok()) {
            match v.trim().parse::<usize>() {
                Ok(n) => spec.disconnect_after_chunks = Some(n),
                Err(_) => tracing::warn!(
                    target: "openproxy::simulation",
                    "ignoring invalid {HDR_DISCONNECT} value {v:?}"
                ),
            }
        }
        spec
    }

    /// Whether any fault is armed.
    pub fn is_empty(&self) -> bool {
        self.status.is_none()
            && self.latency_ms == 0
            && self.response_override.is_none()
            && self.disconnect_after_chunks.is_none()
    }
}

/// Execution-result middleware (plan §2.4). This bead: status fault on
/// complete responses. Produces the exact `(status, envelope, retry_after)`
/// the real provider would return for the given format.
pub struct FaultInjector;

impl FaultInjector {
    /// Status fault for one format. Returns `None` when no status armed
    /// (caller passes the result through untouched).
    pub fn status_fault(
        format: ProviderFormat,
        provider: &str,
        spec: &FaultSpec,
    ) -> Option<(u16, serde_json::Value, Option<u64>)> {
        let status = spec.status?;
        let body = match format {
            ProviderFormat::OpenAI | ProviderFormat::OpenAICompatible => {
                openai_error(status, provider)
            }
            ProviderFormat::Anthropic
            | ProviderFormat::AnthropicCompatible
            | ProviderFormat::ClaudeCompatible => anthropic_error(status, provider),
            ProviderFormat::Gemini => gemini_error(status, provider),
        };
        let retry_after = if status == 429 { Some(2) } else { None };
        Some((status, body, retry_after))
    }
}

fn openai_error(status: u16, provider: &str) -> serde_json::Value {
    let (message, err_type, code) = match status {
        400 => (
            format!("Simulated bad request for provider {provider}."),
            "invalid_request_error",
            "sim_bad_request",
        ),
        429 => (
            format!("Simulated rate limit exceeded for provider {provider}."),
            "rate_limit_error",
            "rate_limit_exceeded",
        ),
        500 => (
            format!("Simulated internal error for provider {provider}."),
            "server_error",
            "internal_error",
        ),
        _ => (
            format!("Simulated service unavailable for provider {provider}."),
            "server_error",
            "service_unavailable",
        ),
    };
    serde_json::json!({"error": {
        "message": message, "type": err_type, "param": serde_json::Value::Null, "code": code,
    }})
}

fn anthropic_error(status: u16, provider: &str) -> serde_json::Value {
    let (err_type, message) = match status {
        400 => (
            "invalid_request_error",
            format!("Simulated bad request for provider {provider}."),
        ),
        429 => (
            "rate_limit_error",
            format!("Simulated rate limit exceeded for provider {provider}."),
        ),
        500 => (
            "api_error",
            format!("Simulated internal error for provider {provider}."),
        ),
        _ => (
            "overloaded_error",
            format!("Simulated service unavailable for provider {provider}."),
        ),
    };
    serde_json::json!({"type": "error", "error": {
        "type": err_type, "message": message,
    }})
}

fn gemini_error(status: u16, provider: &str) -> serde_json::Value {
    let (code, message, status_str) = match status {
        400 => (
            400,
            format!("Simulated bad request for provider {provider}."),
            "INVALID_ARGUMENT",
        ),
        429 => (
            429,
            format!("Simulated rate limit exceeded for provider {provider}."),
            "RESOURCE_EXHAUSTED",
        ),
        500 => (
            500,
            format!("Simulated internal error for provider {provider}."),
            "INTERNAL",
        ),
        _ => (
            503,
            format!("Simulated service unavailable for provider {provider}."),
            "UNAVAILABLE",
        ),
    };
    serde_json::json!({"error": {
        "code": code, "message": message, "status": status_str,
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        use reqwest::header::HeaderName;
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(k.parse::<HeaderName>().unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn parse_status_allowlist() {
        for code in ["400", "429", "500", "503"] {
            let s = FaultSpec::parse(&headers(&[(HDR_STATUS, code)]));
            assert_eq!(s.status, Some(code.parse().unwrap()));
        }
        // Not allowlisted → ignored, never crash.
        for code in ["200", "404", "418", "abc", "", "429 "] {
            if code.trim() == "429" {
                continue; // "429 " trims to allowed — tested separately
            }
            let s = FaultSpec::parse(&headers(&[(HDR_STATUS, code)]));
            assert_eq!(s.status, None, "value {code:?}");
        }
        // Whitespace-tolerant for allowlisted values.
        let s = FaultSpec::parse(&headers(&[(HDR_STATUS, " 429 ")]));
        assert_eq!(s.status, Some(429));
    }

    #[test]
    fn parse_latency_saturates() {
        let s = FaultSpec::parse(&headers(&[(HDR_LATENCY_MS, "250")]));
        assert_eq!(s.latency_ms, 250);
        let s = FaultSpec::parse(&headers(&[(HDR_LATENCY_MS, "99999999")]));
        assert_eq!(s.latency_ms, MAX_LATENCY_MS);
        let s = FaultSpec::parse(&headers(&[(HDR_LATENCY_MS, "abc")]));
        assert_eq!(s.latency_ms, 0);
        assert!(FaultSpec::parse(&HeaderMap::new()).is_empty());
    }

    #[test]
    fn parse_response_and_disconnect() {
        let s = FaultSpec::parse(&headers(&[
            (HDR_RESPONSE, "{\"content\":\"hi\"}"),
            (HDR_DISCONNECT, "3"),
        ]));
        assert_eq!(
            s.response_override,
            Some("{\"content\":\"hi\"}".to_string())
        );
        assert_eq!(s.disconnect_after_chunks, Some(3));
        assert!(!s.is_empty());
        let bad = FaultSpec::parse(&headers(&[(HDR_DISCONNECT, "x")]));
        assert_eq!(bad.disconnect_after_chunks, None);
    }

    #[test]
    fn status_fault_envelopes_per_format() {
        use ProviderFormat::*;
        let o = FaultInjector::status_fault(
            OpenAI,
            "openai",
            &FaultSpec {
                status: Some(429),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(o.0, 429);
        assert_eq!(o.1["error"]["type"], "rate_limit_error");
        assert_eq!(o.2, Some(2), "Retry-After on 429");

        let a = FaultInjector::status_fault(
            Anthropic,
            "anthropic",
            &FaultSpec {
                status: Some(503),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(a.0, 503);
        assert_eq!(a.1["type"], "error");
        assert_eq!(a.2, None, "no Retry-After on 503");

        let g = FaultInjector::status_fault(
            Gemini,
            "gemini",
            &FaultSpec {
                status: Some(400),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(g.0, 400);
        assert_eq!(g.1["error"]["status"], "INVALID_ARGUMENT");

        // Compatible formats share envelopes.
        let oc = FaultInjector::status_fault(
            OpenAICompatible,
            "x",
            &FaultSpec {
                status: Some(500),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(oc.1.get("error").is_some());
        let ac = FaultInjector::status_fault(
            AnthropicCompatible,
            "x",
            &FaultSpec {
                status: Some(500),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(ac.1["type"], "error");
    }

    #[test]
    fn no_status_passes_through() {
        let r =
            FaultInjector::status_fault(ProviderFormat::OpenAI, "openai", &FaultSpec::default());
        assert!(r.is_none(), "non-fault result untouched");
    }
}
