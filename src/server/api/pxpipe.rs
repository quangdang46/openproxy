//! PXPIPE token-saver API.
//!
//! Mirrors 9router's `/api/pxpipe/*` endpoints (dashboard/src/app/(dashboard)/
//! dashboard/pxpipe + api/pxpipe/*). PXPIPE is an optional external npm token
//! compressor; openproxy does not manage its lifecycle, so these endpoints
//! report the library-mode skeleton state and settings-driven configuration.
//!
//!   * `GET  /api/pxpipe/status`   — install/version/config status
//!   * `POST /api/pxpipe/health`   — health checks (GET mirrors)
//!   * `GET  /api/pxpipe/stats`    — compression windows + timeline + recent
//!   * `GET  /api/pxpipe/logs`     — install log + transform events

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::api::require_dashboard_or_management_api_key;
use crate::server::state::AppState;

/// Build the PXPIPE sub-router.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/pxpipe/status", get(status))
        .route("/api/pxpipe/health", get(health).post(health))
        .route("/api/pxpipe/stats", get(stats))
        .route("/api/pxpipe/logs", get(logs))
}

/// `GET /api/pxpipe/status`
///
/// Reports the library-mode skeleton: PXPIPE is not installed/managed by
/// openproxy, so install fields are false/empty. Settings-driven values
/// reflect the current `Settings` (pxpipeEnabled etc.).
async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }
    let settings = state.db.snapshot().settings.clone();
    Json(json!({
        "installed": false,
        "installing": false,
        "version": Value::Null,
        "path": Value::Null,
        "running": false,
        "loadedAt": Value::Null,
        "uptimeMs": 0,
        "npmAvailable": false,
        "mode": "library",
        "enabled": settings.pxpipe_enabled,
        "autoInstall": settings.pxpipe_auto_install,
        "minChars": settings.pxpipe_min_chars,
        "timeoutMs": settings.pxpipe_timeout_ms,
    }))
    .into_response()
}

/// `POST /api/pxpipe/health` (GET mirrors)
async fn health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }
    Json(json!({
        "healthy": false,
        "checks": [
            { "id": "installed", "label": "PXPIPE installed", "ok": false, "detail": "PXPIPE is not managed by OpenProxy" },
            { "id": "module", "label": "Transform module loads", "ok": false, "detail": null },
            { "id": "transform", "label": "Test request transforms", "ok": false, "detail": null }
        ],
        "error": "PXPIPE not installed"
    }))
    .into_response()
}

fn empty_window() -> Value {
    json!({
        "requests": 0,
        "compressed": 0,
        "bypassed": 0,
        "errors": 0,
        "tokensBeforeEst": 0,
        "tokensAfterEst": 0,
        "tokensSavedEst": 0,
        "savedPct": 0,
        "imagesGenerated": 0,
        "compressionTimeMs": 0,
        "avgCompressionMs": 0
    })
}

/// `GET /api/pxpipe/stats`
async fn stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }
    Json(json!({
        "windows": {
            "all": empty_window(),
            "today": empty_window(),
            "yesterday": empty_window(),
            "last7d": empty_window(),
            "last30d": empty_window()
        },
        "timeline": [],
        "recent": []
    }))
    .into_response()
}

#[derive(Deserialize)]
struct LogsQuery {
    limit: Option<usize>,
}

/// `GET /api/pxpipe/logs?limit=50`
async fn logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(_query): Query<LogsQuery>,
) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }
    Json(json!({ "installLog": "", "events": [] })).into_response()
}
