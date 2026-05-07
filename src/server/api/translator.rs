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
use std::collections::BTreeMap;
use tokio::time::{self, Duration};

use crate::core::translator::TranslationFormat;
use crate::server::console_logs::ConsoleLogEvent;
use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/translator/translate", post(translate_log))
        .route("/api/translator/formats", get(get_formats))
        .route("/api/translator/load", post(load_translations))
        .route("/api/translator/save", post(save_translations))
        .route("/api/translator/send", post(send_translated_log))
        .route(
            "/api/translator/console-logs",
            get(get_console_logs).delete(delete_console_logs),
        )
        .route(
            "/api/translator/console-logs/stream",
            get(stream_console_logs),
        )
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateRequest {
    pub text: String,
    pub format: Option<String>,
    pub target_format: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct TranslateResponse {
    pub original: String,
    pub translated: String,
    pub format: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendLogRequest {
    pub message: String,
    pub level: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct SendLogResponse {
    pub success: bool,
    pub message_id: String,
}

// Simple translator - just returns formatted output
fn translate_to_format(text: &str, format: TranslationFormat) -> String {
    match format {
        TranslationFormat::OpenAi => text.to_string(),
        TranslationFormat::Claude => format!("[Claude] {}", text),
        TranslationFormat::Gemini => format!("[Gemini] {}", text),
    }
}

async fn translate_log(Json(req): Json<TranslateRequest>) -> Json<TranslateResponse> {
    let format = match req.format.as_deref() {
        Some("claude") => TranslationFormat::Claude,
        Some("gemini") => TranslationFormat::Gemini,
        _ => TranslationFormat::OpenAi,
    };

    let target = req.target_format.as_deref().unwrap_or("openai");
    let target_format = match target {
        "claude" => TranslationFormat::Claude,
        "gemini" => TranslationFormat::Gemini,
        _ => TranslationFormat::OpenAi,
    };

    let translated = translate_to_format(&req.text, target_format);

    Json(TranslateResponse {
        original: req.text,
        translated,
        format: target.to_string(),
    })
}

#[derive(Debug, serde::Serialize)]
pub struct FormatInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

async fn get_formats() -> Json<Vec<FormatInfo>> {
    Json(vec![
        FormatInfo {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            description: "OpenAI streaming format".to_string(),
        },
        FormatInfo {
            id: "claude".to_string(),
            name: "Claude".to_string(),
            description: "Anthropic Claude streaming format".to_string(),
        },
        FormatInfo {
            id: "gemini".to_string(),
            name: "Gemini".to_string(),
            description: "Google Gemini streaming format".to_string(),
        },
    ])
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadTranslationsRequest {
    pub source: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LoadTranslationsResponse {
    pub translations: BTreeMap<String, String>,
    pub loaded: usize,
}

async fn load_translations(
    Json(_req): Json<LoadTranslationsRequest>,
) -> Json<LoadTranslationsResponse> {
    let mut translations = BTreeMap::new();
    translations.insert("error".to_string(), "error - ERROR".to_string());
    translations.insert("warning".to_string(), "warning - WARN".to_string());
    translations.insert("info".to_string(), "info - INFO".to_string());
    translations.insert("debug".to_string(), "debug - DEBUG".to_string());

    Json(LoadTranslationsResponse {
        loaded: translations.len(),
        translations,
    })
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveTranslationsRequest {
    pub translations: BTreeMap<String, String>,
}

async fn save_translations(
    State(state): State<AppState>,
    Json(req): Json<SaveTranslationsRequest>,
) -> Json<serde_json::Value> {
    let result = state
        .db
        .update(|db| {
            if let Ok(value) = serde_json::to_value(&req.translations) {
                db.extra
                    .insert("translator_translations".to_string(), value);
            }
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true, "count": req.translations.len() })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
    }
}

async fn get_console_logs(State(state): State<AppState>) -> Json<serde_json::Value> {
    let logs = state.console_logs.get_logs().await;
    Json(serde_json::json!({ "success": true, "logs": logs }))
}

async fn delete_console_logs(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.console_logs.clear().await;
    Json(serde_json::json!({ "success": true }))
}

async fn stream_console_logs(State(state): State<AppState>) -> Response {
    let initial_logs = state.console_logs.get_logs().await;
    let mut receiver = state.console_logs.subscribe();

    let body_stream = stream! {
        if !initial_logs.is_empty() {
            let payload = serde_json::json!({ "type": "init", "logs": initial_logs });
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
                            let payload = serde_json::json!({ "type": "line", "line": line });
                            yield Ok(bytes::Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Ok(ConsoleLogEvent::Clear) => {
                            let payload = serde_json::json!({ "type": "clear" });
                            yield Ok(bytes::Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let payload = serde_json::json!({
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

fn format_console_line(req: &SendLogRequest) -> String {
    let source = req.source.as_deref().unwrap_or("Translator");
    let level = req.level.as_deref().unwrap_or("info").to_ascii_uppercase();
    format!("[{}] [{}] {}", source, level, req.message)
}

async fn send_translated_log(
    State(state): State<AppState>,
    Json(req): Json<SendLogRequest>,
) -> Json<SendLogResponse> {
    let message_id = uuid::Uuid::new_v4().to_string();
    state
        .console_logs
        .append_line(format_console_line(&req))
        .await;

    Json(SendLogResponse {
        success: true,
        message_id,
    })
}
