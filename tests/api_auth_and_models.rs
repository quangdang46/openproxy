use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hmac::{Hmac, Mac};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, Combo, CustomModel, ProviderConnection};
use sha2::Sha256;
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
    }
}

fn active_key_with_machine_id(key: &str, machine_id: &str) -> ApiKey {
    ApiKey {
        machine_id: Some(machine_id.into()),
        ..active_key(key)
    }
}

fn cli_token(machine_id: &str, key_id: &str) -> String {
    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(b"endpoint-proxy-api-key-secret").unwrap();
    mac.update(machine_id.as_bytes());
    mac.update(key_id.as_bytes());
    let crc = hex::encode(mac.finalize().into_bytes());
    format!("sk-{machine_id}-{key_id}-{}", &crc[..8])
}

fn connection(
    provider: &str,
    default_model: Option<&str>,
    enabled_models: &[&str],
    active: bool,
) -> ProviderConnection {
    let mut provider_specific_data = BTreeMap::new();
    if !enabled_models.is_empty() {
        provider_specific_data.insert(
            "enabledModels".into(),
            serde_json::Value::Array(
                enabled_models
                    .iter()
                    .map(|value| serde_json::Value::String((*value).to_string()))
                    .collect(),
            ),
        );
    }

    ProviderConnection {
        id: format!("{provider}-conn"),
        provider: provider.to_string(),
        auth_type: "apikey".into(),
        name: Some(provider.into()),
        priority: Some(1),
        is_active: Some(active),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: default_model.map(str::to_string),
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: Some("provider-key".into()),
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
        provider_specific_data,
        extra: BTreeMap::new(),
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![
            active_key("valid-bearer"),
            active_key_with_machine_id(&cli_token("machine1", "cli01"), "machine1"),
            ApiKey {
                is_active: Some(false),
                ..active_key("inactive-key")
            },
        ];
        state.combos = vec![Combo {
            id: "combo-1".into(),
            name: "writer".into(),
            models: vec!["openai/gpt-4.1".into()],
            kind: None,
            created_at: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }];
        state.provider_connections = vec![
            connection("openai", Some("gpt-4.1"), &[], true),
            connection("groq", None, &["llama-3.3-70b"], true),
            connection("deepseek", Some("deepseek-chat"), &[], false),
        ];
        state.custom_models = vec![
            CustomModel {
                provider_alias: "openai".into(),
                id: "gpt-custom".into(),
                r#type: "llm".into(),
                name: Some("Custom".into()),
                extra: BTreeMap::new(),
            },
            CustomModel {
                provider_alias: "openai".into(),
                id: "text-embedding-3-large".into(),
                r#type: "embedding".into(),
                name: Some("Embedding".into()),
                extra: BTreeMap::new(),
            },
        ];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn valid_bearer_key_allows_models_request() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "Bearer valid-bearer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn bearer_scheme_is_case_insensitive() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "bearer valid-bearer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn valid_x_api_key_allows_models_request() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("x-api-key", "valid-bearer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn missing_invalid_and_inactive_keys_return_unauthorized() {
    for request in [
        Request::builder()
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .uri("/v1/models")
            .header("authorization", "Bearer missing-key")
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .uri("/v1/models")
            .header("authorization", "Bearer inactive-key")
            .body(Body::empty())
            .unwrap(),
    ] {
        let app = openproxy::build_app(app_state().await);
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn bearer_takes_precedence_over_x_api_key() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "Bearer wrong-key")
                .header("x-api-key", "valid-bearer")
                .header("x-9r-cli-token", cli_token("machine1", "cli01"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn valid_cli_token_allows_models_request() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("x-9r-cli-token", cli_token("machine1", "cli01"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn cli_token_machine_id_mismatch_is_unauthorized() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.api_keys = vec![active_key_with_machine_id(
                &cli_token("machine1", "cli01"),
                "othermachine",
            )];
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("x-9r-cli-token", cli_token("machine1", "cli01"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn valid_key_still_resolves_with_many_stored_keys() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.api_keys = (0..2_000)
                .map(|index| active_key(&format!("bulk-key-{index:04}")))
                .collect();
            db.api_keys.push(active_key_with_machine_id(
                &cli_token("machine1", "cli01"),
                "machine1",
            ));
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("x-9r-cli-token", cli_token("machine1", "cli01"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn models_endpoint_returns_combo_active_connection_and_custom_llm_models() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "Bearer valid-bearer")
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
    assert_eq!(json["object"], "list");

    let data = json["data"].as_array().unwrap();
    assert!(data.iter().all(|item| item["object"] == "model"));

    let ids: Vec<String> = data
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(ids[0], "writer");
    assert!(ids.contains(&"openai/gpt-4.1".to_string()));
    assert!(ids.contains(&"groq/llama-3.3-70b".to_string()));
    assert!(ids.contains(&"openai/gpt-custom".to_string()));
    assert!(!ids.contains(&"deepseek/deepseek-chat".to_string()));
    assert!(!ids.contains(&"openai/text-embedding-3-large".to_string()));
}

#[tokio::test]
async fn models_endpoint_dedupes_duplicate_model_ids() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.provider_connections
                .push(connection("openai", Some("gpt-custom"), &[], true));
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "Bearer valid-bearer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let ids: Vec<&str> = json["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    let count = ids.iter().filter(|id| **id == "openai/gpt-custom").count();

    assert_eq!(count, 1);
}

#[tokio::test]
async fn models_endpoint_returns_empty_list_when_no_models_exist() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.combos.clear();
            db.provider_connections.clear();
            db.custom_models.clear();
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "Bearer valid-bearer")
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

    assert_eq!(json["object"], "list");
    assert_eq!(json["data"], serde_json::Value::Array(Vec::new()));
}

#[tokio::test]
async fn models_availability_get_matches_js_issue_payload() {
    let state = app_state().await;
    let future = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339();
    state
        .db
        .update(|db| {
            let mut cooldown = connection("openai", Some("gpt-4o"), &[], true);
            cooldown.id = "cooldown-conn".into();
            cooldown.priority = Some(2);
            cooldown.name = Some("OpenAI Primary".into());
            cooldown.extra.insert(
                "modelLock_gpt-4o-mini".into(),
                serde_json::Value::String(future.clone()),
            );

            let mut unavailable = connection("anthropic", Some("claude"), &[], true);
            unavailable.id = "unavailable-conn".into();
            unavailable.priority = Some(1);
            unavailable.name = None;
            unavailable.email = Some("anthropic@example.com".into());
            unavailable.test_status = Some("unavailable".into());
            unavailable.last_error = Some("Provider offline".into());

            db.provider_connections = vec![cooldown, unavailable];
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/models/availability")
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

    assert_eq!(json["unavailableCount"], 2);
    let models = json["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["provider"], "anthropic");
    assert_eq!(models[0]["model"], "__all");
    assert_eq!(models[0]["status"], "unavailable");
    assert!(models[0].get("until").is_none());
    assert_eq!(models[0]["connectionId"], "unavailable-conn");
    assert_eq!(models[0]["connectionName"], "anthropic@example.com");
    assert_eq!(models[0]["lastError"], "Provider offline");

    assert_eq!(models[1]["provider"], "openai");
    assert_eq!(models[1]["model"], "gpt-4o-mini");
    assert_eq!(models[1]["status"], "cooldown");
    assert_eq!(models[1]["until"], future);
    assert_eq!(models[1]["connectionId"], "cooldown-conn");
    assert_eq!(models[1]["connectionName"], "OpenAI Primary");
    assert_eq!(models[1]["lastError"], serde_json::Value::Null);
}

#[tokio::test]
async fn models_availability_post_clears_cooldown_like_js() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            let mut locked = connection("openai", Some("gpt-4o"), &[], true);
            locked.id = "locked-conn".into();
            locked.test_status = Some("unavailable".into());
            locked.last_error = Some("temporary failure".into());
            locked.last_error_at = Some("2026-05-06T10:00:00Z".into());
            locked.backoff_level = Some(3);
            locked.extra.insert(
                "modelLock_gpt-4o-mini".into(),
                serde_json::Value::String("2026-05-06T12:00:00Z".into()),
            );

            let mut untouched = connection("openai", Some("gpt-4o"), &[], true);
            untouched.id = "untouched-conn".into();
            untouched.test_status = Some("unavailable".into());
            untouched.last_error = Some("still locked".into());

            db.provider_connections = vec![locked, untouched];
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/models/availability")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "action": "clearCooldown",
                        "provider": "openai",
                        "model": "gpt-4o-mini"
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
    assert_eq!(json, json!({ "ok": true }));

    let snapshot = state.db.snapshot();
    let locked = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == "locked-conn")
        .unwrap();
    assert_eq!(
        locked.extra.get("modelLock_gpt-4o-mini"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(locked.test_status.as_deref(), Some("active"));
    assert_eq!(locked.last_error, None);
    assert_eq!(locked.last_error_at, None);
    assert_eq!(locked.backoff_level, Some(0));
    assert!(locked.updated_at.as_deref().is_some());

    let untouched = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == "untouched-conn")
        .unwrap();
    assert_eq!(untouched.test_status.as_deref(), Some("unavailable"));
    assert_eq!(untouched.last_error.as_deref(), Some("still locked"));
    assert!(untouched.extra.get("modelLock_gpt-4o-mini").is_none());
}

#[tokio::test]
async fn models_availability_post_rejects_invalid_request() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/models/availability")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "action": "wrong" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json, json!({ "error": "Invalid request" }));
}
