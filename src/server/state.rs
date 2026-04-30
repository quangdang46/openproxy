use std::sync::Arc;

use crate::core::executor::ClientPool;
use crate::db::Db;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub client_pool: Arc<ClientPool>,
}

impl AppState {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            client_pool: Arc::new(ClientPool::new()),
        }
    }
}
