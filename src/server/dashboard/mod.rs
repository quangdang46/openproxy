use axum::{routing::get, Router};
use axum::response::Html;

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(dashboard))
        .route("/dashboard", get(dashboard))
}

async fn dashboard() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
}
