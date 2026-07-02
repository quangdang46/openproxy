//! HTTP endpoints for the native MCP server.
//!
//! Provides an SSE-based MCP transport endpoint so any MCP client (Claude
//! Desktop, Cursor, Cline, etc.) can discover and invoke OpenProxy's
//! administrative tools directly via the MCP protocol — no child process bridge
//! required.
//!
//! Wire protocol:
//!   * `GET  /api/mcp-server/sse`         — SSE stream. First frame is
//!     `event: endpoint\ndata: /api/mcp-server/message?sessionId=<uuid>\n\n`
//!     followed by `event: message\ndata: <jsonrpc-response>\n\n` for responses.
//!   * `POST /api/mcp-server/message`     — Accept one JSON-RPC 2.0 frame and
//!     process it against the built-in tool registry. Returns 202 (async).
//!
//! This is DIFFERENT from `/api/mcp/<plugin>/...` which bridges to external
//! stdio-based MCP child processes. This endpoint IS the MCP server.

use std::sync::Arc;

use async_stream::stream;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::core::mcp::server;
use crate::server::state::AppState;

/// Broadcast capacity for SSE event channels.
const SSE_CAPACITY: usize = 64;

#[derive(Debug, Deserialize)]
struct SessionQuery {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

/// Build the sub-router mounted at `/api/mcp-server` and `/api/mcp`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/mcp-server/sse", get(sse_handler))
        .route("/api/mcp-server/message", post(message_handler))
        // Stateless JSON-RPC endpoint for MCP clients that don't need SSE
        // streaming. Accepts any valid MCP method (initialize, tools/list,
        // tools/call, resources/list, resources/read) and returns the
        // response directly.
        .route("/api/mcp", post(stateless_mcp_handler))
}

/// SSE handler: opens a long-lived connection and pushes MCP JSON-RPC responses
/// as SSE `message` events.
async fn sse_handler(State(state): State<AppState>) -> Response {
    let (sender, _) = broadcast::channel::<String>(SSE_CAPACITY);

    let session_id = Uuid::new_v4().to_string();
    let endpoint_frame =
        format!("event: endpoint\ndata: /api/mcp-server/message?sessionId={session_id}\n\n");

    // Store the sender keyed by session ID for the message handler to find.
    // Use a simple drop-guard pattern: register, then remove on disconnect.
    let store = McpSessionStore::global();
    store.register(&session_id, sender.clone());

    let mut rx = sender.subscribe();
    let session_for_cleanup = session_id.clone();

    let body_stream = stream! {
        yield Ok::<_, std::convert::Infallible>(bytes::Bytes::from(endpoint_frame));
        loop {
            match rx.recv().await {
                Ok(line) => {
                    let frame = format!("event: message\ndata: {line}\n\n");
                    yield Ok(bytes::Bytes::from(frame));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        target: "openproxy::mcp",
                        skipped,
                        "MCP server SSE session lagged; dropping frames"
                    );
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
        store.unregister(&session_for_cleanup);
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header(header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|err| {
            tracing::error!(target: "openproxy::mcp", error = %err, "build MCP server SSE response");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        })
}

/// Message handler: processes a JSON-RPC 2.0 request synchronously and sends
/// the response back over the SSE channel identified by `sessionId`.
async fn message_handler(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let session_id = match query.session_id {
        Some(ref sid) if !sid.is_empty() => sid.clone(),
        _ => {
            // No session ID — this is a stateless request. Process inline
            // and return the response directly (for clients that don't use SSE).
            let response = server::handle_mcp_request(&state, &body);
            return Json(response).into_response();
        }
    };

    let store = McpSessionStore::global();
    let sender = match store.get(&session_id) {
        Some(s) => s,
        None => {
            // Session not found; process stateless.
            let response = server::handle_mcp_request(&state, &body);
            return Json(response).into_response();
        }
    };

    let response = server::handle_mcp_request(&state, &body);
    let response_str = serde_json::to_string(&response).unwrap_or_default();
    let _ = sender.send(response_str);

    StatusCode::ACCEPTED.into_response()
}

/// Stateless JSON-RPC MCP endpoint.
///
/// Accepts any valid MCP method (`initialize`, `tools/list`, `tools/call`,
/// `resources/list`, `resources/read`) and returns the response directly
/// in the HTTP response body. No SSE session required.
///
/// This is the simplest way for MCP clients to interact with OpenProxy's
/// built-in tools — just POST a JSON-RPC 2.0 request and get a response back.
async fn stateless_mcp_handler(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let response = server::handle_mcp_request(&state, &body);
    Json(response).into_response()
}

// ─── Session Store ─────────────────────────────────────────────────────────

/// In-memory store of SSE broadcast senders, keyed by session ID.
struct McpSessionStore {
    inner: std::sync::Mutex<std::collections::HashMap<String, broadcast::Sender<String>>>,
}

impl McpSessionStore {
    fn global() -> &'static McpSessionStore {
        static STORE: std::sync::OnceLock<McpSessionStore> = std::sync::OnceLock::new();
        STORE.get_or_init(|| McpSessionStore {
            inner: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn register(&self, id: &str, sender: broadcast::Sender<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(id.to_string(), sender);
        }
    }

    fn unregister(&self, id: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.remove(id);
        }
    }

    fn get(&self, id: &str) -> Option<broadcast::Sender<String>> {
        if let Ok(guard) = self.inner.lock() {
            guard.get(id).cloned()
        } else {
            None
        }
    }
}
