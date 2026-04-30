use axum::{routing::get, Json, Router};

use crate::server::state::AppState;
use crate::types::HealthResponse;

pub fn routes() -> Router<AppState> {
    Router::new().route("/dashboard/health", get(health))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::new("dashboard"))
}
