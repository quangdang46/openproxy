use axum::http::HeaderMap;
use dashmap::DashMap;
use jsonwebtoken::{decode, DecodingKey, Validation};
use once_cell::sync::Lazy;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::core::auth::{parse_api_key, CLI_TOKEN_HEADER};
use crate::db::Db;
use crate::types::ApiKey;

pub mod login_limiter;
pub mod oidc;
pub mod revocations;
pub mod saml;

use revocations::RevocationStore;
pub use revocations::JTI_CLEANUP_TTL_SECS;

/// Periodically removes expired revocations. Dashboard JWT tokens have a max
/// TTL of 7 days, so any revocation record older than that can never match a
/// live token. Runs once at startup and then every hour.
pub fn spawn_jti_cleanup() {
    tokio::spawn(async move {
        loop {
            // Run once at startup as well, then every hour.
            cleanup_expired_jtis();
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
}

pub const API_KEY_HEADER: &str = "x-api-key";
pub const AUTHORIZATION_HEADER: &str = "authorization";
pub const AUTH_COOKIE_NAME: &str = "auth_token";
pub const GOOGLE_API_KEY_HEADER: &str = "x-goog-api-key";

/// Internal header injected by [`crate::server::middleware::incoming_request_middleware`]
/// when the request URI carries a `?key=` query parameter. Checked last in
/// the priority chain so that header-based auth always wins.
pub const QUERY_KEY_HEADER: &str = "x-9r-query-key";

/// Resolves the JWT signing secret at runtime:
/// 1. `JWT_SECRET` env var if set and non-empty.
/// 2. Otherwise a cryptographically-random 256-bit hex string generated
///    exactly once per process lifetime. This means the secret changes
///    on every server restart, invalidating all existing sessions.
/// Placeholder that earlier versions of `.env.example` shipped in the clear.
/// Anyone who has read a public copy of this repository knows it, so a session
/// signed with it is forgeable by anyone. Refuse it outright rather than
/// warning and continuing.
pub const KNOWN_INSECURE_JWT_PLACEHOLDER: &str = "openproxy-default-secret-change-me";

static JWT_SECRET: Lazy<String> = Lazy::new(|| {
    if let Ok(secret) = std::env::var("JWT_SECRET") {
        if secret == KNOWN_INSECURE_JWT_PLACEHOLDER {
            panic!(
                "JWT_SECRET is set to the placeholder value that shipped in older \
                 .env.example files. Anyone can read it, so dashboard sessions \
                 signed with it are forgeable. Unset JWT_SECRET for a random \
                 per-process secret, or set it to $(openssl rand -hex 32)."
            );
        }
    }
    std::env::var("JWT_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let mut buf = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut buf);
            hex::encode(buf)
        })
});

/// The process-wide revocation store, rooted at a real data directory. It is
/// installed the first time a request carries a live `Db` (see
/// [`require_dashboard_session`]) or explicitly by [`init_revocation_store`].
///
/// It is durable: a logout or an epoch bump reaches `revoked-jtis.jsonl` and
/// `token-epoch` under the data directory before the call returns, so a
/// restarted process does not resurrect the sessions the user was told were
/// dead. See [`revocations`] for the on-disk contract.
static REVOCATIONS: Lazy<RwLock<Option<Arc<RevocationStore>>>> = Lazy::new(|| RwLock::new(None));

/// Revocations and epoch bumps recorded before any data directory was known —
/// a logout route, or a password change, can run before the first request that
/// reaches the session gate. Kept in memory (exactly as they always were) and
/// folded into the durable store the moment one is installed, so nothing
/// observed in that window is silently dropped.
///
/// Deliberately not a `RevocationStore`: there is no directory to write to, and
/// guessing one would put test state in the operator's real data dir.
static PENDING_REVOCATIONS: Lazy<DashMap<String, u64>> = Lazy::new(DashMap::new);
static PENDING_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Root a fresh [`RevocationStore`] at `data_dir` and make it the process-wide
/// one, replacing whatever was installed before. Tests use this to stand up a
/// new process over the same directory.
pub fn init_revocation_store(data_dir: &Path) -> Arc<RevocationStore> {
    let store = Arc::new(RevocationStore::new(data_dir));
    store.load();
    *lock_installed() = Some(store.clone());
    store
}

/// The installed store, or `None` while it is still unanchored.
fn installed() -> Option<Arc<RevocationStore>> {
    lock_installed().clone()
}

/// The store for a request that has the live `Db` in hand. The `Db`'s data
/// directory is the authoritative one, so this is where the store gets rooted
/// — and where anything recorded by the pre-anchor handlers is adopted.
fn store_for_db(data_dir: &Path) -> Arc<RevocationStore> {
    if let Some(store) = installed() {
        return store;
    }
    let store = init_revocation_store(data_dir);
    // Adopt before anyone can observe the store: a logout that landed moments
    // ago must not vanish just because the first gated request arrived after it.
    for entry in PENDING_REVOCATIONS.iter() {
        store.revoke(entry.key().as_str());
    }
    if PENDING_EPOCH.load(Ordering::Relaxed) > store.epoch() {
        store.set_epoch(PENDING_EPOCH.load(Ordering::Relaxed));
    }
    PENDING_REVOCATIONS.clear();
    PENDING_EPOCH.store(store.epoch(), Ordering::Relaxed);
    store
}

fn lock_installed() -> std::sync::RwLockWriteGuard<'static, Option<Arc<RevocationStore>>> {
    REVOCATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Expose the resolved JWT secret (env var or random fallback) for use by
/// downstream modules that need to sign tokens.
pub fn jwt_secret() -> &'static str {
    &JWT_SECRET
}

/// Generate a `jti` value that embeds the current token epoch.
/// The format is `<epoch>:<uuid>`.
pub fn generate_jti() -> String {
    let epoch = installed()
        .map(|store| store.epoch())
        .unwrap_or_else(|| PENDING_EPOCH.load(Ordering::Relaxed));
    let id = uuid::Uuid::new_v4();
    format!("{epoch}:{id}")
}

/// Parse a `jti` and check whether its epoch matches the current token epoch.
/// Returns `true` if the token was issued under the current (valid) epoch.
pub fn is_jti_valid(jti: &str) -> bool {
    let Some(epoch) = jti.split(':').next().and_then(|s| s.parse::<u64>().ok()) else {
        return false;
    };
    let current = installed()
        .map(|store| store.epoch())
        .unwrap_or_else(|| PENDING_EPOCH.load(Ordering::Relaxed));
    epoch == current
}

/// Increment the global token epoch, effectively invalidating all tokens ever
/// issued before this call — including those not in the per-jti blocklist.
/// Use this for sensitive operations such as password changes.
///
/// Once the store is rooted the new epoch is on disk before this returns, so a
/// restart cannot undo the invalidation the caller is about to report.
pub fn increment_token_epoch() {
    match installed() {
        Some(store) => {
            store.bump_epoch();
        }
        None => {
            PENDING_EPOCH.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentedKeySource {
    AuthorizationBearer,
    ApiKeyHeader,
    GoogleApiKeyHeader,
    CliTokenHeader,
    QueryKeyParam,
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
    /// [`revocations::RevocationStore`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
    /// OIDC identity claims (embedded by the OIDC callback) — used by
    /// `/api/auth/status` to render the header identity chip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// SAML identity claims (embedded by the SAML ACS handler, 9router
    /// saml/acs/route.js: `setDashboardAuthCookie({ saml: true, samlEmail,
    /// samlName })`). `saml: true` marks the session; name/email carry the
    /// picked display claims. Serialized snake_case (saml_name/saml_email)
    /// because the ACS handler builds the JWT from a serde_json map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saml: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saml_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saml_email: Option<String>,
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
            name: None,
            email: None,
            saml: None,
            saml_name: None,
            saml_email: None,
        });
    }

    let token = extract_auth_token(headers).ok_or(DashboardAuthError::Missing)?;
    let revocations = store_for_db(&db.data_dir);
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
    // A jti is MANDATORY, not optional.
    //
    // It used to be `if let Some(ref jti) = ...`, so a token carrying no jti
    // skipped BOTH the epoch check and the per-session revocation check. The
    // signature still had to verify, so this was not a forgery on its own —
    // but it meant that once the signing secret was known (a copied
    // .env.example default is one way), an attacker could mint a token that
    // ignored a password change AND ignored logout. Every token this server
    // issues carries a jti (see generate_jti), so requiring one breaks
    // nothing legitimate and closes the bypass.
    let Some(ref jti) = decoded.claims.jti else {
        return Err(DashboardAuthError::Invalid);
    };
    // Reject tokens from a previous epoch (password change, bulk revoke).
    if !revocations.is_jti_valid(jti) {
        return Err(DashboardAuthError::Invalid);
    }
    // Reject individually-revoked tokens (per-session logout).
    if revocations.is_revoked(jti) {
        return Err(DashboardAuthError::Invalid);
    }
    Ok(decoded.claims)
}

/// Revoke a dashboard session by its `jti` (JWT ID). The revoked token will
/// be rejected by [`require_dashboard_session`] on subsequent requests, and by
/// the next process to start. Idempotent: calling this multiple times with the
/// same `jti` is a no-op.
/// The revocation timestamp is recorded so that [`cleanup_expired_jtis`] can
/// evict stale entries.
pub fn revoke_jti(jti: &str) {
    match installed() {
        Some(store) => store.revoke(jti),
        None => {
            PENDING_REVOCATIONS.insert(jti.to_string(), revocations::now_unix());
        }
    }
}

/// Remove entries from the revocation store that are older than
/// [`JTI_CLEANUP_TTL_SECS`], and compact the on-disk log so it does not grow
/// without bound. Dashboard JWT tokens have a max TTL of 7 days, so any
/// revocation record older than that can never match a live token and is safe
/// to evict.
///
/// Called periodically by a background task spawned in [`spawn_jti_cleanup`],
/// which also runs once at startup so a file left stale by an older build heals
/// on the first boot.
pub fn cleanup_expired_jtis() {
    match installed() {
        Some(store) => {
            store.prune(revocations::now_unix());
        }
        None => {
            let cutoff = revocations::now_unix().saturating_sub(JTI_CLEANUP_TTL_SECS);
            PENDING_REVOCATIONS.retain(|_jti, revoked_at| *revoked_at > cutoff);
        }
    }
}

fn extract_presented_key(headers: &HeaderMap) -> Option<PresentedKey> {
    // Debug: log all header names
    let header_names: Vec<String> = headers.keys().map(|k| k.to_string()).collect();
    tracing::debug!(
        "extract_presented_key: headers={:?}, has_authorization={}",
        header_names,
        headers.get(AUTHORIZATION_HEADER).is_some()
    );

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

    if let Some(key) = headers
        .get(GOOGLE_API_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    {
        return Some(PresentedKey {
            key,
            source: PresentedKeySource::GoogleApiKeyHeader,
        });
    }

    if let Some(key) = headers
        .get(CLI_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    {
        return Some(PresentedKey {
            key,
            source: PresentedKeySource::CliTokenHeader,
        });
    }

    headers
        .get(QUERY_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|key| PresentedKey {
            key: key.to_string(),
            source: PresentedKeySource::QueryKeyParam,
        })
}

pub fn require_api_key(headers: &HeaderMap, db: &Db) -> Result<ApiKey, AuthError> {
    let presented = extract_presented_key(headers).ok_or(AuthError::Missing)?;
    let snapshot = db.snapshot();
    let api_key = snapshot
        .api_key_map
        .get(&presented.key)
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

/// Like [`require_api_key`] but reloads the snapshot once on `AuthError::Invalid`
/// before giving up. Handles the case where the CLI added a key while the
/// server was already running and the in-memory snapshot is stale.
pub async fn require_api_key_with_reload(
    headers: &HeaderMap,
    db: &Db,
) -> Result<ApiKey, AuthError> {
    match require_api_key(headers, db) {
        Err(AuthError::Invalid) => {
            // Snapshot may be stale: the CLI wrote a new key via SQLite
            // but the server hasn't seen it yet. Reload once and retry.
            if db.reload_snapshot().await.is_ok() {
                require_api_key(headers, db)
            } else {
                Err(AuthError::Invalid)
            }
        }
        other => other,
    }
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
