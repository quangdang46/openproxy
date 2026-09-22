//! Simulation mode types (bead sim-01).
//!
//! [`ProviderExecutionMode`] is the configured-or-effective mode of a single
//! provider. [`ResolvedMode`] pairs the effective mode with what was configured
//! and *why* they differ ([`EffectiveReason`]).
//!
//! Resolution logic lives in bead sim-03 — this file is types only.

use std::fmt;

/// Execution mode of one provider.
///
/// `Replay` / `Hybrid` intentionally absent: Phase-2 epic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderExecutionMode {
    /// Forward to the real provider (production path, untouched).
    #[default]
    Real,
    /// Synthesize a protocol-faithful response, zero network.
    Mock,
}

impl fmt::Display for ProviderExecutionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Real => write!(f, "real"),
            Self::Mock => write!(f, "mock"),
        }
    }
}

/// Why the effective mode is what it is (plan §3.3 `effectiveReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EffectiveReason {
    /// Per-provider `providers.mode` column.
    ProviderConfig,
    /// Per-request `x-openproxy-sim: mock` header (real→mock only).
    RequestHeader,
    /// `OPENPROXY_DEV_MOCK=1` environment override (safety boundary).
    EnvForce,
    /// `settings.dev_mock_all=true` override (safety boundary).
    SettingsForce,
    /// Nothing configured — the default.
    #[default]
    Default,
}

impl fmt::Display for EffectiveReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderConfig => write!(f, "provider-config"),
            Self::RequestHeader => write!(f, "request-header"),
            Self::EnvForce => write!(f, "OPENPROXY_DEV_MOCK"),
            Self::SettingsForce => write!(f, "settings-force"),
            Self::Default => write!(f, "default"),
        }
    }
}

/// Resolved mode for one provider on one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedMode {
    /// Mode that will actually execute.
    pub mode: ProviderExecutionMode,
    /// Mode stored in configuration (DB column).
    pub configured: ProviderExecutionMode,
    /// Why `mode` is what it is.
    pub reason: EffectiveReason,
}

impl ResolvedMode {
    /// The default: real configured, real effective, no override.
    pub fn default_real() -> Self {
        Self {
            mode: ProviderExecutionMode::Real,
            configured: ProviderExecutionMode::Real,
            reason: EffectiveReason::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_is_real() {
        assert_eq!(
            ProviderExecutionMode::default(),
            ProviderExecutionMode::Real
        );
        let r = ResolvedMode::default_real();
        assert_eq!(r.mode, ProviderExecutionMode::Real);
        assert_eq!(r.configured, ProviderExecutionMode::Real);
        assert_eq!(r.reason, EffectiveReason::Default);
    }

    #[test]
    fn display_strings_are_exact() {
        assert_eq!(ProviderExecutionMode::Real.to_string(), "real");
        assert_eq!(ProviderExecutionMode::Mock.to_string(), "mock");
        assert_eq!(
            EffectiveReason::ProviderConfig.to_string(),
            "provider-config"
        );
        assert_eq!(EffectiveReason::RequestHeader.to_string(), "request-header");
        assert_eq!(EffectiveReason::EnvForce.to_string(), "OPENPROXY_DEV_MOCK");
        assert_eq!(EffectiveReason::SettingsForce.to_string(), "settings-force");
        assert_eq!(EffectiveReason::Default.to_string(), "default");
    }
}
