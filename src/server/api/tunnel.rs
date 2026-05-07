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
        .route("/api/tunnel/tailscale-start-daemon", post(tailscale_start_daemon))
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
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match state.tunnel_manager.stop().await {
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
    let tailscale_running =
        settings.tailscale_enabled || matches!(tunnel.provider.as_deref(), Some("tailscale"));

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "tunnel": {
                "enabled": settings.tunnel_enabled && matches!(tunnel.provider.as_deref(), Some("cloudflare")),
                "tunnelUrl": settings.tunnel_url,
                "shortId": "",
                "publicUrl": "",
                "running": tunnel.running && matches!(tunnel.provider.as_deref(), Some("cloudflare"))
            },
            "tailscale": {
                "enabled": settings.tailscale_enabled,
                "tunnelUrl": settings.tailscale_url,
                "running": tailscale_running
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
    stop_tunnel(State(state), headers).await
}

async fn enable_tailscale(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let body = StartTunnelRequest {
        provider: Some("tailscale".to_string()),
        port: infer_port(&headers),
    };
    start_tunnel(State(state), headers, Json(body)).await
}

async fn disable_tailscale(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    stop_tunnel(State(state), headers).await
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
            "loggedIn": false,
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
            let auth_url = combined.lines()
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

async fn tailscale_start_daemon(
    Json(_req): Json<TailscaleDaemonRequest>,
) -> impl IntoResponse {
    match Command::new("tailscaled").arg("--state=/var/lib/tailscale/tailscaled.state").spawn() {
        Ok(_) => Json(json!({ "success": true })),
        Err(e) => Json(json!({ "success": false, "error": format!("tailscaled failed: {e}") })),
    }
}
