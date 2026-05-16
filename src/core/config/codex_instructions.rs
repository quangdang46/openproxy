//! Port of `open-sse/config/codexInstructions.js`. Default system
//! instructions injected into Codex requests when the upstream did not
//! supply its own. Verbatim from upstream.

/// Long-form Codex coding-agent system prompt. Verbatim port of the
/// upstream `CODEX_DEFAULT_INSTRUCTIONS` constant.
pub const CODEX_DEFAULT_INSTRUCTIONS: &str = include_str!("data/codex_default_instructions.txt");
