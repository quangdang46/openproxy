//! Centralized OAuth credential refresh manager.
//!
//! Tracks per-provider credential state (access_token, refresh_token,
//! expires_at) in a concurrent `DashMap`, provides per-provider mutual
//! exclusion for refresh operations, and detects unrecoverable errors
//! such as `invalid_grant` and `revoked` tokens.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::Mutex;

use crate::core::config::app_constants::refresh_lead;
use crate::oauth::token_refresh::{dispatch_oauth_refresh, needs_refresh_with_lead};
use crate::oauth::TOKEN_EXPIRY_BUFFER_MS;
use crate::types::ProviderConnection;

/// In-memory credential state for a single provider connection.
#[derive(Clone, Debug)]
pub struct CredentialState {
    /// The current access token.
    pub access_token: String,
    /// Optional refresh token.
    pub refresh_token: Option<String>,
    /// RFC 3339 timestamp of when the access token expires.
    pub expires_at: Option<String>,
}

/// Centralized credential refresh manager.
///
/// Maintains per-provider credential state and refresh locks, so that
/// concurrent callers for the same provider do not issue duplicate
/// refresh requests.
pub struct CredentialManager {
    /// Map from provider key (e.g. `"claude:conn_abc"`) to credential state.
    states: DashMap<String, CredentialState>,
    /// Per-key mutex to serialize concurrent refresh attempts for the
    /// same provider+connection pair.
    locks: DashMap<String, Arc<Mutex<()>>>,
}

impl Default for CredentialManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialManager {
    pub fn new() -> Self {
        Self {
            states: DashMap::new(),
            locks: DashMap::new(),
        }
    }

    /// Return the lock key for a provider + connection_id pair.
    fn lock_key(provider: &str, conn_id: &str) -> String {
        format!("{}:{}", provider, conn_id)
    }

    /// Check whether the stored credential state is still valid, or
    /// proactively refresh when the token is close to expiry.
    ///
    /// Uses the provider-specific lead time from `app_constants::refresh_lead`,
    /// falling back to `TOKEN_EXPIRY_BUFFER_MS` (5 minutes) when no lead is
    /// configured for the provider.
    ///
    /// On success, returns an updated `ProviderConnection` with fresh tokens.
    /// On unrecoverable errors (e.g. `invalid_grant`, `revoked`), returns the
    /// error string.
    pub async fn refresh_if_needed(
        &self,
        provider: &str,
        creds: &ProviderConnection,
    ) -> Result<ProviderConnection, String> {
        let key = Self::lock_key(provider, &creds.id);

        // Fast path: check whether a refresh is needed based on stored state
        // (or the connection's own expiry if we have no in-memory state yet).
        let needs_refresh = self.check_needs_refresh(provider, creds, &key);

        if !needs_refresh {
            return Ok(build_connection_from(creds));
        }

        // Ensure we have a refresh token to work with.
        let refresh_token = creds
            .refresh_token
            .as_deref()
            .ok_or_else(|| "No refresh token available for credential refresh".to_string())?;

        // Acquire the per-provider lock.  Multiple tasks refreshing the same
        // provider+connection pair will serialize here.
        let lock = {
            self.locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .value()
                .clone()
        };

        let _guard = lock.lock().await;

        // Double-check after acquiring the lock: another task may have
        // refreshed in the meantime.
        if !self.check_needs_refresh(provider, creds, &key) {
            return Ok(build_connection_from(creds));
        }

        // Perform the refresh via the existing dispatch layer.  The dispatch
        // function handles per-provider HTTP refresh, deduplication, and retry.
        let result =
            dispatch_oauth_refresh(provider, refresh_token, &creds.provider_specific_data).await?;

        // Validate the response.
        let access_token = result.access_token;

        // Preserve the existing refresh token if the response did not include
        // a new one (some providers do not rotate the refresh token).
        let new_refresh_token = result.refresh_token.or_else(|| creds.refresh_token.clone());

        // Compute the new expiry timestamp from `expires_in` (seconds).
        let expires_at: Option<String> = result.expires_in.map(|expires_in| {
            let expiry = chrono::Utc::now() + chrono::Duration::seconds(expires_in);
            expiry.to_rfc3339()
        });

        // Update in-memory state.
        let new_state = CredentialState {
            access_token: access_token.clone(),
            refresh_token: new_refresh_token.clone(),
            expires_at: expires_at.clone(),
        };
        self.states.insert(key, new_state);

        // Build and return the updated ProviderConnection.
        let mut conn = build_connection_from(creds);
        conn.access_token = Some(access_token);
        conn.refresh_token = new_refresh_token;
        conn.expires_at = expires_at;
        Ok(conn)
    }

    /// Determine whether a refresh should be attempted for the given provider
    /// connection, consulting both the in-memory state and the connection's
    /// own `expires_at` timestamp.
    fn check_needs_refresh(&self, provider: &str, creds: &ProviderConnection, key: &str) -> bool {
        // If there is no refresh token, we can never refresh.
        if creds.refresh_token.is_none() {
            return false;
        }

        let lead_ms = refresh_lead(provider)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(TOKEN_EXPIRY_BUFFER_MS);

        // Prefer in-memory state over the connection's own fields.
        if let Some(state) = self.states.get(key) {
            return needs_refresh_with_lead(&state.expires_at, lead_ms);
        }

        // No in-memory state — check the connection's own expiry.
        needs_refresh_with_lead(&creds.expires_at, lead_ms)
    }

    /// Check whether a given error from `refresh_if_needed` is unrecoverable.
    ///
    /// Unrecoverable errors include `invalid_grant`, `revoked`, or similar
    /// token-invalid responses that indicate the user must re-authenticate
    /// via the OAuth flow again.
    pub fn is_unrecoverable_error(err: &str) -> bool {
        let low = err.to_lowercase();
        low.contains("invalid_grant")
            || low.contains("invalid grant")
            || low.contains("revoked")
            || low.contains("access_denied")
            || low.contains("expired_token")
            || low.contains("invalid_client")
            || low.contains("unauthorized_client")
    }

    /// Remove the stored state for a given provider+connection pair.
    ///
    /// The next call to `refresh_if_needed` will re-evaluate from the
    /// connection's own fields.
    pub fn invalidate(&self, provider: &str, conn_id: &str) {
        let key = Self::lock_key(provider, conn_id);
        self.states.remove(&key);
    }

    /// Clear all stored credential state.
    pub fn clear(&self) {
        self.states.clear();
    }

    /// Return the number of tracked credential states (for diagnostics).
    pub fn tracked_count(&self) -> usize {
        self.states.len()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a new `ProviderConnection` copying all fields from `src`.
///
/// This is necessary because `ProviderConnection` does not derive `Clone`.
fn build_connection_from(src: &ProviderConnection) -> ProviderConnection {
    ProviderConnection {
        id: src.id.clone(),
        provider: src.provider.clone(),
        auth_type: src.auth_type.clone(),
        name: src.name.clone(),
        priority: src.priority,
        is_active: src.is_active,
        created_at: src.created_at.clone(),
        updated_at: src.updated_at.clone(),
        display_name: src.display_name.clone(),
        email: src.email.clone(),
        global_priority: src.global_priority,
        default_model: src.default_model.clone(),
        access_token: src.access_token.clone(),
        refresh_token: src.refresh_token.clone(),
        expires_at: src.expires_at.clone(),
        token_type: src.token_type.clone(),
        scope: src.scope.clone(),
        id_token: src.id_token.clone(),
        project_id: src.project_id.clone(),
        api_key: src.api_key.clone(),
        test_status: src.test_status.clone(),
        last_tested: src.last_tested.clone(),
        last_error: src.last_error.clone(),
        last_error_at: src.last_error_at.clone(),
        rate_limited_until: src.rate_limited_until.clone(),
        expires_in: src.expires_in,
        error_code: src.error_code.clone(),
        consecutive_use_count: src.consecutive_use_count,
        backoff_level: src.backoff_level,
        consecutive_errors: src.consecutive_errors,
        proxy_url: src.proxy_url.clone(),
        proxy_label: src.proxy_label.clone(),
        use_connection_proxy: src.use_connection_proxy,
        runtime_transport: src.runtime_transport.as_ref().map(|rt| {
            crate::types::RuntimeTransport {
                base_url: rt.base_url.clone(),
            }
        }),
        provider_specific_data: src.provider_specific_data.clone(),
        extra: src.extra.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- is_unrecoverable_error --

    #[test]
    fn test_is_unrecoverable_error_positive() {
        assert!(CredentialManager::is_unrecoverable_error("invalid_grant"));
        assert!(CredentialManager::is_unrecoverable_error(
            "The authorization grant has been revoked"
        ));
        assert!(CredentialManager::is_unrecoverable_error("access_denied"));
        assert!(CredentialManager::is_unrecoverable_error("invalid_client"));
        assert!(CredentialManager::is_unrecoverable_error(
            "unauthorized_client"
        ));
        assert!(CredentialManager::is_unrecoverable_error("expired_token"));
    }

    #[test]
    fn test_is_unrecoverable_error_negative() {
        assert!(!CredentialManager::is_unrecoverable_error("network error"));
        assert!(!CredentialManager::is_unrecoverable_error(
            "500 internal server error"
        ));
        assert!(!CredentialManager::is_unrecoverable_error("timeout"));
        assert!(!CredentialManager::is_unrecoverable_error(
            "429 too many requests"
        ));
    }

    // -- lock_key --

    #[test]
    fn test_lock_key_format() {
        assert_eq!(
            CredentialManager::lock_key("claude", "conn_abc123"),
            "claude:conn_abc123"
        );
        assert_eq!(
            CredentialManager::lock_key("codex", "conn_xyz"),
            "codex:conn_xyz"
        );
    }

    // -- new / clear / tracked_count --

    #[test]
    fn test_new_manager_is_empty() {
        let cm = CredentialManager::new();
        assert_eq!(cm.tracked_count(), 0);
        assert!(cm.states.is_empty());
        assert!(cm.locks.is_empty());
    }

    #[test]
    fn test_clear_removes_all_state() {
        let cm = CredentialManager::new();
        cm.states.insert(
            "test:conn".into(),
            CredentialState {
                access_token: "tok".into(),
                refresh_token: None,
                expires_at: None,
            },
        );
        assert_eq!(cm.tracked_count(), 1);
        cm.clear();
        assert_eq!(cm.tracked_count(), 0);
    }

    #[test]
    fn test_invalidate_removes_specific_key() {
        let cm = CredentialManager::new();
        cm.states.insert(
            "p:conn_a".into(),
            CredentialState {
                access_token: "a".into(),
                refresh_token: None,
                expires_at: None,
            },
        );
        cm.states.insert(
            "p:conn_b".into(),
            CredentialState {
                access_token: "b".into(),
                refresh_token: None,
                expires_at: None,
            },
        );
        assert_eq!(cm.tracked_count(), 2);

        cm.invalidate("p", "conn_a");
        assert_eq!(cm.tracked_count(), 1);
        assert!(cm.states.get("p:conn_a").is_none());
        assert!(cm.states.get("p:conn_b").is_some());
    }

    // -- check_needs_refresh --

    #[test]
    fn test_check_needs_refresh_no_refresh_token() {
        let cm = CredentialManager::new();
        let conn = ProviderConnection {
            id: "id".into(),
            provider: "test".into(),
            refresh_token: None,
            ..make_empty_connection()
        };
        assert!(!cm.check_needs_refresh("test", &conn, "test:id"));
    }

    #[test]
    fn test_check_needs_refresh_expired_triggers_refresh() {
        let cm = CredentialManager::new();
        let conn = ProviderConnection {
            id: "id".into(),
            provider: "test".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some("2020-01-01T00:00:00Z".into()),
            ..make_empty_connection()
        };
        assert!(cm.check_needs_refresh("test", &conn, "test:id"));
    }

    #[test]
    fn test_check_needs_refresh_future_skips_in_memory_state() {
        let cm = CredentialManager::new();
        // Far-future expiry — no refresh needed.
        let conn = ProviderConnection {
            id: "id".into(),
            provider: "test".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some("2099-12-31T23:59:59Z".into()),
            ..make_empty_connection()
        };
        assert!(!cm.check_needs_refresh("test", &conn, "test:id"));

        // But if in-memory state says it IS expired, use the in-memory value.
        cm.states.insert(
            "test:id".into(),
            CredentialState {
                access_token: "old".into(),
                refresh_token: Some("rt".into()),
                expires_at: Some("2020-01-01T00:00:00Z".into()),
            },
        );
        assert!(cm.check_needs_refresh("test", &conn, "test:id"));
    }

    // -- refresh_if_needed returns Ok when no refresh token (no-op) --

    #[tokio::test]
    async fn test_refresh_noop_when_no_refresh_token() {
        let cm = CredentialManager::new();
        let conn = ProviderConnection {
            id: "no_rt".into(),
            provider: "test".into(),
            refresh_token: None,
            ..make_empty_connection()
        };

        // No refresh needed because no refresh token -> returns Ok copy
        let result = cm.refresh_if_needed("test", &conn).await.unwrap();
        assert_eq!(result.id, "no_rt");
        assert!(result.access_token.is_none());
    }

    // -- build_connection_from round-trips fields --

    #[test]
    fn test_build_connection_from_preserves_fields() {
        let conn = ProviderConnection {
            id: "my_id".into(),
            provider: "my_provider".into(),
            auth_type: "oauth".into(),
            name: Some("My Conn".into()),
            priority: Some(10),
            is_active: Some(true),
            created_at: Some("2024-01-01T00:00:00Z".into()),
            updated_at: Some("2024-06-01T00:00:00Z".into()),
            display_name: Some("Display".into()),
            email: Some("test@example.com".into()),
            global_priority: Some(5),
            default_model: Some("gpt-4".into()),
            access_token: Some("at".into()),
            refresh_token: Some("rt".into()),
            expires_at: Some("2099-01-01T00:00:00Z".into()),
            token_type: Some("Bearer".into()),
            scope: Some("openid".into()),
            id_token: Some("idtok".into()),
            project_id: Some("proj".into()),
            api_key: Some("apikey".into()),
            test_status: Some("ok".into()),
            last_tested: Some("2024-06-01T00:00:00Z".into()),
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: Some(3600),
            error_code: None,
            consecutive_use_count: Some(10),
            backoff_level: Some(0),
            consecutive_errors: Some(0),
            proxy_url: Some("http://proxy".into()),
            proxy_label: Some("proxy".into()),
            use_connection_proxy: Some(false),
            runtime_transport: Some(crate::types::RuntimeTransport {
                base_url: Some("https://custom.url".into()),
            }),
            provider_specific_data: {
                let mut m = std::collections::BTreeMap::new();
                m.insert("key".into(), serde_json::json!("val"));
                m
            },
            extra: std::collections::BTreeMap::new(),
        };

        let built = build_connection_from(&conn);

        assert_eq!(built.id, "my_id");
        assert_eq!(built.provider, "my_provider");
        assert_eq!(built.access_token.as_deref(), Some("at"));
        assert_eq!(built.refresh_token.as_deref(), Some("rt"));
        assert_eq!(built.expires_at.as_deref(), Some("2099-01-01T00:00:00Z"));
        assert_eq!(built.name.as_deref(), Some("My Conn"));
        assert_eq!(built.priority, Some(10));
        assert_eq!(built.is_active, Some(true));
        assert_eq!(built.email.as_deref(), Some("test@example.com"));
        assert_eq!(
            built
                .provider_specific_data
                .get("key")
                .and_then(|v| v.as_str()),
            Some("val")
        );
        assert_eq!(
            built
                .runtime_transport
                .as_ref()
                .and_then(|rt| rt.base_url.as_deref()),
            Some("https://custom.url")
        );
    }

    // -----------------------------------------------------------------------
    // Helpers

    /// Returns a `ProviderConnection` with all optional fields set to `None`
    /// and string fields set to empty, suitable as a base for tests.
    fn make_empty_connection() -> ProviderConnection {
        ProviderConnection {
            id: String::new(),
            provider: String::new(),
            auth_type: String::new(),
            name: None,
            priority: None,
            is_active: None,
            created_at: None,
            updated_at: None,
            display_name: None,
            email: None,
            global_priority: None,
            default_model: None,
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: None,
            test_status: None,
            last_tested: None,
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: None,
            error_code: None,
            consecutive_use_count: None,
            backoff_level: None,
            consecutive_errors: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            runtime_transport: None,
            provider_specific_data: std::collections::BTreeMap::new(),
            extra: std::collections::BTreeMap::new(),
        }
    }
}
