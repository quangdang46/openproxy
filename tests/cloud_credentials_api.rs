use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ModelAliasTarget, ProviderConnection, ProviderModelRef};
use serde_json::json;
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

fn connection(provider: &str, active: bool) -> ProviderConnection {
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
        global_priority: Some(7),
        default_model: Some("gpt-4.1".into()),
        access_token: Some("old-access".into()),
        refresh_token: Some("old-refresh".into()),
        expires_at: Some("2026-01-01T00:00:00.000Z".into()),
        token_type: None,
        scope: None,
        id_token: None,
        project_id: Some("proj-1".into()),
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
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![
            active_key("valid-bearer"),
            ApiKey {
                is_active: Some(false),
                ..active_key("inactive-bearer")
            },
        ];
        state.provider_connections = vec![connection("openai", true), connection("groq", false)];
        state.model_aliases.insert(
            "draft".into(),
            ModelAliasTarget::Path("openai/gpt-4.1".into()),
        );
        state.model_aliases.insert(
            "realtime".into(),
            ModelAliasTarget::Mapping(ProviderModelRef {
                provider: "openai".into(),
                model: "gpt-4o-realtime-preview".into(),
                extra: BTreeMap::new(),
            }),
        );
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn authorized_request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

#[tokio::test]
async fn cloud_auth_matches_9router_payload() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::POST,
            "/api/cloud/auth",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "connections": [
                {
                    "provider": "openai",
                    "authType": "apikey",
                    "apiKey": "provider-key",
                    "accessToken": "old-access",
                    "refreshToken": "old-refresh",
                    "projectId": "proj-1",
                    "expiresAt": "2026-01-01T00:00:00.000Z",
                    "priority": 1,
                    "globalPriority": 7,
                    "defaultModel": "gpt-4.1",
                    "isActive": true
                }
            ],
            "modelAliases": {
                "draft": "openai/gpt-4.1",
                "realtime": "openai/gpt-4o-realtime-preview"
            }
        })
    );
}

#[tokio::test]
async fn cloud_routes_require_authorization_bearer_like_9router() {
    for request in [
        Request::builder()
            .method(Method::POST)
            .uri("/api/cloud/auth")
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .method(Method::POST)
            .uri("/api/cloud/auth")
            .header("x-api-key", "valid-bearer")
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .method(Method::POST)
            .uri("/api/cloud/auth")
            .header("authorization", "bearer valid-bearer")
            .body(Body::empty())
            .unwrap(),
    ] {
        let app = openproxy::build_app(app_state().await);
        let response = app.oneshot(request).await.unwrap();
        let (status, json) = response_json(response).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json, json!({ "error": "Missing API key" }));
    }

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/cloud/auth")
                .header("authorization", "Bearer inactive-bearer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json, json!({ "error": "Invalid API key" }));
}

#[tokio::test]
async fn cloud_credentials_update_matches_9router_and_persists_tokens() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::PUT,
            "/api/cloud/credentials/update",
            Body::from(
                json!({
                    "provider": "openai",
                    "credentials": {
                        "accessToken": "new-access",
                        "refreshToken": "new-refresh",
                        "expiresIn": 3600
                    }
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Credentials updated for provider: openai"
        })
    );

    let snapshot = state.db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|conn| conn.provider == "openai")
        .expect("openai connection");
    assert_eq!(connection.access_token.as_deref(), Some("new-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("new-refresh"));
    assert_ne!(
        connection.expires_at.as_deref(),
        Some("2026-01-01T00:00:00.000Z")
    );
}

#[tokio::test]
async fn cloud_credentials_update_matches_9router_errors() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::PUT,
            "/api/cloud/credentials/update",
            Body::from(r#"{"provider":"","credentials":null}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json,
        json!({ "error": "Provider and credentials required" })
    );

    let response = app
        .oneshot(authorized_request(
            Method::PUT,
            "/api/cloud/credentials/update",
            Body::from(
                json!({
                    "provider": "missing",
                    "credentials": {
                        "accessToken": "new-access"
                    }
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        json,
        json!({ "error": "No active connection found for provider: missing" })
    );
}

#[tokio::test]
async fn cloud_model_resolve_matches_9router_contract() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cloud/model/resolve",
            Body::from(r#"{"alias":"draft"}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "alias": "draft",
            "provider": "openai",
            "model": "gpt-4.1"
        })
    );

    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cloud/model/resolve",
            Body::from(r#"{}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Missing alias" }));

    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cloud/model/resolve",
            Body::from(r#"{"alias":"unknown"}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json, json!({ "error": "Alias not found" }));
}

#[tokio::test]
async fn cloud_models_alias_routes_match_9router_contract() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cloud/models/alias",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "aliases": {
                "draft": "openai/gpt-4.1",
                "realtime": "openai/gpt-4o-realtime-preview"
            }
        })
    );

    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::PUT,
            "/api/cloud/models/alias",
            Body::from(r#"{"model":"","alias":"draft"}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Model and alias required" }));

    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::PUT,
            "/api/cloud/models/alias",
            Body::from(r#"{"model":"openai/gpt-4.1-mini","alias":"draft"}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json,
        json!({ "error": "Alias 'draft' already in use for model 'openai/gpt-4.1'" })
    );

    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::PUT,
            "/api/cloud/models/alias",
            Body::from(r#"{"model":"anthropic/claude-sonnet-4","alias":"sonnet"}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "model": "anthropic/claude-sonnet-4",
            "alias": "sonnet",
            "message": "Alias 'sonnet' set for model 'anthropic/claude-sonnet-4'"
        })
    );

    let snapshot = state.db.snapshot();
    assert_eq!(
        snapshot.model_aliases.get("sonnet"),
        Some(&ModelAliasTarget::Path("anthropic/claude-sonnet-4".into()))
    );
}

#[tokio::test]
async fn cloud_credentials_route_is_not_exposed() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cloud/credentials",
            Body::empty(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
