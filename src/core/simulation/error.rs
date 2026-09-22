//! Simulation error types (bead sim-05).
//!
//! [`SimulationError`] is the subsystem error. The one variant the executor
//! must surface explicitly is [`SimulationError::Unsupported`]: mock was
//! requested for a provider format with no registered simulator. It must
//! render as "mock unavailable for this provider format" — never a silent
//! crash, never a fake MOCK badge (plan §3.6).
//!
//! Conversion into [`crate::core::executor::ExecutorError`] is intentionally
//! explicit (`From` impl in `engine.rs`, bead sim-06) so the mapping stays
//! visible at the dispatch site.

use std::fmt;

/// Subsystem error for `src/core/simulation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulationError {
    /// Mock requested for a format with no registered simulator.
    Unsupported {
        /// Provider name as configured (e.g. `"my-custom"`).
        provider: String,
        /// Format string (`ProviderFormat::as_str()`).
        format: String,
    },
    /// Simulator failed to build a response (bug — not a provider error).
    Internal(String),
    /// Provider-correct request rejection (bead sim-08): malformed body,
    /// unknown model. Carries the exact HTTP status + error envelope the real
    /// provider would return, so callers render it without translation.
    Validation {
        /// HTTP status (400 malformed, 404 unknown model).
        status: u16,
        /// Exact provider error envelope JSON.
        body: serde_json::Value,
        /// Optional Retry-After seconds (429 path, bead sim-12 extends).
        retry_after: Option<u64>,
    },
}

impl SimulationError {
    /// Human-facing message for API/CLI/dashboard surfaces.
    pub fn message(&self) -> String {
        match self {
            Self::Unsupported { provider, format } => format!(
                "mock unavailable for this provider format (provider: {provider}, format: {format})"
            ),
            Self::Internal(detail) => format!("simulation internal error: {detail}"),
            Self::Validation { status, body, .. } => {
                format!("simulation validation error {status}: {body}")
            }
        }
    }

    /// Whether the provider carries simulation **configuration** but the
    /// format cannot **execute** (plan §3.6 `simulationSupported: false`).
    pub fn simulation_supported(&self) -> bool {
        false
    }
}

impl fmt::Display for SimulationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for SimulationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_message_names_provider_and_format() {
        let e = SimulationError::Unsupported {
            provider: "my-custom".into(),
            format: "weird_format".into(),
        };
        let msg = e.message();
        assert!(msg.contains("my-custom"), "missing provider: {msg}");
        assert!(msg.contains("weird_format"), "missing format: {msg}");
        assert!(
            msg.contains("mock unavailable"),
            "must say unavailable: {msg}"
        );
        assert!(!e.simulation_supported());
    }

    #[test]
    fn validation_message() {
        let e = SimulationError::Validation {
            status: 404,
            body: serde_json::json!({"error": {"code": "model_not_found"}}),
            retry_after: None,
        };
        let msg = e.message();
        assert!(msg.contains("404"), "missing status: {msg}");
        assert!(!e.simulation_supported());
    }

    #[test]
    fn internal_message() {
        let e = SimulationError::Internal("boom".into());
        assert!(e.message().contains("boom"));
    }
}
