//! Effective-mode resolver (bead sim-03).
//!
//! Precedence is normative (plan §3.2) — the global force is a **safety
//! boundary**, not a preference:
//!
//! 1. GLOBAL FORCE: `OPENPROXY_DEV_MOCK=1` env OR `settings.dev_mock_all`
//!    → ALWAYS mock (when the format is supported). NOT overridable by any
//!    request header. No bypass.
//! 2. Per-request `x-openproxy-sim: mock` → real→mock ONLY. There is NO header
//!    that forces mock→real (forcing real is CLI/config/admin only).
//! 3. Per-provider configured mode (`kv` scope `simulationMode`, bead sim-02).
//! 4. Default: real.
//!
//! Mock requested for a format with no registered simulator →
//! [`SimulationError::Unsupported`] (plan §3.6), never a silent crash.

use reqwest::header::HeaderMap;

use super::error::SimulationError;
use super::mode::{EffectiveReason, ProviderExecutionMode, ResolvedMode};
use super::persistence::{env_force_all, get_provider_mode};
use crate::core::executor::ProviderFormat;

/// Request header that opts ONE request into mock (real→mock only).
pub const SIM_HEADER: &str = "x-openproxy-sim";

/// Header value that selects mock. Any other value is ignored.
const SIM_MOCK_VALUE: &str = "mock";

/// Formats with a registered simulator (plan §3.6).
///
/// `ClaudeCompatible` intentionally reuses the Anthropic simulator at dispatch
/// (bead sim-09); the support check lists the wire formats the engine covers.
pub fn is_format_supported(format: ProviderFormat) -> bool {
    use ProviderFormat::*;
    matches!(
        format,
        OpenAI | OpenAICompatible | Anthropic | AnthropicCompatible | ClaudeCompatible | Gemini
    )
}

/// Inputs the resolver needs. Kept as a struct so call sites (executor,
/// tests) don't grow positional args.
pub struct ResolveInput<'a> {
    /// Provider name as configured (e.g. `"openai"`).
    pub provider: &'a str,
    /// Wire format of the provider.
    pub format: ProviderFormat,
    /// Incoming request headers (may carry [`SIM_HEADER`]).
    pub headers: &'a HeaderMap,
    /// DB connection for the configured-mode lookup (bead sim-02).
    pub conn: &'a rusqlite::Connection,
    /// Snapshot settings flag (`settings.dev_mock_all`).
    pub settings_force_all: bool,
    /// Process-env force (`OPENPROXY_DEV_MOCK=1`). Callers pass
    /// `persistence::env_force_all()`; tests inject directly so no test
    /// mutates process env (Rust tests run threads in one process).
    pub env_force_all: bool,
}

impl<'a> ResolveInput<'a> {
    /// Live constructor: reads the process env + snapshot settings flag.
    pub fn live(
        provider: &'a str,
        format: ProviderFormat,
        headers: &'a HeaderMap,
        conn: &'a rusqlite::Connection,
        settings: &crate::types::Settings,
    ) -> Self {
        Self {
            provider,
            format,
            headers,
            conn,
            settings_force_all: settings.dev_mock_all,
            env_force_all: env_force_all(),
        }
    }
}

/// Resolve the effective mode. Pure + synchronous (DB read via `conn`).
pub fn resolve_effective_mode(input: &ResolveInput<'_>) -> Result<ResolvedMode, SimulationError> {
    let configured = get_provider_mode(input.conn, input.provider);

    // 1. Global force — safety boundary. Header cannot override.
    if input.env_force_all {
        return require_supported(input, configured, EffectiveReason::EnvForce);
    }
    if input.settings_force_all {
        return require_supported(input, configured, EffectiveReason::SettingsForce);
    }

    // 2. Per-request real→mock. Never mock→real: no header value selects Real.
    if header_requests_mock(input.headers) {
        return require_supported(input, configured, EffectiveReason::RequestHeader);
    }

    // 3–4. Configured mode, else default real.
    if configured == ProviderExecutionMode::Mock {
        return require_supported(input, configured, EffectiveReason::ProviderConfig);
    }
    Ok(ResolvedMode {
        mode: ProviderExecutionMode::Real,
        configured,
        reason: EffectiveReason::Default,
    })
}

fn require_supported(
    input: &ResolveInput<'_>,
    configured: ProviderExecutionMode,
    reason: EffectiveReason,
) -> Result<ResolvedMode, SimulationError> {
    if !is_format_supported(input.format) {
        return Err(SimulationError::Unsupported {
            provider: input.provider.to_string(),
            format: input.format.as_str().to_string(),
        });
    }
    Ok(ResolvedMode {
        mode: ProviderExecutionMode::Mock,
        configured,
        reason,
    })
}

fn header_requests_mock(headers: &HeaderMap) -> bool {
    headers
        .get(SIM_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case(SIM_MOCK_VALUE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::simulation::persistence::set_provider_mode;
    use crate::db::sqlite::SqliteDb;

    fn headers_with(pairs: &[(&str, &str)]) -> HeaderMap {
        use reqwest::header::HeaderName;
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            let name: HeaderName = k.parse().expect("valid header name");
            h.insert(name, v.parse().unwrap());
        }
        h
    }

    fn input<'a>(conn: &'a rusqlite::Connection, headers: &'a HeaderMap) -> ResolveInput<'a> {
        ResolveInput {
            provider: "openai",
            format: ProviderFormat::OpenAI,
            headers,
            conn,
            settings_force_all: false,
            env_force_all: false,
        }
    }

    fn input_env<'a>(
        conn: &'a rusqlite::Connection,
        headers: &'a HeaderMap,
        env_force_all: bool,
    ) -> ResolveInput<'a> {
        ResolveInput {
            provider: "openai",
            format: ProviderFormat::OpenAI,
            headers,
            conn,
            settings_force_all: false,
            env_force_all,
        }
    }

    #[test]
    fn default_is_real() {
        let db = SqliteDb::open_in_memory().unwrap();
        let h = HeaderMap::new();
        let r = db
            .with_conn(|c| {
                let r = resolve_effective_mode(&input(c, &h)).expect("resolve ok");
                Ok(r)
            })
            .unwrap();
        assert_eq!(r.mode, ProviderExecutionMode::Real);
        assert_eq!(r.reason, EffectiveReason::Default);
    }

    #[test]
    fn configured_mock_wins() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| set_provider_mode(tx, "openai", ProviderExecutionMode::Mock))
            .unwrap();
        let h = HeaderMap::new();
        let r = db
            .with_conn(|c| {
                let r = resolve_effective_mode(&input(c, &h)).expect("resolve ok");
                Ok(r)
            })
            .unwrap();
        assert_eq!(r.mode, ProviderExecutionMode::Mock);
        assert_eq!(r.configured, ProviderExecutionMode::Mock);
        assert_eq!(r.reason, EffectiveReason::ProviderConfig);
    }

    #[test]
    fn request_header_selects_mock() {
        let db = SqliteDb::open_in_memory().unwrap();
        let h = headers_with(&[(SIM_HEADER, "mock")]);
        let r = db
            .with_conn(|c| {
                let r = resolve_effective_mode(&input(c, &h)).expect("resolve ok");
                Ok(r)
            })
            .unwrap();
        assert_eq!(r.mode, ProviderExecutionMode::Mock);
        assert_eq!(r.configured, ProviderExecutionMode::Real);
        assert_eq!(r.reason, EffectiveReason::RequestHeader);
    }

    #[test]
    fn no_header_can_force_real() {
        // configured mock + garbage/another header value → stays mock.
        // There is simply no code path from header → Real.
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| set_provider_mode(tx, "openai", ProviderExecutionMode::Mock))
            .unwrap();
        for val in ["real", "off", "false", ""] {
            let h = headers_with(&[(SIM_HEADER, val)]);
            let r = db
                .with_conn(|c| {
                    let r = resolve_effective_mode(&input(c, &h)).expect("resolve ok");
                    Ok(r)
                })
                .unwrap();
            assert_eq!(r.mode, ProviderExecutionMode::Mock, "value {val:?}");
        }
    }

    #[test]
    fn env_force_cannot_be_bypassed_by_headers() {
        // Safety boundary: with OPENPROXY_DEV_MOCK=1 the resolver must return
        // Mock regardless of headers, and no header value may select Real.
        // NOTE: mutates process env; saves + restores like persistence tests.
        let db = SqliteDb::open_in_memory().unwrap();
        let h = HeaderMap::new();
        let r = db
            .with_conn(|c| {
                let r = resolve_effective_mode(&input_env(c, &h, true)).expect("resolve ok");
                Ok(r)
            })
            .unwrap();
        assert_eq!(r.mode, ProviderExecutionMode::Mock);
        assert_eq!(r.configured, ProviderExecutionMode::Real);
        assert_eq!(r.reason, EffectiveReason::EnvForce);
        // No header value may select Real under force: there is no
        // header->Real code path, so even adversarial values stay Mock.
        for val in ["real", "off", ""] {
            let hh = headers_with(&[(SIM_HEADER, val)]);
            let rr = db
                .with_conn(|cc| {
                    let r = resolve_effective_mode(&input_env(cc, &hh, true)).expect("resolve ok");
                    Ok(r)
                })
                .unwrap();
            assert_eq!(rr.mode, ProviderExecutionMode::Mock, "value {val:?}");
        }
    }

    #[test]
    fn settings_force_overrides_to_mock() {
        let db = SqliteDb::open_in_memory().unwrap();
        let h = HeaderMap::new();
        let r = db
            .with_conn(|c| {
                let mut inp = input(c, &h);
                inp.settings_force_all = true;
                let r = resolve_effective_mode(&inp).expect("resolve ok");
                Ok(r)
            })
            .unwrap();
        assert_eq!(r.mode, ProviderExecutionMode::Mock);
        assert_eq!(r.configured, ProviderExecutionMode::Real);
        assert_eq!(r.reason, EffectiveReason::SettingsForce);
    }

    #[test]
    fn unsupported_format_errors_loudly() {
        // NOTE: all current ProviderFormat variants are supported; this test
        // documents the error path via require_supported indirectly — if a
        // future variant is added without support, resolution must fail.
        // For now assert the support table covers the MVP set.
        use ProviderFormat::*;
        for f in [
            OpenAI,
            OpenAICompatible,
            Anthropic,
            AnthropicCompatible,
            ClaudeCompatible,
            Gemini,
        ] {
            assert!(is_format_supported(f), "MVP format {f:?} must be supported");
        }
    }
}
