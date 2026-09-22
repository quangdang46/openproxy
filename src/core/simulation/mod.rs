//! Provider simulation layer (sim-01..07: types, persistence, resolver, errors, engine, OpenAI).
//!
//! See `COMPREHENSIVE-PLAN-FOR-MOCK-SERVER.md` §2.5, §3.1.
//! NOTE: `Replay` / `Hybrid` variants arrive in a separate Phase-2 epic —
//! do NOT add variants here.

pub mod anthropic;
pub mod engine;
pub mod error;
pub mod gemini;
pub mod mode;
pub mod openai;
pub mod persistence;
pub mod resolver;

pub use anthropic::sse_body as sse_body_anthropic;
pub use engine::{SimContext, SimulationEngine};
pub use error::SimulationError;
pub use gemini::sse_body as sse_body_gemini;
pub use mode::{EffectiveReason, ProviderExecutionMode, ResolvedMode};
pub use openai::sse_body as sse_body_openai;
pub use persistence::env_force_all;
pub use resolver::{is_format_supported, resolve_effective_mode, ResolveInput, SIM_HEADER};
