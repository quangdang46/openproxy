use axum::{http::StatusCode, response::Response, routing::get, Router};
use serde_json::json;

use crate::server::state::AppState;

use super::cors::*;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/tags", get(get_tags))
}

async fn get_tags() -> Response {
    let body = json!({
        "models": [
            {
                "name": "llama3.2",
                "modified_at": "2025-12-26T00:00:00Z",
                "size": 2000000000_u64,
                "digest": "abc123def456",
                "details": {
                    "format": "gguf",
                    "family": "llama",
                    "parameter_size": "3B",
                    "quantization_level": "Q4_K_M"
                }
            },
            {
                "name": "qwen2.5",
                "modified_at": "2025-12-26T00:00:00Z",
                "size": 4000000000_u64,
                "digest": "def456abc123",
                "details": {
                    "format": "gguf",
                    "family": "qwen",
                    "parameter_size": "7B",
                    "quantization_level": "Q4_K_M"
                }
            }
        ]
    });

    with_cors_json(StatusCode::OK, body)
}
