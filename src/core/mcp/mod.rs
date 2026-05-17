//! Local stdio→SSE bridge for MCP plugins. Ports
//! `src/lib/mcp/stdioSseBridge.js` from upstream 9router so existing MCP
//! clients (Claude desktop, etc.) can connect to openproxy via the same
//! `/api/mcp/<plugin>/sse` + `/api/mcp/<plugin>/message` wire protocol.
//!
//! Layout:
//!   * [`smart_filter`] — text noise stripper + frame filter (pure logic).
//!   * [`plugins`]      — built-in plugin catalog + allowlist + custom store.
//!   * [`bridge`]       — per-plugin child process + broadcast channel.
//!
//! The HTTP handlers live in `crate::server::api::mcp`.

pub mod bridge;
pub mod plugins;
pub mod smart_filter;
