//! Bead openproxy-dl63 — the SSE stream's end-of-life contract.
//!
//! Everything here drives a real request through the router against a mock
//! upstream, because the unit tests for these behaviours call the helper in
//! isolation and that is precisely how the previous round of stream-lifecycle
//! defects got through six review rounds while every helper test was green.
//!
//! Three contracts are pinned:
//!
//!  1. A Responses passthrough that ends without ever reaching a terminal event
//!     is handed a synthesized `response.failed` followed by `[DONE]`
//!     (9router `formatIncompleteOpenAIResponsesStreamFailure`,
//!     stream.js:466-479). A Responses client waits for a terminal event; a
//!     stream that merely ends leaves it waiting.
//!  2. A stream that DID reach a terminal event, and every non-Responses
//!     format, is handed nothing extra. 9router closes the stream on a
//!     network close or a stall rather than injecting a synthetic
//!     `data: {"error":…}` frame into a stream that may already have committed
//!     output (streamHandler.js:155-163, :208).
//!  3. The streamed body carries 9router's client-facing header set
//!     (`SSE_HEADERS_CORS`, sseConstants.js:18-23) — including a CORS origin,
//!     and NOT the `X-Accel-Buffering` that belongs to the internal
//!     nginx-facing variant.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProviderNode, Settings};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn provider_node(id: &str, prefix: &str, api_type: &str, base_url: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: "Compatible".into(),
        prefix: Some(prefix.into()),
        api_type: Some(api_type.into()),
        base_url: Some(base_url.into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn connection(id: &str, provider: &str, api_key: &str, default_model: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: Some(default_model.into()),
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: Some(api_key.into()),
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

async fn seeded_state(nodes: Vec<ProviderNode>, connections: Vec<ProviderConnection>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.provider_nodes = nodes;
        state.provider_connections = connections;
        state.settings = Settings {
            require_login: false,
            ..Settings::default()
        };
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

/// Mount an SSE upstream that returns `body` verbatim and then closes — no
/// trailing `[DONE]` unless the fixture has one.
async fn mount_sse(upstream: &MockServer, path: &str, body: &'static str) {
    Mock::given(method("POST"))
        .and(path_matcher(path))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(upstream)
        .await;
}

async fn post_streaming(
    state: AppState,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    openproxy::build_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn sse_text(response: axum::response::Response) -> String {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert_eq!(status, StatusCode::OK, "body was:\n{text}");
    text
}

/// A Responses stream that produced output and then simply stopped: the last
/// frame is `response.output_item.done`, with no `response.completed` after it.
const RESPONSES_STREAM_WITHOUT_TERMINAL: &str = concat!(
    "event: response.created\n",
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_trunc\",\"created_at\":1712345678}}\n\n",
    "event: response.output_item.done\n",
    "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"partial answer\"}],\"role\":\"assistant\"}}\n\n",
);

/// The same stream, properly terminated.
const RESPONSES_STREAM_WITH_TERMINAL: &str = concat!(
    "event: response.created\n",
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_done\",\"created_at\":1712345678}}\n\n",
    "event: response.output_item.done\n",
    "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"final answer\"}],\"role\":\"assistant\"}}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_done\",\"status\":\"completed\",\"usage\":{\"input_tokens\":11,\"output_tokens\":3,\"total_tokens\":14}}}\n\n",
);

/// A provider whose TARGET format is Responses, so a Responses-shaped client
/// request stays a Responses passthrough end to end
/// (`get_target_format_for_provider`, registry.rs:370).
const RESPONSES_PROVIDER: &str = "perplexity-agent";

async fn responses_app(upstream: &MockServer) -> AppState {
    let mut creds = connection(
        "node-responses",
        RESPONSES_PROVIDER,
        "upstream-key",
        "gpt-4.1",
    );
    creds.provider_specific_data.insert(
        "apiType".into(),
        serde_json::Value::String("responses".into()),
    );
    seeded_state(
        vec![provider_node(
            RESPONSES_PROVIDER,
            "custom",
            "responses",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![creds],
    )
    .await
}

fn responses_request() -> serde_json::Value {
    json!({
        "model": "custom/gpt-4.1",
        "input": [{"role": "user", "content": "hi"}],
        "stream": true,
    })
}

/// A Responses request needs the client to speak Responses too, so it goes to
/// `/v1/responses` — which is what makes the plan a Responses passthrough.
const RESPONSES_ENDPOINT: &str = "/v1/responses";

/// A long, RTK-recognisable `git log` — the shape the token saver actually
/// compacts. A run of filler characters is not compressed by anything, so a
/// fixture made of them would make this test pass for the wrong reason.
fn tool_output_fixture() -> String {
    let mut out = String::from("commit 0123456789abcdef0123456789abcdef01234567\nAuthor: Dev <dev@example.com>\nDate:   Mon Jan 1 00:00:00 2024 +0000\n\n    a change nobody needs at length\n\n");
    for i in 0..200 {
        out.push_str(&format!(
            "commit {i:040x}\nAuthor: Dev <dev@example.com>\nDate:   Mon Jan 1 00:00:00 2024 +0000\n\n    another change\n\n"
        ));
    }
    assert!(out.len() > 20_000);
    out
}

/// A Chat Completions upstream behind a custom node.
async fn chat_app(upstream: &MockServer, node: &str, prefix: &str, model: &str) -> AppState {
    seeded_state(
        vec![provider_node(
            node,
            prefix,
            "chat",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection(node, node, "upstream-key", model)],
    )
    .await
}

fn chat_request(model: &str) -> serde_json::Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": true,
    })
}

/// The last recorded usage row, which is the one this request just wrote.
fn last_usage(state: &AppState) -> openproxy::types::UsageEntry {
    state
        .usage_tracker()
        .get_usage_db()
        .history
        .last()
        .cloned()
        .expect("the request recorded a usage row")
}

/// The headline defect: a Responses passthrough that closes without a terminal
/// event reaches the client with no terminal at all, so the client waits on an
/// event that is never coming.
#[tokio::test]
async fn a_responses_passthrough_that_ends_without_a_terminal_gets_response_failed_then_done() {
    let upstream = MockServer::start().await;
    mount_sse(
        &upstream,
        "/v1/responses",
        RESPONSES_STREAM_WITHOUT_TERMINAL,
    )
    .await;

    let text = sse_text(
        post_streaming(
            responses_app(&upstream).await,
            RESPONSES_ENDPOINT,
            responses_request(),
        )
        .await,
    )
    .await;

    // The upstream frames are relayed, not swallowed.
    assert!(text.contains("partial answer"), "{text}");

    let failure_at = text
        .find("event: response.failed")
        .unwrap_or_else(|| panic!("no response.failed was synthesized:\n{text}"));
    let done_at = text
        .find("data: [DONE]")
        .unwrap_or_else(|| panic!("no terminator:\n{text}"));
    assert!(
        failure_at < done_at,
        "the failure frame must precede [DONE]:\n{text}"
    );

    // And the failure carries 9router's structured error, not a bare string.
    let data = text[failure_at..]
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .expect("the failure frame carries data");
    let payload: serde_json::Value = serde_json::from_str(data).expect("data is JSON");
    assert_eq!(payload["type"], "response.failed");
    assert_eq!(payload["response"]["status"], "failed");
    assert_eq!(payload["response"]["error"]["code"], "stream_disconnected");
    assert_eq!(
        payload["response"]["error"]["message"],
        "stream closed before response.completed"
    );
}

/// A stream that reached `response.completed` must not be handed a
/// contradiction afterwards.
#[tokio::test]
async fn a_responses_passthrough_that_saw_a_terminal_is_not_given_a_second_one() {
    let upstream = MockServer::start().await;
    mount_sse(&upstream, "/v1/responses", RESPONSES_STREAM_WITH_TERMINAL).await;

    let text = sse_text(
        post_streaming(
            responses_app(&upstream).await,
            RESPONSES_ENDPOINT,
            responses_request(),
        )
        .await,
    )
    .await;

    assert!(text.contains("final answer"), "{text}");
    assert_eq!(
        text.matches("event: response.failed").count(),
        0,
        "a completed stream must not be failed afterwards:\n{text}"
    );
    assert!(text.contains("data: [DONE]"), "{text}");
}

/// The synthesis is Responses-only, and — the finding that started this — no
/// format gets a synthetic `data: {"error":…}` frame injected into a stream
/// that has already produced output.
#[tokio::test]
async fn a_chat_stream_that_ends_early_is_not_given_a_synthesized_terminal() {
    let upstream = MockServer::start().await;
    mount_sse(
        &upstream,
        "/v1/chat/completions",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
    )
    .await;

    let state = seeded_state(
        vec![provider_node(
            "node-chat",
            "chat",
            "chat",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection(
            "node-chat",
            "node-chat",
            "upstream-key",
            "gpt-4o-mini",
        )],
    )
    .await;

    let text = sse_text(
        post_streaming(
            state,
            "/v1/chat/completions",
            json!({
                "model": "chat/gpt-4o-mini",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true,
            }),
        )
        .await,
    )
    .await;

    assert!(text.contains("partial"), "{text}");
    assert!(
        !text.contains("response.failed"),
        "a chat stream must never see a Responses terminal:\n{text}"
    );
    assert!(
        !text.contains(r#""type":"server_error""#),
        "no synthetic error frame may be injected mid-stream:\n{text}"
    );
}

/// 9router keeps `X-Accel-Buffering` in the internal nginx-facing variant and
/// puts a CORS origin in the client-facing one. The streamed body used to get
/// the no-buffer set and no CORS at all.
#[tokio::test]
async fn the_streamed_body_carries_the_client_facing_sse_headers() {
    let upstream = MockServer::start().await;
    mount_sse(
        &upstream,
        "/v1/chat/completions",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
    )
    .await;

    let state = seeded_state(
        vec![provider_node(
            "node-hdr",
            "hdr",
            "chat",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection(
            "node-hdr",
            "node-hdr",
            "upstream-key",
            "gpt-4o-mini",
        )],
    )
    .await;

    let response = post_streaming(
        state,
        "/v1/chat/completions",
        json!({
            "model": "hdr/gpt-4o-mini",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;

    let headers = response.headers();
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    assert_eq!(get("content-type").as_deref(), Some("text/event-stream"));
    assert_eq!(get("cache-control").as_deref(), Some("no-cache"));
    assert_eq!(get("connection").as_deref(), Some("keep-alive"));
    assert_eq!(
        get("access-control-allow-origin").as_deref(),
        Some("*"),
        "a browser client talking to the proxy directly needs the origin"
    );
    assert_eq!(
        get("x-accel-buffering"),
        None,
        "9router reserves X-Accel-Buffering for the internal nginx variant"
    );
}

// ---------------------------------------------------------------------------
// Usage accounting at end of stream
// ---------------------------------------------------------------------------

/// A stream that produced text but never sent a usage frame used to record
/// `tokens = None` and zero cost, which then reads as a genuinely free request
/// against the per-key monthly budget. The estimate is marked so the dashboard
/// can tell a guess from a measurement.
#[tokio::test]
async fn a_stream_with_content_and_no_usage_frame_still_records_tokens() {
    let upstream = MockServer::start().await;
    mount_sse(
        &upstream,
        "/v1/chat/completions",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a reasonably long answer that no usage frame ever accounts for\"}}]}\n\ndata: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" and more\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )
    .await;

    let state = chat_app(&upstream, "node-usage", "usage", "gpt-4o-mini").await;
    sse_text(
        post_streaming(
            state.clone(),
            "/v1/chat/completions",
            chat_request("usage/gpt-4o-mini"),
        )
        .await,
    )
    .await;

    let entry = last_usage(&state);
    let tokens = entry.tokens.expect("tokens must be recorded, not None");
    assert!(
        tokens.completion_tokens.unwrap_or(0) > 0,
        "a stream with content must not record zero output tokens"
    );
    assert!(
        tokens.prompt_tokens.unwrap_or(0) > 0,
        "the request body is known, so the prompt is estimable"
    );
    assert_eq!(
        tokens.extra.get("estimated"),
        Some(&json!(true)),
        "an estimate must be marked as one"
    );
}

/// The provider's own numbers are ground truth and must survive untouched.
#[tokio::test]
async fn a_stream_with_a_real_usage_frame_is_recorded_verbatim() {
    let upstream = MockServer::start().await;
    mount_sse(
        &upstream,
        "/v1/chat/completions",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":41,\"completion_tokens\":7,\"total_tokens\":48}}\n\ndata: [DONE]\n\n",
    )
    .await;

    let state = chat_app(&upstream, "node-realusage", "realusage", "gpt-4o-mini").await;
    sse_text(
        post_streaming(
            state.clone(),
            "/v1/chat/completions",
            chat_request("realusage/gpt-4o-mini"),
        )
        .await,
    )
    .await;

    let entry = last_usage(&state);
    let tokens = entry.tokens.expect("tokens must be recorded");
    assert_eq!(tokens.prompt_tokens, Some(41));
    assert_eq!(tokens.completion_tokens, Some(7));
    assert_eq!(tokens.total_tokens, Some(48));
    assert!(
        !tokens.extra.contains_key("estimated"),
        "a measured usage frame is not an estimate"
    );
}

// ---------------------------------------------------------------------------
// TTS requests must not pay for content the pipeline deletes
// ---------------------------------------------------------------------------

/// 9router strips `role: "tool"` messages and the `tools` array from a TTS
/// request BEFORE any token saver runs. Running the savers first recorded
/// non-zero `bytes_saved` for content that was then thrown away — and billed
/// the compression cost for it.
#[tokio::test]
async fn a_tts_request_strips_tool_content_before_the_savers_see_it() {
    let upstream = MockServer::start().await;
    mount_sse(
        &upstream,
        "/v1/chat/completions",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tts-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
    )
    .await;

    let state = chat_app(&upstream, "node-tts", "ttsnode", "tts-1").await;
    state
        .db
        .update(|db| {
            db.settings.rtk_enabled = true;
        })
        .await
        .unwrap();

    // RTK autodetects the SHAPE of tool output before compressing it
    // (autodetect.rs:46), so the fixture has to be something it recognises.
    let long_tool_output = tool_output_fixture();
    let text = sse_text(post_streaming(
        state.clone(),
        "/v1/chat/completions",
        json!({
            "model": "ttsnode/tts-1",
            "messages": [
                {"role": "user", "content": "read this"},
                {"role": "tool", "tool_call_id": "call_1", "content": long_tool_output}
            ],
            "tools": [{
                "type": "function",
                "function": {"name": "read", "description": "reads", "parameters": {"type": "object"}}
            }],
            "stream": true,
        }),
    )
    .await)
    .await;
    assert!(text.contains("ok"), "{text}");

    let entry = last_usage(&state);
    assert_eq!(
        entry.bytes_saved, 0,
        "TTS requests must record no compression — the content is deleted"
    );
    assert_eq!(entry.bytes_before, 0);
    assert_eq!(entry.bytes_after, 0);
}

/// The move cannot be "fixed" by switching RTK off for everyone: a non-TTS
/// request with the same body still compresses.
#[tokio::test]
async fn a_non_tts_request_with_the_same_body_still_records_compression() {
    let upstream = MockServer::start().await;
    mount_sse(
        &upstream,
        "/v1/chat/completions",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
    )
    .await;

    let state = chat_app(&upstream, "node-nontts", "nontts", "gpt-4o-mini").await;
    state
        .db
        .update(|db| {
            db.settings.rtk_enabled = true;
        })
        .await
        .unwrap();

    let long_tool_output = tool_output_fixture();
    let text = sse_text(post_streaming(
        state.clone(),
        "/v1/chat/completions",
        json!({
            "model": "nontts/gpt-4o-mini",
            "messages": [
                {"role": "user", "content": "read this"},
                {"role": "tool", "tool_call_id": "call_1", "content": long_tool_output}
            ],
            "tools": [{
                "type": "function",
                "function": {"name": "read", "description": "reads", "parameters": {"type": "object"}}
            }],
            "stream": true,
        }),
    )
    .await)
    .await;
    assert!(text.contains("ok"), "{text}");

    let entry = last_usage(&state);
    assert!(
        entry.bytes_before > entry.bytes_after,
        "a non-TTS request must still be compressed: {entry:?}"
    );
    assert!(entry.bytes_saved > 0, "{entry:?}");
}
