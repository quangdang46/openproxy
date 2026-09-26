use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;

/// Shared CORS headers applied to all responses.
///
/// Note: `Access-Control-Allow-Credentials` is intentionally omitted because
/// we use `Access-Control-Allow-Origin: *`, and the CORS spec forbids combining
/// a wildcard origin with credentials. See Fetch §3.2.
pub const CORS_HEADERS: [(HeaderName, HeaderValue); 4] = [
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
];

/// Client-facing SSE headers, ported from 9router `SSE_HEADERS_CORS`
/// (open-sse/utils/sseConstants.js:18-23).
///
/// `X-Accel-Buffering` is deliberately NOT here. 9router keeps it in
/// `SSE_HEADERS_NO_BUFFER` — the variant for web-cookie executors sitting
/// behind nginx — and the client-facing table carries a permissive CORS
/// origin instead. OpenProxy assembled its streamed headers inline and picked
/// the no-buffer set for everything, so a browser client talking to the proxy
/// directly got a body with no `Access-Control-Allow-Origin` at all.
pub const SSE_HEADERS_CORS: [(HeaderName, HeaderValue); 4] = [
    (
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("text/event-stream"),
    ),
    (
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-cache"),
    ),
    (
        HeaderName::from_static("connection"),
        HeaderValue::from_static("keep-alive"),
    ),
    (
        HeaderName::from_static("access-control-allow-origin"),
        HeaderValue::from_static("*"),
    ),
];

/// The internal, nginx-facing variant (9router `SSE_HEADERS_NO_BUFFER`,
/// sseConstants.js:11-15): disables proxy buffering, and — like 9router —
/// does not advertise a CORS origin, because the executor is called
/// server-to-server.
pub const SSE_HEADERS_NO_BUFFER: [(HeaderName, HeaderValue); 3] = [
    (
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("text/event-stream"),
    ),
    (
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-cache"),
    ),
    (
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    ),
];

/// Apply one of the SSE header tables above to a response.
fn apply_sse_headers(response: &mut Response, table: &[(HeaderName, HeaderValue)]) {
    let headers = response.headers_mut();
    for (name, value) in table {
        headers.insert(name.clone(), value.clone());
    }
}

/// Apply the client-facing SSE header table.
pub fn apply_sse_headers_cors(response: &mut Response) {
    apply_sse_headers(response, &SSE_HEADERS_CORS);
}

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

#[cfg(test)]
mod sse_header_table_tests {
    use super::*;

    fn header_pairs(response: &Response) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        pairs.sort();
        pairs
    }

    #[test]
    fn the_client_facing_table_matches_9routers_four_headers() {
        let mut response = StatusCode::OK.into_response();
        apply_sse_headers_cors(&mut response);
        assert_eq!(
            header_pairs(&response),
            vec![
                ("access-control-allow-origin".to_string(), "*".to_string()),
                ("cache-control".to_string(), "no-cache".to_string()),
                ("connection".to_string(), "keep-alive".to_string()),
                ("content-type".to_string(), "text/event-stream".to_string()),
            ]
        );
        assert!(response.headers().get("x-accel-buffering").is_none());
    }

    #[test]
    fn the_no_buffer_variant_still_carries_x_accel_buffering() {
        let mut response = StatusCode::OK.into_response();
        apply_sse_headers(&mut response, &SSE_HEADERS_NO_BUFFER);
        assert_eq!(response.headers().get("x-accel-buffering").unwrap(), "no");
        // 9router's no-buffer table is for server-to-server executors and
        // carries no CORS origin.
        assert!(response
            .headers()
            .get("access-control-allow-origin")
            .is_none());
    }
}
