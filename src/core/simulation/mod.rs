//! Provider simulation layer (beads sim-01 types, sim-02 persistence, sim-05 errors).
//!
//! See `COMPREHENSIVE-PLAN-FOR-MOCK-SERVER.md` §2.5, §3.1.
//! NOTE: `Replay` / `Hybrid` variants arrive in a separate Phase-2 epic —
//! do NOT add variants here.

pub mod error;
pub mod mode;
pub mod persistence;
pub mod resolver;

pub use error::SimulationError;
pub use mode::{EffectiveReason, ProviderExecutionMode, ResolvedMode};
pub use resolver::{is_format_supported, resolve_effective_mode, ResolveInput, SIM_HEADER};
