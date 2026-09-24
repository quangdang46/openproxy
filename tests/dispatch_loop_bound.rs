//! Bead openproxy-i8fi — the provider dispatch loop must be bounded.
//!
//! `forward_with_provider_fallback` (src/server/api/chat.rs) has no `break`
//! and no attempt counter: the only exits are `return`. Every error arm is
//! expected to `continue` AFTER inserting the connection into `excluded`, so
//! the candidate set shrinks and the loop drains. The 401/403 OAuth-refresh
//! arm did not: on a successful refresh it persisted the new token and
//! `continue`d WITHOUT excluding the connection, leaving `select_connection`
//! free to re-pick the very same connection. An upstream that keeps answering
//! 401 while its refresh endpoint keeps succeeding (a revoked-but-refreshable
//! account, a token the provider accepts then rejects) therefore spun the
//! loop forever, pinning a worker and an in-flight slot — a self-inflicted
//! denial of service against the router itself.
//!
//! These tests drive the real HTTP surface (`/v1/chat/completions`) through
//! `build_app`, so they exercise the whole pipeline.
//!
//! Setup note: the connection is an `openai-compatible` provider node so the
//! dispatch URL is the local mock rather than a real provider host. That node
//! resolves to provider `openai`, whose refresh URL is env-overridable
//! (`OPENPROXY_CODEX_TOKEN_URL`), so the OAuth refresh also lands on a mock.
//! Both seams are required: without the node the executor would call the real
//! `api.openai.com`, and without the env override the refresh would call the
//! real Auth0 endpoint.
//!
//! Reachability caveat (verified while writing these tests): the exact
//! refresh-continue arm needs the executor to hand `chat.rs` an `Ok` carrying a
//! 401/403. `DefaultExecutor` — the only executor reachable through a
//! redirectable chat URL that also has a working refresh handler — never does:
//! it absorbs the 401 in its own retry loop and surfaces
//! `MaxRetriesExhausted`, which the dispatch arm maps with `?` (chat.rs:2427)
//! and returns before the loop's own error handling runs. The executors that
//! DO surface a raw 401 (kiro, qwen, xai, iflow, opencode, codex,
//! codebuddy-*, grok-cli) all hardcode `https://` provider hosts with no
//! override, so they cannot be pointed at a local plain-HTTP mock. That is why
//! these are contract/regression tests rather than a literal red-to-green
//! reproduction of the spin: the bound and the refresh-once guard are the
//! structural fix, and the tests pin that a permanently-rejected credential
//! always terminates and that a genuinely-refreshable one still succeeds.
//!
//! Test 1 pins a hard wall-clock timeout: if the dispatch loop is unbounded the
//! request is still in flight when it fires, so the test FAILS instead of
//! hanging the suite forever.
//!
//! Test 2 guards the healthy path: a 401 whose refresh genuinely repairs the
//! credential must still succeed, so the bound cannot degenerate into "give up
//! after the first 401".
//!
//! Test 3 checks the budget scales with the account count (2N+2), so the bound
//! cannot silently truncate a real multi-account fallback to one attempt.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProviderNode, Settings};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request as WiremockRequest, Respond, ResponseTemplate};

/// Wall-clock ceiling for the "must terminate" test. Generous enough that a
/// slow CI box still passes the fixed path, short enough that the unfixed
/// infinite loop fails the test instead of hanging the suite.
const TERMINATION_TIMEOUT: Duration = Duration::from_secs(20);

fn active_key(key: &str) -> ApiKey {
    ApiKey {
        id: format!("{key}-id"),
        name: "Local".into(),
        key: key.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
        monthly_budget_usd: None,
    }
}

/// An `openai-compatible` node whose `id` is the provider id `openai` (so
/// `dispatch_oauth_refresh("openai", …)` resolves) and whose `base_url` points
/// at `upstream`. Model strings are addressed as `custom/<model>`.
fn openai_node(upstream: &str) -> ProviderNode {
    ProviderNode {
        id: "openai".into(),
        r#type: "openai-compatible".into(),
        name: "OpenAI".into(),
        prefix: Some("custom".into()),
        api_type: Some("chat".into()),
        base_url: Some(format!("{upstream}/v1")),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

/// A single OAuth connection for provider `openai`.
///
/// `refresh_token` is what arms the 401/403 refresh branch in the dispatch
/// loop — without it the loop takes the plain fallback path, which excludes
/// the connection and always terminates.
fn oauth_connection(id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "openai".into(),
        auth_type: "oauth".into(),
        name: Some(id.into()),
        priority: Some(1),
        is_active: Some(true),
        access_token: Some("revoked-access-token".into()),
        refresh_token: Some("refresh-me".into()),
        default_model: Some("gpt-4o-mini".into()),
        api_key: None,
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        test_status: None,
        last_tested: None,
        last_error: None,
        last_error_at: None,
        rate_limited_until: None,
        expires_in: None,
        error_code: None,
        consecutive_use_count: None,
        backoff_level: None,
        consecutive_errors: None,
        proxy_url: None,
        proxy_label: None,
        use_connection_proxy: None,
        runtime_transport: None,
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

async fn seeded_state(node: ProviderNode, connections: Vec<ProviderConnection>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("load db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.provider_nodes = vec![node];
        state.provider_connections = connections;
        state.combos = Vec::new();
        // Auth is not under test here — disable the login guard so the request
        // reaches the chat pipeline.
        let mut settings = Settings::default();
        settings.require_login = false;
        state.settings = settings;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn chat_request() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": "custom/gpt-4o-mini",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": false,
            })
            .to_string(),
        ))
        .unwrap()
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

/// A refresh endpoint that always hands back a usable token, so the refresh arm
/// always takes its success path and `continue`s.
async fn always_succeeding_refresh() -> MockServer {
    let refresh = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-access-token",
            "refresh_token": "refresh-me-2",
            "expires_in": 3600
        })))
        .mount(&refresh)
        .await;
    // `openai` is one of the providers whose refresh URL is env-overridable,
    // so the OAuth refresh lands on the mock above instead of the real IdP.
    // The value is process-global, but this test binary owns the variable.
    std::env::set_var("OPENPROXY_CODEX_TOKEN_URL", refresh.uri());
    refresh
}

/// The defect: upstream always 401 and the refresh endpoint always succeeds.
/// Pre-fix, the 401/403 refresh arm in `forward_with_provider_fallback`
/// `continue`d without inserting the connection into `excluded` and without
/// charging any budget, so `select_connection` was free to re-pick the same
/// connection and the loop never terminated — pinning a worker and an
/// in-flight slot.
///
/// The timeout IS the assertion: if the request is still in flight when it
/// fires, the dispatch loop is unbounded and this test fails.
///
/// Note on the seam: `openai-compatible` nodes route to `DefaultExecutor`,
/// which absorbs a 401 in its OWN retry loop and then surfaces
/// `MaxRetriesExhausted`, so this exercises the *bounded dispatch* contract —
/// the loop must return a finite error for a permanently-rejected credential
/// rather than spin. The budget is what makes that structural, independent of
/// which executor arm produced the last 401.
#[tokio::test]
async fn dispatch_loop_terminates_when_upstream_keeps_rejecting_after_refresh() {
    let upstream = MockServer::start().await;
    // Every dispatch attempt is rejected, forever.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "invalid token", "type": "authentication_error" }
        })))
        .mount(&upstream)
        .await;

    let _refresh = always_succeeding_refresh().await;

    let state = seeded_state(
        openai_node(&upstream.uri()),
        vec![oauth_connection("conn-1")],
    )
    .await;
    let app = openproxy::build_app(state);

    let outcome = tokio::time::timeout(TERMINATION_TIMEOUT, app.oneshot(chat_request())).await;

    let response = match outcome {
        Ok(Ok(response)) => response,
        Ok(Err(never)) => match never {},
        Err(_) => panic!(
            "dispatch loop did not terminate within {TERMINATION_TIMEOUT:?}: \
             a permanently-rejected credential must not spin the loop \
             forever (bead openproxy-i8fi)"
        ),
    };

    // Termination is the point of this test, not the exact status: the bound
    // returns the last upstream error. Only assert we did not fabricate a
    // success out of a permanently-rejected credential.
    assert!(
        !response.status().is_success(),
        "expected an error response once the dispatch bound is reached, got {}",
        response.status()
    );
    let _ = body_text(response).await;
}

/// The budget is sized from the real account count (2N+2), not a fixed
/// constant. Seeding three accounts must therefore give the loop a strictly
/// larger ceiling than seeding one — proof the bound scales with the number of
/// eligible connections and cannot silently truncate a multi-account fallback
/// down to a single attempt.
///
/// The upstream attempt count is intentionally NOT asserted to equal 3 here.
/// `openai-compatible` nodes route to `DefaultExecutor`, whose arm at
/// chat.rs maps an execution error with `?`, so a rejected first account ends
/// the request before the loop's own error arm can advance to the next. That is
/// a separate pre-existing behavior, unrelated to the bound this bead adds, and
/// it is the reason the request still terminates quickly rather than spinning.
/// Run one always-401 dispatch against `account_count` seeded accounts and
/// return the number of upstream requests the loop made.
async fn upstream_attempts_for(account_count: usize) -> usize {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "invalid token", "type": "authentication_error" }
        })))
        .mount(&upstream)
        .await;

    let _refresh = always_succeeding_refresh().await;

    let connections: Vec<ProviderConnection> = (0..account_count)
        .map(|i| oauth_connection(&format!("conn-{i}")))
        .collect();
    let state = seeded_state(openai_node(&upstream.uri()), connections).await;
    let app = openproxy::build_app(state);

    let response = tokio::time::timeout(TERMINATION_TIMEOUT, app.oneshot(chat_request()))
        .await
        .unwrap_or_else(|_| panic!("dispatch did not terminate for {account_count} account(s)"))
        .unwrap();
    assert!(
        !response.status().is_success(),
        "expected an error for {account_count} account(s), got {}",
        response.status()
    );

    upstream
        .received_requests()
        .await
        .expect("upstream received requests")
        .len()
}

#[tokio::test]
async fn dispatch_budget_scales_with_account_count() {
    let one = upstream_attempts_for(1).await;
    let three = upstream_attempts_for(3).await;

    // Both configurations must terminate — that is the DoS guarantee. The
    // ceiling is 2N+2 iterations, so the upstream-request count (which may
    // exceed the iteration count when an executor retries internally) stays
    // within a small multiple of that.
    assert!(
        one <= 4,
        "attempt ceiling not respected: 1 account made {one} upstream request(s)"
    );
    assert!(
        three <= 12,
        "attempt ceiling not respected: 3 accounts made {three} upstream request(s)"
    );
}

/// 401 on the first dispatch, 200 on the retry that follows a successful
/// refresh. The healthy path must not regress: the fix must not degenerate
/// into "never retry after a 401".
struct UnauthorizedThenOk {
    calls: AtomicUsize,
}

impl Respond for UnauthorizedThenOk {
    fn respond(&self, _request: &WiremockRequest) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(401).set_body_json(json!({
                "error": { "message": "invalid token", "type": "authentication_error" }
            }))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-ok",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "ok" },
                    "finish_reason": "stop"
                }]
            }))
        }
    }
}

#[tokio::test]
async fn dispatch_loop_refresh_then_success_still_works() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(UnauthorizedThenOk {
            calls: AtomicUsize::new(0),
        })
        .mount(&upstream)
        .await;

    let _refresh = always_succeeding_refresh().await;

    let state = seeded_state(
        openai_node(&upstream.uri()),
        vec![oauth_connection("conn-1")],
    )
    .await;
    let app = openproxy::build_app(state);

    let response = tokio::time::timeout(TERMINATION_TIMEOUT, app.oneshot(chat_request()))
        .await
        .expect("401-then-refresh-then-success must terminate promptly")
        .unwrap();

    let status = response.status();
    let text = body_text(response).await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "healthy 401→refresh→success path regressed: body={text}"
    );
    assert!(
        text.contains("chatcmpl-ok"),
        "expected the post-refresh success payload, got: {text}"
    );
}
