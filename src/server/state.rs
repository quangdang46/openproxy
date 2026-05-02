use std::sync::Arc;

use crate::core::account_fallback::AccountRegistry;
use crate::core::executor::ClientPool;
use crate::core::usage::UsageTracker;
use crate::db::Db;
use crate::oauth::pending::PendingFlowStore;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub client_pool: Arc<ClientPool>,
    pub pending_flows: PendingFlowStore,
    pub account_registry: Arc<AccountRegistry>,
}

impl AppState {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            client_pool: Arc::new(ClientPool::new()),
            pending_flows: PendingFlowStore::new(),
            account_registry: Arc::new(AccountRegistry::default()),
        }
    }

    /// Returns a UsageTracker for tracking request/response usage.
    /// The tracker is created fresh each call to ensure it picks up
    /// the latest pricing configuration from the database.
    pub fn usage_tracker(&self) -> UsageTracker {
        UsageTracker::new(self.db.clone())
    }
}
