use axum::extract::State;
use axum::{
    routing::post,
    Json, Router,
};

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/locale", post(set_locale))
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLocaleRequest {
    pub locale: String,
}

#[derive(Debug, serde::Serialize)]
pub struct LocaleResponse {
    pub success: bool,
    pub locale: String,
}

// Supported locales
const SUPPORTED_LOCALES: &[&str] = &[
    "en", "en-US", "zh", "zh-CN", "zh-TW", "ja", "ko", "es", "fr", "de", "pt", "ru", "ar", "hi", "vi",
];

fn is_supported_locale(locale: &str) -> bool {
    SUPPORTED_LOCALES.contains(&locale) || locale.starts_with("en") || locale.starts_with("zh") || locale.starts_with("ja") || locale.starts_with("ko")
}

fn normalize_locale(locale: &str) -> String {
    let locale = locale.trim();
    match locale {
        "en" => "en-US".to_string(),
        "zh" => "zh-CN".to_string(),
        "ja" => "ja-JP".to_string(),
        "ko" => "ko-KR".to_string(),
        "fr" => "fr-FR".to_string(),
        "de" => "de-DE".to_string(),
        "es" => "es-ES".to_string(),
        "pt" => "pt-BR".to_string(),
        "ru" => "ru-RU".to_string(),
        "ar" => "ar-SA".to_string(),
        "hi" => "hi-IN".to_string(),
        "vi" => "vi-VN".to_string(),
        _ if locale.contains('-') => locale.to_string(),
        _ => locale.to_uppercase(),
    }
}

async fn set_locale(
    State(_state): State<AppState>,
    Json(req): Json<SetLocaleRequest>,
) -> Json<serde_json::Value> {
    let locale = req.locale;

    if !is_supported_locale(&locale) {
        return Json(serde_json::json!({
            "success": false,
            "error": "Invalid locale"
        }));
    }

    let normalized = normalize_locale(&locale);

    Json(serde_json::json!({
        "success": true,
        "locale": normalized
    }))
}