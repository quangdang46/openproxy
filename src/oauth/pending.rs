//! Temporary storage for OAuth PKCE flows pending completion
//!
//! Stores pending authorization code flows with their code_verifiers and state
//! parameters. Flows expire after TTL to prevent security issues.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default TTL for pending OAuth flows in seconds (10 minutes)
const DEFAULT_TTL_SECS: i64 = 600;

/// Error types for pending flow operations
#[derive(Debug, Clone)]
pub enum PendingError {
    /// Flow insertion failed due to internal error
    InsertFailed,
    /// Flow not found
    NotFound,
    /// Flow has expired
    Expired,
}

/// Credentials for Kiro AWS SSO OIDC flow
#[derive(Debug, Clone)]
pub struct KiroCredentials {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone)]
pub struct PendingOAuthFlow {
    pub state: String,
    pub code_verifier: String,
    pub provider: String,
    pub account_id: String,
    pub redirect_uri: Option<String>,
    pub device_code: Option<String>,
    pub user_code: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    /// Kiro OIDC client credentials, set when using Kiro's special device code flow
    pub kiro_credentials: Option<KiroCredentials>,
}

impl PendingOAuthFlow {
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.expires_at < now
    }
}

/// Thread-safe in-memory store for pending OAuth flows
#[derive(Clone)]
pub struct PendingFlowStore {
    store: Arc<RwLock<HashMap<String, PendingOAuthFlow>>>,
}

impl PendingFlowStore {
    pub fn new() -> Self {
        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Insert a pending flow into the store.
    /// Overwrites any existing flow with the same state (idempotent for retry).
    pub fn insert(&self, flow: PendingOAuthFlow) -> Result<(), PendingError> {
        let state = flow.state.clone();
        let mut store = self.store.write().map_err(|_| PendingError::InsertFailed)?;
        store.insert(state, flow);
        Ok(())
    }

    /// Retrieve a pending flow by its state parameter.
    /// Returns None if not found or if the flow has expired.
    pub fn get(&self, state: &str) -> Option<PendingOAuthFlow> {
        let store = self.store.read().ok()?;
        let flow = store.get(state)?;
        if flow.is_expired() {
            return None;
        }
        Some(flow.clone())
    }

    /// Remove and return a pending flow by its state parameter.
    /// Returns None if not found.
    pub fn remove(&self, state: &str) -> Option<PendingOAuthFlow> {
        let mut store = self.store.write().ok()?;
        store.remove(state)
    }

    pub fn cleanup_expired(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        if let Ok(mut store) = self.store.write() {
            store.retain(|_, flow| flow.expires_at >= now);
        }
    }

    /// Returns the number of pending flows currently stored.
    pub fn len(&self) -> usize {
        self.store.read().map(|s| s.len()).unwrap_or(0)
    }

    /// Returns true if there are no pending flows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for PendingFlowStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_flow(state: &str, expires_in_secs: i64) -> PendingOAuthFlow {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        PendingOAuthFlow {
            state: state.to_string(),
            code_verifier: "test_verifier".to_string(),
            provider: "claude".to_string(),
            account_id: "account_123".to_string(),
            redirect_uri: None,
            device_code: None,
            user_code: None,
            created_at: now,
            expires_at: now + expires_in_secs,
            kiro_credentials: None,
        }
    }

    #[test]
    fn test_insert_and_get() {
        let store = PendingFlowStore::new();
        let flow = create_test_flow("state1", 600);
        store.insert(flow.clone()).unwrap();

        let retrieved = store.get("state1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().state, "state1");
    }

    #[test]
    fn test_get_not_found() {
        let store = PendingFlowStore::new();
        let retrieved = store.get("nonexistent");
        assert!(retrieved.is_none());
    }

    #[test]
    fn test_remove() {
        let store = PendingFlowStore::new();
        let flow = create_test_flow("state1", 600);
        store.insert(flow).unwrap();

        let removed = store.remove("state1");
        assert!(removed.is_some());

        let retrieved = store.get("state1");
        assert!(retrieved.is_none());
    }

    #[test]
    fn test_expired_flow() {
        let store = PendingFlowStore::new();
        // Create a flow that expired in the past
        let flow = create_test_flow("expired", -1);
        store.insert(flow).unwrap();

        let retrieved = store.get("expired");
        assert!(retrieved.is_none());
    }

    #[test]
    fn test_cleanup_expired() {
        let store = PendingFlowStore::new();
        // Insert one valid and one expired flow
        let valid_flow = create_test_flow("valid", 600);
        let expired_flow = create_test_flow("expired", -1);
        store.insert(valid_flow).unwrap();
        store.insert(expired_flow).unwrap();

        store.cleanup_expired();

        assert!(store.get("valid").is_some());
        assert!(store.get("expired").is_none());
    }

    #[test]
    fn test_len() {
        let store = PendingFlowStore::new();
        assert_eq!(store.len(), 0);

        store.insert(create_test_flow("state1", 600)).unwrap();
        assert_eq!(store.len(), 1);

        store.insert(create_test_flow("state2", 600)).unwrap();
        assert_eq!(store.len(), 2);

        store.remove("state1");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_insert_overwrites_existing() {
        let store = PendingFlowStore::new();
        let flow1 = create_test_flow("state1", 600);
        let flow2 = create_test_flow("state1", 300); // Same state, different expiry

        store.insert(flow1).unwrap();
        store.insert(flow2).unwrap();

        // Should still only have one entry
        assert_eq!(store.len(), 1);
    }
}