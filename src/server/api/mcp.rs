//! HTTP routes for MCP stdio→SSE bridge. Mirrors upstream 9router's wire
//! protocol byte-for-byte so existing MCP clients (Claude desktop's
//! `--transport sse`, etc.) connect unchanged.
//!
//! Wire protocol:
//!   * `GET  /api/mcp/<plugin>/sse`     — server-sent events. First frame is
//!     `event: endpoint\ndata: /api/mcp/<plugin>/message?sessionId=<uuid>\n\n`
//!     followed by `event: message\ndata: <json>\n\n` for every JSON-RPC
//!     frame the child writes to stdout.
//!   * `POST /api/mcp/<plugin>/message` — accept one JSON-RPC frame in the
//!     body and forward it to the child's stdin. Returns 202.

use std::path::PathBuf;

use async_stream::stream;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::mcp::bridge;
use crate::core::mcp::plugins::find_plugin;
use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/mcp/{plugin}/sse", get(sse_handler))
        .route("/api/mcp/{plugin}/message", post(message_handler))
}

fn data_dir(state: &AppState) -> PathBuf {
    state.db.data_dir.clone()
}

async fn sse_handler(State(state): State<AppState>, Path(plugin): Path<String>) -> Response {
    let dir = data_dir(&state);
    if find_plugin(&plugin, &dir).is_none() {
        return (StatusCode::NOT_FOUND, format!("Unknown plugin: {plugin}")).into_response();
    }

    let entry = match bridge::get_or_spawn(&plugin, &dir) {
        Ok(e) => e,
        Err(err) => {
            tracing::error!(
                target: "openproxy::mcp",
                plugin = %plugin,
                error = %err,
                "failed to spawn MCP bridge"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to spawn `{plugin}`: {err}"),
            )
                .into_response();
        }
    };

    let session_id = Uuid::new_v4().to_string();
    let endpoint_frame =
        format!("event: endpoint\ndata: /api/mcp/{plugin}/message?sessionId={session_id}\n\n");

    let mut rx = entry.subscribe();
    let plugin_for_log = plugin.clone();
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
                        plugin = %plugin_for_log,
                        skipped,
                        "SSE session lagged; some frames were dropped"
                    );
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header(header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|err| {
            tracing::error!(target: "openproxy::mcp", error = %err, "build SSE response");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        })
}

async fn message_handler(
    State(state): State<AppState>,
    Path(plugin): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let dir = data_dir(&state);
    if find_plugin(&plugin, &dir).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Unknown plugin: {plugin}") })),
        )
            .into_response();
    }

    let entry = match bridge::get(&plugin) {
        Some(e) => e,
        None => {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!("Bridge not running for `{plugin}`; open the SSE stream first.")
                })),
            )
                .into_response();
        }
    };

    if let Err(err) = entry.send_to_child(&body).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response();
    }

    StatusCode::ACCEPTED.into_response()
}
