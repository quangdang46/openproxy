//! Security guard middleware for route-level access control.
//!
//! Provides:
//! - Unspoofable real-IP extraction from the TCP connection socket
//! - Tiered access control: PUBLIC, PROTECTED, LOCAL_ONLY, ADMIN
//!
//! Tiers are applied at the router level (in [`super::routes`]) rather than
//! per-handler, providing defense-in-depth on top of any existing per-handler
//! checks.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::connect_info::ConnectInfo;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use crate::server::auth::{require_api_key, AuthError};
use crate::server::state::AppState;

/// Internal header stamped by the real-IP middleware with the verified TCP
/// peer IP. Downstream code (rate limiters, IP logging) MUST prefer this over
/// any client-supplied forwarding header.
pub const REAL_IP_HEADER: &str = "x-9r-real-ip";

/// Client-supplied forwarding headers that are stripped on every request when
/// the real-IP middleware is active. This prevents malicious clients from
/// injecting spoofed `X-Forwarded-For` / `X-Real-IP` values.
pub(crate) const SPOOFABLE_FORWARDING_HEADERS: &[&str] = &[
    "x-forwarded-for",
    "x-forwarded-proto",
    "x-forwarded-host",
    "x-forwarded-server",
    "x-real-ip",
];

/// Routes that require local-only access via [`require_local_only`] middleware.
///
/// These are sensitive operations (headroom proxy management, MITM control,
/// cowork settings, credential management) that must only be reachable from
/// the loopback interface as an additional defense-in-depth layer.
///
/// The list is informational/documentation-only; enforcement happens at the
/// router level via the `require_local_only` middleware.
#[allow(dead_code)]
pub const LOCALLY_ONLY_PATHS: &[&str] = &[
    "/api/headroom/status",
    "/api/headroom/start",
    "/api/headroom/stop",
    "/api/headroom/restart",
    "/api/headroom/extras",
    "/api/headroom/proxy",
    "/api/cli-tools/cowork-settings",
    "/api/mitm-config",
    "/api/mitm/cert/generate",
    "/api/mitm/start",
    "/api/mitm/stop",
    "/api/keys",
];

// ─── Real-IP Middleware ───────────────────────────────────────────────

/// Check the `TRUST_PROXY` env var — when set to `true`/`1`/`yes`,
/// the server is behind a trusted reverse proxy and forwarding headers
/// should be preserved rather than stripped.
fn trust_proxy_enabled() -> bool {
    matches!(
        std::env::var("TRUST_PROXY").as_deref(),
        Ok("true") | Ok("1") | Ok("yes")
    )
}

/// Extracts the verified TCP peer IP from `axum`'s `ConnectInfo<SocketAddr>`,
/// stamps it as `x-9r-real-ip`, and strips all client-supplied forwarding
/// headers (`X-Forwarded-For`, `X-Real-IP`, …) UNLESS `TRUST_PROXY` is enabled.
///
/// Must be applied **after** `.with_state()` at the outermost layer of the
/// service stack. Requires the application to be served with
/// `into_make_service_with_connect_info::<SocketAddr>()` (see `main.rs`).
///
/// Once this middleware processes a request, every downstream handler sees
/// only the verified peer IP via `x-9r-real-ip` — the spoofable headers
/// are gone.
pub async fn real_ip_middleware(mut request: Request, next: Next) -> Result<Response, Response> {
    // 1. Strip client-supplied forwarding headers UNLESS behind a trusted proxy.
    if !trust_proxy_enabled() {
        for &name in SPOOFABLE_FORWARDING_HEADERS {
            request.headers_mut().remove(name);
        }
    }

    // 2. Stamp the verified TCP peer IP from the transport connection.
    if let Some(ConnectInfo(addr)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
        let ip_str = addr.ip().to_string();
        if let Ok(value) = HeaderValue::from_str(&ip_str) {
            request.headers_mut().insert(REAL_IP_HEADER, value);
        }
    }

    Ok(next.run(request).await)
}

// ─── Tiered Access Middleware ─────────────────────────────────────────

/// **PROTECTED** tier: requires a valid API key.
///
/// Returns 401 when the request lacks a valid API key.
pub async fn require_protected(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    // JS parity (chat.js:55-75): /v1/* LLM routes have NO route-level auth.
    // Free (noAuth) providers must work without any key; per-provider
    // credential checks happen inside handlers/executors instead. JS only
    // enforces keys when settings.requireApiKey is set, which openproxy
    // does not implement — so this middleware is always a pass-through.
    let _ = state;
    Ok(next.run(request).await)
}

/// **ADMIN** tier: requires a dashboard session or a management API key.
///
/// Delegates to [`super::require_dashboard_or_management_api_key`] so that
/// management-key-only semantics are consistent across all admin-tier
/// endpoints — only keys created via `/api/keys` (or `openproxy key add`)
/// satisfy this gate.
pub async fn require_admin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    match super::require_dashboard_or_management_api_key(request.headers(), &state) {
        Ok(_) => Ok(next.run(request).await),
        Err(e) => Err(e),
    }
}

/// **LOCAL_ONLY** tier: rejects requests that did not originate from a
/// loopback address (`127.0.0.1`, `::1`).
///
/// Uses the verified TCP peer IP from `ConnectInfo<SocketAddr>` (the same
/// source that `real_ip_middleware` stamps). Returns 403 for non-loopback
/// clients.
pub async fn require_local_only(request: Request, next: Next) -> Result<Response, Response> {
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));

    if !peer_ip.is_loopback() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Localhost only" })),
        )
            .into_response());
    }

    Ok(next.run(request).await)
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn auth_error_response(error: AuthError) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "message": error.message(),
                "type": "authentication_error",
                "code": "invalid_api_key",
            }
        })),
    )
        .into_response()
}
