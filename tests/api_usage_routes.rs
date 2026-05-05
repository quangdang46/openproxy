use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderNode, TokenUsage, UsageEntry};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "usage-routes-test-key";

async fn build_test_app() -> axum::Router {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));

    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "test-key-id".to_string(),
            name: "test".to_string(),
            key: TEST_KEY.to_string(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: Default::default(),
        }];
        state.settings.require_login = false;
        state.provider_nodes = vec![ProviderNode {
            id: "openai".to_string(),
            r#type: "provider".to_string(),
            name: "OpenAI".to_string(),
            prefix: None,
            api_type: None,
            base_url: None,
            created_at: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }];
    })
    .await
    .expect("seed auth");

    db.update_usage(|usage| {
        let mut extra = BTreeMap::new();
        extra.insert("id".to_string(), json!("detail-1"));
        extra.insert("latency".to_string(), json!({ "ttft": 123, "total": 456 }));
        extra.insert(
            "request".to_string(),
            json!({ "messages": [{ "role": "user", "content": "hello" }] }),
        );
        extra.insert("response".to_string(), json!({ "content": "world" }));

        usage.history = vec![UsageEntry {
            timestamp: Some("2026-05-06T10:15:00Z".to_string()),
            provider: Some("openai".to_string()),
            model: "gpt-4.1".to_string(),
            tokens: Some(TokenUsage {
                prompt_tokens: Some(100),
                input_tokens: None,
                completion_tokens: Some(50),
                output_tokens: None,
                total_tokens: Some(150),
                extra: BTreeMap::new(),
            }),
            connection_id: Some("conn-1".to_string()),
            api_key: None,
            endpoint: Some("/v1/chat/completions".to_string()),
            cost: Some(0.5),
            status: Some("success".to_string()),
            extra,
        }];
        usage.total_requests_lifetime = 1;
    })
    .await
    .expect("seed usage");

    openproxy::build_app(AppState::new(db))
}

#[tokio::test]
async fn usage_chart_route_returns_sidecar_compatible_buckets() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/usage/chart?period=7d")
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

    let buckets = json.as_array().expect("chart array");
    assert_eq!(buckets.len(), 7);
    assert!(buckets[0]["label"].is_string());
    assert!(buckets[0]["tokens"].is_number());
    assert!(buckets[0]["cost"].is_number());
}

#[tokio::test]
async fn usage_chart_route_rejects_invalid_period() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/usage/chart?period=90d")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn usage_providers_route_returns_filter_options() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/usage/providers")
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

    assert_eq!(
        json,
        json!({
            "providers": [
                { "id": "openai", "name": "OpenAI" }
            ]
        })
    );
}

#[tokio::test]
async fn usage_request_details_route_returns_paginated_records() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/usage/request-details?page=1&pageSize=20&provider=openai")
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

    assert_eq!(json["pagination"]["page"], 1);
    assert_eq!(json["pagination"]["pageSize"], 20);
    assert_eq!(json["pagination"]["totalItems"], 1);
    assert_eq!(json["details"][0]["id"], "detail-1");
    assert_eq!(json["details"][0]["tokens"]["prompt_tokens"], 100);
    assert_eq!(json["details"][0]["latency"]["ttft"], 123);
}
