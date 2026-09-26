//! Bead openproxy-4inl — findings a previous pass reported as living outside
//! the footprint that owned them, re-entered and landed here.
//!
//! Each test drives the real router (or the real public entry point the
//! handler exposes) rather than re-calling the helper in isolation: the
//! request-body threading bug, for one, was invisible to every helper-level
//! test because the tests supplied the body by hand while production never did.
//!
//! Covered here:
//!   1. The chat SSE path hands the request body to the response translators,
//!      so a stream with no `usage` block still meters its input half.
//!   2. `track_request` writes no history row for an all-zero usage pair, the
//!      way 9router's `saveUsageStats` returns early.
//!   3. `/v1/web/fetch` honours `comboStickyRoundRobinLimit` on the combo path.
//!   4. `/v1/search` expands a combo name instead of reading it as a provider.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use openproxy::core::usage::{CompressionStats, UsageTracker};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{
    ApiKey, Combo, ComboStrategyConfig, ProviderConnection, ProviderNode, Settings, TokenUsage,
};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

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

fn combo(name: &str, models: &[&str]) -> Combo {
    Combo {
        id: format!("{name}-id"),
        name: name.to_string(),
        models: models.iter().map(|value| value.to_string()).collect(),
        disabled_models: Vec::new(),
        kind: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

async fn seeded_state(
    nodes: Vec<ProviderNode>,
    connections: Vec<ProviderConnection>,
    combos: Vec<Combo>,
    settings: Settings,
) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.provider_nodes = nodes;
        state.provider_connections = connections;
        state.combos = combos;
        state.settings = settings;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn open_settings() -> Settings {
    Settings {
        require_login: false,
        ..Settings::default()
    }
}

async fn post(state: AppState, uri: &str, body: Value) -> axum::response::Response {
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

async fn response_text(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn response_json(response: axum::response::Response) -> Value {
    serde_json::from_str(&response_text(response).await).unwrap_or(Value::Null)
}

fn tokens(prompt: u64, completion: u64) -> TokenUsage {
    TokenUsage {
        prompt_tokens: Some(prompt),
        input_tokens: None,
        completion_tokens: Some(completion),
        output_tokens: None,
        total_tokens: Some(prompt + completion),
        reasoning_tokens: None,
        cached_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        extra: BTreeMap::new(),
    }
}

// ─── 1. The chat SSE path threads the request body into the translators ─────

/// `ResponseTransformState::request_body` is the ONLY source the response
/// translators have for the input half of an estimated usage block:
/// `usage::estimate_input_tokens(Some(body))` is what
/// `terminal_usage_block` calls, and a `None` body leaves the input estimate
/// at 0 while the output half still estimates correctly. The field and its
/// readers landed together, but nothing in production ever wrote it — so every
/// stream metered 0 input tokens.
///
/// The population has to sit in chat.rs, at each of the two `t_state` sites the
/// two stream arms build (`UpstreamResponse::Reqwest` and `::Hyper`), or the
/// translators see `None`. That is a source-level contract rather than a
/// behavioural one: the client-visible usage only diverges on a request whose
/// response needs FORMAT TRANSLATION, and this router currently returns an
/// empty client stream for every translated-SSE shape tried here (see the bead
/// report) while the passthrough shape streams fine — so no end-to-end
/// assertion of the divergence is available yet.
///
/// This cannot match its own source: the needles live in `src/server/api/chat.rs`,
/// the assertion lives in this file.
#[test]
fn both_stream_arms_seed_the_request_body_into_the_transform_state() {
    let chat_rs = include_str!("../src/server/api/chat.rs");
    for (needle, arm) in [
        (
            "s.request_body = Some(request_body.clone());",
            "Reqwest stream arm",
        ),
        (
            "s.request_body = Some(request_body2.clone());",
            "Hyper stream arm",
        ),
    ] {
        assert_eq!(
            chat_rs.matches(needle).count(),
            1,
            "the {arm} must seed request_body exactly once, so every translated \
             stream can size the input half of its estimated usage"
        );
    }
    // Both arms must build the state under the same guard, or a passthrough
    // stream would pay for a clone it never reads.
    assert_eq!(
        chat_rs
            .matches("let mut t_state = if needs_stream_translation {")
            .count(),
        2,
        "expected one t_state per stream arm"
    );
}

/// The estimator really does read the body — the counterpart the wiring above
/// feeds. A `None` body is what production was handing it.
#[test]
fn the_terminal_usage_estimate_is_input_less_without_a_request_body() {
    use openproxy::core::translator::registry::ResponseTransformState;
    use openproxy::core::translator::response::claude_to_openai::claude_to_openai_streaming;
    use openproxy::core::translator::response::usage::{estimate_input_tokens, BUFFER_TOKENS};

    let terminal = |request_body: Option<Value>| {
        let mut state = ResponseTransformState {
            request_body,
            ..Default::default()
        };
        claude_to_openai_streaming(CLAUDE_STREAM_WITHOUT_USAGE.as_bytes(), &mut state)
            .last()
            .and_then(|line| {
                line.trim()
                    .strip_prefix("data: ")
                    .and_then(|p| serde_json::from_str::<Value>(p).ok())
            })
            .map(|chunk| chunk["usage"].clone())
            .unwrap_or(Value::Null)
    };

    let request = json!({"messages": [{"role": "user", "content": "hi"}]});
    let with_body = terminal(Some(request.clone()));
    let without_body = terminal(None);

    assert_eq!(
        with_body["completion_tokens"], 11,
        "the output half never depends on the request body"
    );
    assert_eq!(with_body["estimated"], true);
    // Without a body the block is the bare +2000 head-room 9router pads every
    // client-visible usage with — the whole input estimate is missing, so the
    // request meters as ~free no matter how large the prompt was.
    assert_eq!(without_body["prompt_tokens"], BUFFER_TOKENS);
    assert_eq!(
        with_body["prompt_tokens"].as_u64().unwrap(),
        estimate_input_tokens(Some(&request)) + BUFFER_TOKENS
    );
}

/// A Claude-shaped stream with no `usage` block — the shape every provider
/// emits unless the client asked for `stream_options.include_usage`.
const CLAUDE_STREAM_WITHOUT_USAGE: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_up\",\"model\":\"claude-sonnet-4\",\"content\":[]}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"The quick brown fox jumps over the lazy dog.\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

// ─── 2. Zero-usage responses write no usage row ────────────────────────────

/// 9router `saveUsageStats` (requestDetail.js:105) returns before
/// `saveRequestUsage` when both token counts are 0, so an SSE response that
/// carried no usage leaves no history row and no `total_requests_lifetime`
/// bump. `track_request` priced and pushed unconditionally.
#[tokio::test]
async fn zero_usage_response_writes_no_usage_row() {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    let tracker = UsageTracker::new(db.clone());

    let zero = Some(tokens(0, 0));
    tracker
        .track_request(
            "openai",
            "gpt-4.1",
            zero.as_ref(),
            Some("conn-1"),
            Some("k"),
            Some("/v1/chat/completions"),
            Some(CompressionStats::default()),
        )
        .await;

    let snapshot = db.usage_snapshot();
    assert!(
        snapshot.history.is_empty(),
        "an all-zero usage pair must not be persisted, got {:?}",
        snapshot.history
    );
    assert_eq!(
        snapshot.total_requests_lifetime, 0,
        "a usage-less response must not raise the lifetime counter"
    );

    // A real pair still records, so the guard is not simply "never write".
    let real = Some(tokens(120, 45));
    tracker
        .track_request(
            "openai",
            "gpt-4.1",
            real.as_ref(),
            Some("conn-1"),
            Some("k"),
            Some("/v1/chat/completions"),
            None,
        )
        .await;
    let snapshot = db.usage_snapshot();
    assert_eq!(snapshot.history.len(), 1, "a metered request must record");
    assert_eq!(snapshot.total_requests_lifetime, 1);
}

#[tokio::test]
async fn absent_usage_writes_no_usage_row() {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    let tracker = UsageTracker::new(db.clone());

    // The shape `record_streaming_usage` hands over when the stream carried no
    // usage at all: `None`, not a zeroed struct.
    tracker
        .track_request(
            "anthropic",
            "claude-sonnet-4",
            None,
            Some("conn-1"),
            Some("k"),
            Some("/v1/messages"),
            None,
        )
        .await;

    assert!(db.usage_snapshot().history.is_empty());
}

// ─── 3. /v1/web/fetch honours comboStickyRoundRobinLimit ───────────────────

/// Round-robin rotation pins the index for `sticky_limit` consecutive requests
/// (combo.js:213 → `get_rotated_models`). The fetch path went through
/// `execute_combo_strategy`, which hardcodes 1, so a 2-member fetch combo
/// rotated on every call and the two members alternated.
///
/// Neither member has credentials, so both fail locally with a message naming
/// the member and no request leaves the process. The combo reports its LAST
/// member's failure, which is the second entry of the rotated order — so the
/// observed sequence is B,B,A,A with a sticky limit of 2 and B,A,B,A without.
#[tokio::test]
async fn web_fetch_combo_uses_the_configured_sticky_limit() {
    // A unique name keeps the process-global rotation slot for this test.
    let combo_name = format!("sticky-fetch-{}", uuid::Uuid::new_v4());
    let mut combo_strategies = BTreeMap::new();
    combo_strategies.insert(
        combo_name.clone(),
        openproxy::types::ComboStrategyEntry::Config(ComboStrategyConfig {
            fallback_strategy: Some("round-robin".into()),
            ..ComboStrategyConfig::default()
        }),
    );

    let state = seeded_state(
        Vec::new(),
        Vec::new(),
        vec![combo(&combo_name, &["jina-reader", "firecrawl"])],
        Settings {
            require_login: false,
            combo_strategy: "round-robin".into(),
            combo_strategies,
            combo_sticky_round_robin_limit: 2,
            ..Settings::default()
        },
    )
    .await;

    let mut reported: Vec<String> = Vec::new();
    for _ in 0..4 {
        let response = post(
            state.clone(),
            "/v1/web/fetch",
            json!({"provider": combo_name, "url": "https://example.com", "format": "markdown"}),
        )
        .await;
        // /v1/web/fetch answers a bare `{"error": "..."}` string, not the
        // OpenAI error object the chat surfaces use.
        let body = response_json(response).await;
        reported.push(body["error"].as_str().unwrap_or("").to_string());
    }

    let names: Vec<&str> = reported
        .iter()
        .map(|m| {
            if m.contains("firecrawl") {
                "firecrawl"
            } else if m.contains("jina-reader") {
                "jina-reader"
            } else {
                "other"
            }
        })
        .collect();
    assert_eq!(
        names,
        vec!["firecrawl", "firecrawl", "jina-reader", "jina-reader"],
        "a sticky limit of 2 must pin each member for two requests; got {reported:?}"
    );
}

// ─── 4. /v1/search expands a combo name ────────────────────────────────────

/// 9router search.js:73-86 tests the provider/model string against the combo
/// table before it resolves a provider id. OpenProxy had no such branch, so a
/// combo name fell through `resolve_search_provider` to the `.unwrap_or(
/// "serper")` default and the combo's own members were never dispatched.
#[tokio::test]
async fn search_expands_a_combo_name_instead_of_defaulting_to_serper() {
    // A unique name so no other test's combo table interferes.
    let combo_name = format!("sticky-search-{}", uuid::Uuid::new_v4());
    let state = seeded_state(
        Vec::new(),
        Vec::new(),
        // The single member is deliberately NOT a search provider, so the
        // response names it — proof the combo branch ran. Before the fix this
        // reported "No active credentials found for search provider: serper".
        vec![combo(&combo_name, &["not-a-search-provider"])],
        open_settings(),
    )
    .await;

    let response = post(
        state,
        "/v1/chat/search",
        json!({"model": combo_name, "query": "who won the 2024 world series"}),
    )
    .await;
    let body = response_json(response).await;
    let message = body["error"]["message"].as_str().unwrap_or("").to_string();

    assert!(
        message.contains("not-a-search-provider"),
        "the combo's member must be the one reported, got: {message}"
    );
    assert!(
        !message.contains("search provider: serper"),
        "the combo name was read as a literal provider and defaulted to serper: {message}"
    );
}

/// The regression guard for the other direction: a name that is NOT a combo
/// keeps resolving through the literal provider table, so a plain
/// `model: "exa"` still fans out over the cross-provider failover order.
#[tokio::test]
async fn search_without_a_combo_still_uses_the_literal_provider() {
    let state = seeded_state(Vec::new(), Vec::new(), Vec::new(), open_settings()).await;

    let response = post(
        state,
        "/v1/chat/search",
        json!({"model": "exa", "query": "who won the 2024 world series"}),
    )
    .await;
    let body = response_json(response).await;
    let message = body["error"]["message"].as_str().unwrap_or("").to_string();

    assert!(
        message.contains("No active credentials found for search provider"),
        "an unconfigured literal provider still reports the credential gap, got: {message}"
    );
}
