//! Provider simulation layer — types only (bead sim-01).
//!
//! See `COMPREHENSIVE-PLAN-FOR-MOCK-SERVER.md` §2.5, §3.1.
//! NOTE: `Replay` / `Hybrid` variants arrive in a separate Phase-2 epic —
//! do NOT add variants here.

pub mod mode;

pub use mode::{EffectiveReason, ProviderExecutionMode, ResolvedMode};
