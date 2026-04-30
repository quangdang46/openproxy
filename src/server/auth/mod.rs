use axum::http::HeaderMap;

use crate::db::Db;
use crate::types::ApiKey;

pub const API_KEY_HEADER: &str = "x-api-key";
pub const AUTHORIZATION_HEADER: &str = "authorization";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Missing,
    Invalid,
    Inactive,
}

impl AuthError {
    pub fn message(&self) -> &'static str {
        match self {
            AuthError::Missing => "Missing API key",
            AuthError::Invalid => "Invalid API key",
            AuthError::Inactive => "Inactive API key",
        }
    }
}

pub fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        let mut parts = value.split_whitespace();
        if let (Some(scheme), Some(token)) = (parts.next(), parts.next()) {
            if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }

    headers
        .get(API_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn require_api_key(headers: &HeaderMap, db: &Db) -> Result<ApiKey, AuthError> {
    let key = extract_api_key(headers).ok_or(AuthError::Missing)?;
    let snapshot = db.snapshot();
    let api_key = snapshot
        .api_keys
        .iter()
        .find(|api_key| api_key.key == key)
        .cloned()
        .ok_or(AuthError::Invalid)?;

    if !api_key.is_active() {
        return Err(AuthError::Inactive);
    }

    Ok(api_key)
}
