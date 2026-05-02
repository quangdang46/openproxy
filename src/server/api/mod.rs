mod chat;
mod media;
mod oauth;

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::server::auth::{require_api_key, AuthError};
use crate::server::state::AppState;
use crate::types::{HealthResponse, ProviderConnection};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/v1/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat::chat_completions))
        .route("/v1/audio/transcriptions", post(media::audio_transcriptions))
        .route("/v1/audio/speech", post(media::audio_speech))
        .route("/v1/embeddings", post(media::embeddings))
        .route("/v1/images/generations", post(media::images_generations))
        .route("/v1/search", post(media::search))
        .merge(oauth::routes())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::new("api"))
}

async fn list_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let snapshot = state.db.snapshot();
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut seen = HashSet::new();
    let mut data = Vec::new();

    for combo in &snapshot.combos {
        push_model(
            &mut seen,
            &mut data,
            combo.name.clone(),
            "combo".into(),
            created,
        );
    }

    for connection in snapshot
        .provider_connections
        .iter()
        .filter(|connection| connection.is_active())
    {
        for model_id in models_for_connection(connection) {
            let (id, owned_by) = normalize_model_id(&connection.provider, &model_id);
            push_model(&mut seen, &mut data, id, owned_by, created);
        }
    }

    for model in snapshot
        .custom_models
        .iter()
        .filter(|model| model.r#type.is_empty() || model.r#type == "llm")
    {
        let (id, owned_by) = normalize_model_id(&model.provider_alias, &model.id);
        push_model(&mut seen, &mut data, id, owned_by, created);
    }

    Json(ModelListResponse {
        object: "list",
        data,
    })
    .into_response()
}

fn models_for_connection(connection: &ProviderConnection) -> Vec<String> {
    if let Some(enabled_models) = connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(Value::as_array)
    {
        let models: Vec<_> = enabled_models
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();

        if !models.is_empty() {
            return models;
        }
    }

    connection
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| vec![value.to_string()])
        .unwrap_or_default()
}

fn normalize_model_id(provider: &str, model_id: &str) -> (String, String) {
    if model_id.contains('/') {
        let owned_by = model_id.split('/').next().unwrap_or(provider).to_string();
        (model_id.to_string(), owned_by)
    } else {
        (format!("{provider}/{model_id}"), provider.to_string())
    }
}

fn push_model(
    seen: &mut HashSet<String>,
    data: &mut Vec<ModelCard>,
    id: String,
    owned_by: String,
    created: u64,
) {
    if !seen.insert(id.clone()) {
        return;
    }

    let root = id.split('/').next_back().unwrap_or(&id).to_string();
    data.push(ModelCard {
        id,
        object: "model",
        created,
        owned_by,
        permission: Vec::new(),
        root,
        parent: None,
    });
}

pub(super) fn auth_error_response(error: AuthError) -> Response {
    let status = StatusCode::UNAUTHORIZED;
    (
        status,
        Json(json!({
            "error": {
                "message": error.message(),
                "type": "authentication_error",
                "code": "invalid_api_key"
            }
        })),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
struct ModelListResponse {
    object: &'static str,
    data: Vec<ModelCard>,
}

#[derive(Debug, Serialize)]
struct ModelCard {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: String,
    permission: Vec<Value>,
    root: String,
    parent: Option<String>,
}
