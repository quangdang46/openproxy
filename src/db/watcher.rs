//! Background watcher — no-op since SQLite is now the sole runtime store.
//!
//! Previously this watched `db.json` / `usage.json` for changes made by the
//! CLI while the server was running. Now that all writes go through SQLite,
//! the in-memory snapshot is always up to date and the file watcher is no
//! longer needed. The function is retained as a compatibility stub.

use std::sync::Arc;

use crate::db::Db;

/// No-op — SQLite is the sole runtime store; no external files to watch.
pub fn spawn_watcher(db: Arc<Db>) {
    let _ = db;
}
