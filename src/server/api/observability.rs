//! Observability API endpoints.
//!
//! Backed by the shared `ConsoleLogBuffer` so the CLI's `openproxy logs *`
//! commands have a single stable source of log lines:
//!
//! - `GET /api/observability/logs` — snapshot of the in-memory buffer.
//! - `GET /api/observability/stream` — SSE feed of new log lines (used by
//!   `openproxy logs tail --follow`).
//! - `GET /api/observability/stats` — rough counters (total lines, last
//!   timestamp). Combined with the usage stats this gives `logs stats` a
//!   useful payload without invasively re-instrumenting handlers.
//! - `POST /api/observability/clear` — wipe the buffer.

use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use bytes::Bytes;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time;

use crate::server::auth::require_api_key;
use crate::server::console_logs::ConsoleLogEvent;
use crate::server::state::AppState;

use super::auth_error_response;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/observability/logs", get(get_logs))
        .route("/api/observability/stream", get(stream_logs))
        .route("/api/observability/stats", get(get_stats))
        .route("/api/observability/clear", post(clear_logs))
}

async fn get_logs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let logs = state.console_logs.get_logs().await;
    let count = logs.len();
    Json(json!({
        "logs": logs,
        "count": count,
    }))
    .into_response()
}

async fn stream_logs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let mut receiver = state.console_logs.subscribe();
    let body = Body::from_stream(async_stream::stream! {
        // Emit a single ping right away so the client knows the connection is up.
        yield Ok::<Bytes, std::io::Error>(Bytes::from_static(b": connected\n\n"));
        let mut keepalive = time::interval(Duration::from_secs(25));
        loop {
            tokio::select! {
                _ = keepalive.tick() => {
                    yield Ok(Bytes::from_static(b": ping\n\n"));
                }
                event = receiver.recv() => {
                    match event {
                        Ok(ConsoleLogEvent::Line(line)) => {
                            let payload = serde_json::to_string(&json!({
                                "kind": "line",
                                "line": line,
                            }))
                            .unwrap_or_else(|_| "{}".to_string());
                            yield Ok(Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Ok(ConsoleLogEvent::Clear) => {
                            let payload = serde_json::to_string(&json!({"kind": "clear"}))
                                .unwrap_or_else(|_| "{}".to_string());
                            yield Ok(Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            let payload = serde_json::to_string(&json!({
                                "kind": "lagged",
                                "skipped": skipped,
                            }))
                            .unwrap_or_else(|_| "{}".to_string());
                            yield Ok(Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    (
        [
            (axum::http::header::CONTENT_TYPE, "text/event-stream"),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
            (axum::http::header::CONNECTION, "keep-alive"),
        ],
        body,
    )
        .into_response()
}

async fn get_stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let logs = state.console_logs.get_logs().await;
    let usage_db = state.usage_tracker().get_usage_db();
    let level_counts = count_levels(&logs);
    let payload: Value = json!({
        "logBufferLines": logs.len(),
        "totalRequestsLifetime": usage_db.total_requests_lifetime,
        "totalHistoryEntries": usage_db.history.len(),
        "levels": level_counts,
    });
    Json(payload).into_response()
}

async fn clear_logs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    state.console_logs.clear().await;

    Json(json!({
        "success": true,
        "message": "Logs cleared",
    }))
    .into_response()
}

fn count_levels(logs: &[String]) -> serde_json::Value {
    let mut info = 0u64;
    let mut warn = 0u64;
    let mut error = 0u64;
    let mut debug = 0u64;
    let mut trace = 0u64;
    for line in logs {
        let upper = line.to_ascii_uppercase();
        if upper.contains(" ERROR ") || upper.contains("ERROR:") {
            error += 1;
        } else if upper.contains(" WARN ") || upper.contains("WARN:") {
            warn += 1;
        } else if upper.contains(" DEBUG ") || upper.contains("DEBUG:") {
            debug += 1;
        } else if upper.contains(" TRACE ") || upper.contains("TRACE:") {
            trace += 1;
        } else if upper.contains(" INFO ") || upper.contains("INFO:") {
            info += 1;
        }
    }
    json!({
        "info": info,
        "warn": warn,
        "error": error,
        "debug": debug,
        "trace": trace,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_levels_recognizes_tracing_format() {
        let lines = vec![
            "2026-01-01T00:00:00Z  INFO openproxy: ready".to_string(),
            "2026-01-01T00:00:01Z  WARN openproxy: slow".to_string(),
            "2026-01-01T00:00:02Z ERROR openproxy: failed".to_string(),
        ];
        let v = count_levels(&lines);
        assert_eq!(v["info"], 1);
        assert_eq!(v["warn"], 1);
        assert_eq!(v["error"], 1);
    }
}
