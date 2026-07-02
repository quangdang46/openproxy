use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;

/// Shared CORS headers applied to all responses.
pub const CORS_HEADERS: [(HeaderName, HeaderValue); 5] = [
    (
        HeaderName::from_static("access-control-allow-origin"),
        HeaderValue::from_static("*"),
    ),
    (
        HeaderName::from_static("access-control-allow-methods"),
        HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
    ),
    (
        HeaderName::from_static("access-control-allow-headers"),
        HeaderValue::from_static(
            "Content-Type, Authorization, X-Requested-With, x-api-key, x-goog-api-key, x-9r-cli-token",
        ),
    ),
    (
        HeaderName::from_static("access-control-max-age"),
        HeaderValue::from_static("86400"),
    ),
    (
        HeaderName::from_static("access-control-allow-credentials"),
        HeaderValue::from_static("true"),
    ),
];

/// Apply CORS headers to any axum Response.
pub fn with_cors_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    for (name, value) in CORS_HEADERS.iter() {
        headers.insert(name.clone(), value.clone());
    }
    response
}

/// Return a CORS preflight response (204 NO CONTENT).
pub fn cors_preflight_response() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    for (name, value) in CORS_HEADERS.iter() {
        headers.insert(name.clone(), value.clone());
    }
    response
}

/// Return a JSON response with CORS headers.
pub fn with_cors_json(status: StatusCode, body: Value) -> Response {
    let mut response = (status, Json(body)).into_response();
    let headers = response.headers_mut();
    for (name, value) in CORS_HEADERS.iter() {
        headers.insert(name.clone(), value.clone());
    }
    response
}
