//! Logged-out and password-invalidated dashboard sessions must stay dead
//! across a process restart.
//!
//! Both the per-jti revocation list and the bulk-invalidation epoch used to be
//! process-local `Lazy`/`AtomicU64` statics. A restart therefore handed every
//! previously-logged-out cookie a full token lifetime of life again, and
//! silently un-did the "all sessions have been invalidated" that
//! `POST /api/auth/password` promises — a restart is exactly what an attacker
//! with a stolen cookie would try.
//!
//! `9router` is canonical here only for the *shape* of the fix: its
//! `loadJwtSecret` (src/lib/auth/dashboardSession.js:12-22) is the one piece of
//! auth state it bothers to keep under `DATA_DIR`, precisely so a restart does
//! not change the answer. 9router itself never revokes server-side
//! (`dashboardSession.js:72-74` deletes the cookie and keeps nothing), so these
//! tests cover OpenProxy's own stronger path — they would not exist if the
//! reference had no revocation list to lose.
//!
//! These are process-global by nature — one store per process — so the two
//! cases share this file and are serialised rather than run in parallel.

use std::sync::{Arc, LazyLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use jsonwebtoken::{encode, EncodingKey, Header};
use openproxy::db::Db;
use openproxy::server::auth::{generate_jti, init_revocation_store, jwt_secret};
use openproxy::server::state::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

/// The store is process-global, so the two tests below would otherwise fight
/// over it. The guard is held across awaits, so this has to be the async mutex:
/// a std one would block the runtime thread while the test it guards is parked.
static SERIAL: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Deserialize, serde::Serialize)]
struct Claims {
    authenticated: bool,
    exp: usize,
    jti: Option<String>,
}

fn now_secs() -> usize {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as usize
}

/// Mint a dashboard JWT the same way the login/OIDC/SAML issuers do: signed
/// with the shared secret, carrying a jti from the live store.
fn mint_token() -> String {
    encode(
        &Header::default(),
        &Claims {
            authenticated: true,
            exp: now_secs() + 3600,
            jti: Some(generate_jti()),
        },
        &EncodingKey::from_secret(jwt_secret().as_bytes()),
    )
    .expect("encode dashboard token")
}

fn cookie_headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("auth_token={token}")).expect("cookie header"),
    );
    headers
}

async fn post(app: axum::Router, uri: &str, cookie: &str, body: Value) -> axum::response::Response {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("auth_token={cookie}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    app.oneshot(request).await.unwrap()
}

/// A `Db` over `dir` with the dashboard login switched on, plus a revocation
/// store rooted there. Both are needed: the store is process-global, so a test
/// that skipped this would inherit the previous test's directory and quietly
/// assert against the wrong on-disk state.
async fn seeded_db(dir: &std::path::Path) -> Arc<Db> {
    let db = Arc::new(Db::load_from(dir).await.expect("db"));
    db.update(|state| {
        state.settings.require_login = true;
    })
    .await
    .expect("seed db");
    let _ = init_revocation_store(dir);
    db
}

/// Log out, restart, and the old cookie must still be refused — while a second
/// session that was never logged out keeps working, so this cannot be satisfied
/// by a store that rejects everything.
#[tokio::test]
async fn logged_out_cookie_is_still_rejected_after_a_restart() {
    let _guard = SERIAL.lock().await;
    let dir = tempdir().expect("tempdir");
    let db = seeded_db(dir.path()).await;
    let app = openproxy::build_app(AppState::new(db.clone()));

    let logged_out = mint_token();
    // Minted before the restart and never logged out — the control.
    let still_valid = mint_token();

    let response = post(
        app.clone(),
        "/api/auth/logout",
        &logged_out,
        json!({ "session_id": null }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("logout must clear the cookie");
    assert!(
        set_cookie.contains("Max-Age=0"),
        "logout must expire the cookie, got {set_cookie}"
    );

    assert!(
        openproxy::server::auth::require_dashboard_session(&cookie_headers(&logged_out), &db)
            .is_err(),
        "the cookie must be dead immediately after logout, or the revocation never landed"
    );

    // The restart: a fresh store over the same data directory, as a new
    // process would build it.
    let _ = init_revocation_store(dir.path());

    assert!(
        openproxy::server::auth::require_dashboard_session(&cookie_headers(&logged_out), &db)
            .is_err(),
        "a logged-out session must stay logged out across a restart"
    );
    assert!(
        openproxy::server::auth::require_dashboard_session(&cookie_headers(&still_valid), &db)
            .is_ok(),
        "a session that was never logged out must not be collateral damage"
    );
}

/// A password change answers the user "All sessions have been invalidated".
/// A restart must not quietly make that false.
#[tokio::test]
async fn password_change_still_rejects_outstanding_tokens_after_a_restart() {
    let _guard = SERIAL.lock().await;
    let dir = tempdir().expect("tempdir");
    let db = seeded_db(dir.path()).await;
    let app = openproxy::build_app(AppState::new(db.clone()));

    let session_a = mint_token();
    let session_b = mint_token();

    // First change sets the password: `current_hash` is None, so the handler
    // deliberately leaves the fresh session alone.
    let first = post(
        app.clone(),
        "/api/auth/password",
        &session_a,
        json!({ "newPassword": "first-password-123" }),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    // Second change rotates an already-set password, which is the branch that
    // promises `sessionsInvalidated`.
    let second = post(
        app.clone(),
        "/api/auth/password",
        &session_a,
        json!({
            "currentPassword": "first-password-123",
            "newPassword": "second-password-456",
        }),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("read body"),
    )
    .expect("json body");
    assert_eq!(
        body.get("sessionsInvalidated").and_then(Value::as_bool),
        Some(true),
        "the invalidation branch was not taken, so this test proves nothing: {body}"
    );

    // The restart.
    let _ = init_revocation_store(dir.path());

    for (label, token) in [("first", &session_a), ("second", &session_b)] {
        assert!(
            openproxy::server::auth::require_dashboard_session(&cookie_headers(token), &db)
                .is_err(),
            "the {label} session predates the password change and must stay rejected after a restart"
        );
    }

    // Control, minted after the restart: a login is all the user should need.
    let after_change = mint_token();
    assert!(
        openproxy::server::auth::require_dashboard_session(&cookie_headers(&after_change), &db)
            .is_ok(),
        "a session minted after the password change must be accepted after a restart"
    );
}
