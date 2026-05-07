use axum::{
    extract::{Path, Query, State},
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
        if let Some(v) = connection
            .provider_specific_data
            .get(*key)
            .and_then(Value::as_str)
        {
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
        name: conn.name.clone().unwrap_or_else(|| conn.provider.clone()),
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

    for conn in snapshot
        .provider_connections
        .iter()
        .filter(|c| c.is_active())
    {
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
    let exists = snapshot.provider_connections.iter().any(|c| c.id == id);
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TtsVoicesQuery {
    lang: Option<String>,
}

async fn get_deepgram_voices(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<TtsVoicesQuery>,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }
    let snapshot = state.db.snapshot();
    let api_key = snapshot
        .provider_connections
        .iter()
        .find(|c| c.provider == "deepgram" && c.is_active())
        .and_then(|c| c.api_key.as_ref());
    let Some(api_key) = api_key else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "No Deepgram connection found" })),
        )
            .into_response();
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let resp = match client
        .get("https://api.deepgram.com/v1/models")
        .header("Authorization", format!("Token {api_key}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Deepgram API failed: {e}") })),
            )
                .into_response()
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("Deepgram API {status}: {text}") })),
        )
            .into_response();
    }
    let data: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Parse error: {e}") })),
            )
                .into_response()
        }
    };
    let tts_models = data
        .get("tts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut by_lang: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for m in &tts_models {
        let canonical = m
            .get("canonical_name")
            .or_else(|| m.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("en");
        let name = m.get("name").and_then(Value::as_str).unwrap_or(canonical);
        let gender = m
            .get("metadata")
            .and_then(Value::as_object)
            .and_then(|md| md.get("tags"))
            .and_then(Value::as_array)
            .and_then(|tags| {
                tags.iter().find_map(|t| {
                    let s = t.as_str()?;
                    (s == "masculine" || s == "feminine").then_some(s.to_string())
                })
            })
            .unwrap_or_default();
        let codes: Vec<String> = m
            .get("languages")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| vec![canonical.rsplit('-').next().unwrap_or("en").to_string()]);
        for code in &codes {
            let entry = by_lang
                .entry(code.clone())
                .or_insert_with(|| serde_json::json!({"code": code, "name": code, "voices": []}));
            let list = entry
                .as_object_mut()
                .unwrap()
                .get_mut("voices")
                .unwrap()
                .as_array_mut()
                .unwrap();
            if !list
                .iter()
                .any(|v| v.get("id") == Some(&serde_json::json!(canonical)))
            {
                list.push(serde_json::json!({"id": canonical, "name": name, "gender": gender, "lang": code}));
            }
        }
    }
    if let Some(lang) = query.lang.as_deref() {
        return Json(serde_json::json!({"voices": by_lang.get(lang).and_then(|v| v.get("voices")).cloned().unwrap_or(serde_json::json!([]))})).into_response();
    }
    let languages: Vec<serde_json::Value> = by_lang
        .iter()
        .map(|(code, _)| serde_json::json!({"code": code, "name": code}))
        .collect();
    Json(serde_json::json!({"languages": languages, "byLang": by_lang})).into_response()
}

async fn get_inworld_voices(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<TtsVoicesQuery>,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }
    let snapshot = state.db.snapshot();
    let api_key = snapshot
        .provider_connections
        .iter()
        .find(|c| c.provider == "inworld" && c.is_active())
        .and_then(|c| c.api_key.as_ref());
    let Some(api_key) = api_key else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "No Inworld connection found" })),
        )
            .into_response();
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let resp = match client
        .get("https://api.inworld.ai/tts/v1/voices")
        .header("Authorization", format!("Basic {api_key}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Inworld API failed: {e}") })),
            )
                .into_response()
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("Inworld API {status}: {text}") })),
        )
            .into_response();
    }
    let data: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Parse error: {e}") })),
            )
                .into_response()
        }
    };
    let voices = data
        .get("voices")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut by_lang: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for v in &voices {
        let vid = v.get("voiceId").and_then(Value::as_str).unwrap_or_default();
        let dname = v.get("displayName").and_then(Value::as_str).unwrap_or(vid);
        let gender = v.get("gender").and_then(Value::as_str).unwrap_or("");
        let codes: Vec<String> = v
            .get("languages")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| vec!["en".to_string()]);
        for code in &codes {
            let entry = by_lang
                .entry(code.clone())
                .or_insert_with(|| serde_json::json!({"code": code, "name": code, "voices": []}));
            let list = entry
                .as_object_mut()
                .unwrap()
                .get_mut("voices")
                .unwrap()
                .as_array_mut()
                .unwrap();
            if !list
                .iter()
                .any(|vv| vv.get("id") == Some(&serde_json::json!(vid)))
            {
                list.push(
                    serde_json::json!({"id": vid, "name": dname, "gender": gender, "lang": code}),
                );
            }
        }
    }
    if let Some(lang) = query.lang.as_deref() {
        return Json(serde_json::json!({"voices": by_lang.get(lang).and_then(|v| v.get("voices")).cloned().unwrap_or(serde_json::json!([]))})).into_response();
    }
    let languages: Vec<serde_json::Value> = by_lang
        .iter()
        .map(|(code, _)| serde_json::json!({"code": code, "name": code}))
        .collect();
    Json(serde_json::json!({"languages": languages, "byLang": by_lang})).into_response()
}



// ===== TTS Voice Routes =====

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TtsVoiceQuery {
    provider: Option<String>,
    lang: Option<String>,
    api_key: Option<String>,
}

/// GET /api/media-providers/tts/voices
async fn get_tts_voices(
    State(state): State<AppState>,
    Query(query): Query<TtsVoiceQuery>,
) -> axum::response::Response {
    let provider = query.provider.as_deref().unwrap_or("edge-tts");
    match provider {
        "edge-tts" => get_edge_tts_voices_impl(query.lang.as_deref()).await,
        "elevenlabs" => {
            let api_key = query.api_key.as_deref().unwrap_or("");
            if api_key.is_empty() {
                let snapshot = state.db.snapshot();
                let key = snapshot.provider_connections.iter()
                    .find(|c| c.provider == "elevenlabs" && c.is_active())
                    .and_then(|c| c.api_key.as_ref());
                match key {
                    Some(k) => get_elevenlabs_voices_impl(k, query.lang.as_deref()).await,
                    None => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "No ElevenLabs API key" }))).into_response(),
                }
            } else {
                get_elevenlabs_voices_impl(api_key, query.lang.as_deref()).await
            }
        }
        "local-device" => Json(serde_json::json!({ "voices": [], "languages": [], "byLang": {} })).into_response(),
        _ => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": format!("Provider '{}' does not support voice listing", provider) }))).into_response(),
    }
}

async fn get_edge_tts_voices_impl(lang_filter: Option<&str>) -> axum::response::Response {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let resp = match client
        .get("https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list?trustedclienttoken=6A5AA1D4EAFF4E9FB37E23D68491D6F4")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": format!("Edge TTS failed: {e}") }))).into_response(),
    };

    let voices: Vec<serde_json::Value> = match resp.json().await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": format!("Parse error: {e}") }))).into_response(),
    };

    let mut result_voices = Vec::new();
    let mut by_lang: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    for v in &voices {
        let short_name = v.get("ShortName").and_then(Value::as_str).unwrap_or("");
        let friendly = v.get("FriendlyName").and_then(Value::as_str).unwrap_or(short_name);
        let locale = v.get("Locale").and_then(Value::as_str).unwrap_or("en-US");
        let gender = v.get("Gender").and_then(Value::as_str).unwrap_or("Neutral");
        let parts: Vec<&str> = locale.split('-').collect();
        let lang = parts.first().unwrap_or(&"en").to_string();
        let country = parts.get(1).unwrap_or(&"").to_string();
        let name = friendly.replace("Microsoft ", "").replace(" Online (Natural) - ", " (");
        let voice = serde_json::json!({"id": short_name, "name": name, "locale": locale, "lang": lang, "country": country, "gender": gender});
        if let Some(filter) = lang_filter { if lang != filter { continue; } }
        let entry = by_lang.entry(lang.clone()).or_insert_with(|| serde_json::json!({"code": &lang, "name": &lang, "voices": []}));
        entry.as_object_mut().unwrap().get_mut("voices").unwrap().as_array_mut().unwrap().push(voice.clone());
        result_voices.push(voice);
    }

    let languages: Vec<serde_json::Value> = by_lang.iter().map(|(code, _)| serde_json::json!({"code": code, "name": code})).collect();
    Json(serde_json::json!({"voices": result_voices, "languages": languages, "byLang": by_lang})).into_response()
}

/// GET /api/media-providers/tts/elevenlabs/voices
async fn get_elevenlabs_voices(
    State(state): State<AppState>,
    Query(query): Query<TtsVoiceQuery>,
) -> axum::response::Response {
    let snapshot = state.db.snapshot();
    let api_key = snapshot.provider_connections.iter()
        .find(|c| c.provider == "elevenlabs" && c.is_active())
        .and_then(|c| c.api_key.as_ref());
    match api_key {
        Some(key) => get_elevenlabs_voices_impl(key, query.lang.as_deref()).await,
        None => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "No ElevenLabs connection found" }))).into_response(),
    }
}

async fn get_elevenlabs_voices_impl(api_key: &str, lang_filter: Option<&str>) -> axum::response::Response {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap_or_default();
    let resp = match client.get("https://api.elevenlabs.io/v1/voices").header("xi-api-key", api_key).send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": format!("ElevenLabs API failed: {e}") }))).into_response(),
    };
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": format!("ElevenLabs API {status}: {text}") }))).into_response();
    }
    let data: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": format!("Parse error: {e}") }))).into_response(),
    };
    let voices = data.get("voices").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut by_lang: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for v in &voices {
        let vid = v.get("voice_id").and_then(Value::as_str).unwrap_or_default();
        let name = v.get("name").and_then(Value::as_str).unwrap_or(vid);
        let gender = v.get("labels").and_then(|l| l.get("gender")).and_then(Value::as_str).unwrap_or("");
        let lang = v.get("labels").and_then(|l| l.get("language")).and_then(Value::as_str).unwrap_or("en");
        let category = v.get("category").and_then(Value::as_str).unwrap_or("");
        let voice = serde_json::json!({"id": vid, "name": name, "gender": gender, "lang": lang, "category": category});
        if let Some(filter) = lang_filter { if lang != filter { continue; } }
        let entry = by_lang.entry(lang.to_string()).or_insert_with(|| serde_json::json!({"code": lang, "name": lang, "voices": []}));
        entry.as_object_mut().unwrap().get_mut("voices").unwrap().as_array_mut().unwrap().push(voice);
    }
    if let Some(filter) = lang_filter {
        let voices = by_lang.get(filter).and_then(|v| v.get("voices")).cloned().unwrap_or(serde_json::json!([]));
        return Json(serde_json::json!({"voices": voices})).into_response();
    }
    let languages: Vec<serde_json::Value> = by_lang.iter().map(|(code, _)| serde_json::json!({"code": code, "name": code})).collect();
    Json(serde_json::json!({"languages": languages, "byLang": by_lang})).into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/media-providers", get(list_media_providers))
        .route("/api/media-providers", post(add_media_provider))
        .route("/api/media-providers/{id}", delete(delete_media_provider))
        .route(
            "/api/media-providers/tts/deepgram/voices",
            get(get_deepgram_voices),
        )
        .route(
            "/api/media-providers/tts/inworld/voices",
            get(get_inworld_voices),
        )
        .route(
            "/api/media-providers/tts/voices",
            get(get_tts_voices),
        )
        .route(
            "/api/media-providers/tts/elevenlabs/voices",
            get(get_elevenlabs_voices),
        )
}
