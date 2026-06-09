use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
    Json, Router,
};
use bcrypt::verify;
use chrono::{Duration as ChronoDuration, Utc};
use jsonwebtoken::{encode, EncodingKey, Header as JwtHeader};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::server::auth::login_limiter::LockoutError;
use crate::server::auth::oidc::{
    code_challenge_from_verifier, generate_code_verifier, generate_state_token,
};
use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::Settings;

#[derive(Debug, Deserialize)]
pub struct PasswordLoginRequest {
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AuthTokenClaims {
    authenticated: bool,
    exp: usize,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub api_key_id: String,
    pub created_at: i64,
    pub last_active: i64,
    pub is_valid: bool,
}

#[derive(Debug, Deserialize)]
pub struct LogoutRequest {
    pub session_id: Option<String>,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// POST /api/auth/login
/// Creates a JWT cookie for browser dashboard auth.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PasswordLoginRequest>,
) -> Response {
    let snapshot = state.db.snapshot();
    if is_tunnel_request(&headers, &snapshot.settings) && !snapshot.settings.tunnel_dashboard_access
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Dashboard access via tunnel is disabled" })),
        )
            .into_response();
    }

    let client_ip = client_ip_from_headers(&headers);

    // Reserve the attempt slot before checking the password so an attacker
    // cannot bypass the limit by timing requests around bcrypt. A successful
    // password check immediately resets the counter via the second call below.
    if let Err(LockoutError::Locked { retry_after_secs }) =
        state.login_limiter.check_and_record(client_ip, false)
    {
        return lockout_response(retry_after_secs);
    }

    let provided_password = req.password;
    let valid = match settings_password_hash(&snapshot.settings) {
        Some(hash) => verify(&provided_password, hash).unwrap_or(false),
        None => {
            let initial_password =
                std::env::var("INITIAL_PASSWORD").unwrap_or_else(|_| "123456".to_string());
            provided_password == initial_password
        }
    };

    if !valid {
        if let Err(LockoutError::Locked { retry_after_secs }) =
            state.login_limiter.check_and_record(client_ip, false)
        {
            return lockout_response(retry_after_secs);
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Invalid password" })),
        )
            .into_response();
    }

    let _ = state.login_limiter.check_and_record(client_ip, true);

    let expires_at = now_secs() + 86400;
    let secret = std::env::var("JWT_SECRET")
        .unwrap_or_else(|_| "openproxy-default-secret-change-me".to_string());
    let token = match encode(
        &JwtHeader::default(),
        &AuthTokenClaims {
            authenticated: true,
            exp: expires_at as usize,
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    ) {
        Ok(token) => token,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to issue auth token: {error}") })),
            )
                .into_response();
        }
    };

    let secure_cookie = std::env::var("AUTH_COOKIE_SECURE").ok().as_deref() == Some("true")
        || headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.eq_ignore_ascii_case("https"))
            .unwrap_or(false);

    let mut response = Json(json!({ "success": true })).into_response();
    let cookie = build_auth_cookie(&token, 86400, secure_cookie);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

/// GET /api/auth/oidc/login
///
/// Start the OIDC Authorization Code + PKCE flow. Generates a fresh
/// `state`, `nonce`, and PKCE verifier; stashes them in short-lived
/// HttpOnly cookies; and 302-redirects to the IdP's `authorization_endpoint`.
///
/// Returns 400 when OIDC is not configured (no `OIDC_*` env vars at boot).
pub async fn oidc_login(headers: HeaderMap, State(state): State<AppState>) -> Response {
    // Apply login rate limiter to prevent DoS against the IdP redirect.
    let client_ip = client_ip_from_headers(&headers);
    if let Err(LockoutError::Locked { retry_after_secs }) =
        state.login_limiter.check_and_record(client_ip, false)
    {
        return lockout_response(retry_after_secs);
    }

    let client = match state.oidc_client.as_ref() {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "OIDC not configured" })),
            )
                .into_response();
        }
    };

    let code_verifier = generate_code_verifier();
    let code_challenge = code_challenge_from_verifier(&code_verifier);
    let state_val = generate_state_token();
    let nonce = generate_state_token();

    let auth_url = client.build_authorize_url(&state_val, &nonce, &code_challenge);

    let mut response = Redirect::to(&auth_url).into_response();
    let max_age = 600; // 10 minutes — long enough for the round-trip
    let secure = false; // login flows are local; cookie still HttpOnly
    for (name, value) in [
        ("oidc_state", state_val.as_str()),
        ("oidc_nonce", nonce.as_str()),
        ("oidc_verifier", code_verifier.as_str()),
    ] {
        if let Ok(hv) = HeaderValue::from_str(&build_oidc_cookie(name, value, max_age, secure)) {
            response.headers_mut().append(header::SET_COOKIE, hv);
        }
    }
    response
}

/// GET /api/auth/oidc/callback?code=…&state=…
///
/// IdP redirect target. Verifies the state cookie, exchanges the code
/// for tokens, verifies the signed `id_token` against the IdP's JWKS,
/// and on success issues the dashboard session cookie and 302-redirects
/// to `/`.
pub async fn oidc_callback(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let client = match state.oidc_client.as_ref() {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "OIDC not configured" })),
            )
                .into_response();
        }
    };

    // Pull OIDC handshake cookies back out — they were set by
    // /oidc/login and have to round-trip through the browser.
    let cookie_state = crate::server::auth::extract_cookie(&headers, "oidc_state");
    let cookie_nonce = crate::server::auth::extract_cookie(&headers, "oidc_nonce");
    let cookie_verifier = crate::server::auth::extract_cookie(&headers, "oidc_verifier");
    let (state_cookie, nonce_cookie, verifier) = match (cookie_state, cookie_nonce, cookie_verifier)
    {
        (Some(s), Some(n), Some(v)) => (s, n, v),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing OIDC handshake cookies — restart the login flow" })),
            )
                .into_response();
        }
    };

    let code = match params.get("code") {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing code parameter" })),
            )
                .into_response();
        }
    };

    let returned_state = params.get("state").cloned().unwrap_or_default();
    if returned_state != state_cookie {
        // Mismatched state is a CSRF signal — refuse without consuming
        // an attempt slot so the legitimate user can retry.
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "State mismatch" })),
        )
            .into_response();
    }

    if let Some(error) = params.get("error") {
        let desc = params.get("error_description").cloned().unwrap_or_default();
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("OIDC provider error: {error}"),
                "error_description": desc,
            })),
        )
            .into_response();
    }

    let client_ip = client_ip_from_headers(&headers);
    if let Err(LockoutError::Locked { retry_after_secs }) =
        state.login_limiter.check_and_record(client_ip, false)
    {
        return lockout_response(retry_after_secs);
    }

    let token_resp = match client.exchange_code(&code, &verifier).await {
        Ok(r) => r,
        Err(error) => {
            tracing::warn!(?error, "OIDC token exchange failed");
            let _ = state.login_limiter.check_and_record(client_ip, false);
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": "Token exchange failed" })),
            )
                .into_response();
        }
    };

    let id_token = match token_resp.get("id_token").and_then(|v| v.as_str()) {
        Some(t) => t.to_string(),
        None => {
            let _ = state.login_limiter.check_and_record(client_ip, false);
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "No id_token in token response" })),
            )
                .into_response();
        }
    };

    let jwks = match client.fetch_jwks().await {
        Ok(j) => j,
        Err(error) => {
            tracing::warn!(?error, "OIDC JWKS fetch failed");
            let _ = state.login_limiter.check_and_record(client_ip, false);
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": "Failed to fetch JWKS" })),
            )
                .into_response();
        }
    };

    let claims = match client.verify_id_token(&id_token, &jwks, Some(&nonce_cookie)) {
        Ok(c) => c,
        Err(error) => {
            tracing::warn!(?error, "OIDC id_token verification failed");
            let _ = state.login_limiter.check_and_record(client_ip, false);
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "ID token verification failed" })),
            )
                .into_response();
        }
    };

    // Authenticated. Mark the limiter as a success so a single bad
    // pre-auth probe doesn't pollute the failure budget.
    let _ = state.login_limiter.check_and_record(client_ip, true);

    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("oidc-user")
        .to_string();
    let name = claims
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&email)
        .to_string();

    let now = Utc::now().timestamp();
    let exp = (Utc::now() + ChronoDuration::days(7)).timestamp();
    let token_claims = json!({
        "sub": email,
        "email": email,
        "name": name,
        "authenticated": true,
        "iat": now,
        "exp": exp,
    });
    let secret = std::env::var("JWT_SECRET")
        .unwrap_or_else(|_| "openproxy-default-secret-change-me".to_string());
    let token = match encode(
        &JwtHeader::default(),
        &token_claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    ) {
        Ok(t) => t,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to issue session token: {error}") })),
            )
                .into_response();
        }
    };

    let secure_cookie = std::env::var("AUTH_COOKIE_SECURE").ok().as_deref() == Some("true")
        || headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.eq_ignore_ascii_case("https"))
            .unwrap_or(false);

    let mut response = Redirect::to("/").into_response();
    let cookie = build_auth_cookie(&token, 7 * 24 * 60 * 60, secure_cookie);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    for name in ["oidc_state", "oidc_nonce", "oidc_verifier"] {
        if let Ok(hv) = HeaderValue::from_str(&build_oidc_cookie(name, "", 0, false)) {
            response.headers_mut().append(header::SET_COOKIE, hv);
        }
    }
    response
}

fn build_oidc_cookie(name: &str, value: &str, max_age_seconds: i64, secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_seconds}{secure_flag}"
    )
}

/// POST /api/auth/logout
/// Invalidates the current session
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LogoutRequest>,
) -> Response {
    if crate::server::auth::extract_auth_token(&headers).is_some() {
        let mut response = Json(json!({
            "success": true,
            "message": "Logged out"
        }))
        .into_response();
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_static("auth_token=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
        );
        return response;
    }

    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let mut sessions = state.sessions.write().await;

    // If session_id provided, remove that specific session
    if let Some(session_id) = req.session_id {
        if let Some(session) = sessions.get(&session_id) {
            if session.api_key_id == api_key.id {
                sessions.remove(&session_id);
                return Json(json!({
                    "success": true,
                    "message": "Session logged out"
                }))
                .into_response();
            } else {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "success": false,
                        "error": "Session belongs to different user"
                    })),
                )
                    .into_response();
            }
        }
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "success": false,
                "error": "Session not found"
            })),
        )
            .into_response();
    }

    // Otherwise, remove all sessions for this API key
    sessions.retain(|_, session| session.api_key_id != api_key.id);

    Json(json!({
        "success": true,
        "message": "All sessions logged out"
    }))
    .into_response()
}

/// GET /api/auth/session/:session_id
/// Get session info
pub async fn get_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let _api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let sessions = state.sessions.read().await;

    match sessions.get(&session_id) {
        Some(session) => {
            let now = now_secs();
            let is_valid = now < (session.created_at + 86400);
            Json(SessionResponse {
                session_id: session.session_id.clone(),
                api_key_id: session.api_key_id.clone(),
                created_at: session.created_at,
                last_active: session.last_active,
                is_valid,
            })
            .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "Session not found"
            })),
        )
            .into_response(),
    }
}

/// GET /api/auth/sessions
/// List all sessions for the current API key
pub async fn list_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let sessions = state.sessions.read().await;
    let now = now_secs();

    let session_list: Vec<SessionResponse> = sessions
        .values()
        .filter(|s| s.api_key_id == api_key.id)
        .map(|session| {
            let is_valid = now < (session.created_at + 86400);
            SessionResponse {
                session_id: session.session_id.clone(),
                api_key_id: session.api_key_id.clone(),
                created_at: session.created_at,
                last_active: session.last_active,
                is_valid,
            }
        })
        .collect();

    Json(json!({
        "sessions": session_list,
        "count": session_list.len()
    }))
    .into_response()
}

/// DELETE /api/auth/sessions
/// Invalidate all sessions for the current API key
pub async fn delete_all_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let mut sessions = state.sessions.write().await;
    let before = sessions.len();
    sessions.retain(|_, session| session.api_key_id != api_key.id);
    let after = sessions.len();

    Json(json!({
        "success": true,
        "message": format!("Invalidated {} sessions", before - after)
    }))
    .into_response()
}

/// GET /api/user
/// Returns the current dashboard user's profile info.
///
/// OpenProxy is a single-user dashboard guarded by either a JWT cookie
/// (set by `POST /api/auth/login`) or a management API key. Since the
/// dashboard does not model multiple users, this endpoint synthesizes a
/// stable identity from the live auth/settings state so the Profile page
/// can render meaningful data.
pub async fn get_user(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) =
        crate::server::api::require_dashboard_or_management_api_key(&headers, &state)
    {
        return response;
    }

    let snapshot = state.db.snapshot();
    let has_password = settings_password_hash(&snapshot.settings).is_some();
    let auth_method = if crate::server::auth::extract_auth_token(&headers).is_some() {
        "dashboard_session"
    } else {
        "management_api_key"
    };

    Json(json!({
        "username": "admin",
        "email": null,
        "role": "owner",
        "authMethod": auth_method,
        "hasPassword": has_password,
        "requireLogin": snapshot.settings.require_login,
    }))
    .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/sessions", get(list_sessions))
        .route("/api/auth/sessions", delete(delete_all_sessions))
        .route("/api/auth/session/{session_id}", get(get_session))
        .route("/api/auth/oidc/login", get(oidc_login))
        .route("/api/auth/oidc/callback", get(oidc_callback))
        .route("/api/user", get(get_user))
}

fn settings_password_hash(settings: &Settings) -> Option<&str> {
    if let Some(hash) = settings.password.as_deref() {
        return Some(hash);
    }
    settings
        .extra
        .get("password")
        .and_then(|value| value.as_str())
}

fn is_tunnel_request(headers: &HeaderMap, settings: &Settings) -> bool {
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

    tunnel_host(&settings.tunnel_url).is_some_and(|tunnel_host| tunnel_host == host)
        || tunnel_host(&settings.tailscale_url).is_some_and(|tailscale_host| tailscale_host == host)
}

fn tunnel_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    url::Url::parse(trimmed)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host.to_ascii_lowercase()))
}

fn build_auth_cookie(token: &str, max_age_seconds: i64, secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!(
        "auth_token={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_seconds}{secure_flag}"
    )
}

/// Best-effort client IP extraction. The dashboard binds `127.0.0.1`, so most
/// real callers are either loopback or coming through a reverse proxy.
///
/// Order: `X-Forwarded-For` (first hop), `X-Real-IP`, else loopback. When
/// deployed without a trusted proxy the loopback fallback keeps the limiter
/// from accidentally using `0.0.0.0` as a shared bucket.
fn client_ip_from_headers(headers: &HeaderMap) -> std::net::IpAddr {
    if let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    {
        if let Some(first) = value.split(',').next() {
            if let Ok(ip) = first.trim().parse::<std::net::IpAddr>() {
                return ip;
            }
        }
    }
    if let Some(value) = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
    {
        if let Ok(ip) = value.trim().parse::<std::net::IpAddr>() {
            return ip;
        }
    }
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
}

/// HTTP 429 response for a rate-limited login attempt. Includes a
/// `Retry-After` header (seconds) and a JSON body the dashboard can render.
fn lockout_response(retry_after_secs: u64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": "Too many login attempts",
            "retry_after_secs": retry_after_secs,
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().append(header::RETRY_AFTER, value);
    }
    response
}
