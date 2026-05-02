//! Dashboard module - serves static dashboard files from the dashboard/ directory.

use axum::{
    body::Body,
    extract::Path,
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::path::PathBuf;

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index_handler))
        .route("/page.js", get(index_handler))
        .route("/dashboard/{*path}", get(static_handler))
}

async fn index_handler() -> impl IntoResponse {
    let index_path = PathBuf::from("dashboard/page.js");
    match tokio::fs::read_to_string(&index_path).await {
        Ok(content) => axum::response::Html(content).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Dashboard not found").into_response(),
    }
}

async fn static_handler(
    Path(path): Path<String>,
) -> impl IntoResponse {
    let file_path = PathBuf::from("dashboard").join(&path);

    if file_path.exists() && file_path.is_file() {
        match tokio::fs::read(&file_path).await {
            Ok(content) => {
                let mime = get_mime_type(&path);
                axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, mime)
                    .body(Body::from(content))
                    .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed").into_response())
            }
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Failed to read file").into_response(),
        }
    } else {
        (StatusCode::NOT_FOUND, "File not found").into_response()
    }
}

fn get_mime_type(path: &str) -> &'static str {
    if path.ends_with(".js") {
        "application/javascript"
    } else if path.ends_with(".css") {
        "text/css"
    } else if path.ends_with(".html") {
        "text/html"
    } else if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "text/plain"
    }
}