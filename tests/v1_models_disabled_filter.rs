use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const DISABLED_LLM: &str = "openai/gpt-4o-mini";
const ENABLED_LLM: &str = "openai/gpt-4o";

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

fn openai_connection(prefix: Option<&str>) -> ProviderConnection {
    let mut provider_specific_data = BTreeMap::new();
    if let Some(prefix) = prefix {
        provider_specific_data.insert("prefix".into(), json!(prefix));
    }

    ProviderConnection {
        id: "openai-conn".into(),
        provider: "openai".into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        api_key: Some("provider-key".into()),
        provider_specific_data,
        ..Default::default()
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn set_disabled(state: &AppState, disabled: Value) {
    state
        .db
        .update(move |db| {
            db.extra.insert("disabledModels".into(), disabled);
        })
        .await
        .expect("seed disabled models");
}

async fn get(state: &AppState, uri: &str) -> (StatusCode, Value) {
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", "Bearer valid-bearer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).expect("json body"))
}

async fn advertised_ids(state: &AppState, uri: &str) -> Vec<String> {
    let (status, body) = get(state, uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    body["data"]
        .as_array()
        .unwrap_or_else(|| panic!("GET {uri} returned no data array: {body}"))
        .iter()
        .map(|card| card["id"].as_str().expect("model id").to_string())
        .collect()
}

#[tokio::test]
async fn models_endpoint_omits_disabled_models() {
    let state = app_state().await;
    set_disabled(
        &state,
        json!({ "openai": [DISABLED_LLM.trim_start_matches("openai/")] }),
    )
    .await;

    let ids = advertised_ids(&state, "/v1/models").await;
    assert!(
        !ids.contains(&DISABLED_LLM.to_string()),
        "{DISABLED_LLM} was disabled on the Providers page but is still advertised by /v1/models"
    );
    assert!(
        ids.contains(&ENABLED_LLM.to_string()),
        "an enabled sibling must survive the disable"
    );
}

#[tokio::test]
async fn models_by_kind_omits_disabled_models() {
    let state = app_state().await;
    set_disabled(
        &state,
        json!({ "openai": ["text-embedding-3-small", "tts-1", "dall-e-3", "whisper-1"] }),
    )
    .await;

    for (uri, disabled_id) in [
        ("/v1/models/embedding", "openai/text-embedding-3-small"),
        ("/v1/models/tts", "openai/tts-1"),
        ("/v1/models/image", "openai/dall-e-3"),
        ("/v1/models/stt", "openai/whisper-1"),
    ] {
        let ids = advertised_ids(&state, uri).await;
        assert!(
            !ids.contains(&disabled_id.to_string()),
            "{disabled_id} was disabled but is still advertised by {uri}"
        );
    }
}

#[tokio::test]
async fn models_endpoint_honours_a_disable_made_under_the_static_alias() {
    // A prefixed connection is listed under the output alias, while the
    // Providers page toggles the model under the static alias.
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.provider_connections = vec![openai_connection(Some("oa"))];
        })
        .await
        .unwrap();
    set_disabled(&state, json!({ "openai": ["gpt-4o-mini"] })).await;

    let ids = advertised_ids(&state, "/v1/models").await;
    assert!(
        !ids.contains(&"oa/gpt-4o-mini".to_string()),
        "a model disabled under the static alias must still be hidden from a prefixed connection"
    );
    assert!(
        ids.contains(&"oa/gpt-4o".to_string()),
        "an enabled sibling must survive the disable"
    );
}

#[tokio::test]
async fn api_models_and_v1_models_agree_on_the_disabled_set() {
    let state = app_state().await;
    set_disabled(&state, json!({ "openai": ["gpt-4o-mini"] })).await;

    let (status, dashboard) = get(&state, "/api/models").await;
    assert_eq!(status, StatusCode::OK, "GET /api/models");

    let dashboard_openai: Vec<String> = dashboard["models"]
        .as_array()
        .expect("models array")
        .iter()
        .filter(|model| model["provider"] == json!("openai") && model["kind"] == json!("llm"))
        .map(|model| model["fullModel"].as_str().expect("fullModel").to_string())
        .collect();

    assert!(
        !dashboard_openai.contains(&DISABLED_LLM.to_string()),
        "the dashboard list itself should already hide the disabled model"
    );

    let v1 = advertised_ids(&state, "/v1/models").await;
    assert!(
        !v1.contains(&DISABLED_LLM.to_string()),
        "{DISABLED_LLM} must be absent from both surfaces, not just one"
    );
    for id in &dashboard_openai {
        assert!(
            v1.contains(id),
            "{id} is enabled on the dashboard but missing from /v1/models"
        );
    }
}
