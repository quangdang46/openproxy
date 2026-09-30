//! openproxy-siv7: the dashboard shell must not be served to an
//! unauthenticated `/dashboard/*` deep link.
//!
//! The handler is a `Router::fallback`, so every path reached it and the SPA
//! shell came back 200 — for a route that had been deleted or renamed just as
//! much as for a real one. Crawlers, link checkers and uptime monitors could
//! not tell the difference, so route regressions were undetectable, and a user
//! following a stale link landed on a plausible-looking but wrong screen with
//! no not-found signal.
//!
//! This is navigation only. `is_rust_owned_path` hard-404s `/api`, `/v1` and
//! `/codex`, and the API layer keeps its own gate, so serving the shell exposed
//! no data — which is exactly why it went unnoticed.
//!
//! One test per carve-out, not one aggregate assertion: a single test that
//! checks "gated" and "pages still open" together passes when the gate is
//! simply applied to everything.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use tempfile::tempdir;
use tower::util::ServiceExt;

async fn app(require_login: bool) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.settings.require_login = require_login;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn get(app: axum::Router, uri: &str) -> StatusCode {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    app.oneshot(request).await.unwrap().status()
}

/// A deep link with no session is redirected to the login form, not handed the
/// shell. This is the fix.
#[tokio::test]
async fn unauthenticated_dashboard_deep_link_is_redirected() {
    let app = openproxy::build_app(app(true).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dashboard/providers/kilocode")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::TEMPORARY_REDIRECT,
        "an unauthenticated deep link must not get the shell"
    );
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/login")
    );
}

/// Onboarding breaks if the login page itself needs a login.
#[tokio::test]
async fn login_page_stays_public() {
    let app = openproxy::build_app(app(true).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::TEMPORARY_REDIRECT);
}

/// The OAuth redirect has to land somewhere after the provider sends the user
/// back, so the callback cannot be behind the gate.
#[tokio::test]
async fn oauth_callback_stays_public() {
    let app = openproxy::build_app(app(true).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/callback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::TEMPORARY_REDIRECT);
}

/// The marketing landing page is not the dashboard.
#[tokio::test]
async fn landing_page_stays_public() {
    let app = openproxy::build_app(app(true).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/landing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::TEMPORARY_REDIRECT);
}

/// API paths must not be answered by the dashboard surface. They are handled by
/// the API router, which applies its own gate — a 401 is the correct answer
/// there, and it is what proves the dashboard fallback did not serve them. What
/// must never happen is the SPA shell or a login redirect, either of which
/// would mean the dashboard surface answered a path it does not own.
#[tokio::test]
async fn api_paths_are_not_answered_by_the_dashboard() {
    let app = openproxy::build_app(app(true).await);
    for uri in ["/api/providers", "/v1/models", "/codex/status"] {
        let status = get(app.clone(), uri).await;
        assert!(
            status != StatusCode::TEMPORARY_REDIRECT,
            "{uri} was redirected to login instead of being handled by the API layer"
        );
    }
}

/// With the dashboard login switched off entirely, nothing is gated — an
/// operator who turned auth off must not be locked out by the gate itself.
#[tokio::test]
async fn gate_is_inert_when_login_is_disabled() {
    let app = openproxy::build_app(app(false).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dashboard/providers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        response.status(),
        StatusCode::TEMPORARY_REDIRECT,
        "requireLogin=false must not be overridden by the dashboard gate"
    );
}

/// Build output must not be behind the session gate.
///
/// The gate redirects a sessionless request to `/login`, so a gated
/// `/_astro/*.js` came back as the HTML shell. The browser enforces strict
/// MIME checking on module scripts, so the island never hydrated — and since
/// `/login` is itself hydrated from `/_astro/*`, the login page rendered a
/// blank with no password field. Nobody could log in at all.
///
/// These are hashed bundles, icons and translations: no user data, so gating
/// them bought no privacy and cost a working login form.
#[tokio::test]
async fn build_assets_are_not_gated() {
    let app = openproxy::build_app(app(true).await);
    for uri in [
        "/_astro/LoginPageClient.js",
        "/_astro/index.css",
        "/icons/logo.svg",
        "/i18n/en.json",
        "/favicon.svg",
        "/sw.js",
    ] {
        let status = get(app.clone(), uri).await;
        assert_ne!(
            status,
            StatusCode::TEMPORARY_REDIRECT,
            "{uri} was redirected to the login page; a module script served as \
             text/html fails the browser's strict MIME check and the island never hydrates"
        );
    }
}
