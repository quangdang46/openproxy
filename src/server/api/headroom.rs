//! Headroom proxy management API
//!
//! Provides three endpoints:
//! - `GET /api/headroom/status`  — check whether the Headroom proxy is reachable
//! - `POST /api/headroom/start`  — attempt to start the local Headroom proxy
//! - `POST /api/headroom/stop`   — attempt to stop the local Headroom proxy
//!
//! The Headroom URL is read from the current settings so the proxy is
//! configurable at runtime without a restart.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::server::api::require_dashboard_or_management_api_key;
use crate::server::state::AppState;

/// GET /api/headroom/status
///
/// Probes the configured `headroom_url` health endpoint and returns
/// whether the proxy is reachable, plus metadata about the environment.
pub async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let snapshot = state.db.snapshot();
    let url = snapshot.settings.headroom_url.clone();

    if url.is_empty() {
        return Json(json!({
            "installed": false,
            "running": false,
            "python": null,
            "loading": false,
            "localUrl": false,
            "canStart": false,
            "managedPid": false,
        }))
        .into_response();
    }

    // Probe the headroom health endpoint
    let health_url = format!("{}/health", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let running = match client.get(&health_url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    };

    // We don't manage a local PID from the API layer — the CLI handles that.
    // `localUrl` = true means the URL points to localhost.
    let local_url = url.contains("localhost") || url.contains("127.0.0.1") || url.contains("::1");

    Json(json!({
        "installed": running || local_url,
        "running": running,
        "python": std::env::var("HEADROOM_PYTHON").ok(),
        "loading": false,
        "localUrl": local_url,
        "canStart": local_url && !running,
        "managedPid": false,
    }))
    .into_response()
}

/// POST /api/headroom/start
///
/// Attempts to start a local Headroom proxy subprocess.
/// Only supported when the configured URL is on localhost and a Python
/// runtime is available.  Returns an error otherwise.
pub async fn start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let snapshot = state.db.snapshot();
    let url = snapshot.settings.headroom_url.clone();

    // Must be a local URL
    let is_local = url.contains("localhost") || url.contains("127.0.0.1") || url.contains("::1");
    if !is_local {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Headroom is configured for a remote URL; start it externally."
            })),
        )
            .into_response();
    }

    // Check for Python
    let python = std::env::var("HEADROOM_PYTHON").unwrap_or_else(|_| "python3".into());
    let which = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "which {python} 2>/dev/null || which python3 2>/dev/null || which python 2>/dev/null"
        ))
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        });

    let python_path = match which {
        Some(p) if !p.is_empty() => p,
        _ => {
            return (
                StatusCode::PRECONDITION_FAILED,
                Json(json!({
                    "error": "Python ≥ 3.10 is required to start Headroom locally."
                })),
            )
                .into_response();
        }
    };

    // Spawn headroom proxy as a background subprocess.
    // We use tokio::spawn + std::process::Command so the child outlives
    // the request handler.  The server doesn't manage the PID after this;
    // the CLI or the user is responsible for lifecycle.
    let _child = tokio::task::spawn_blocking(move || {
        let result = std::process::Command::new(&python_path)
            .arg("-m")
            .arg("headroom")
            .arg("proxy")
            .env("HEADROOM_HOST", &url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        match result {
            Ok(mut child) => {
                // Let it run; we don't wait here.
                let _ = child.wait();
            }
            Err(e) => {
                tracing::warn!("Failed to start Headroom proxy: {e}");
            }
        }
    });

    Json(json!({
        "started": true,
        "message": "Headroom proxy starting…"
    }))
    .into_response()
}

/// POST /api/headroom/stop
///
/// Attempts to stop the Headroom proxy by sending a shutdown signal to
/// the health endpoint (or by killing known subprocesses).
pub async fn stop(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let snapshot = state.db.snapshot();
    let url = snapshot.settings.headroom_url.clone();

    if url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "No headroom URL configured." })),
        )
            .into_response();
    }

    // Try the shutdown endpoint first (common for Python-based proxies)
    let shutdown_url = format!("{}/shutdown", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let _ = client.post(&shutdown_url).send().await;

    // Also try killing known headroom processes on local URLs
    let is_local = url.contains("localhost") || url.contains("127.0.0.1") || url.contains("::1");
    if is_local {
        let _ = std::process::Command::new("pkill")
            .arg("-f")
            .arg("headroom")
            .output();
    }

    Json(json!({
        "stopped": true,
        "message": "Headroom proxy stopped."
    }))
    .into_response()
}
