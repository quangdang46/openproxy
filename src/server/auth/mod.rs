use axum::http::HeaderMap;
use dashmap::DashSet;
use jsonwebtoken::{decode, DecodingKey, Validation};
use once_cell::sync::Lazy;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::auth::{parse_api_key, CLI_TOKEN_HEADER};
use crate::db::Db;
use crate::types::ApiKey;

pub mod login_limiter;
pub mod oidc;

pub const API_KEY_HEADER: &str = "x-api-key";
pub const AUTHORIZATION_HEADER: &str = "authorization";
pub const AUTH_COOKIE_NAME: &str = "auth_token";

/// Resolves the JWT signing secret at runtime:
/// 1. `JWT_SECRET` env var if set and non-empty.
/// 2. Otherwise a cryptographically-random 256-bit hex string generated
///    exactly once per process lifetime. This means the secret changes
///    on every server restart, invalidating all existing sessions.
static JWT_SECRET: Lazy<String> = Lazy::new(|| {
    std::env::var("JWT_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let mut buf = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut buf);
            hex::encode(buf)
        })
});

/// Revoked JWT `jti` (JWT ID) values. Populated on logout; checked during
/// every `require_dashboard_session` call. Entries are never evicted — the
/// session TTL caps at 7d and the in-memory cost is negligible.
static REVOKED_JTIS: Lazy<DashSet<String>> = Lazy::new(DashSet::new);

/// Monotonically-increasing epoch used for bulk token invalidation. Each time
/// we need to revoke *all* outstanding tokens (e.g. on password change) the
/// epoch is incremented. Tokens signed with an older epoch are rejected by
/// [`require_dashboard_session`].
///
/// The epoch is embedded in `DashboardClaims.jti` as a prefix
/// (`"<epoch>:<uuid>"`). This avoids an extra claim field and keeps the
/// blocklist size small — only the per-token jtis need DashSet entries, while
/// epoch-wide revocations are handled by a simple integer compare.
static TOKEN_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Expose the resolved JWT secret (env var or random fallback) for use by
/// downstream modules that need to sign tokens.
pub fn jwt_secret() -> &'static str {
    &JWT_SECRET
}

/// Generate a `jti` value that embeds the current token epoch.
/// The format is `<epoch>:<uuid>`.
pub fn generate_jti() -> String {
    let epoch = TOKEN_EPOCH.load(Ordering::Relaxed);
    let id = uuid::Uuid::new_v4();
    format!("{epoch}:{id}")
}

/// Parse a `jti` and check whether its epoch matches the current token epoch.
/// Returns `true` if the token was issued under the current (valid) epoch.
pub fn is_jti_valid(jti: &str) -> bool {
    let Some(epoch_str) = jti.split(':').next() else {
        return false;
    };
    let Ok(epoch) = epoch_str.parse::<u64>() else {
        return false;
    };
    epoch == TOKEN_EPOCH.load(Ordering::Relaxed)
}

/// Increment the global token epoch, effectively invalidating all tokens ever
/// issued before this call — including those not in the per-jti blocklist.
/// Use this for sensitive operations such as password changes.
pub fn increment_token_epoch() {
    TOKEN_EPOCH.fetch_add(1, Ordering::Relaxed);
}

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
    /// JWT ID — a unique per-token identifier. Used for revocation via
    /// [`REVOKED_JTIS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
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
    if !snapshot.settings.require_login {
        return Ok(DashboardClaims {
            authenticated: true,
            exp: usize::MAX,
            jti: None,
        });
    }

    let token = extract_auth_token(headers).ok_or(DashboardAuthError::Missing)?;
    let validation = Validation::default();
    let decoded = decode::<DashboardClaims>(
        &token,
        &DecodingKey::from_secret(JWT_SECRET.as_bytes()),
        &validation,
    )
    .map_err(|_| DashboardAuthError::Invalid)?;
    if !decoded.claims.authenticated {
        return Err(DashboardAuthError::Invalid);
    }
    // Reject tokens from a previous epoch (password change, bulk revoke).
    if let Some(ref jti) = decoded.claims.jti {
        if !is_jti_valid(jti) {
            return Err(DashboardAuthError::Invalid);
        }
        // Reject individually-revoked tokens (per-session logout).
        if REVOKED_JTIS.contains(jti) {
            return Err(DashboardAuthError::Invalid);
        }
    }
    Ok(decoded.claims)
}

/// Revoke a dashboard session by its `jti` (JWT ID). The revoked token will
/// be rejected by [`require_dashboard_session`] on subsequent requests.
/// Idempotent: calling this multiple times with the same `jti` is a no-op.
pub fn revoke_jti(jti: &str) {
    REVOKED_JTIS.insert(jti.to_string());
}

fn extract_presented_key(headers: &HeaderMap) -> Option<PresentedKey> {
    // Debug: log all header names
    let header_names: Vec<String> = headers.keys().map(|k| k.to_string()).collect();
    tracing::debug!("extract_presented_key: headers={:?}, has_authorization={}", 
        header_names, headers.get(AUTHORIZATION_HEADER).is_some());
    
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
