use axum::http::HeaderMap;
use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

use crate::core::auth::{parse_api_key, CLI_TOKEN_HEADER};
use crate::db::Db;
use crate::types::ApiKey;

pub const API_KEY_HEADER: &str = "x-api-key";
pub const AUTHORIZATION_HEADER: &str = "authorization";
pub const AUTH_COOKIE_NAME: &str = "auth_token";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentedKeySource {
    AuthorizationBearer,
    ApiKeyHeader,
    CliTokenHeader,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PresentedKey {
    key: String,
    source: PresentedKeySource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Missing,
    Invalid,
    Inactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardAuthError {
    Missing,
    Invalid,
}

impl DashboardAuthError {
    pub fn message(&self) -> &'static str {
        match self {
            DashboardAuthError::Missing => "Missing auth token",
            DashboardAuthError::Invalid => "Invalid auth token",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardClaims {
    pub authenticated: bool,
    pub exp: usize,
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
    extract_presented_key(headers).map(|presented| presented.key)
}

pub fn extract_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie_header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookie_header.split(';').find_map(|segment| {
        let mut parts = segment.trim().splitn(2, '=');
        let key = parts.next()?.trim();
        let value = parts.next()?.trim();
        (key == name && !value.is_empty()).then(|| value.to_string())
    })
}

pub fn extract_auth_token(headers: &HeaderMap) -> Option<String> {
    extract_cookie(headers, AUTH_COOKIE_NAME)
}

pub fn require_dashboard_session(
    headers: &HeaderMap,
    db: &Db,
) -> Result<DashboardClaims, DashboardAuthError> {
    let snapshot = db.snapshot();
    if snapshot.settings.require_login == false {
        return Ok(DashboardClaims {
            authenticated: true,
            exp: usize::MAX,
        });
    }

    let token = extract_auth_token(headers).ok_or(DashboardAuthError::Missing)?;
    let secret = std::env::var("JWT_SECRET")
        .unwrap_or_else(|_| "9router-default-secret-change-me".to_string());
    let validation = Validation::default();
    let decoded = decode::<DashboardClaims>(
        &token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|_| DashboardAuthError::Invalid)?;
    if !decoded.claims.authenticated {
        return Err(DashboardAuthError::Invalid);
    }
    Ok(decoded.claims)
}

fn extract_presented_key(headers: &HeaderMap) -> Option<PresentedKey> {
    if let Some(value) = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        let mut parts = value.split_whitespace();
        if let (Some(scheme), Some(token)) = (parts.next(), parts.next()) {
            if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
                return Some(PresentedKey {
                    key: token.to_string(),
                    source: PresentedKeySource::AuthorizationBearer,
                });
            }
        }
    }

    if let Some(key) = headers
        .get(API_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    {
        return Some(PresentedKey {
            key,
            source: PresentedKeySource::ApiKeyHeader,
        });
    }

    headers
        .get(CLI_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|key| PresentedKey {
            key: key.to_string(),
            source: PresentedKeySource::CliTokenHeader,
        })
}

pub fn require_api_key(headers: &HeaderMap, db: &Db) -> Result<ApiKey, AuthError> {
    let presented = extract_presented_key(headers).ok_or(AuthError::Missing)?;
    let snapshot = db.snapshot();
    let api_key = snapshot
        .api_keys
        .iter()
        .find(|api_key| api_key.key == presented.key)
        .cloned()
        .ok_or(AuthError::Invalid)?;

    if !api_key.is_active() {
        return Err(AuthError::Inactive);
    }

    if presented.source == PresentedKeySource::CliTokenHeader {
        validate_cli_token(&presented.key, &api_key)?;
    }

    Ok(api_key)
}

fn validate_cli_token(token: &str, api_key: &ApiKey) -> Result<(), AuthError> {
    let Some(expected_machine_id) = api_key
        .machine_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let parsed = parse_api_key(token).ok_or(AuthError::Invalid)?;
    match parsed.machine_id.as_deref() {
        Some(machine_id) if machine_id == expected_machine_id => Ok(()),
        _ => Err(AuthError::Invalid),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::{extract_api_key, API_KEY_HEADER, AUTHORIZATION_HEADER};
    use crate::core::auth::CLI_TOKEN_HEADER;

    #[test]
    fn extract_api_key_preserves_header_precedence_with_cli_token_fallback() {
        let mut headers = HeaderMap::new();
        headers.insert(CLI_TOKEN_HEADER, HeaderValue::from_static("cli-token"));
        assert_eq!(extract_api_key(&headers).as_deref(), Some("cli-token"));

        headers.insert(API_KEY_HEADER, HeaderValue::from_static("x-api-key-token"));
        assert_eq!(
            extract_api_key(&headers).as_deref(),
            Some("x-api-key-token")
        );

        headers.insert(
            AUTHORIZATION_HEADER,
            HeaderValue::from_static("Bearer bearer-token"),
        );
        assert_eq!(extract_api_key(&headers).as_deref(), Some("bearer-token"));
    }
}
