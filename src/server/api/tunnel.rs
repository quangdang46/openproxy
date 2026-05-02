use axum::extract::State;
use axum::response::IntoResponse;
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::core::tunnel::TunnelProvider;
use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tunnel/start", post(start_tunnel))
        .route("/api/tunnel/stop", post(stop_tunnel))
        .route("/api/tunnel/status", get(tunnel_status))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartTunnelRequest {
    provider: Option<String>,
    port: Option<u16>,
}

async fn start_tunnel(
    State(state): State<AppState>,
    Json(body): Json<StartTunnelRequest>,
) -> impl IntoResponse {
    let provider_str = body.provider.as_deref().unwrap_or("cloudflare");
    let port = body.port.unwrap_or(20128);

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
            (axum::http::StatusCode::OK, Json(json!({
                "message": "Tunnel started",
                "status": status,
            })))
            .into_response()
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn stop_tunnel(State(state): State<AppState>) -> impl IntoResponse {
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

async fn tunnel_status(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.tunnel_manager.status().await;
    (axum::http::StatusCode::OK, Json(json!({ "status": status }))).into_response()
}
