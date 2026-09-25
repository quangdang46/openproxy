//! Dashboard module.
//!
//! Routing fallback for any request that isn't handled by `/api/*`, `/v1/*`,
//! or `/codex/*`. Three serving modes, picked by `AppState`:
//!
//! 1. **Reverse proxy** (`dashboard_sidecar_url` set) — used by UI developers
//!    against the Astro dev server (`pnpm --dir web run dev`). The legacy
//!    sidecar code path is preserved exactly for parity.
//! 2. **On-disk** (`web_dir` set) — serves `index.html` + assets from a
//!    directory. Useful when iterating on a pre-built `web/dist/` without
//!    rebuilding the Rust binary.
//! 3. **Embedded** (default) — `web/dist/` is baked into the binary at build
//!    time via `rust-embed`. Single-binary distribution.

use std::path::{Path, PathBuf};

use axum::{
    body::Body,
    extract::State,
    http::{
        header::{self, HeaderName},
        HeaderMap, HeaderValue, Method, Request, StatusCode, Uri,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use bytes::Bytes;
use futures_util::TryStreamExt;
#[cfg(feature = "embed-web")]
use rust_embed::RustEmbed;

use crate::server::auth::require_dashboard_session;
use crate::server::state::AppState;

/// Embedded copy of `web/dist/`, baked at build time. The `embed-web` feature
/// (default) gates this so headless builds still compile.
#[cfg(feature = "embed-web")]
#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct WebAssets;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .fallback(dashboard_fallback)
        .layer(middleware::from_fn_with_state(state, dashboard_access_gate))
}

/// Pages that must stay reachable without a session, or onboarding breaks:
/// you cannot log in from a login page that needs a login, and the OAuth
/// callback has to land somewhere after the provider redirect.
fn is_public_page(path: &str) -> bool {
    matches!(path, "/" | "/login" | "/callback" | "/landing")
}

/// Gate the dashboard shell.
///
/// The shell was served for ANY unauthenticated `/dashboard/*` deep link, so a
/// crawler or an uptime monitor saw 200 for a route that had been deleted or
/// renamed — route regressions were undetectable — and a stale deep link landed
/// on a plausible-looking but wrong screen with no not-found signal. This is
/// navigation only: `is_rust_owned_path` hard-404s `/api`, `/v1` and `/codex`,
/// and the API layer keeps its own gate, so no request path or data is exposed
/// by serving the shell.
///
/// The tunnel/tailscale host gate is applied here for the same reason it is
/// applied to the API: a deployment reached over a tunnel should not serve the
/// dashboard to a host it was not meant to be reachable from.
async fn dashboard_access_gate(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let path = request.uri().path().to_string();
    if is_public_page(&path) || is_rust_owned_path(&path) {
        return Ok(next.run(request).await);
    }

    let snapshot = state.db.snapshot();
    let settings = &snapshot.settings;

    if let Err(error) = require_dashboard_session(request.headers(), &state.db) {
        // Without a session there is nothing to show but the login form, and
        // a redirect is what a browser follows and a crawler records.
        let _ = error;
        return Ok(axum::response::Redirect::temporary("/login").into_response());
    }

    if is_tunnel_host(request.headers(), settings) && !settings.tunnel_dashboard_access {
        return Ok((
            StatusCode::FORBIDDEN,
            "Dashboard is not reachable over a tunnel or tailscale host",
        )
            .into_response());
    }

    Ok(next.run(request).await)
}

/// Whether the request arrived on a configured tunnel/tailscale host.
/// Mirrors the API surface's check so both agree on what "tunnel" means.
fn is_tunnel_host(headers: &HeaderMap, settings: &crate::types::Settings) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(':')
                .next()
                .unwrap_or(value)
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    if host.is_empty() {
        return false;
    }
    let matches = |url: &str| {
        url.split("://")
            .nth(1)
            .unwrap_or(url)
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .eq_ignore_ascii_case(&host)
    };
    (!settings.tunnel_url.trim().is_empty() && matches(&settings.tunnel_url))
        || (!settings.tailscale_url.trim().is_empty() && matches(&settings.tailscale_url))
}

async fn dashboard_fallback(State(state): State<AppState>, request: Request<Body>) -> Response {
    let path = request.uri().path().to_string();
    if is_rust_owned_path(&path) {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }

    // Mode 1: reverse proxy
    if state.dashboard_sidecar_url.is_some() {
        return proxy_dashboard_request(state, request).await;
    }

    // Mode 2: disk override (--web-dir)
    if let Some(dir) = state.web_dir.as_deref() {
        return serve_from_disk(dir, request.uri()).await;
    }

    // Mode 3: embedded assets
    serve_embedded(request.uri()).await
}

/// Paths that are owned by the API routers — never served by the dashboard.
fn is_rust_owned_path(path: &str) -> bool {
    path == "/api"
        || path.starts_with("/api/")
        || path == "/v1"
        || path.starts_with("/v1/")
        || path == "/codex"
        || path.starts_with("/codex/")
}

// ────────────────────────────────────────────────────────────────────────────
// Mode 3: embedded
// ────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "embed-web")]
async fn serve_embedded(uri: &Uri) -> Response {
    let raw = uri.path();
    let candidate = normalize_asset_path(raw);

    if let Some(resp) = lookup_embedded(candidate) {
        return resp;
    }

    // Astro `build.format: 'file'` outputs `dashboard.html` rather than
    // `dashboard/index.html`. URLs from the dashboard SPA never include the
    // `.html` extension (`/dashboard`, `/dashboard/endpoint`), so we try the
    // `<path>.html` variant before the SPA shell fallback. Without this the
    // server returns the redirect-stub `index.html` for `/dashboard`, which
    // points back at `/dashboard` and produces an infinite meta-refresh loop
    // (see bug report #1).
    if !looks_like_asset(candidate) {
        let html_candidate = format!("{candidate}.html");
        if let Some(resp) = lookup_embedded(&html_candidate) {
            return resp;
        }
        // Also try the directory-style layout `<path>/index.html` for
        // forward compatibility if Astro is switched to `format: 'directory'`.
        let dir_candidate = format!("{candidate}/index.html");
        if let Some(resp) = lookup_embedded(&dir_candidate) {
            return resp;
        }
        // Dynamic-segment fallback: for paths like `/dashboard/providers/<uuid>`
        // where the final segment is a user-created ID unknown at build time,
        // try a `_dynamic.html` placeholder page in the parent directory. The
        // Astro build emits this file so the correct page shell (e.g.
        // ProviderDetailPageClient) is served instead of the generic dashboard.
        if let Some(resp) = dynamic_segment_fallback(candidate, lookup_embedded) {
            return resp;
        }
        // SPA fallback: requests without a file extension are client-router
        // routes. Serve the SPA shell so the JS router can take over.
        if let Some(resp) = lookup_embedded("dashboard.html") {
            return resp;
        }
        if let Some(resp) = lookup_embedded("index.html") {
            return resp;
        }
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

#[cfg(not(feature = "embed-web"))]
async fn serve_embedded(_uri: &Uri) -> Response {
    let body = "OpenProxy was built without the embedded dashboard. \
                Pass --dashboard-sidecar-url <URL> or --web-dir <PATH> to serve the UI.";
    (StatusCode::NOT_FOUND, body).into_response()
}

#[cfg(feature = "embed-web")]
fn lookup_embedded(path: &str) -> Option<Response> {
    let file = WebAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();

    let etag = format!("\"{}\"", hex::encode(file.metadata.sha256_hash()));
    let cache = cache_control_for(path);

    let mut resp = Response::new(Body::from(file.data.into_owned()));
    if let Ok(value) = HeaderValue::from_str(mime.as_ref()) {
        resp.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&etag) {
        resp.headers_mut().insert(header::ETAG, value);
    }
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Some(resp)
}

fn normalize_asset_path(raw: &str) -> &str {
    let trimmed = raw.trim_start_matches('/');
    if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    }
}

fn looks_like_asset(path: &str) -> bool {
    // Treat any final segment with a `.` as an asset. Catches `.js`, `.css`,
    // `.svg`, `.woff2`, dotfiles, etc. SPA routes typically have no dot.
    path.rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'))
}

/// For a path like `dashboard/providers/8d82774e-…`, try
/// `dashboard/providers/_dynamic.html`. The Astro build emits this placeholder
/// so that user-created provider UUIDs (unknown at build time) still get the
/// correct page shell instead of the generic `dashboard.html` fallback.
fn dynamic_segment_fallback<F>(candidate: &str, lookup: F) -> Option<Response>
where
    F: Fn(&str) -> Option<Response>,
{
    // Only attempt this for paths with at least two segments (parent + slug).
    if let Some(parent) = candidate.rsplit_once('/').map(|(p, _)| p) {
        let fallback = format!("{parent}/_dynamic.html");
        if let Some(resp) = lookup(&fallback) {
            return Some(resp);
        }
    }
    None
}

fn cache_control_for(path: &str) -> &'static str {
    // HTML shells must not be cached: a stale shell breaks code-split bundles
    // after a redeploy. Other assets are content-hashed by the Astro build
    // pipeline (`_astro/<hash>.js`) and safe to cache forever.
    if path.ends_with(".html") || path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Mode 2: on-disk override
// ────────────────────────────────────────────────────────────────────────────

async fn serve_from_disk(root: &Path, uri: &Uri) -> Response {
    let candidate = normalize_asset_path(uri.path());

    if let Some(resp) = read_disk_asset(root, candidate) {
        return resp;
    }
    if !looks_like_asset(candidate) {
        // Mirror the embedded path: try `<path>.html` first, then `<path>/index.html`,
        // then fall back to the SPA shell.
        let html_candidate = format!("{candidate}.html");
        if let Some(resp) = read_disk_asset(root, &html_candidate) {
            return resp;
        }
        let dir_candidate = format!("{candidate}/index.html");
        if let Some(resp) = read_disk_asset(root, &dir_candidate) {
            return resp;
        }
        // Dynamic-segment fallback (mirrors embedded mode logic above).
        if let Some(resp) = dynamic_segment_fallback(candidate, |p| read_disk_asset(root, p)) {
            return resp;
        }
        if let Some(resp) = read_disk_asset(root, "dashboard.html") {
            return resp;
        }
        if let Some(resp) = read_disk_asset(root, "index.html") {
            return resp;
        }
    }
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

fn read_disk_asset(root: &Path, rel: &str) -> Option<Response> {
    // Reject parent-directory escapes; rely on the OS for everything else.
    if rel.split('/').any(|seg| seg == "..") {
        return None;
    }
    let mut path = PathBuf::from(root);
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        path.push(seg);
    }
    let bytes = std::fs::read(&path).ok()?;
    let mime = mime_guess::from_path(&path).first_or_octet_stream();

    let mut resp = Response::new(Body::from(bytes));
    if let Ok(value) = HeaderValue::from_str(mime.as_ref()) {
        resp.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control_for(rel)),
    );
    Some(resp)
}

// ────────────────────────────────────────────────────────────────────────────
// Mode 1: reverse proxy (legacy sidecar — preserved for development)
// ────────────────────────────────────────────────────────────────────────────

async fn proxy_dashboard_request(state: AppState, request: Request<Body>) -> Response {
    let Some(target_uri) = build_target_uri(&state, request.uri()) else {
        return (
            StatusCode::BAD_GATEWAY,
            "Dashboard sidecar target URL is invalid.",
        )
            .into_response();
    };

    // dashboard_client is `Some` iff dashboard_sidecar_url is set. The caller
    // already checked sidecar_url, so this should always be Some here.
    let client = match state.dashboard_client.as_ref() {
        Some(c) => c.as_ref().clone(),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Dashboard sidecar URL is set but reverse-proxy client is missing.",
            )
                .into_response();
        }
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

fn build_target_uri(state: &AppState, uri: &Uri) -> Option<String> {
    let origin = state
        .dashboard_sidecar_url
        .as_deref()?
        .trim_end_matches('/');
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    Some(format!("{}{}", origin, path_and_query))
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
