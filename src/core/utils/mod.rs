//! Utilities ported from `open-sse/utils/` in 9router. Pure-logic helpers
//! (no streaming machinery) live here; streaming/transport-level helpers
//! that depend on Node-specific abstractions are reimplemented inline by
//! the relevant executor instead.

pub mod bypass_handler;
pub mod claude_cloaking;
pub mod claude_header_cache;
pub mod client_detector;
pub mod cursor_checksum;
pub mod error;
pub mod project_id_cache;
pub mod reasoning_content_injector;
pub mod session_manager;
pub mod tool_deduper;
