//! The MITM endpoints must answer a dashboard session, not only an API key.
//!
//! `get_config`, `update_config`, `generate_cert`, `start_mitm` and `stop_mitm`
//! called `require_api_key_with_reload`, which reads a Bearer key out of the
//! headers and has no notion of a session. The dashboard sends none: its shared
//! client (`web/src/shared/utils/backendApi.ts`) fetches with
//! `credentials: "same-origin"` and no `Authorization` header, so the session
//! cookie is the only credential it has. Every one of those calls came back
//! `401 {"code":"invalid_api_key"}` — the MITM page could not read its own
//! state, and because the config read sits in the shared layout, the 401 fired
//! on every dashboard route.
//!
//! Four handlers in the same file already used
//! `require_dashboard_or_management_api_key` — the tier that accepts a session
//! or a management key — so the file contradicted itself. This pins the whole
//! MITM surface to the accepting tier, and keeps the "no credential at all is
//! still refused" half, which is the direction that actually protects anything.
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use jsonwebtoken::{encode, EncodingKey, Header};
use openproxy::db::Db;
use openproxy::server::auth::{generate_jti, jwt_secret};
use openproxy::server::state::AppState;
use serde::Serialize;
use tempfile::tempdir;
use tower::util::ServiceExt;

#[derive(Serialize)]
struct DashboardClaims {
    authenticated: bool,
    exp: usize,
    jti: String,
}

/// Mint a dashboard JWT the way the login issuer does — signed with the shared
/// secret and carrying a jti, which the session gate rejects without.
fn cookie() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as usize;
    let token = encode(
        &Header::default(),
        &DashboardClaims {
            authenticated: true,
            exp: now + 3600,
            jti: generate_jti(),
        },
        &EncodingKey::from_secret(jwt_secret().as_bytes()),
    )
    .expect("dashboard token");
    format!("auth_token={token}")
}

async fn app() -> axum::Router {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    openproxy::build_app(AppState::new(db))
}

async fn get(uri: &str, cookie: Option<&str>) -> StatusCode {
    let mut builder = Request::builder().uri(uri);
    if let Some(value) = cookie {
        builder = builder.header(header::COOKIE, value);
    }
    app()
        .await
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response")
        .status()
}

/// Reading the MITM config from the dashboard must work. This is the fix: the
/// config read runs in the shared layout, so a 401 here broke every route, and
/// the MITM page itself could not show its own state.
#[tokio::test]
async fn dashboard_session_can_read_mitm_config() {
    let status = get("/api/mitm-config", Some(&cookie())).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the dashboard authenticates with a session cookie only; rejecting it \
         401s every page, because this call sits in the shared layout"
    );
}

/// The other half, and the one that matters for safety: accepting a session must
/// not have turned the route into an open one. No credential is still refused.
#[tokio::test]
async fn mitm_config_still_requires_a_credential() {
    let status = get("/api/mitm-config", None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "accepting the dashboard session must not have opened the route to \
         unauthenticated callers"
    );
}
