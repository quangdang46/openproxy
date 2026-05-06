use std::time::Duration;

use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::json;

use crate::server::auth::AUTHORIZATION_HEADER;
use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/shutdown", post(shutdown))
}

async fn shutdown(headers: HeaderMap) -> Response {
    if std::env::var("NODE_ENV").ok().as_deref() == Some("production") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "message": "Not allowed in production"
            })),
        )
            .into_response();
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "message": "Unauthorized"
            })),
        )
            .into_response();
    }

    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::process::exit(0);
    });

    Json(json!({
        "success": true,
        "message": "Shutting down..."
    }))
    .into_response()
}
