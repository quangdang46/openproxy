use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::json;

use crate::server::auth::AUTHORIZATION_HEADER;
use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        // Script-facing: authenticated by SHUTDOWN_SECRET, not by a session.
        // 9router parity — it must stay outside the admin middleware, or a
        // script with the secret would need a dashboard cookie as well.
        .route("/api/shutdown", post(shutdown_with_secret))
        // Dashboard-facing: the browser holds a session cookie and never sees
        // SHUTDOWN_SECRET, so the secret route could never satisfy it. Same
        // shutdown, different credential.
        .route("/api/dashboard/shutdown", post(shutdown_with_session))
}

/// Refuse in production before any credential is examined, matching 9router.
fn is_production() -> bool {
    std::env::var("OPENPROXY_ENV")
        .or_else(|_| std::env::var("NODE_ENV"))
        .unwrap_or_default()
        == "production"
}

/// Signal the shutdown and answer. The response gets a moment to flush before
/// the process stops.
fn perform_shutdown(state: AppState) -> Response {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        state.signal_shutdown();
    });

    Json(json!({
        "success": true,
        "message": "Shutting down..."
    }))
    .into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({ "success": false, "message": message })),
    )
        .into_response()
}

async fn shutdown_with_secret(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // JS parity: production is detected via NODE_ENV (custom-server) — also
    // accept OPENPROXY_ENV so either variable gates the shutdown route.
    if is_production() {
        return error(StatusCode::FORBIDDEN, "Not allowed in production");
    }

    let secret = std::env::var("SHUTDOWN_SECRET").ok();
    let authorization = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok());

    if secret.as_deref().is_none()
        || authorization
            != secret
                .as_deref()
                .map(|secret| format!("Bearer {secret}"))
                .as_deref()
    {
        return error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    perform_shutdown(state)
}

/// The dashboard's Shutdown button. Authenticated by the same contract as every
/// other dashboard route — a session cookie or a management API key — so the
/// browser can actually call it.
async fn shutdown_with_session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) =
        crate::server::api::require_dashboard_or_management_api_key(&headers, &state)
    {
        return response;
    }

    if is_production() {
        return error(StatusCode::FORBIDDEN, "Not allowed in production");
    }

    perform_shutdown(state)
}
