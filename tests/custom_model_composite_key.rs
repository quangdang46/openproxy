//! Custom models are keyed `providerAlias|id|type` in kv, the way 9router's
//! `customKey()` does. Keying on the bare `id` collides the moment two
//! providers customize a model of the same name: the kv table's primary key is
//! `(scope, key)`, so the second write hits `ON CONFLICT … DO UPDATE` and the
//! first provider's row is gone — including across a restart, which is where
//! it hurts, because the provider page's Available Models is a core surface
//! that must survive a binary rebuild.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::{json, Value};
use tempfile::{tempdir, TempDir};
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

async fn post_custom(app: axum::Router, provider_alias: &str, id: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/models/custom")
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "providerAlias": provider_alias, "id": id, "type": "llm" }).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn state_in(dir: &TempDir) -> AppState {
    let db = Arc::new(Db::load_from(dir.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn kv_keys(dir: &TempDir) -> Vec<String> {
    let conn = rusqlite::Connection::open(dir.path().join("openproxy.sqlite")).expect("open kv");
    let mut stmt = conn
        .prepare("SELECT key FROM kv WHERE scope = 'customModels' ORDER BY key")
        .expect("prepare");
    stmt.query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .map(Result::unwrap)
        .collect()
}

#[tokio::test]
async fn custom_models_with_same_id_survive_two_providers_across_a_reload() {
    let dir = tempdir().expect("tempdir");
    let state = state_in(&dir).await;
    let app = openproxy::build_app(state.clone());

    for (status, _) in [
        post_custom(app.clone(), "openai", "gpt-4o").await,
        post_custom(app.clone(), "anthropic", "gpt-4o").await,
    ] {
        assert_eq!(status, StatusCode::OK);
    }

    assert_eq!(
        kv_keys(&dir),
        vec!["anthropic|gpt-4o|llm", "openai|gpt-4o|llm"],
        "composite keys, 9router customKey()"
    );

    // Reload from SQLite, as a binary restart would.
    drop(app);
    drop(state);
    let reloaded = state_in(&dir).await;
    let models = reloaded.db.snapshot().custom_models.clone();
    assert_eq!(models.len(), 2, "both providers' models must survive");
    let mut aliases: Vec<String> = models.iter().map(|m| m.provider_alias.clone()).collect();
    aliases.sort();
    assert_eq!(aliases, vec!["anthropic", "openai"]);
}

#[tokio::test]
async fn deleting_one_providers_custom_model_leaves_the_other() {
    let dir = tempdir().expect("tempdir");
    let state = state_in(&dir).await;
    state
        .db
        .update(|db| {
            for alias in ["openai", "anthropic"] {
                db.custom_models.push(openproxy::types::CustomModel {
                    provider_alias: alias.into(),
                    id: "gpt-4o".into(),
                    r#type: "llm".into(),
                    name: None,
                    extra: BTreeMap::new(),
                });
            }
        })
        .await
        .expect("seed two custom models");
    assert_eq!(
        kv_keys(&dir),
        vec!["anthropic|gpt-4o|llm", "openai|gpt-4o|llm"],
        "two providers customizing the same model id are two rows"
    );

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/api/models/custom?providerAlias=openai&id=gpt-4o&type=llm")
                .header("authorization", "Bearer valid-bearer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(
        kv_keys(&dir),
        vec!["anthropic|gpt-4o|llm"],
        "a scoped delete must not take the other provider's row with it"
    );
    let models = state.db.snapshot().custom_models.clone();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].provider_alias, "anthropic");
}
