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
async fn compat_count_tokens_matches_js_estimate_and_sets_cors_headers() {
    let app = openproxy::build_app(seeded_state(Vec::new(), Vec::new()).await);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages/count_tokens")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "messages": [
                            { "role": "user", "content": "abcd" },
                            { "role": "assistant", "content": [{ "type": "text", "text": "efghij" }] }
                        ]
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
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-methods")
            .unwrap(),
        "POST, OPTIONS"
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["input_tokens"], 3);
}

#[tokio::test]
async fn messages_route_promotes_system_field_before_forwarding() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "stream": false,
            "messages": [
                { "role": "system", "content": "Be terse" },
                { "role": "user", "content": "Ping" }
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Pong"
                },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "compat",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", "upstream-key")],
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "compat/gpt-4o-mini",
                        "system": "Be terse",
                        "messages": [{ "role": "user", "content": "Ping" }],
                        "stream": false
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
}

#[tokio::test]
async fn responses_compact_normalizes_input_and_sets_compact_flag() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "stream": false,
            "max_tokens": 32,
            "_compact": true,
            "messages": [
                { "role": "system", "content": "Be terse" },
                { "role": "user", "content": "Ping" }
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-2",
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Compact Pong"
                },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "compat",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", "upstream-key")],
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "compat/gpt-4o-mini",
                        "instructions": "Be terse",
                        "input": "Ping",
                        "max_output_tokens": 32,
                        "stream": false
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
}
