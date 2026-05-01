use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::core::rtk::CompressionLevel;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, Combo, ProviderConnection, ProviderNode, Settings};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_partial_json, header, method, path};
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
    }
}

fn provider_node(id: &str, prefix: &str, base_url: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: "Compatible".into(),
        prefix: Some(prefix.into()),
        api_type: Some("chat".into()),
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
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: Some("gpt-4o-mini".into()),
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
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

async fn seeded_state(
    nodes: Vec<ProviderNode>,
    connections: Vec<ProviderConnection>,
    combos: Vec<Combo>,
) -> AppState {
    seeded_state_with_settings(nodes, connections, combos, Settings::default()).await
}

async fn seeded_state_with_settings(
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

#[tokio::test]
async fn chat_completions_streams_openai_compatible_response() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "stream": true,
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            ),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
        Vec::new(),
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
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
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("data: {\"choices\""));
    assert!(text.contains("data: [DONE]"));
}

#[tokio::test]
async fn chat_completions_injects_caveman_prompt_for_long_requests() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-caveman",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let settings = Settings {
        caveman_enabled: true,
        caveman_level: "ultra".into(),
        ..Settings::default()
    };
    let state = seeded_state_with_settings(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
        Vec::new(),
        settings,
    )
    .await;
    let long_prompt = "Need concise summary of massive transcript. ".repeat(220);

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [
                            { "role": "system", "content": "Existing rules" },
                            { "role": "user", "content": long_prompt }
                        ],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = upstream
        .received_requests()
        .await
        .expect("received requests");
    assert_eq!(requests.len(), 1);

    let forwarded: serde_json::Value = requests[0].body_json().expect("forwarded body");
    assert_eq!(forwarded["model"], "gpt-4o-mini");
    let messages = forwarded["messages"].as_array().expect("messages array");
    let system = messages[0]["content"].as_str().expect("system content");
    assert!(system.starts_with("Existing rules"));
    assert!(system.contains(CompressionLevel::Ultra.prompt()));
}

#[tokio::test]
async fn chat_completions_skips_caveman_prompt_for_short_requests() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-short",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let settings = Settings {
        caveman_enabled: true,
        caveman_level: "lite".into(),
        ..Settings::default()
    };
    let state = seeded_state_with_settings(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
        Vec::new(),
        settings,
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [
                            { "role": "user", "content": "hi" }
                        ],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = upstream
        .received_requests()
        .await
        .expect("received requests");
    assert_eq!(requests.len(), 1);

    let forwarded: serde_json::Value = requests[0].body_json().expect("forwarded body");
    let messages = forwarded["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "hi");
}

#[tokio::test]
async fn chat_completions_preserves_chat_content_part_schema_when_injecting_caveman() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-parts",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let settings = Settings {
        caveman_enabled: true,
        caveman_level: "full".into(),
        ..Settings::default()
    };
    let state = seeded_state_with_settings(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
        Vec::new(),
        settings,
    )
    .await;
    let long_prompt = "Need concise summary of massive transcript. ".repeat(220);

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [
                            {
                                "role": "developer",
                                "content": [{ "type": "text", "text": "Keep exact schema" }]
                            },
                            { "role": "user", "content": long_prompt }
                        ],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = upstream
        .received_requests()
        .await
        .expect("received requests");
    assert_eq!(requests.len(), 1);

    let forwarded: serde_json::Value = requests[0].body_json().expect("forwarded body");
    let parts = forwarded["messages"][0]["content"]
        .as_array()
        .expect("developer content parts");
    assert_eq!(
        parts[0],
        json!({ "type": "text", "text": "Keep exact schema" })
    );
    assert_eq!(
        parts.last().expect("last developer part"),
        &json!({ "type": "text", "text": CompressionLevel::Full.prompt() })
    );
}

#[tokio::test]
async fn chat_completions_falls_back_to_next_account_on_retryable_error() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer bad-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": { "message": "rate limit exceeded" }
        })))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer good-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-success",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![
            connection("conn-bad", "node-openai", 1, "bad-key"),
            connection("conn-good", "node-openai", 2, "good-key"),
        ],
        Vec::new(),
    )
    .await;

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
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
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "chatcmpl-success");

    let snapshot = state.db.snapshot();
    let first = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == "conn-bad")
        .unwrap();
    assert!(first.extra.contains_key("modelLock_gpt-4o-mini"));
    assert_eq!(first.error_code.as_deref(), Some("429"));
}

#[tokio::test]
async fn chat_completions_uses_combo_fallback_across_models() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({ "model": "gpt-fail" })))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": { "message": "temporary upstream issue" }
        })))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({ "model": "gpt-pass" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-combo",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "combo ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let combo = Combo {
        id: "combo-1".into(),
        name: "writer".into(),
        models: vec!["custom/gpt-fail".into(), "custom/gpt-pass".into()],
        kind: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };
    let mut combo_connection = connection("conn-1", "node-openai", 1, "upstream-key");
    combo_connection.default_model = None;
    combo_connection
        .provider_specific_data
        .insert("enabledModels".into(), json!(["gpt-fail", "gpt-pass"]));

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![combo_connection],
        vec![combo],
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "writer",
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
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "chatcmpl-combo");
}

#[tokio::test]
async fn chat_completions_skips_accounts_that_do_not_advertise_requested_model() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer good-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-model-aware",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let mut unsupported = connection("conn-unsupported", "node-openai", 1, "bad-key");
    unsupported.default_model = None;
    unsupported
        .provider_specific_data
        .insert("enabledModels".into(), json!(["gpt-other"]));

    let mut supported = connection("conn-supported", "node-openai", 2, "good-key");
    supported.default_model = None;
    supported
        .provider_specific_data
        .insert("enabledModels".into(), json!(["gpt-4o-mini"]));

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![unsupported, supported],
        Vec::new(),
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
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
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "chatcmpl-model-aware");
}

#[tokio::test]
async fn chat_completions_returns_retry_after_while_model_is_cooling_down() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "120")
                .set_body_json(json!({
                    "error": { "message": "rate limit exceeded" }
                })),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
        Vec::new(),
    )
    .await;

    let request_body = json!({
        "model": "custom/gpt-4o-mini",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": false,
    })
    .to_string();

    let app = openproxy::build_app(state.clone());
    let first = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(request_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    let first_retry_after: i64 = first
        .headers()
        .get("retry-after")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(first_retry_after >= 100);

    let app = openproxy::build_app(state);
    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(request_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
    let second_retry_after: i64 = second
        .headers()
        .get("retry-after")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(second_retry_after >= 100);
    let body = axum::body::to_bytes(second.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cooling down"));
}

#[tokio::test]
async fn chat_completions_does_not_cool_down_entire_connection_for_model_specific_404() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({ "model": "gpt-missing" })))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": { "message": "model not found" }
        })))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({ "model": "gpt-ok" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-ok",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let mut connection = connection("conn-1", "node-openai", 1, "upstream-key");
    connection.default_model = None;
    connection
        .provider_specific_data
        .insert("enabledModels".into(), json!(["gpt-missing", "gpt-ok"]));

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection],
        Vec::new(),
    )
    .await;

    let app = openproxy::build_app(state.clone());
    let missing = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-missing",
                        "messages": [{ "role": "user", "content": "hi" }],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let snapshot = state.db.snapshot();
    let stored = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == "conn-1")
        .expect("stored connection");
    assert!(stored.extra.contains_key("modelLock_gpt-missing"));
    assert!(stored.rate_limited_until.is_none());

    let app = openproxy::build_app(state);
    let ok = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-ok",
                        "messages": [{ "role": "user", "content": "hi again" }],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn chat_completions_supports_enabled_models_with_nested_slashes() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(
            json!({ "model": "meta-llama/llama-3.3-70b" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-nested-model",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "nested ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let mut connection = connection("conn-1", "node-openai", 1, "upstream-key");
    connection.default_model = None;
    connection
        .provider_specific_data
        .insert("enabledModels".into(), json!(["meta-llama/llama-3.3-70b"]));

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection],
        Vec::new(),
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/meta-llama/llama-3.3-70b",
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
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "chatcmpl-nested-model");
}

#[tokio::test]
async fn chat_completions_preserves_earliest_retry_after_when_all_accounts_fail() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer first-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "120")
                .set_body_json(json!({
                    "error": { "message": "rate limit exceeded" }
                })),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer second-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": { "message": "temporary upstream issue" }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![
            connection("conn-1", "node-openai", 1, "first-key"),
            connection("conn-2", "node-openai", 2, "second-key"),
        ],
        Vec::new(),
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
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
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let retry_after: i64 = response
        .headers()
        .get("retry-after")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((20..=40).contains(&retry_after));
}

#[tokio::test]
async fn chat_completions_rejects_connections_without_credentials() {
    let upstream = MockServer::start().await;

    let mut missing_credentials = connection("conn-1", "node-openai", 1, "unused-key");
    missing_credentials.api_key = None;
    missing_credentials.default_model = Some("gpt-4o-mini".into());

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![missing_credentials],
        Vec::new(),
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
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
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("No credentials"));
    assert!(upstream.received_requests().await.unwrap().is_empty());
}
