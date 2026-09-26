//! Translated SSE streams must reach the client as content.
//!
//! ## Why this file exists
//!
//! Every other SSE test in this repo either exercises passthrough (client and
//! upstream share a wire format) or covers only the non-streaming JSON path.
//! Translated streaming — the `else if needs_stream_translation` arm in
//! `src/server/api/chat.rs`, which buffers per line through
//! `drain_complete_sse_lines` and re-frames each event — had no client-visible
//! coverage at all.
//!
//! That arm is the primary path for any client whose format differs from the
//! upstream's, and Claude Code posting `/v1/messages` to an OpenAI-compatible
//! provider is one of the four core product surfaces in AGENTS.md. A defect
//! there returns 200 and an empty body: no error, no log, nothing a test that
//! only checks the status code would catch.
//!
//! The three shapes below cover the two directions plus the case where the
//! de-framing buffer is actually load-bearing (a frame split across transport
//! reads), which is where a translation-specific bug would hide.
//!
//! Assertions are on bytes the client can see, never on internal state. A test
//! that re-derives the expectation from the same function it exercises passes
//! whether or not the code works.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, Combo, ProviderConnection, ProviderNode, Settings};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider_node(id: &str, kind: &str, prefix: &str, base_url: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: kind.into(),
        name: "Node".into(),
        prefix: Some(prefix.into()),
        api_type: None,
        base_url: Some(base_url.into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn connection(id: &str, provider: &str, priority: u32, api_key: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        priority: Some(priority),
        is_active: Some(true),
        api_key: Some(api_key.into()),
        ..Default::default()
    }
}

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

async fn seeded_state(nodes: Vec<ProviderNode>, connections: Vec<ProviderConnection>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.provider_nodes = nodes;
        state.provider_connections = connections;
        state.combos = Vec::<Combo>::new();
        // Auth is not what these tests are checking, and the login guard would
        // reject before the translation arm is ever reached.
        let mut settings = Settings::default();
        settings.require_login = false;
        state.settings = settings;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Every `data:` payload in an SSE body, with the `data: ` prefix stripped.
fn sse_payloads(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The concatenated text of every OpenAI `delta.content` across the stream.
fn openai_delta_text(body: &str) -> String {
    sse_payloads(body)
        .iter()
        .filter(|p| *p != "[DONE]")
        .filter_map(|p| serde_json::from_str::<Value>(p).ok())
        .filter_map(|v| {
            v["choices"][0]["delta"]["content"]
                .as_str()
                .map(|s| s.to_string())
        })
        .collect()
}

/// The concatenated text of every Anthropic `content_block_delta` across the
/// stream.
fn anthropic_delta_text(body: &str) -> String {
    sse_payloads(body)
        .iter()
        .filter_map(|p| serde_json::from_str::<Value>(p).ok())
        .filter_map(|v| v["delta"]["text"].as_str().map(|s| s.to_string()))
        .collect()
}

/// An OpenAI client against an Anthropic-compatible upstream: the upstream
/// speaks Anthropic's `content_block_delta` events, and the client must still
/// receive OpenAI `choices[].delta.content` frames carrying the same text.
#[tokio::test]
async fn openai_client_reads_an_anthropic_upstream_stream() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\",\"role\":\"assistant\"}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "anthropic-compatible-1",
            "anthropic-compatible",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection(
            "conn-1",
            "anthropic-compatible-1",
            1,
            "upstream-key",
        )],
    )
    .await;

    let response = openproxy::build_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/claude-sonnet-4",
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": true,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;

    // The defect this file was written for is a 200 with an empty body, so
    // assert the content first and the shape second — a stream that carries no
    // text fails here regardless of how well-formed its frames are.
    assert_eq!(
        openai_delta_text(&body),
        "hello world",
        "translated stream must carry the upstream text to the client, got: {body}"
    );
    assert!(
        body.contains("[DONE]"),
        "an OpenAI stream terminates with [DONE], got: {body}"
    );
}

/// The other direction: a Claude client against an OpenAI-compatible upstream.
#[tokio::test]
async fn claude_client_reads_an_openai_upstream_stream() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello \"}}]}\n\n",
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"world\"}}]}\n\n",
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "openai-compatible-1",
            "openai-compatible",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection(
            "conn-1",
            "openai-compatible-1",
            1,
            "upstream-key",
        )],
    )
    .await;

    let response = openproxy::build_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "max_tokens": 64,
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": true,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;

    assert_eq!(
        anthropic_delta_text(&body),
        "hello world",
        "translated stream must carry the upstream text to the client, got: {body}"
    );
    assert!(
        body.contains("message_stop"),
        "an Anthropic stream terminates with message_stop, got: {body}"
    );
}

/// A frame split across transport reads. The de-framing buffer is the only
/// part of the translation arm that holds state between chunks, so a partial
/// frame is where a translation bug would hide — and it is invisible to a test
/// whose upstream sends every frame in one write.
#[tokio::test]
async fn a_frame_split_across_transport_reads_still_translates() {
    let upstream = MockServer::start().await;
    // The second event is deliberately cut mid-JSON. A de-framer that emits on
    // newline alone, or that flushes a partial buffer, drops or corrupts it.
    let split = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"tex";
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                concat!(
                    "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"m\",\"model\":\"m\",\"role\":\"assistant\"}}}}\n\n",
                    "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n",
                    "{split}",
                    "t\":\"split frame\"}}\n\n",
                    "event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
                    "event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
                ),
                split = split
            ),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "anthropic-compatible-1",
            "anthropic-compatible",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection(
            "conn-1",
            "anthropic-compatible-1",
            1,
            "upstream-key",
        )],
    )
    .await;

    let response = openproxy::build_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/claude-sonnet-4",
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": true,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;

    assert_eq!(
        openai_delta_text(&body),
        "split frame",
        "a frame split mid-JSON must be buffered until complete, got: {body}"
    );
}

/// The same cross-format pair with `stream: false`. If the JSON path returns
/// the client's format while the SSE path does not, the defect is isolated to
/// the streaming arm and not to format resolution.
#[tokio::test]
async fn the_non_streaming_path_translates_the_same_pair() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "m",
            "content": [{"type": "text", "text": "hello world"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 2},
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "anthropic-compatible-1",
            "anthropic-compatible",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection(
            "conn-1",
            "anthropic-compatible-1",
            1,
            "upstream-key",
        )],
    )
    .await;

    let response = openproxy::build_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/claude-sonnet-4",
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    let json: Value = serde_json::from_str(&body).expect("json body");

    assert!(
        json["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|s| s.contains("hello world")),
        "the non-streaming path must translate to the CLIENT's format, got: {body}"
    );
    assert!(
        json.get("choices").is_some() && json.get("content").is_none(),
        "a chat/completions client must receive `choices`, not a bare Anthropic \
         `content` array, got: {body}"
    );
}
