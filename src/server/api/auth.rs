use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
    Json, Router,
};
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::{Duration as ChronoDuration, Utc};
use jsonwebtoken::{encode, EncodingKey, Header as JwtHeader};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::server::auth::login_limiter::LockoutError;
use crate::server::auth::oidc::{
    code_challenge_from_verifier, generate_code_verifier, generate_state_token,
};
use crate::server::auth::{
    increment_token_epoch, jwt_secret, require_api_key, require_api_key_with_reload, revoke_jti,
};

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
    /// JWT ID — unique per-token identifier for revocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jti: Option<String>,
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

    // Block password login when auth is OIDC-only and OIDC is configured.
    // Do this before the rate limiter so failed OIDC-mode password posts
    // do not consume lockout budget.
    let auth_mode = resolve_auth_mode(&snapshot.settings);
    let oidc_configured = is_oidc_configured(&state);
    // 9router parity (login/route.js): authMode sso/saml/oidc dispatches by
    // ssoType — password login is disabled when the active SSO protocol is
    // configured.
    if matches!(auth_mode.as_str(), "sso" | "saml" | "oidc") {
        let sso = resolve_sso_type(&snapshot.settings);
        if sso == "saml" && saml_configured(&snapshot.settings) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "Password login is disabled. Use SAML SSO sign in." })),
            )
                .into_response();
        }
        if sso == "oidc" && oidc_configured {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "Password login is disabled. Use OIDC sign in." })),
            )
                .into_response();
        }
    } else if auth_mode == "oidc" && oidc_configured {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Password login is disabled. Use OIDC sign in." })),
        )
            .into_response();
    }

    // Reserve the attempt slot before checking the password so an attacker
    // cannot bypass the limit by timing requests around bcrypt. A successful
    // password check immediately resets the counter via the second call below.
    if let Err(LockoutError::Locked { retry_after_secs }) =
        state.login_limiter.check_and_record(client_ip, false).await
    {
        return lockout_response(retry_after_secs);
    }

    let provided_password = req.password;
    let valid = match settings_password_hash(&snapshot.settings) {
        Some(hash) => verify(&provided_password, hash).unwrap_or(false),
        None => crate::core::auth::timing_safe_eq(
            &provided_password,
            &crate::core::auth::dashboard_initial_password(),
        ),
    };

    if !valid {
        if let Err(LockoutError::Locked { retry_after_secs }) =
            state.login_limiter.check_and_record(client_ip, false).await
        {
            return lockout_response(retry_after_secs);
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "Invalid password",
                "resetHint": "Forgot password? Run `openproxy auth reset-password` on the host to restore the generated initial password.",
            })),
        )
            .into_response();
    }

    let _ = state.login_limiter.check_and_record(client_ip, true).await;

    let expires_at = now_secs() + 86400;
    let jti = crate::server::auth::generate_jti();
    let token = match encode(
        &JwtHeader::default(),
        &AuthTokenClaims {
            authenticated: true,
            exp: expires_at as usize,
            jti: Some(jti),
        },
        &EncodingKey::from_secret(jwt_secret().as_bytes()),
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

    // Force a password change when the default password is still in use and the
    // client is remote (keeps local UX intact; mirrors 9router).
    let has_stored_hash = settings_password_hash(&snapshot.settings).is_some();
    let must_change_password =
        !has_stored_hash && std::env::var("INITIAL_PASSWORD").is_err() && !client_ip.is_loopback();

    let mut response = Json(json!({
        "success": true,
        "mustChangePassword": must_change_password,
    }))
    .into_response();
    let cookie = build_auth_cookie(&token, 86400, secure_cookie);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

/// GET /api/auth/status — Check if the browser has a dashboard session and
/// return login-page metadata (auth mode, OIDC/SAML readiness, password state)
/// plus the session identity (displayName/loginMethod + OIDC/SAML claims)
/// when logged in — 9router status/route.js (65197ad1) + Header.js parity.
pub async fn auth_status(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let session = crate::server::auth::require_dashboard_session(&headers, &state.db).ok();
    let logged_in = session.is_some();
    let snapshot = state.db.snapshot();
    let settings = &snapshot.settings;
    let has_password = settings_password_hash(settings).is_some();
    let oidc_configured = is_oidc_configured(&state);
    let auth_mode = resolve_auth_mode(settings);
    let oidc_login_label = resolve_oidc_login_label(settings);
    let saml_login_label = resolve_saml_login_label(settings);

    // When require_login is off, require_dashboard_session returns empty
    // claims — but a present auth_token cookie may still carry SSO identity
    // (JS always reads the session cookie to derive the chip). Prefer the
    // decoded token identity so the chip works in both modes.
    let claims = decode_dashboard_token(&headers).ok().or(session);
    let oidc_name = claims
        .as_ref()
        .and_then(|c| c.name.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let oidc_email = claims
        .as_ref()
        .and_then(|c| c.email.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // SAML identity claims (embedded by the SAML ACS handler, 9router
    // saml/acs/route.js) take precedence over OIDC — mirrors the JS
    // displayName/loginMethod derivation order.
    let saml_login = claims.as_ref().and_then(|c| c.saml).unwrap_or(false);
    let saml_name = claims
        .as_ref()
        .and_then(|c| c.saml_name.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let saml_email = claims
        .as_ref()
        .and_then(|c| c.saml_email.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let login_method = if saml_login {
        "SAML"
    } else if oidc_name.is_some() || oidc_email.is_some() {
        "OIDC"
    } else {
        "Password"
    };
    let display_name = saml_name
        .clone()
        .or_else(|| saml_email.clone())
        .or_else(|| oidc_name.clone())
        .or_else(|| oidc_email.clone())
        .unwrap_or_default();

    let saml_is_configured = saml_configured(settings);
    let mut body = json!({
        "authenticated": logged_in,
        "requireLogin": settings.require_login,
        "hasPassword": has_password,
        "authMode": auth_mode,
        "oidcConfigured": oidc_configured,
        "oidcLoginLabel": oidc_login_label,
        "oidcEnabled": settings.oidc_enabled,
        "samlConfigured": saml_is_configured,
        "samlLoginLabel": saml_login_label,
        "ssoType": resolve_sso_type(settings),
    });
    if let Some(obj) = body.as_object_mut() {
        obj.insert("displayName".into(), json!(display_name));
        obj.insert("loginMethod".into(), json!(login_method));
        obj.insert("oidcName".into(), json!(oidc_name));
        obj.insert("oidcEmail".into(), json!(oidc_email));
        obj.insert("oidcLogin".into(), json!(login_method == "OIDC"));
        obj.insert("samlName".into(), json!(saml_name));
        obj.insert("samlEmail".into(), json!(saml_email));
        obj.insert("samlLogin".into(), json!(saml_login));
    }
    Json(body).into_response()
}

/// Decode the dashboard session cookie (auth_token) into its claims without
/// requiring `require_login`. Used by `auth_status` to surface the OIDC
/// identity chip even when login is optional.
fn decode_dashboard_token(
    headers: &HeaderMap,
) -> Result<crate::server::auth::DashboardClaims, String> {
    use jsonwebtoken::{decode, DecodingKey, Validation};
    let token = crate::server::auth::extract_auth_token(headers).ok_or("missing cookie")?;
    let decoded = decode::<crate::server::auth::DashboardClaims>(
        &token,
        &DecodingKey::from_secret(crate::server::auth::jwt_secret().as_bytes()),
        &Validation::default(),
    )
    .map_err(|e| e.to_string())?;
    if !decoded.claims.authenticated {
        return Err("not authenticated".into());
    }
    Ok(decoded.claims)
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
        state.login_limiter.check_and_record(client_ip, false).await
    {
        return lockout_response(retry_after_secs);
    }

    let client = {
        let guard = state.oidc_client.read().await;
        match guard.as_ref() {
            Some(c) => c.clone(),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "OIDC not configured" })),
                )
                    .into_response();
            }
        }
    };

    let code_verifier = generate_code_verifier();
    let code_challenge = code_challenge_from_verifier(&code_verifier);
    let state_val = generate_state_token();
    let nonce = generate_state_token();

    let auth_url = match client.build_authorize_url(&state_val, &nonce, &code_challenge) {
        Ok(url) => url,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("OIDC URL error: {e}") })),
            )
                .into_response();
        }
    };

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
    let client = {
        let guard = state.oidc_client.read().await;
        match guard.as_ref() {
            Some(c) => c.clone(),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "OIDC not configured" })),
                )
                    .into_response();
            }
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
        state.login_limiter.check_and_record(client_ip, false).await
    {
        return lockout_response(retry_after_secs);
    }

    let token_resp = match client.exchange_code(&code, &verifier).await {
        Ok(r) => r,
        Err(error) => {
            tracing::warn!(?error, "OIDC token exchange failed");
            let _ = state.login_limiter.check_and_record(client_ip, false).await;
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
            let _ = state.login_limiter.check_and_record(client_ip, false).await;
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
            let _ = state.login_limiter.check_and_record(client_ip, false).await;
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
            let _ = state.login_limiter.check_and_record(client_ip, false).await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "ID token verification failed" })),
            )
                .into_response();
        }
    };

    // Authenticated. Mark the limiter as a success so a single bad
    // pre-auth probe doesn't pollute the failure budget.
    let _ = state.login_limiter.check_and_record(client_ip, true).await;

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
    // 9router dashboardSession SESSION_MAX_AGE_SEC = 24h for every login
    // incl. SAML/OIDC ACS — not 7 days.
    let exp = (Utc::now() + ChronoDuration::days(1)).timestamp();
    let jti = crate::server::auth::generate_jti();
    let token_claims = json!({
        "sub": email,
        "email": email,
        "name": name,
        "authenticated": true,
        "iat": now,
        "exp": exp,
        "jti": jti,
    });
    let token = match encode(
        &JwtHeader::default(),
        &token_claims,
        &EncodingKey::from_secret(jwt_secret().as_bytes()),
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
    let cookie = build_auth_cookie(&token, 24 * 60 * 60, secure_cookie);
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
    if let Some(token) = crate::server::auth::extract_auth_token(&headers) {
        // Revoke by jti if we can decode it.
        if let Ok(decoded) = jsonwebtoken::decode::<AuthTokenClaims>(
            &token,
            &jsonwebtoken::DecodingKey::from_secret(jwt_secret().as_bytes()),
            &jsonwebtoken::Validation::default(),
        ) {
            if let Some(ref jti) = decoded.claims.jti {
                revoke_jti(jti);
            }
        }

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

    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
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
    let _api_key = match require_api_key_with_reload(&headers, &state.db).await {
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
    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
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
    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordChangeRequest {
    /// The existing dashboard password (plaintext). Required when a password
    /// hash is already stored in settings.
    current_password: Option<String>,
    /// The new dashboard password (plaintext). Will be bcrypt-hashed before
    /// storage. Must be at least 8 characters.
    new_password: String,
}

/// POST /api/auth/password
///
/// Change the dashboard password. The caller must present a valid dashboard
/// session (JWT cookie) or management API key.
///
/// - Verifies `current_password` against the stored bcrypt hash (if any).
/// - Bcrypt-hashes `new_password` and persists it in `settings.password`.
/// - Revokes all existing JWT sessions so the user must log in again.
pub async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PasswordChangeRequest>,
) -> Response {
    // Require either a dashboard session or management API key.
    if let Err(response) =
        crate::server::api::require_dashboard_or_management_api_key(&headers, &state)
    {
        return response;
    }

    let new_password = req.new_password.trim();
    if new_password.len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "New password must be at least 8 characters long" })),
        )
            .into_response();
    }

    let snapshot = state.db.snapshot();
    let current_hash = settings_password_hash(&snapshot.settings);

    // If a password hash already exists, require the current password for
    // verification.
    if let Some(hash) = current_hash {
        match req.current_password {
            Some(ref current) if !current.is_empty() => {
                if !verify(current, hash).unwrap_or(false) {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({ "error": "Current password is incorrect" })),
                    )
                        .into_response();
                }
            }
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Current password is required to set a new password" })),
                )
                    .into_response();
            }
        }
    }

    // Bcrypt-hash the new password.
    let hashed = match hash(new_password, DEFAULT_COST) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "failed to bcrypt-hash new password");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to hash password" })),
            )
                .into_response();
        }
    };

    // Persist the new password hash in settings.
    if let Err(e) = state
        .db
        .update(|db| {
            db.settings.password = Some(hashed);
            // Also clear the legacy `extra["password"]` field if present.
            db.settings.extra.remove("password");
        })
        .await
    {
        tracing::error!(error = %e, "failed to persist new password");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to save password" })),
        )
            .into_response();
    }

    // Only revoke existing sessions when rotating an already-set password.
    // First-time password set (force-change after default login) keeps the
    // freshly issued session cookie valid so the user can enter the dashboard.
    if current_hash.is_some() {
        crate::server::auth::increment_token_epoch();
        return Json(json!({
            "success": true,
            "message": "Password changed. All sessions have been invalidated. Please log in again.",
            "sessionsInvalidated": true,
        }))
        .into_response();
    }

    Json(json!({
        "success": true,
        "message": "Password set successfully.",
        "sessionsInvalidated": false,
    }))
    .into_response()
}

/// POST /api/auth/oidc/test
///
/// Validate OIDC discovery (and optionally the client secret) without
/// completing a full login. Uses the request body when provided, otherwise
/// falls back to the currently saved settings.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OidcTestRequest {
    issuer_url: Option<String>,
    client_id: Option<String>,
    scopes: Option<String>,
    client_secret: Option<String>,
}

pub async fn oidc_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OidcTestRequest>,
) -> Response {
    // Require a dashboard session when login is required.
    let snapshot = state.db.snapshot();
    if snapshot.settings.require_login {
        if let Err(err) = crate::server::auth::require_dashboard_session(&headers, &state.db) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": err.message() })),
            )
                .into_response();
        }
    }

    let issuer_url = req
        .issuer_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(snapshot.settings.oidc_issuer_url.as_str())
        .to_string();
    let client_id = req
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(snapshot.settings.oidc_client_id.as_str())
        .to_string();
    let scopes = req
        .scopes
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(snapshot.settings.oidc_scopes.as_str())
        .to_string();
    let client_secret = req
        .client_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(snapshot.settings.oidc_client_secret.as_str())
        .to_string();

    if issuer_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Issuer URL is required" })),
        )
            .into_response();
    }
    if client_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Client ID is required" })),
        )
            .into_response();
    }

    let redirect_uri = std::env::var("OIDC_REDIRECT_URI")
        .unwrap_or_else(|_| "http://127.0.0.1:4623/api/auth/oidc/callback".to_string());

    // Discovery-only probe: pass a placeholder secret when none is available so
    // discover() can still fetch the openid-configuration document.
    let probe_secret = if client_secret.is_empty() {
        "__probe__".to_string()
    } else {
        client_secret.clone()
    };

    let client = match crate::server::auth::oidc::OidcClient::discover(
        &issuer_url,
        &client_id,
        &probe_secret,
        &redirect_uri,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("OIDC discovery failed: {e}") })),
            )
                .into_response();
        }
    };

    let mut client_secret_tested = false;
    let mut client_secret_valid: Option<bool> = None;
    let mut secret_message = String::new();

    if !client_secret.is_empty() {
        // Soft probe: POST an intentionally invalid code. A client-auth error
        // means the secret is wrong; other token-endpoint errors (invalid_grant
        // etc.) mean discovery + client credentials are fine.
        client_secret_tested = true;
        let http = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("Failed to build HTTP client: {e}") })),
                )
                    .into_response();
            }
        };
        match http
            .post(&client.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", "__oidc_test_invalid_code__"),
                ("redirect_uri", redirect_uri.as_str()),
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("code_verifier", "__oidc_test_invalid_verifier__"),
            ])
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                let lower = body_text.to_ascii_lowercase();
                if status.as_u16() == 401
                    || lower.contains("invalid_client")
                    || lower.contains("unauthorized_client")
                    || lower.contains("client authentication failed")
                {
                    client_secret_valid = Some(false);
                    secret_message = "Client secret was rejected by the token endpoint".into();
                } else {
                    // Any other response (including invalid_grant for our fake code)
                    // means client credentials were accepted.
                    client_secret_valid = Some(true);
                    secret_message = "Client secret appears valid".into();
                }
            }
            Err(e) => {
                client_secret_valid = None;
                secret_message = format!("Could not reach token endpoint: {e}");
            }
        }

        if client_secret_valid == Some(false) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "discoveryOk": true,
                    "clientSecretTested": true,
                    "clientSecretValid": false,
                    "issuerUrl": issuer_url,
                    "clientId": client_id,
                    "scopes": scopes,
                    "redirectUri": redirect_uri,
                    "authorizationEndpoint": client.authorization_endpoint,
                    "tokenEndpoint": client.token_endpoint,
                    "jwksUri": client.jwks_uri,
                    "error": format!("Discovery loaded, but the client secret is not valid: {secret_message}"),
                })),
            )
                .into_response();
        }
    }

    Json(json!({
        "ok": true,
        "discoveryOk": true,
        "clientSecretTested": client_secret_tested,
        "clientSecretValid": client_secret_valid,
        "issuerUrl": issuer_url,
        "clientId": client_id,
        "scopes": scopes,
        "redirectUri": redirect_uri,
        "authorizationEndpoint": client.authorization_endpoint,
        "tokenEndpoint": client.token_endpoint,
        "jwksUri": client.jwks_uri,
        "message": secret_message,
    }))
    .into_response()
}

/// SAML SSO helpers: settings snapshot to SamlSettings.
fn saml_settings_from(settings: &Settings) -> crate::server::auth::saml::SamlSettings {
    crate::server::auth::saml::SamlSettings {
        entry_point: settings.saml_entry_point.clone(),
        issuer: settings.saml_issuer.clone(),
        cert: settings.saml_cert.clone(),
        attribute_email: settings.saml_attribute_email.clone(),
        attribute_name: settings.saml_attribute_name.clone(),
    }
}

fn saml_configured(settings: &Settings) -> bool {
    crate::server::auth::saml::is_saml_configured(&settings.saml_entry_point, &settings.saml_cert)
}

/// Effective SSO protocol: explicit ssoType, else legacy authMode.
/// Mirrors the JS settings.ssoType || (authMode === saml ? saml : oidc).
fn resolve_sso_type(settings: &Settings) -> &str {
    let explicit = settings.sso_type.trim();
    if explicit.eq_ignore_ascii_case("saml") {
        return "saml";
    }
    if explicit.eq_ignore_ascii_case("oidc") {
        return "oidc";
    }
    if settings.auth_mode.trim().eq_ignore_ascii_case("saml") {
        "saml"
    } else {
        "oidc"
    }
}

fn saml_origin(headers: &HeaderMap, settings: &Settings) -> String {
    use crate::server::auth::saml::saml_base_url;
    let fwd_proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok());
    let fwd_host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok());
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    saml_base_url(
        settings
            .extra
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        fwd_proto,
        fwd_host,
        host,
        None,
    )
}

fn build_saml_cookie(name: &str, value: &str, max_age_seconds: i64, secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_seconds}{secure_flag}"
    )
}

fn saml_cookie_secure(headers: &HeaderMap) -> bool {
    // Mirror the dashboard session-cookie logic (issues #456/#459).
    std::env::var("AUTH_COOKIE_SECURE").ok().as_deref() == Some("true")
        || headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.eq_ignore_ascii_case("https"))
            .unwrap_or(false)
}

/// GET /api/auth/saml/start - build AuthnRequest, stash ID in saml_state
/// cookie, 302 to the IdP. Mirrors start/route.js.
pub async fn saml_start(headers: HeaderMap, State(state): State<AppState>) -> Response {
    use crate::server::auth::saml::{build_authorize_url, generate_request_id};
    let snapshot = state.db.snapshot();
    if !saml_configured(&snapshot.settings) {
        return Redirect::to("/login?error=saml_not_configured").into_response();
    }
    let client_ip = client_ip_from_headers(&headers);
    if let Err(LockoutError::Locked { retry_after_secs }) =
        state.login_limiter.check_and_record(client_ip, false).await
    {
        return lockout_response(retry_after_secs);
    }
    let settings = saml_settings_from(&snapshot.settings);
    let origin = saml_origin(&headers, &snapshot.settings);
    let request_id = generate_request_id();
    let authorize_url = match build_authorize_url(&settings, &origin, &request_id) {
        Ok(u) => u,
        Err(e) => {
            let _ = state.login_limiter.check_and_record(client_ip, false).await;
            return Redirect::to(&format!(
                "/login?error={}",
                urlencoding::encode(&e.to_string())
            ))
            .into_response();
        }
    };
    let mut response = Redirect::to(&authorize_url).into_response();
    if let Ok(hv) = HeaderValue::from_str(&build_saml_cookie(
        "saml_state",
        &request_id,
        600,
        saml_cookie_secure(&headers),
    )) {
        response.headers_mut().append(header::SET_COOKIE, hv);
    }
    response
}

/// POST /api/auth/saml/acs - IdP assertion callback. Verifies
/// InResponseTo + XML-DSig + conditions, issues the dashboard session
/// cookie, 302 to /dashboard. Mirrors acs/route.js.
pub async fn saml_acs(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    use crate::server::auth::saml::{
        assertion_expiry_unix, assertion_replay_id, is_assertion_replayed, mark_assertion_used,
        pick_saml_display_name, pick_saml_email, validate_saml_response,
    };
    let snapshot = state.db.snapshot();
    let settings = snapshot.settings.clone();
    let client_ip = client_ip_from_headers(&headers);
    if let Err(LockoutError::Locked { retry_after_secs }) =
        state.login_limiter.check_and_record(client_ip, false).await
    {
        let origin = saml_origin(&headers, &settings);
        return Redirect::to(&format!(
            "{origin}/login?error={}",
            urlencoding::encode(&format!(
                "Too many failed attempts. Try again in {retry_after_secs}s."
            ))
        ))
        .into_response();
    }
    let stored_request_id =
        crate::server::auth::extract_cookie(&headers, "saml_state").unwrap_or_default();
    let clear_cookie = build_saml_cookie("saml_state", "", 0, saml_cookie_secure(&headers));
    let saml_response =
        serde_urlencoded::from_str::<std::collections::HashMap<String, String>>(&body)
            .ok()
            .and_then(|m| m.get("SAMLResponse").cloned())
            .unwrap_or_default();
    let fail = |msg: String| {
        let origin = saml_origin(&headers, &settings);
        let mut resp = Redirect::to(&format!(
            "{origin}/login?error={}",
            urlencoding::encode(&msg)
        ))
        .into_response();
        if let Ok(hv) = HeaderValue::from_str(&clear_cookie) {
            resp.headers_mut().append(header::SET_COOKIE, hv);
        }
        resp
    };
    if saml_response.trim().is_empty() {
        let _ = state.login_limiter.check_and_record(client_ip, false).await;
        return fail("saml_missing_response".into());
    }
    if !saml_configured(&settings) {
        let _ = state.login_limiter.check_and_record(client_ip, false).await;
        return fail("saml_not_configured".into());
    }
    let saml_settings = saml_settings_from(&settings);
    let now = Utc::now().timestamp();
    let origin = saml_origin(&headers, &settings);
    let expected_acs = format!("{origin}/api/auth/saml/acs");
    let profile = match validate_saml_response(
        &saml_response,
        &stored_request_id,
        &saml_settings,
        now,
        &expected_acs,
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("SAML ACS validation failed: {e}");
            let _ = state.login_limiter.check_and_record(client_ip, false).await;
            return fail(e.to_string());
        }
    };
    // Single-use replay cache (issues #454/#458): reject an assertion ID
    // already consumed within its lifetime window.
    let replay_id = {
        // Re-derive from the raw response: Assertion @ID, else InResponseTo.
        let xml_bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            saml_response.trim(),
        )
        .unwrap_or_default();
        let xml = String::from_utf8_lossy(&xml_bytes).into_owned();
        match crate::server::auth::saml::extract_assertion(&xml) {
            Some(ax) => assertion_replay_id(&ax, &xml),
            None => assertion_replay_id("", &xml),
        }
    };
    if is_assertion_replayed(&replay_id, now) {
        let _ = state.login_limiter.check_and_record(client_ip, false).await;
        return fail("saml_assertion_replayed".into());
    }
    // Mark consumed BEFORE issuing the session so a concurrent replay of the
    // same POST cannot mint a second session (issues #454/#458).
    {
        let xml_bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            saml_response.trim(),
        )
        .unwrap_or_default();
        let xml = String::from_utf8_lossy(&xml_bytes).into_owned();
        let exp_ts = match crate::server::auth::saml::extract_assertion(&xml) {
            Some(ax) => assertion_expiry_unix(&ax, &xml, now),
            None => assertion_expiry_unix("", &xml, now),
        };
        mark_assertion_used(&replay_id, exp_ts);
    }
    let email = pick_saml_email(&profile, &saml_settings);
    let mut name = pick_saml_display_name(&profile, &saml_settings);
    if name.trim().is_empty() {
        name = "SAML user".to_string();
    }
    let _ = state.login_limiter.check_and_record(client_ip, true).await;
    let now_ts = now_secs();
    // 9router dashboardSession SESSION_MAX_AGE_SEC = 24h for every login
    // incl. SAML/OIDC ACS — not 7 days.
    let exp = (Utc::now() + ChronoDuration::days(1)).timestamp();
    let jti = crate::server::auth::generate_jti();
    let sub = if email.is_empty() {
        name.clone()
    } else {
        email.clone()
    };
    let token_claims = json!({
        "sub": sub,
        "email": email,
        "name": name,
        "authenticated": true,
        // 9router saml/acs/route.js: setDashboardAuthCookie({ saml: true,
        // samlEmail, samlName }) — the `saml` flag drives loginMethod.
        "saml": true,
        "saml_email": email,
        "saml_name": name,
        "iat": now_ts,
        "exp": exp as usize,
        "jti": jti,
    });
    let token = match encode(
        &JwtHeader::default(),
        &token_claims,
        &EncodingKey::from_secret(jwt_secret().as_bytes()),
    ) {
        Ok(t) => t,
        Err(error) => {
            return fail(format!("Failed to issue session token: {error}"));
        }
    };
    let secure_cookie = std::env::var("AUTH_COOKIE_SECURE").ok().as_deref() == Some("true")
        || headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.eq_ignore_ascii_case("https"))
            .unwrap_or(false);
    let origin = saml_origin(&headers, &settings);
    let mut response = Redirect::to(&format!("{origin}/dashboard")).into_response();
    let cookie = build_auth_cookie(&token, 24 * 60 * 60, secure_cookie);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    if let Ok(hv) = HeaderValue::from_str(&clear_cookie) {
        response.headers_mut().append(header::SET_COOKIE, hv);
    }
    response
}

/// GET /api/auth/saml/metadata - export SP XML metadata.
/// Mirrors metadata/route.js.
pub async fn saml_metadata(headers: HeaderMap, State(state): State<AppState>) -> Response {
    use crate::server::auth::saml::generate_saml_metadata;
    let snapshot = state.db.snapshot();
    let origin = saml_origin(&headers, &snapshot.settings);
    let xml = generate_saml_metadata(&origin, &saml_settings_from(&snapshot.settings));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/xml"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        xml,
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct SamlTestRequest {
    pub saml_entry_point: Option<String>,
    pub saml_issuer: Option<String>,
    pub saml_cert: Option<String>,
}

/// POST /api/auth/saml/test - validate candidate SAML config.
/// Mirrors test/route.js.
pub async fn saml_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SamlTestRequest>,
) -> Response {
    use crate::server::auth::saml::{format_x509_certificate, rsa_public_key_from_cert_pem};
    let snapshot = state.db.snapshot();
    if snapshot.settings.require_login {
        if let Err(err) = crate::server::auth::require_dashboard_session(&headers, &state.db) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": err.message() })),
            )
                .into_response();
        }
    }
    let entry_point = req
        .saml_entry_point
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(snapshot.settings.saml_entry_point.as_str())
        .to_string();
    let issuer = req
        .saml_issuer
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let s = snapshot.settings.saml_issuer.trim();
            if s.is_empty() {
                "urn:9router:sp"
            } else {
                s
            }
        })
        .to_string();
    let cert = req
        .saml_cert
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(snapshot.settings.saml_cert.as_str())
        .to_string();
    if entry_point.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Single Sign-On Service URL (samlEntryPoint) is required" })),
        )
            .into_response();
    }
    if url::Url::parse(&entry_point).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Single Sign-On Service URL must be a valid URL" })),
        )
            .into_response();
    }
    if issuer.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "SP Entity ID / Issuer (samlIssuer) is required" })),
        )
            .into_response();
    }
    if cert.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "IdP X.509 Certificate (samlCert) is required" })),
        )
            .into_response();
    }
    if format_x509_certificate(&cert).is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid IdP X.509 Certificate format" })),
        )
            .into_response();
    }
    // Parse the cert and extract an RSA public key BEFORE accepting it
    // (issues #457/#460): a garbage cert would otherwise break ACS while
    // password login stays blocked (SSO lockout, manual DB fix to recover).
    let pem = format_x509_certificate(&cert);
    if let Err(e) = rsa_public_key_from_cert_pem(&pem) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("IdP X.509 Certificate is not a valid RSA certificate: {e}") })),
        )
            .into_response();
    }
    let origin = saml_origin(&headers, &snapshot.settings);
    Json(json!({
        "ok": true,
        "samlEntryPoint": entry_point,
        "samlIssuer": issuer,
        "certValid": true,
        "acsUrl": format!("{origin}/api/auth/saml/acs"),
        "metadataUrl": format!("{origin}/api/auth/saml/metadata"),
        "message": "SAML 2.0 configuration verified successfully.",
    }))
    .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/saml/start", get(saml_start))
        .route("/api/auth/saml/acs", post(saml_acs))
        .route("/api/auth/saml/metadata", get(saml_metadata))
        .route("/api/auth/saml/test", post(saml_test))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/password", post(change_password))
        .route("/api/auth/sessions", get(list_sessions))
        .route("/api/auth/sessions", delete(delete_all_sessions))
        .route("/api/auth/session/{session_id}", get(get_session))
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/oidc/login", get(oidc_login))
        // 9router parity: login page calls /api/auth/oidc/start.
        .route("/api/auth/oidc/start", get(oidc_login))
        .route("/api/auth/oidc/callback", get(oidc_callback))
        .route("/api/auth/oidc/test", post(oidc_test))
        .route("/api/user", get(get_user))
}

pub(crate) fn settings_password_hash(settings: &Settings) -> Option<&str> {
    if let Some(hash) = settings.password.as_deref() {
        return Some(hash);
    }
    settings
        .extra
        .get("password")
        .and_then(|value| value.as_str())
}

/// Verify a plaintext dashboard password for sensitive re-auth actions
/// (database export/import). Mirrors 9router `verifyDashboardPassword`:
/// bcrypt against the stored hash when present, otherwise the persisted
/// initial password (see [`crate::core::auth::dashboard_initial_password`]).
pub(crate) fn verify_dashboard_password(password: Option<&str>, settings: &Settings) -> bool {
    let Some(password) = password.map(str::trim).filter(|p| !p.is_empty()) else {
        return false;
    };
    if let Some(hash) = settings_password_hash(settings) {
        return verify(password, hash).unwrap_or(false);
    }
    crate::core::auth::timing_safe_eq(password, &crate::core::auth::dashboard_initial_password())
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
/// Order:
///   1. `x-9r-real-ip` — unspoofable TCP peer IP stamped by
///      [`crate::server::api::guard::real_ip_middleware`] (Fix 1). This is the
///      only trusted source; the headers below are only checked when this is
///      absent (e.g. in test environments that bypass the middleware).
///   2. `X-Forwarded-For` (first hop) — only when TRUST_PROXY=true.
///   3. `X-Real-IP` — only when TRUST_PROXY=true.
///   4. Loopback (`127.0.0.1`) — safe fallback when nothing else matches.
fn client_ip_from_headers(headers: &HeaderMap) -> std::net::IpAddr {
    // Priority 1: unspoofable TCP peer IP. The guard middleware strips
    // all client-supplied forwarding headers and stamps this one from
    // the verified connection socket.
    if let Some(value) = headers
        .get(super::guard::REAL_IP_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        if let Ok(ip) = value.trim().parse::<std::net::IpAddr>() {
            return ip;
        }
    }

    // Priority 2-3: reverse-proxy headers (only trusted when explicitly
    // enabled via TRUST_PROXY=true).
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
    let reset_hint = "Forgot password? Run `openproxy auth reset-password` on the host to restore the generated initial password.";
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": format!(
                "Too many failed attempts. Try again in {retry_after_secs}s."
            ),
            "retry_after_secs": retry_after_secs,
            // camelCase alias for the login UI countdown
            "retryAfter": retry_after_secs,
            "resetHint": reset_hint,
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().append(header::RETRY_AFTER, value);
    }
    response
}

/// Resolve dashboard auth mode for the login page.
///
/// Preference order:
/// 1. Explicit `settings.auth_mode` (`password` | `oidc` | `both`)
/// 2. Explicit `authMode` in settings.extra
/// 3. `settings.oidc_enabled` → `"both"` (password kept as recovery)
/// 4. Default `"password"`
fn resolve_auth_mode(settings: &Settings) -> String {
    let mode = settings.auth_mode.trim();
    if matches!(mode, "password" | "oidc" | "sso" | "saml" | "both") {
        return mode.to_string();
    }
    if let Some(mode) = settings
        .extra
        .get("authMode")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| matches!(*value, "password" | "oidc" | "sso" | "saml" | "both"))
    {
        return mode.to_string();
    }
    if settings.oidc_enabled {
        "both".to_string()
    } else {
        "password".to_string()
    }
}

fn resolve_oidc_login_label(settings: &Settings) -> String {
    let label = settings.oidc_login_label.trim();
    if !label.is_empty() {
        return label.to_string();
    }
    if let Some(label) = settings
        .extra
        .get("oidcLoginLabel")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return label.to_string();
    }
    if let Ok(label) = std::env::var("OIDC_LOGIN_LABEL") {
        let trimmed = label.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "Sign in with OIDC".to_string()
}

/// Resolve the SAML sign-in button label — 9router status/route.js
/// (65197ad1): `(settings.samlLoginLabel || "Sign in with SAML SSO")`.
fn resolve_saml_login_label(settings: &Settings) -> String {
    let label = settings.saml_login_label.trim();
    if !label.is_empty() {
        return label.to_string();
    }
    if let Some(label) = settings
        .extra
        .get("samlLoginLabel")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return label.to_string();
    }
    if let Ok(label) = std::env::var("SAML_LOGIN_LABEL") {
        let trimmed = label.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "Sign in with SAML SSO".to_string()
}

fn is_oidc_configured(state: &AppState) -> bool {
    // Runtime client may be set from settings or env; also accept env-only readiness.
    if state
        .oidc_client
        .try_read()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
    {
        return true;
    }
    std::env::var("OIDC_ISSUER")
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
        && std::env::var("OIDC_CLIENT_ID")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
}
