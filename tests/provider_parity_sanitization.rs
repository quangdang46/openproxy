use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProviderNode};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROVIDER: &str = "o3test";
const MODEL: &str = "gpt-4o-mini";

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

fn provider_node(prefix: &str, base_url: &str) -> ProviderNode {
    ProviderNode {
        id: prefix.into(),
        r#type: "openai-compatible".into(),
        name: prefix.to_string(),
        prefix: Some(prefix.into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url.into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn connection(provider: &str, api_key: &str) -> ProviderConnection {
    ProviderConnection {
        id: provider.to_string(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        name: Some(provider.into()),
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
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn seeded_openai_state(upstream: &MockServer) -> AppState {
    let base = format!("{}/v1", upstream.uri());
    seeded_state(
        vec![provider_node(PROVIDER, &base)],
        vec![connection(PROVIDER, "upstream-key")],
    )
    .await
}

#[tokio::test]
async fn input_sanitization_strips_control_chars_before_upstream() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "messages": [{
                "role": "user",
                "content": "Clean prompt with tab\there and newline\nhere"
            }]
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                "data: {\"choices\":[{\"delta\":{\"content\":\"sanitized\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":9,\"total_tokens\":10}}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            ),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let app = openproxy::build_app(seeded_openai_state(&upstream).await);

    let dirty = "Clean prompt with tab\there and newline\nhere\u{0000}\u{007F}";
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/api/chat")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": format!("{PROVIDER}/{MODEL}"),
                        "stream": true,
                        "messages": [{
                            "role": "user",
                            "content": dirty
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
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("sanitized"),
        "upstream content reached client: {text}"
    );
    assert!(text.contains("data: [DONE]"), "streaming finished cleanly");
}

#[tokio::test]
async fn input_sanitization_preserves_code_indentation_and_newlines() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "messages": [{
                "role": "user",
                "content": "fn main() {\n    println!(\"hi\");\n}"
            }]
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                "data: {\"choices\":[{\"delta\":{\"content\":\"indented-ok\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            ),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let app = openproxy::build_app(seeded_openai_state(&upstream).await);

    let code_block = "fn main() {\n    println!(\"hi\");\n}";
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/api/chat")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": format!("{PROVIDER}/{MODEL}"),
                        "stream": true,
                        "messages": [{
                            "role": "user",
                            "content": code_block
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
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("indented-ok"),
        "upstream content reached client: {text}"
    );
}

#[tokio::test]
async fn input_sanitization_noop_for_clean_payload() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "stream": false,
            "messages": [{
                "role": "system",
                "content": "You are a helpful assistant."
            }, {
                "role": "user",
                "content": "Hello, world!"
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-clean",
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "Hello!" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let app = openproxy::build_app(seeded_openai_state(&upstream).await);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/api/chat")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": format!("{PROVIDER}/{MODEL}"),
                        "stream": false,
                        "messages": [{
                            "role": "system",
                            "content": "You are a helpful assistant."
                        }, {
                            "role": "user",
                            "content": "Hello, world!"
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
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("Hello!"),
        "clean payload passed through: {text}"
    );
}

#[tokio::test]
async fn response_sanitization_strips_breaking_fields() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "messages": [{
                "role": "user",
                "content": "strip breaking fields"
            }]
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                "data: {\"choices\":[{\"delta\":{\"content\":\"sanitized-response\"},\"finish_reason\":null}],\"service_tier\":\"priority\",\"x_groq\":{\"usage_tree\":true}}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":6,\"total_tokens\":16,\"usage_breakdown\":{\"wasted\":100}}}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            ),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let app = openproxy::build_app(seeded_openai_state(&upstream).await);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/api/chat")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": format!("{PROVIDER}/{MODEL}"),
                        "stream": true,
                        "messages": [{
                            "role": "user",
                            "content": "strip breaking fields"
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
    let text = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        text.contains("sanitized-response"),
        "core content survived sanitization: {text}"
    );
    assert!(
        text.contains("data: [DONE]"),
        "streaming terminated cleanly: {text}"
    );
    assert!(
        !text.contains("service_tier"),
        "service_tier should be stripped from streamed response: {text}"
    );
    assert!(
        !text.contains("usage_breakdown"),
        "usage.usage_breakdown should be stripped: {text}"
    );
    assert!(
        !text.contains("x_groq"),
        "x_groq.* should be stripped from streamed response: {text}"
    );
}
