//! Provider simulation layer (sim-01..11: types, persistence, resolver, errors,
//! engine, OpenAI, Anthropic, Gemini, models).
//!
//! See `COMPREHENSIVE-PLAN-FOR-MOCK-SERVER.md` §2.5, §3.1.
//! NOTE: `Replay` / `Hybrid` variants arrive in a separate Phase-2 epic —
//! do NOT add variants here.

pub mod anthropic;
pub mod engine;
pub mod error;
pub mod fault;
pub mod gemini;
pub mod mode;
pub mod models;
pub mod openai;
pub mod persistence;
pub mod resolver;

/// Whether the simulation engine may activate in this build.
///
/// `false` for `cargo build`, `cargo install` and every release binary — the
/// default feature set does not include `simulation`. The engine is still
/// compiled; this is a gate, not a deletion, so `--features simulation` and
/// the engine's own tests keep working.
///
/// The reason it is off by default: `/v1/models` advertises the real provider
/// catalog, while the simulator only knows its own built-in model list. With
/// mock reachable, every advertised id 404'd at the simulator — the one
/// configuration where a listed model does not route anywhere.
pub const ENABLED: bool = cfg!(feature = "simulation");

/// The activation rule, as a pure function of its four inputs.
///
/// Split out so the gate is testable in a single build. The executor, the chat
/// dispatch ladder and the CLI all funnel here; the pure form lets a test
/// assert that a closed gate wins over every other signal without needing a
/// second feature combination compiled.
pub fn should_simulate(
    gate_open: bool,
    env_forced: bool,
    force_mock: bool,
    header_mock: bool,
) -> bool {
    gate_open && (env_forced || force_mock || header_mock)
}

pub use anthropic::sse_body as sse_body_anthropic;
pub use engine::{SimContext, SimulationEngine};
pub use error::SimulationError;
pub use fault::{FaultInjector, FaultSpec, OverrideAction};
pub use gemini::sse_body as sse_body_gemini;
pub use mode::{EffectiveReason, ProviderExecutionMode, ResolvedMode};
pub use openai::sse_body as sse_body_openai;
pub use persistence::env_force_all;
pub use resolver::{
    is_format_supported, resolve_effective_mode, status_for, ProviderModeStatus, ResolveInput,
    SIM_HEADER,
};

#[cfg(test)]
mod gate_tests {
    use super::{should_simulate, ENABLED};

    /// A closed gate wins over every activation signal, including a
    /// `sim-stub-` connection id, an env force, the stored `devMockAll`
    /// setting, and a per-request `x-openproxy-sim: mock` header. A released
    /// binary has to be un-simulatable by *all* of them at once — testing
    /// them one at a time would pass a gate that only covered the first.
    #[test]
    fn a_closed_gate_overrides_every_signal() {
        for (env, force, header) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, true),
            (true, true, true),
        ] {
            assert!(
                !should_simulate(false, env, force, header),
                "a closed gate simulated with env={env} force_mock={force} header={header}"
            );
        }
    }

    /// An open gate still honours the signals — the gate is a ceiling, not a
    /// replacement for the rules above it.
    #[test]
    fn an_open_gate_still_requires_a_signal() {
        assert!(!should_simulate(true, false, false, false));
        assert!(should_simulate(true, true, false, false));
        assert!(should_simulate(true, false, true, false));
        assert!(should_simulate(true, false, false, true));
    }

    /// Runs only in a build without the `simulation` feature, i.e. what
    /// `cargo build`, `cargo install` and a release binary produce. The
    /// other two assertions above hold in any build, so this is the one that
    /// pins the default.
    #[cfg(not(feature = "simulation"))]
    #[test]
    fn the_default_build_cannot_simulate() {
        assert!(
            !ENABLED,
            "the simulation engine is reachable in a default build; `simulation` \
             must not be in the default feature set"
        );
    }
}
