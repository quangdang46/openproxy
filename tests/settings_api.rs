use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "settings-api-test-key";

fn active_key() -> ApiKey {
    ApiKey {
        id: "key-1".into(),
        name: "Local".into(),
        key: TEST_KEY.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state
            .settings
            .extra
            .insert("password".into(), Value::String("hashed-secret".into()));
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn get_settings_requires_auth_and_redacts_password() {
    let app = openproxy::build_app(app_state().await);

    let unauthenticated = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let authenticated = app
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(authenticated.status(), StatusCode::OK);
    let body = axum::body::to_bytes(authenticated.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["hasPassword"], true);
    assert_eq!(json["enableRequestLogs"], false);
    assert_eq!(json["enableTranslator"], false);
    assert!(json.get("password").is_none());
}

#[tokio::test]
async fn patch_settings_updates_values_and_rejects_password_fields() {
    let app = openproxy::build_app(app_state().await);

    let updated = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/settings")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "requireLogin": false,
                        "providerStrategies": {
                            "openai": "latency"
                        },
                        "comboStrategies": {
                            "writer": "cost"
                        },
                        "rtkEnabled": false,
                        "cavemanEnabled": true,
                        "cavemanLevel": "ultra",
                        "tunnelDashboardAccess": false,
                        "tunnelUrl": "https://demo.example",
                        "tailscaleUrl": "https://tail.example",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(updated.status(), StatusCode::OK);
    let body = axum::body::to_bytes(updated.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["requireLogin"], false);
    assert_eq!(json["providerStrategies"]["openai"], "latency");
    assert_eq!(json["comboStrategies"]["writer"], "cost");
    assert_eq!(json["rtkEnabled"], false);
    assert_eq!(json["cavemanEnabled"], true);
    assert_eq!(json["cavemanLevel"], "ultra");
    assert_eq!(json["tunnelDashboardAccess"], false);
    assert_eq!(json["tunnelUrl"], "https://demo.example");
    assert_eq!(json["tailscaleUrl"], "https://tail.example");
    assert_eq!(json["hasPassword"], true);

    let rejected = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/settings")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "newPassword": "secret123",
                        "currentPassword": "123456",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(rejected.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn settings_database_import_and_require_login_round_trip() {
    let app = openproxy::build_app(app_state().await);

    let imported = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/database")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "apiKeys": [{
                            "id": "key-1",
                            "name": "Local",
                            "key": TEST_KEY,
                            "isActive": true
                        }],
                        "settings": {
                            "password": "hashed-secret",
                            "requireLogin": false,
                            "tunnelDashboardAccess": false,
                            "tunnelUrl": "https://demo.example",
                            "tailscaleUrl": "https://tail.example",
                            "comboStrategy": "latency"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(imported.status(), StatusCode::OK);

    let settings = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(settings.status(), StatusCode::OK);
    let settings_body = axum::body::to_bytes(settings.into_body(), 4096)
        .await
        .unwrap();
    let settings_json: serde_json::Value = serde_json::from_slice(&settings_body).unwrap();
    assert_eq!(settings_json["comboStrategy"], "latency");
    assert_eq!(settings_json["hasPassword"], true);

    let require_login = app
        .oneshot(
            Request::builder()
                .uri("/api/settings/require-login")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(require_login.status(), StatusCode::OK);
    let body = axum::body::to_bytes(require_login.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["requireLogin"], false);
    assert_eq!(json["tunnelDashboardAccess"], false);
    assert_eq!(json["tunnelUrl"], "https://demo.example");
    assert_eq!(json["tailscaleUrl"], "https://tail.example");
}
