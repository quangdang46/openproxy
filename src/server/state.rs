use std::sync::Arc;

use std::collections::HashMap;
use tokio::sync::RwLock;

use crate::core::account_fallback::AccountRegistry;
use crate::core::executor::ClientPool;
use crate::core::tunnel::TunnelManager;
use crate::core::usage::UsageTracker;
use crate::db::Db;
use crate::oauth::pending::PendingFlowStore;
use crate::server::console_logs::{shared_console_log_buffer, ConsoleLogBuffer};
use crate::server::usage_live::UsageLiveState;

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
    pub tunnel_manager: Arc<TunnelManager>,
    pub pending_flows: PendingFlowStore,
    pub account_registry: Arc<AccountRegistry>,
    pub console_logs: Arc<ConsoleLogBuffer>,
    pub usage_live: Arc<UsageLiveState>,
    pub sessions: Arc<RwLock<HashMap<String, SessionInfo>>>,
}

impl AppState {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db: db.clone(),
            client_pool: Arc::new(ClientPool::new()),
            tunnel_manager: Arc::new(TunnelManager::new(db.clone())),
            pending_flows: PendingFlowStore::new(),
            account_registry: Arc::new(AccountRegistry::default()),
            console_logs: shared_console_log_buffer(),
            usage_live: Arc::new(UsageLiveState::new()),
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
