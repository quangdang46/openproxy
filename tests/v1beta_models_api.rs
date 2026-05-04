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
async fn v1beta_models_options_and_list_are_available() {
    let app = openproxy::build_app(
        seeded_state(
            vec![provider_node("node-openai", "custom", "http://example.invalid/v1")],
            vec![connection("conn-1", "node-openai", "upstream-key")],
        )
        .await,
    );

    let options = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1beta/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(options.status(), StatusCode::OK);
    assert_eq!(
        options
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "*"
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1beta/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["models"][0]["name"], "models/node-openai/gpt-4o-mini");
    assert_eq!(
        json["models"][0]["supportedGenerationMethods"][0],
        "generateContent"
    );
}

#[tokio::test]
async fn v1beta_generate_content_converts_request_and_response() {
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
                .uri("/v1beta/models/custom/gpt-4o-mini:generateContent")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "systemInstruction": { "parts": [{ "text": "Be terse" }] },
                        "contents": [{ "role": "user", "parts": [{ "text": "Ping" }] }]
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
            .get("access-control-allow-origin")
            .unwrap(),
        "*"
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["candidates"][0]["content"]["role"], "model");
    assert_eq!(json["candidates"][0]["content"]["parts"][0]["text"], "pong");
    assert_eq!(json["candidates"][0]["finishReason"], "STOP");
    assert_eq!(json["usageMetadata"]["totalTokenCount"], 5);
}

#[tokio::test]
async fn v1beta_stream_generate_content_converts_sse_chunks() {
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
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\ndata: [DONE]\n\n",
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
                .uri("/v1beta/models/custom/gpt-4o-mini:streamGenerateContent?alt=sse")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "contents": [{ "role": "user", "parts": [{ "text": "Ping" }] }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").unwrap(), "text/event-stream");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("\"candidates\""));
    assert!(text.contains("\"text\":\"hello\""));
    assert!(text.contains("\"finishReason\":\"STOP\""));
    assert!(!text.contains("[DONE]"));
}
