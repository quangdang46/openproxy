//! Every dashboard-facing API route must answer a dashboard session.
//!
//! The dashboard's shared client (`web/src/shared/utils/backendApi.ts`) fetches
//! with `credentials: "same-origin"` and no `Authorization` header, so the
//! session cookie is the only credential it has. Handlers that call
//! `require_api_key_with_reload` read a Bearer key out of the headers and have
//! no notion of a session, so every one of them answered `401` in the browser.
//!
//! Three modules had drifted this way. `mitm_config.rs` was fixed separately
//! (the config read sits in the shared layout, so it 401'd on every route);
//! these two are the remaining surfaces — the console-log page and the TTS
//! voice pickers. Both are merged into the "mixed auth, managed per-handler"
//! router, so the per-handler check is the only gate and there is nothing above
//! it to catch the session.
//!
//! The sweep these tests encode is mechanical: enumerate the API paths the
//! dashboard source references, then call each with a session cookie. Asserting
//! on a couple of hand-picked routes would have missed both of these — the
//! console-log page looked clean in a browser because it fetches its logs on a
//! timer, and the voice pickers only fire once a TTS provider is opened.
use std::collections::BTreeMap;
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

/// The routes the dashboard source actually references, and that a session
/// must reach. Keyed by the module that owns them so a failure names the
/// surface the operator was looking at.
fn dashboard_session_routes() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        (
            "console-log page",
            vec![
                "/api/observability/logs",
                "/api/observability/stats",
                "/api/observability/stream",
            ],
        ),
        (
            "TTS voice pickers",
            vec![
                "/api/media-providers/tts/elevenlabs/voices",
                "/api/media-providers/tts/deepgram/voices",
                "/api/media-providers/tts/inworld/voices",
                "/api/media-providers/tts/minimax/voices",
            ],
        ),
    ])
}

#[tokio::test]
async fn dashboard_session_reaches_every_referenced_api_route() {
    let mut rejected: Vec<String> = Vec::new();
    for (surface, routes) in dashboard_session_routes() {
        for uri in routes {
            let status = get(uri, Some(&cookie())).await;
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                rejected.push(format!("{surface}: {uri} -> {status}"));
            }
        }
    }
    assert!(
        rejected.is_empty(),
        "the dashboard has no way to send an API key, so these must accept a \
         session:\n  {}",
        rejected.join("\n  ")
    );
}

/// The direction that protects anything: no credential is still refused. If
/// accepting the session had opened these routes, a loopback caller with
/// nothing at all would be able to read the log buffer.
#[tokio::test]
async fn these_routes_still_require_a_credential() {
    for (surface, routes) in dashboard_session_routes() {
        for uri in routes {
            let status = get(uri, None).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{surface}: {uri} answered {status} with no credential at all"
            );
        }
    }
}
