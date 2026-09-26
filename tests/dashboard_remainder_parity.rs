//! The part of bead `openproxy-vb3m` that `0fc4d916` and `fbe4eb59` did not
//! reach: how the Rust router answers `/` and the `/dashboard/**` deep links
//! underneath it, plus the client router that the page shells navigate with.
//!
//! These are routing decisions, so they are exercised through the axum app
//! against the real embedded `web/dist` rather than asserted on source. The
//! dashboard is a static Astro build (`build.format: 'file'`), which is what
//! makes the 404 provable: by the time a `/dashboard` candidate has failed
//! every probe, no route can still match it.
//!
//! One test per finding, and each names the 9router behaviour it pins. 9router
//! paths are relative to `.tmp/9router` (git 17c4cc76).

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use tempfile::tempdir;
use tower::util::ServiceExt;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// The dashboard gate is off: these tests are about routing, and a `/login`
/// redirect would mask every status code under it.
async fn app() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.settings.require_login = false;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, String) {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// The shell that came back. The Astro build names the page island after the
/// route, so this is enough to tell `/dashboard/providers` from
/// `/dashboard/providers/<uuid>` without depending on bundle hashes.
fn served_shell(body: &str) -> Option<&str> {
    [
        "EndpointPageClient",
        "ProvidersPageClient",
        "ProviderDetailPageClient",
    ]
    .into_iter()
    .find(|marker| body.contains(marker))
}

// ---------------------------------------------------------------------------
// Finding 7 — 404 semantics for unknown /dashboard paths
// ---------------------------------------------------------------------------

/// 9router answers an unmatched route with a real 404 —
/// `cli-tools/[toolId]/page.js:8` (`if (!CLI_TOOLS[toolId]) notFound()`) and
/// `media-providers/[kind]/page.js:179` (`if (!kindConfig) return notFound()`).
/// Falling through to `dashboard.html` answered a deleted or renamed route with
/// 200 and the endpoint page, so a crawler, a link checker or an uptime monitor
/// could not tell the two apart.
#[tokio::test]
async fn unknown_dashboard_path_returns_404() {
    let status = get(
        openproxy::build_app(app().await),
        "/dashboard/definitely-not-a-route",
    )
    .await;
    assert_eq!(
        status.0,
        StatusCode::NOT_FOUND,
        "an unrouted /dashboard path must 404, not fall through to the dashboard shell \
         (9router renders Next's 404 page with HTTP 404)"
    );
}

/// The 404 must be the 404 *rule*, not a blanket refusal: every route that does
/// exist keeps answering 200. A guard that matched `/dashboard` by prefix alone
/// would pass the test above and break the entire product.
#[tokio::test]
async fn built_dashboard_routes_still_serve_their_own_shell() {
    let cases = [
        ("/dashboard", "EndpointPageClient"),
        ("/dashboard/endpoint", "EndpointPageClient"),
        ("/dashboard/providers", "ProvidersPageClient"),
        ("/dashboard/settings/pricing", "PricingPageClient"),
    ];
    for (uri, marker) in cases {
        let (status, body) = get(openproxy::build_app(app().await), uri).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{uri} is a built route and must stay 200"
        );
        assert!(
            body.contains(marker),
            "{uri} must serve the {marker} shell, not a different page's"
        );
    }
}

/// Provider ids are created at runtime, so the build cannot know them and the
/// router falls back to a per-directory `_dynamic` placeholder. That fallback is
/// the reason the 404 has to sit *after* it: a blanket `/dashboard` 404 would
/// 404 every provider deep link.
#[tokio::test]
async fn runtime_provider_deep_links_still_resolve() {
    let (status, body) = get(
        openproxy::build_app(app().await),
        "/dashboard/providers/8d82774e-1f2b-4c3d-9e5f-0a1b2c3d4e5f",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        served_shell(&body),
        Some("ProviderDetailPageClient"),
        "an unknown provider id must reach the detail shell via the _dynamic placeholder"
    );
}

// ---------------------------------------------------------------------------
// Finding 11 — the default landing page
// ---------------------------------------------------------------------------

/// 9router's `src/app/page.js:3-4` is `redirect('/dashboard')` — a real 307.
/// Rendering the endpoint page in place at `/` left the canonical URL off the
/// address bar, so a bookmark, a reload and the in-app nav all disagreed.
#[tokio::test]
async fn root_redirects_to_dashboard() {
    let app = openproxy::build_app(app().await);
    let request = Request::builder().uri("/").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::TEMPORARY_REDIRECT,
        "`/` must be a 307 to /dashboard, not a rendered page (9router src/app/page.js:3-4)"
    );
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/dashboard")
    );
}

/// The reason this is a 307 and not the meta-refresh stub it replaced: a stub
/// in `index.html` fights `build.format: 'file'` and can send the browser back
/// to the page it came from. The redirect has to be one server response, and
/// `/dashboard` has to be a real route for it to land on.
#[tokio::test]
async fn the_redirect_target_is_itself_a_route() {
    let (status, body) = get(openproxy::build_app(app().await), "/dashboard").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("EndpointPageClient"));
}

// ---------------------------------------------------------------------------
// Finding 15 — trailing-slash deep links
// ---------------------------------------------------------------------------

/// 9router inherits Next's `trailingSlash: false` and normalises
/// `/dashboard/providers/` to `/dashboard/providers` before routing. The empty
/// final segment instead read as a provider id, and the detail shell rendered
/// with a blank id.
#[tokio::test]
async fn trailing_slash_on_a_list_page_serves_the_list() {
    let (status, body) = get(openproxy::build_app(app().await), "/dashboard/providers/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        served_shell(&body),
        Some("ProvidersPageClient"),
        "a trailing slash on a list page must resolve to the list, not the _dynamic detail shell"
    );
}

/// The same trap sits on every directory that has a `_dynamic` placeholder, so
/// cover two more rather than proving the rule once.
#[tokio::test]
async fn trailing_slash_matches_its_own_route() {
    for (uri, marker) in [
        (
            "/dashboard/media-providers/tts/",
            "MediaProvidersKindPageClient",
        ),
        ("/dashboard/cli-tools/", "CLIToolsPageClient"),
    ] {
        let (status, body) = get(openproxy::build_app(app().await), uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} is a built route");
        assert!(
            body.contains(marker),
            "{uri} must serve the {marker} shell; the empty final segment reads as an id and \
             reaches the _dynamic detail shell instead"
        );
    }
}

// ---------------------------------------------------------------------------
// Finding 13 — in-app navigation
// ---------------------------------------------------------------------------
//
// Source-contract, not runtime: the dashboard has no JS test runner, so this
// follows `tests/dashboard_pages_parity.rs` and reads the file as text.

fn read_src(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("web")
        .join("src")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// 9router is a Next.js App Router app: 13 files import `next/link`, 29
/// `<Link>` elements and 11 `router.push` call sites mean a sidebar or
/// breadcrumb click never reloads the document. Astro's `ClientRouter` is the
/// same guarantee, and `astro:transitions` ships with astro — it is the import
/// and the component in `<head>` that matter.
#[test]
fn dashboard_uses_a_client_router() {
    let layout = read_src("layouts/Layout.astro");

    assert!(
        layout.contains("from 'astro:transitions'"),
        "Layout.astro must import ClientRouter from astro:transitions"
    );

    let import = layout
        .lines()
        .find(|line| line.contains("from 'astro:transitions'"))
        .expect("Layout.astro must import ClientRouter from astro:transitions");
    assert!(
        import.contains("ClientRouter"),
        "the astro:transitions import must bind ClientRouter, not something else"
    );

    let rendered = layout.find("<ClientRouter />").expect(
        "Layout.astro must render <ClientRouter /> — without it every sidebar, header and \
         breadcrumb click is a full document load (9router uses next/link throughout)",
    );

    let head = layout.find("<head>").expect("Layout.astro has a <head>");
    assert!(
        rendered > head,
        "<ClientRouter /> belongs in <head>, where it installs the router"
    );
}

// ---------------------------------------------------------------------------
// Finding 11, web half — `/` has no page of its own
// ---------------------------------------------------------------------------

/// The 307 is the whole mechanism, so the page must not carry a second one.
/// A meta-refresh here is the shape that produced the original redirect loop.
#[test]
fn the_landing_page_carries_no_redirect_of_its_own() {
    let index = read_src("pages/index.astro");

    for redirect in [
        "http-equiv=\"refresh\"",
        "window.location",
        "EndpointPageClient",
    ] {
        assert!(
            !index.contains(redirect),
            "pages/index.astro must not contain `{redirect}` — the 307 in `dashboard_fallback` \
             is the redirect, and a client-side one fights `build.format: 'file'`"
        );
    }
}
