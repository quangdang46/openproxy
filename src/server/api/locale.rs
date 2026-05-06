use axum::extract::State;
use axum::http::{header::SET_COOKIE, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::post, Json, Router};

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

const LOCALE_COOKIE: &str = "locale";
const LOCALE_COOKIE_MAX_AGE_SECONDS: u64 = 60 * 60 * 24 * 365;
const SUPPORTED_LOCALES: &[&str] = &[
    "en", "vi", "zh-CN", "zh-TW", "ja", "pt-BR", "pt-PT", "ko", "es", "de", "fr", "he", "ar", "ru",
    "pl", "cs", "nl", "tr", "uk", "tl", "id", "th", "hi", "bn", "ur", "ro", "sv", "it", "el", "hu",
    "fi", "da", "no",
];

fn is_supported_locale(locale: &str) -> bool {
    SUPPORTED_LOCALES.contains(&locale)
}

fn normalize_locale(locale: &str) -> String {
    match locale {
        "zh" | "zh-CN" => "zh-CN".to_string(),
        "en" => "en".to_string(),
        "vi" => "vi".to_string(),
        "zh-TW" => "zh-TW".to_string(),
        "ja" => "ja".to_string(),
        "pt-BR" => "pt-BR".to_string(),
        "pt-PT" => "pt-PT".to_string(),
        "ko" => "ko".to_string(),
        "es" => "es".to_string(),
        "de" => "de".to_string(),
        "fr" => "fr".to_string(),
        "he" => "he".to_string(),
        "ar" => "ar".to_string(),
        "ru" => "ru".to_string(),
        "pl" => "pl".to_string(),
        "cs" => "cs".to_string(),
        "nl" => "nl".to_string(),
        "tr" => "tr".to_string(),
        "uk" => "uk".to_string(),
        "tl" => "tl".to_string(),
        "id" => "id".to_string(),
        "th" => "th".to_string(),
        "hi" => "hi".to_string(),
        "bn" => "bn".to_string(),
        "ur" => "ur".to_string(),
        "ro" => "ro".to_string(),
        "sv" => "sv".to_string(),
        "it" => "it".to_string(),
        "el" => "el".to_string(),
        "hu" => "hu".to_string(),
        "fi" => "fi".to_string(),
        "da" => "da".to_string(),
        "no" => "no".to_string(),
        _ => "en".to_string(),
    }
}

async fn set_locale(State(_state): State<AppState>, Json(req): Json<SetLocaleRequest>) -> Response {
    let locale = req.locale;

    if !is_supported_locale(&locale) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Invalid locale"
            })),
        )
            .into_response();
    }

    let normalized = normalize_locale(&locale);
    let cookie =
        format!("{LOCALE_COOKIE}={normalized}; Path=/; Max-Age={LOCALE_COOKIE_MAX_AGE_SECONDS}");
    let mut response = Json(serde_json::json!({
        "success": true,
        "locale": normalized
    }))
    .into_response();
    response
        .headers_mut()
        .append(SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());

    response
}
