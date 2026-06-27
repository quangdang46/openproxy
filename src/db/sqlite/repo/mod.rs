//! Repository layer — per-table CRUD backed by [`super::SqliteDb`].
//!
//! Each function takes `&mut Connection` so callers can bundle multiple
//! operations into a single transaction via `SqliteDb::with_transaction`.

pub mod api_key_repo;
pub mod combo_repo;
pub mod connection_repo;
pub mod kv_repo;
pub mod node_repo;
pub mod pool_repo;
pub mod request_repo;
pub mod usage_repo;
