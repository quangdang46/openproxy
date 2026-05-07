use async_stream::stream;
use axum::body::Body;
use axum::extract::State;
use axum::{
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::time::{self, Duration};

use crate::core::model::get_model_info;
use crate::core::translator::registry::{self, Format};
use crate::server::console_logs::ConsoleLogEvent;
use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/translator/translate", post(translate_pipeline))
        .route("/api/translator/formats", get(get_formats))
        .route("/api/translator/load", post(load_translations))
        .route("/api/translator/save", post(save_translations))
        .route("/api/translator/send", post(send_to_provider))
        .route(
            "/api/translator/console-logs",
            get(get_console_logs).delete(delete_console_logs),
        )
        .route(
            "/api/translator/console-logs/stream",
            get(stream_console_logs),
        )
}

// === 3-step Translator Pipeline ===

/// Unified translate endpoint supporting 3 steps:
/// Step 1: Detect provider, model, source/target format from client body
/// Step 2: Translate source format -> OpenAI intermediate
/// Step 3: Translate OpenAI intermediate -> target, build URL/headers
async fn translate_pipeline(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let step = body.get("step").and_then(Value::as_u64).unwrap_or(0);

    match step {
        1 => step_detect(&body, &state),
        2 => step_to_openai(&body, &state),
        3 => step_to_target(&body, &state).await,
        _ => {
            // Legacy: plain text translation
            (StatusCode::BAD_REQUEST, Json(json!({ "success": false, "error": "Step (1-3) required" }))).into_response()
        }
    }
}

use axum::http::StatusCode;

/// Step 1: Detect provider, model, source format, target format
fn step_detect(body: &Value, state: &AppState) -> Response {
    let client_body = body.get("body").cloned().unwrap_or_else(|| body.clone());
    let model_str = client_body.get("model").and_then(Value::as_str).unwrap_or("");

    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model_str, &snapshot);
    let provider = resolved.provider.unwrap_or_else(|| model_str.to_string());
    let model = resolved.model.clone();

    let source_format = registry::detect_source_format(&client_body);
    let target_format = registry::get_target_format_for_provider(&provider);

    Json(json!({
        "success": true,
        "result": {
            "provider": provider,
            "model": model,
            "sourceFormat": source_format.as_str(),
            "targetFormat": target_format.as_str()
        }
    })).into_response()
}

/// Step 2: Translate source format -> OpenAI intermediate
fn step_to_openai(body: &Value, state: &AppState) -> Response {
    let client_body = body.get("body").cloned().unwrap_or_else(|| body.clone());
    let model_str = client_body.get("model").and_then(Value::as_str).unwrap_or("");
    let stream = client_body.get("stream").and_then(Value::as_bool).unwrap_or(true);

    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model_str, &snapshot);
    let provider = resolved.provider.unwrap_or_else(|| model_str.to_string());
    let model = resolved.model.clone();

    let source_format = registry::detect_source_format(&client_body);
    let reg = registry::global_registry();

    let mut translated = client_body.clone();
    let did_translate = if source_format == Format::OpenAi {
        true // Already OpenAI format
    } else {
        reg.translate_request(source_format, Format::OpenAi, &model, &mut translated, stream, None)
    };

    if !did_translate && source_format != Format::OpenAi {
        return Json(json!({
            "success": false,
            "error": format!("No translator for {} -> openai", source_format.as_str())
        })).into_response();
    }

    Json(json!({
        "success": true,
        "result": {
            "body": translated
        }
    })).into_response()
}

/// Step 3: Translate OpenAI intermediate -> target, build URL/headers via executor
async fn step_to_target(body: &Value, state: &AppState) -> Response {
    let openai_body = body.get("body").cloned().unwrap_or_else(|| body.clone());
    let provider = body.get("provider").and_then(Value::as_str).unwrap_or("");
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");

    if provider.is_empty() || model.is_empty() {
        return Json(json!({ "success": false, "error": "provider and model required" })).into_response();
    }

    let target_format = registry::get_target_format_for_provider(provider);
    let stream = openai_body.get("stream").and_then(Value::as_bool).unwrap_or(true);

    let reg = registry::global_registry();
    let mut translated = openai_body.clone();
    let did_translate = reg.translate_request(Format::OpenAi, target_format, model, &mut translated, stream, None);

    if !did_translate && target_format != Format::OpenAi {
        // Try passthrough - just set the model
        translated = openai_body.clone();
    }

    // Find active connection for URL/headers
    let snapshot = state.db.snapshot();
    let connection = snapshot.provider_connections.iter()
        .find(|c| c.provider == provider && c.is_active.unwrap_or(true));

    let (url, headers) = match connection {
        Some(conn) => {
            let base_url = conn.provider_specific_data.get("baseUrl")
                .and_then(Value::as_str)
                .unwrap_or("");
            let api_url = if base_url.is_empty() {
                default_provider_url(provider)
            } else {
                format!("{}/v1/chat/completions", base_url.trim_end_matches('/'))
            };

            let mut hdrs = serde_json::Map::new();
            if let Some(key) = &conn.api_key {
                hdrs.insert("Authorization".into(), json!(format!("Bearer {}", key)));
            }
            if let Some(token) = &conn.access_token {
                hdrs.insert("Authorization".into(), json!(format!("Bearer {}", token)));
            }
            hdrs.insert("Content-Type".into(), json!("application/json"));

            (api_url, Value::Object(hdrs))
        }
        None => {
            let api_url = default_provider_url(provider);
            let mut hdrs = serde_json::Map::new();
            hdrs.insert("Content-Type".into(), json!("application/json"));
            (api_url, Value::Object(hdrs))
        }
    };

    Json(json!({
        "success": true,
        "result": {
            "url": url,
            "headers": headers,
            "body": translated
        }
    })).into_response()
}

fn default_provider_url(provider: &str) -> String {
    match provider {
        "openai" => "https://api.openai.com/v1/chat/completions".to_string(),
        "anthropic" | "claude" => "https://api.anthropic.com/v1/messages".to_string(),
        "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions".to_string(),
        "deepseek" => "https://api.deepseek.com/v1/chat/completions".to_string(),
        "groq" => "https://api.groq.com/openai/v1/chat/completions".to_string(),
        "openrouter" => "https://openrouter.ai/api/v1/chat/completions".to_string(),
        _ => format!("https://api.{provider}.com/v1/chat/completions"),
    }
}

// === Send to Provider ===

/// POST /api/translator/send - Proxy request to provider and stream response
async fn send_to_provider(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let provider = body.get("provider").and_then(Value::as_str).unwrap_or("");
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let req_body = body.get("body").cloned().unwrap_or_else(|| json!({}));
    let stream = req_body.get("stream").and_then(Value::as_bool).unwrap_or(true);

    if provider.is_empty() || model.is_empty() {
        return Json(json!({ "success": false, "error": "provider, model, and body required" })).into_response();
    }

    let snapshot = state.db.snapshot();
    let connection = snapshot.provider_connections.iter()
        .find(|c| c.provider == provider && c.is_active.unwrap_or(true));

    let Some(connection) = connection else {
        return Json(json!({ "success": false, "error": format!("No active connection for provider: {provider}") })).into_response();
    };

    let base_url = connection.provider_specific_data.get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or("");
    let url = if base_url.is_empty() {
        default_provider_url(provider)
    } else {
        format!("{}/v1/chat/completions", base_url.trim_end_matches('/'))
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default();

    let mut req = client.post(&url)
        .header("Content-Type", "application/json");

    if let Some(key) = &connection.api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    if let Some(token) = &connection.access_token {
        req = req.header("Authorization", format!("Bearer {}", token));
    }

    match req.json(&req_body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let status_code = status.as_u16();
                let error_text = resp.text().await.unwrap_or_default();
                return Json(json!({
                    "success": false,
                    "error": format!("Provider error: {status_code}"),
                    "details": error_text
                })).into_response();
            }

            if stream {
                // Return SSE stream
                let body_stream = stream! {
                    let mut stream = resp.bytes_stream();
                    use futures_util::TryStreamExt;
                    while let Ok(Some(chunk)) = stream.try_next().await {
                        yield Ok::<_, std::convert::Infallible>(chunk);
                    }
                };
                return (
                    [
                        (header::CONTENT_TYPE, "text/event-stream"),
                        (header::CACHE_CONTROL, "no-cache"),
                        (header::CONNECTION, "keep-alive"),
                    ],
                    Body::from_stream(body_stream),
                ).into_response();
            }

            let body_text = resp.text().await.unwrap_or_default();
            (
                [(header::CONTENT_TYPE, "application/json")],
                body_text,
            ).into_response()
        }
        Err(e) => Json(json!({
            "success": false,
            "error": e.to_string()
        })).into_response(),
    }
}

// === Console Logs (unchanged) ===

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendLogRequest {
    pub message: String,
    pub level: Option<String>,
    pub source: Option<String>,
}

fn format_console_line(req: &SendLogRequest) -> String {
    let source = req.source.as_deref().unwrap_or("Translator");
    let level = req.level.as_deref().unwrap_or("info").to_ascii_uppercase();
    format!("[{}] [{}] {}", source, level, req.message)
}

#[derive(Debug, serde::Serialize)]
pub struct SendLogResponse {
    pub success: bool,
    pub message_id: String,
}

async fn get_console_logs(State(state): State<AppState>) -> Json<Value> {
    let logs = state.console_logs.get_logs().await;
    Json(json!({ "success": true, "logs": logs }))
}

async fn delete_console_logs(State(state): State<AppState>) -> Json<Value> {
    state.console_logs.clear().await;
    Json(json!({ "success": true }))
}

async fn stream_console_logs(State(state): State<AppState>) -> Response {
    let initial_logs = state.console_logs.get_logs().await;
    let mut receiver = state.console_logs.subscribe();

    let body_stream = stream! {
        if !initial_logs.is_empty() {
            let payload = json!({ "type": "init", "logs": initial_logs });
            yield Ok::<_, std::convert::Infallible>(bytes::Bytes::from(format!("data: {}\n\n", payload)));
        }

        let mut keepalive = time::interval(Duration::from_secs(25));
        keepalive.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = keepalive.tick() => {
                    yield Ok(bytes::Bytes::from_static(b": ping\n\n"));
                }
                event = receiver.recv() => {
                    match event {
                        Ok(ConsoleLogEvent::Line(line)) => {
                            let payload = json!({ "type": "line", "line": line });
                            yield Ok(bytes::Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Ok(ConsoleLogEvent::Clear) => {
                            let payload = json!({ "type": "clear" });
                            yield Ok(bytes::Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let payload = json!({
                                "type": "init",
                                "logs": state.console_logs.get_logs().await,
                            });
                            yield Ok(bytes::Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONNECTION, "keep-alive"),
        ],
        Body::from_stream(body_stream),
    )
        .into_response()
}

// === Formats, Load, Save (unchanged) ===

#[derive(Debug, Serialize)]
pub struct FormatInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

async fn get_formats() -> Json<Vec<FormatInfo>> {
    Json(vec![
        FormatInfo { id: "openai".into(), name: "OpenAI".into(), description: "OpenAI Chat Completions format".into() },
        FormatInfo { id: "claude".into(), name: "Claude".into(), description: "Anthropic Claude Messages format".into() },
        FormatInfo { id: "gemini".into(), name: "Gemini".into(), description: "Google Gemini format".into() },
        FormatInfo { id: "openai-responses".into(), name: "OpenAI Responses".into(), description: "OpenAI Responses API format".into() },
        FormatInfo { id: "cursor".into(), name: "Cursor".into(), description: "Cursor format".into() },
        FormatInfo { id: "kiro".into(), name: "Kiro".into(), description: "Kiro/AWS Bedrock format".into() },
    ])
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveTranslationsRequest {
    pub file: Option<String>,
    pub content: Option<String>,
    pub translations: Option<BTreeMap<String, String>>,
}

async fn load_translations(Json(_req): Json<Value>) -> Json<Value> {
    Json(json!({ "success": true, "translations": {} }))
}

async fn save_translations(
    State(state): State<AppState>,
    Json(req): Json<SaveTranslationsRequest>,
) -> Json<Value> {
    if let Some(translations) = &req.translations {
        let result = state.db.update(|db| {
            if let Ok(value) = serde_json::to_value(translations) {
                db.extra.insert("translator_translations".into(), value);
            }
        }).await;
        match result {
            Ok(_) => return Json(json!({ "success": true, "count": translations.len() })),
            Err(e) => return Json(json!({ "success": false, "error": e.to_string() })),
        }
    }

    // Save file/content pair
    if let (Some(file), Some(content)) = (&req.file, &req.content) {
        let result = state.db.update(|db| {
            db.extra.insert(format!("translator_file_{file}"), json!(content));
        }).await;
        match result {
            Ok(_) => return Json(json!({ "success": true })),
            Err(e) => return Json(json!({ "success": false, "error": e.to_string() })),
        }
    }

    Json(json!({ "success": true }))
}
