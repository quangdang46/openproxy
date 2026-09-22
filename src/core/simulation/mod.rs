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
