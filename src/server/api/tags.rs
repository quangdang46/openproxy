use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde_json::json;

use crate::server::state::AppState;

const CORS_ALLOW_ORIGIN: &str = "*";
const CORS_ALLOW_METHODS: &str = "GET, OPTIONS";
const CORS_ALLOW_HEADERS: &str = "*";

fn cors_response(
    status: StatusCode,
    body: Option<&'static str>,
    content_type: Option<&str>,
) -> Response {
    let mut response = match body {
        Some(body) => (status, body).into_response(),
        None => status.into_response(),
    };

    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static(CORS_ALLOW_ORIGIN),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(CORS_ALLOW_METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(CORS_ALLOW_HEADERS),
    );

    if let Some(content_type) = content_type {
        if let Ok(value) = HeaderValue::from_str(content_type) {
            headers.insert(header::CONTENT_TYPE, value);
        }
    }

    response
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/tags", get(get_tags).options(options_tags))
}

async fn options_tags() -> Response {
    cors_response(StatusCode::OK, None, None)
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
    })
    .to_string();

    let leaked = Box::leak(body.into_boxed_str());
    cors_response(StatusCode::OK, Some(leaked), Some("application/json"))
}
