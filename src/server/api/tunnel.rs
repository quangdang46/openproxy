use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::process::Command;
use std::time::Duration;

use crate::core::tunnel::TunnelProvider;
use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tunnel/enable", post(enable_tunnel))
        .route("/api/tunnel/disable", post(disable_tunnel))
        .route("/api/tunnel/tailscale-enable", post(enable_tailscale))
        .route("/api/tunnel/tailscale-disable", post(disable_tailscale))
        .route("/api/tunnel/tailscale-check", get(tailscale_check))
        .route("/api/tunnel/start", post(start_tunnel))
        .route("/api/tunnel/stop", post(stop_tunnel))
        .route("/api/tunnel/status", get(tunnel_status))
        .route("/api/tunnel/tailscale-install", post(tailscale_install))
        .route("/api/tunnel/tailscale-login", post(tailscale_login))
        .route(
            "/api/tunnel/tailscale-start-daemon",
            post(tailscale_start_daemon),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartTunnelRequest {
    provider: Option<String>,
    port: Option<u16>,
}

async fn start_tunnel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StartTunnelRequest>,
) -> impl IntoResponse {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let provider_str = body.provider.as_deref().unwrap_or("cloudflare");
    let port = body.port.or_else(|| infer_port(&headers)).unwrap_or(4623);

    let provider = match provider_str.parse::<TunnelProvider>() {
        Ok(p) => p,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    match state.tunnel_manager.start(provider, port).await {
        Ok(()) => {
            let status = state.tunnel_manager.status().await;
            (
                axum::http::StatusCode::OK,
                Json(json!({
                    "message": "Tunnel started",
                    "status": status,
                })),
            )
                .into_response()
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn stop_tunnel(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    stop_tunnel_with_provider(state, headers, None).await
}

/// Shared stop logic that optionally targets a specific provider's settings
/// flags. When `preferred` is `None`, clears both cloudflare+tailscale flags
/// if the running process is already dead (legacy path).
async fn stop_tunnel_with_provider(
    state: AppState,
    headers: HeaderMap,
    preferred: Option<TunnelProvider>,
) -> axum::response::Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let result = match preferred {
        Some(p) => state.tunnel_manager.stop_provider(Some(p)).await,
        None => state.tunnel_manager.stop().await,
    };

    match result {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(json!({ "message": "Tunnel stopped" })),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn tunnel_status(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let tunnel = state.tunnel_manager.status().await;
    let settings = state.db.snapshot().settings.clone();

    // 9router reports intent and run state as separate fields: `settingsEnabled`
    // is what the user asked for, `enabled` is that intent AND a live process.
    // The dashboard reads `settingsEnabled` (EndpointPageClient.tsx:222) so a
    // tunnel the watchdog is restarting never reads back as "user turned it off".
    let cf_intent = settings.tunnel_enabled;
    let ts_intent = settings.tailscale_enabled;

    // The manager holds a single child and a single status, so the live process
    // belongs to whichever provider was started last. Reporting each provider's
    // run state off that one status is what `status_for` gives once the manager
    // is provider-scoped.
    let cf_running = tunnel.running && tunnel.provider.as_deref() == Some("cloudflare");
    let ts_running = tunnel.running && tunnel.provider.as_deref() == Some("tailscale");

    // 9router skips the probe entirely when the user turned tailscale off
    // (manager.js:124) — a disabled funnel has no daemon worth asking about.
    let ts_logged_in = ts_intent && tailscale_logged_in().await;

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "tunnel": {
                "enabled": cf_intent && cf_running,
                "settingsEnabled": cf_intent,
                "tunnelUrl": settings.tunnel_url,
                "shortId": "",
                "publicUrl": "",
                "running": cf_running
            },
            "tailscale": {
                "enabled": ts_intent && ts_running,
                "settingsEnabled": ts_intent,
                "tunnelUrl": settings.tailscale_url,
                "running": ts_running,
                "loggedIn": ts_logged_in
            },
            "download": {
                "installed": command_exists("cloudflared")
            }
        })),
    )
        .into_response()
}

async fn enable_tunnel(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let body = StartTunnelRequest {
        provider: Some("cloudflare".to_string()),
        port: infer_port(&headers),
    };
    start_tunnel(State(state), headers, Json(body)).await
}

async fn disable_tunnel(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    stop_tunnel_with_provider(state, headers, Some(TunnelProvider::Cloudflare)).await
}

async fn enable_tailscale(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let body = StartTunnelRequest {
        provider: Some("tailscale".to_string()),
        port: infer_port(&headers),
    };
    start_tunnel(State(state), headers, Json(body)).await
}

async fn disable_tailscale(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    stop_tunnel_with_provider(state, headers, Some(TunnelProvider::Tailscale)).await
}

async fn tailscale_check(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let daemon_running = Command::new("pgrep")
        .args(["-x", "tailscaled"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "installed": command_exists("tailscale"),
            "loggedIn": tailscale_logged_in().await,
            "platform": std::env::consts::OS,
            "brewAvailable": command_exists("brew"),
            "daemonRunning": daemon_running
        })),
    )
        .into_response()
}

fn infer_port(headers: &HeaderMap) -> Option<u16> {
    headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .and_then(|host| host.rsplit(':').next())
        .and_then(|port| port.parse::<u16>().ok())
}

fn command_exists(command: &str) -> bool {
    Command::new("which")
        .arg(command)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Budget for the whole `loggedIn` probe. 9router gives each socket attempt
/// 1.5s behind a TTL cache (tailscale.js:41); this is the total, and it has to
/// stay short so `GET /api/tunnel/status` cannot hang on a dead daemon.
const TAILSCALE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// 9router probes its own userspace socket, then the system one
/// (tailscale.js:139). OpenProxy never starts a userspace daemon, so only the
/// system socket and the bare invocation (macOS app bundle, Windows) are worth
/// trying.
fn tailscale_socket_flags() -> Vec<Vec<&'static str>> {
    if cfg!(target_os = "windows") {
        vec![vec![]]
    } else {
        vec![
            vec!["--socket", "/var/run/tailscale/tailscaled.sock"],
            vec![],
        ]
    }
}

/// 9router's `loggedIn` (tailscale.js:128): the device is only in the tailnet
/// while the backend is `Running` *and* the node itself is online.
fn tailscale_logged_in_from_status(status: &serde_json::Value) -> bool {
    status
        .get("BackendState")
        .and_then(serde_json::Value::as_str)
        == Some("Running")
        && status
            .pointer("/Self/Online")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

/// Every failure — missing binary, non-zero exit, unparseable stdout, timeout —
/// collapses to `false`, so callers never have to tell them apart.
async fn tailscale_logged_in() -> bool {
    tokio::time::timeout(TAILSCALE_PROBE_TIMEOUT, async {
        for socket in tailscale_socket_flags() {
            let mut probe = tokio::process::Command::new("tailscale");
            probe.args(socket).args(["status", "--json"]);
            let Ok(output) = probe.output().await else {
                continue;
            };
            if !output.status.success() {
                continue;
            }
            let Ok(status) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
                continue;
            };
            // First socket that answers authoritatively decides, exactly as
            // 9router's `probeStatusAsync` does.
            return tailscale_logged_in_from_status(&status);
        }
        false
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dashboard's login poller advances on this boolean, so a `Running`
    /// backend alone must not read as logged in — a stopped backend must not
    /// either, even when the node claims to be online.
    #[test]
    fn logged_in_requires_a_running_backend_and_an_online_node() {
        let status = |backend: &str, online: Option<bool>| {
            let online = online.map(|v| json!({ "Online": v }));
            json!({ "BackendState": backend, "Self": online })
        };

        assert!(tailscale_logged_in_from_status(&status(
            "Running",
            Some(true)
        )));
        assert!(!tailscale_logged_in_from_status(&status(
            "Running",
            Some(false)
        )));
        assert!(!tailscale_logged_in_from_status(&status(
            "Stopped",
            Some(true)
        )));
        // Device removed from the tailnet: the daemon still runs, but `Self`
        // is gone.
        assert!(!tailscale_logged_in_from_status(&status("Running", None)));
        assert!(!tailscale_logged_in_from_status(&json!({})));
        assert!(!tailscale_logged_in_from_status(&json!(null)));
    }
}

async fn tailscale_install() -> impl IntoResponse {
    (
        axum::http::StatusCode::OK,
        Json(json!({
            "success": false,
            "message": "Tailscale install must be performed manually. Install via: curl -fsSL https://tailscale.com/install.sh | sh"
        })),
    )
}

async fn tailscale_login() -> impl IntoResponse {
    match Command::new("tailscale").arg("login").output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = format!("{}{}", stdout, stderr);
            // Extract auth URL from output
            let auth_url = combined
                .lines()
                .find(|l| l.contains("https://login.tailscale.com"))
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            Json(json!({ "success": true, "authUrl": auth_url }))
        }
        Err(e) => Json(json!({ "success": false, "error": format!("tailscale not found: {e}") })),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TailscaleDaemonRequest {
    sudo_password: Option<String>,
}

async fn tailscale_start_daemon(Json(_req): Json<TailscaleDaemonRequest>) -> impl IntoResponse {
    match Command::new("tailscaled")
        .arg("--state=/var/lib/tailscale/tailscaled.state")
        .spawn()
    {
        Ok(_) => Json(json!({ "success": true })),
        Err(e) => Json(json!({ "success": false, "error": format!("tailscaled failed: {e}") })),
    }
}
