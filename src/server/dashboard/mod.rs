//! Dashboard module.

use axum::{
    body::Body,
    extract::State,
    http::{header::HeaderName, HeaderMap, Method, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
    Router,
};
use bytes::Bytes;
use futures_util::TryStreamExt;

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().fallback(proxy_dashboard_request)
}

async fn proxy_dashboard_request(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Response {
    let path = request.uri().path().to_string();
    if is_rust_owned_path(&path) {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }

    let Some(target_uri) = build_target_uri(request.uri()) else {
        return (
            StatusCode::BAD_GATEWAY,
            "Dashboard sidecar target URL is invalid.",
        )
            .into_response();
    };

    let method = request.method().clone();
    let headers = request.headers().clone();
    let body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "Failed to read dashboard proxy request body.",
            )
                .into_response();
        }
    };

    let client = state.dashboard_client.as_ref().clone();
    let mut upstream = client.request(method.clone(), target_uri);
    upstream = copy_request_headers(upstream, &headers, &method);
    if !body_bytes.is_empty() {
        upstream = upstream.body(body_bytes);
    }

    let response = match upstream.send().await {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("Dashboard sidecar request failed: {error}"),
            )
                .into_response();
        }
    };

    proxy_response(response)
}

fn is_rust_owned_path(path: &str) -> bool {
    path == "/api"
        || path.starts_with("/api/")
        || path == "/v1"
        || path.starts_with("/v1/")
        || path == "/codex"
        || path.starts_with("/codex/")
}

fn dashboard_sidecar_origin() -> String {
    std::env::var("DASHBOARD_SIDECAR_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:4624".to_string())
}

fn build_target_uri(uri: &Uri) -> Option<String> {
    let origin = dashboard_sidecar_origin();
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    Some(format!(
        "{}{}",
        origin.trim_end_matches('/'),
        path_and_query
    ))
}

fn copy_request_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &HeaderMap,
    method: &Method,
) -> reqwest::RequestBuilder {
    let hop_headers = connection_header_tokens(headers);
    for (name, value) in headers {
        if should_skip_header(name, &hop_headers, method) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
}

fn should_skip_header(name: &HeaderName, hop_headers: &[String], method: &Method) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    if hop_headers.iter().any(|token| token == &lower) {
        return true;
    }
    matches!(
        lower.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    ) || (*method == Method::GET && lower == "content-type")
}

fn proxy_response(response: reqwest::Response) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let body = Body::from_stream(response.bytes_stream().map_ok(|bytes: Bytes| bytes));

    let mut proxied = Response::new(body);
    *proxied.status_mut() = status;
    let hop_headers = connection_header_tokens(&headers);
    for (name, value) in &headers {
        if hop_headers
            .iter()
            .any(|token| token.eq_ignore_ascii_case(name.as_str()))
        {
            continue;
        }
        if matches!(
            name.as_str().to_ascii_lowercase().as_str(),
            "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailers"
                | "transfer-encoding"
                | "upgrade"
                | "content-length"
        ) {
            continue;
        }
        proxied.headers_mut().insert(name, value.clone());
    }
    proxied
}

fn connection_header_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}
