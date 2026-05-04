use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProviderNode};
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

fn connection(id: &str, provider: &str, api_key: &str) -> ProviderConnection {
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
        proxy_url: None,
        proxy_label: None,
        use_connection_proxy: None,
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
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn v1_api_chat_options_exposes_cors_headers() {
    let app = openproxy::build_app(seeded_state(Vec::new(), Vec::new()).await);

    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/api/chat")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "*"
    );
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-methods")
            .unwrap(),
        "GET, POST, OPTIONS"
    );
}

#[tokio::test]
async fn v1_api_chat_defaults_to_streaming_and_returns_ollama_ndjson() {
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
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            ),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let app = openproxy::build_app(
        seeded_state(
            vec![provider_node(
                "node-openai",
                "custom",
                &format!("{}/v1", upstream.uri()),
            )],
            vec![connection("conn-1", "node-openai", "upstream-key")],
        )
        .await,
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/api/chat")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [{
                            "role": "user",
                            "content": "Ping"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "application/x-ndjson"
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    let lines = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();

    assert_eq!(lines.len(), 2);
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["message"]["content"], "hello");
    assert_eq!(first["done"], false);

    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["done"], true);
    assert_eq!(second["prompt_eval_count"], 1);
    assert_eq!(second["eval_count"], 2);
}

#[tokio::test]
async fn v1_api_chat_non_streaming_converts_openai_json_to_ollama_json() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "stream": false,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "pong"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 3,
                "completion_tokens": 2,
                "total_tokens": 5
            }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let app = openproxy::build_app(
        seeded_state(
            vec![provider_node(
                "node-openai",
                "custom",
                &format!("{}/v1", upstream.uri()),
            )],
            vec![connection("conn-1", "node-openai", "upstream-key")],
        )
        .await,
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/api/chat")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "stream": false,
                        "messages": [{
                            "role": "user",
                            "content": "Ping"
                        }]
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
    assert_eq!(json["model"], "gpt-4o-mini");
    assert_eq!(json["message"]["role"], "assistant");
    assert_eq!(json["message"]["content"], "pong");
    assert_eq!(json["done"], true);
    assert_eq!(json["done_reason"], "stop");
    assert_eq!(json["prompt_eval_count"], 3);
    assert_eq!(json["eval_count"], 2);
}
