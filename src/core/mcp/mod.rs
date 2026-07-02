//! MCP (Model Context Protocol) support for OpenProxy.
//!
//! Two MCP modes:
//!   * **Stdio-bridge mode** — spawns external MCP child processes (e.g.
//!     `browsermcp`) and bridges their stdio to SSE at `/api/mcp/<plugin>/*`.
//!     Layout: [`bridge`], [`plugins`], [`smart_filter`].
//!     The HTTP handlers live in `crate::server::api::mcp`.
//!   * **Native server mode** — implements the MCP JSON-RPC 2.0 protocol
//!     directly inside OpenProxy with a built-in tool registry of ~15
//!     administrative tools. Served at `/api/mcp-server/*` and `/api/mcp`.
//!     Layout: [`server`].

pub mod bridge;
pub mod plugins;
pub mod server;
pub mod smart_filter;
