use std::sync::Arc;

use tokio::sync::RwLock;
use std::collections::HashMap;

use crate::core::account_fallback::AccountRegistry;
use crate::core::executor::ClientPool;
use crate::core::usage::UsageTracker;
use crate::db::Db;
use crate::oauth::pending::PendingFlowStore;

/// Session info stored server-side
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub api_key_id: String,
    pub created_at: i64,
    pub last_active: i64,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub client_pool: Arc<ClientPool>,
    pub pending_flows: PendingFlowStore,
    pub account_registry: Arc<AccountRegistry>,
    pub sessions: Arc<RwLock<HashMap<String, SessionInfo>>>,
}

impl AppState {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            client_pool: Arc::new(ClientPool::new()),
            pending_flows: PendingFlowStore::new(),
            account_registry: Arc::new(AccountRegistry::default()),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Returns a UsageTracker for tracking request/response usage.
    /// The tracker is created fresh each call to ensure it picks up
    /// the latest pricing configuration from the database.
    pub fn usage_tracker(&self) -> UsageTracker {
        UsageTracker::new(self.db.clone())
    }
}
