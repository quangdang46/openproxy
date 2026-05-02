use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::ProviderConnection;

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaProvidersResponse {
    tts: Vec<ProviderSummary>,
    stt: Vec<ProviderSummary>,
    embedding: Vec<ProviderSummary>,
    image: Vec<ProviderSummary>,
    search: Vec<ProviderSummary>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderSummary {
    id: String,
    name: String,
    provider: String,
    is_active: bool,
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddMediaProviderRequest {
    name: String,
    provider: String,
    api_key: Option<String>,
    base_url: Option<String>,
    media_type: String,
    enabled_models: Option<Vec<String>>,
    #[serde(default)]
    extra: BTreeMap<String, Value>,
}

fn detect_media_type(connection: &ProviderConnection) -> Option<String> {
    let provider = connection.provider.to_lowercase();

    if provider.contains("tts")
        || provider.contains("elevenlabs")
        || provider == "edge-tts"
        || provider == "google-tts"
    {
        return Some("tts".to_string());
    }
    if provider.contains("stt")
        || provider.contains("deepgram")
        || provider.contains("whisper")
        || provider.contains("transcription")
    {
        return Some("stt".to_string());
    }
    if provider.contains("embedding")
        || provider.contains("cohere")
        || provider == "openai-embedding"
    {
        return Some("embedding".to_string());
    }
    if provider.contains("image")
        || provider.contains("dalle")
        || provider.contains("flux")
        || provider.contains("stable-diffusion")
    {
        return Some("image".to_string());
    }
    if provider.contains("search") {
        return Some("search".to_string());
    }

    // Check provider_specific_data for type info
    for key in &["mediaType", "media_type", "type"] {
        if let Some(v) = connection.provider_specific_data.get(*key).and_then(Value::as_str) {
            match v {
                "tts" | "stt" | "embedding" | "image" | "search" => return Some(v.to_string()),
                _ => {}
            }
        }
    }

    None
}

fn to_summary(conn: &ProviderConnection) -> ProviderSummary {
    ProviderSummary {
        id: conn.id.clone(),
        name: conn
            .name
            .clone()
            .unwrap_or_else(|| conn.provider.clone()),
        provider: conn.provider.clone(),
        is_active: conn.is_active(),
        display_name: conn.display_name.clone(),
    }
}

async fn list_media_providers(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let snapshot = state.db.snapshot();
    let mut tts = Vec::new();
    let mut stt = Vec::new();
    let mut embedding = Vec::new();
    let mut image = Vec::new();
    let mut search = Vec::new();

    for conn in snapshot.provider_connections.iter().filter(|c| c.is_active()) {
        let summary = to_summary(conn);
        match detect_media_type(conn).as_deref() {
            Some("tts") => tts.push(summary),
            Some("stt") => stt.push(summary),
            Some("embedding") => embedding.push(summary),
            Some("image") => image.push(summary),
            Some("search") => search.push(summary),
            _ => {}
        }
    }

    Json(MediaProvidersResponse {
        tts,
        stt,
        embedding,
        image,
        search,
    })
    .into_response()
}

async fn add_media_provider(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AddMediaProviderRequest>,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let valid_types = ["tts", "stt", "embedding", "image", "search"];
    if !valid_types.contains(&body.media_type.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Invalid media_type. Must be one of: {:?}", valid_types)
            })),
        )
            .into_response();
    }

    let id = format!("mp-{}", Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    let mut provider_specific_data = BTreeMap::new();
    provider_specific_data.insert(
        "mediaType".to_string(),
        Value::String(body.media_type.clone()),
    );

    if let Some(models) = body.enabled_models {
        provider_specific_data.insert(
            "enabledModels".to_string(),
            Value::Array(models.into_iter().map(Value::String).collect()),
        );
    }

    if let Some(base_url) = &body.base_url {
        provider_specific_data.insert("baseUrl".to_string(), Value::String(base_url.clone()));
    }

    for (key, value) in body.extra {
        provider_specific_data.insert(key, value);
    }

    let connection = ProviderConnection {
        id: id.clone(),
        provider: body.provider.clone(),
        auth_type: "api_key".to_string(),
        name: Some(body.name),
        api_key: body.api_key,
        is_active: Some(true),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        provider_specific_data,
        ..Default::default()
    };

    match state
        .db
        .update(|db| {
            db.provider_connections.push(connection);
        })
        .await
    {
        Ok(_) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "success": true,
                "id": id,
                "message": "Media provider added successfully"
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to add media provider: {}", err)
            })),
        )
            .into_response(),
    }
}

async fn delete_media_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let snapshot = state.db.snapshot();
    let exists = snapshot
        .provider_connections
        .iter()
        .any(|c| c.id == id);
    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Media provider not found"
            })),
        )
            .into_response();
    }

    match state
        .db
        .update(|db| {
            db.provider_connections.retain(|conn| conn.id != id);
        })
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "message": "Media provider deleted successfully"
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to delete media provider: {}", err)
            })),
        )
            .into_response(),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/media-providers", get(list_media_providers))
        .route("/api/media-providers", post(add_media_provider))
        .route("/api/media-providers/{id}", delete(delete_media_provider))
}
