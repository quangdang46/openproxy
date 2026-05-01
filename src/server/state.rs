use std::sync::Arc;

use crate::core::executor::ClientPool;
use crate::db::Db;
use crate::oauth::pending::PendingFlowStore;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub client_pool: Arc<ClientPool>,
    pub pending_flows: PendingFlowStore,
}

impl AppState {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            client_pool: Arc::new(ClientPool::new()),
            pending_flows: PendingFlowStore::new(),
        }
    }
}
