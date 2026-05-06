use axum::{response::IntoResponse, routing::get, Router};

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/init", get(get_init))
}

async fn get_init() -> impl IntoResponse {
    (axum::http::StatusCode::OK, "Initialized")
}
