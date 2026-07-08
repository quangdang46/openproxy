//! SQLite backend for OpenProxy persistence.
//!
//! Provides a thin wrapper around `rusqlite::Connection` with:
//! - Synchronous connections wrapped in a `parking_lot::Mutex` (the rest of
//!   the codebase calls into the DB from a `spawn_blocking` task; sync is
//!   faster than async-sqlite for our workloads).
//! - PRAGMA setup on every connect.
//! - Idempotent DDL bootstrap.
//! - Schema-version migration runner.
//!
//! Higher-level repository logic (read/write per table) lives in
//! [`crate::db::sqlite::repo`]. JSON export/import lives in
//! [`crate::db::sqlite::export`] and [`crate::db::sqlite::import`].

pub mod export;
pub mod import;
pub mod migrations;
pub mod repo;
pub mod schema;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::Connection;

pub use schema::SCHEMA_VERSION;

/// Cheap handle to the OpenProxy SQLite DB. Cloning shares the same
/// connection (serialised through the mutex). Callers should perform
/// long transactions on a dedicated `Connection` (see [`connect`]).
#[derive(Clone)]
pub struct SqliteDb {
    inner: Arc<SqliteInner>,
}

struct SqliteInner {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl std::fmt::Debug for SqliteDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteDb")
            .field("path", &self.inner.path)
            .finish()
    }
}

impl SqliteDb {
    /// Open (or create) the SQLite DB at `path`. Runs PRAGMA + DDL +
    /// migrations on the new connection.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(&path)?;
        let inner = SqliteInner {
            conn: Mutex::new(conn),
            path,
        };
        let db = SqliteDb {
            inner: Arc::new(inner),
        };
        db.init()?;
        Ok(db)
    }

    /// Open an in-memory DB. Useful for tests.
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let inner = SqliteInner {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
        };
        let db = SqliteDb {
            inner: Arc::new(inner),
        };
        db.init()?;
        Ok(db)
    }

    /// Apply PRAGMAs, run DDL, run migrations. Safe to call multiple times.
    pub fn init(&self) -> rusqlite::Result<()> {
        let mut conn = self.inner.conn.lock();
        for pragma in schema::PRAGMAS {
            // Ignore result — PRAGMA returns data, not row count.
            let _ = conn.execute_batch(pragma);
        }
        // DDL in one transaction so partial failures roll back.
        let tx = conn.transaction()?;
        for stmt in schema::TABLES_SQL {
            tx.execute_batch(stmt)?;
        }
        migrations::apply_pending_migrations(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Acquire the underlying connection mutex. Caller MUST drop the guard
    /// before issuing long blocking operations, and should prefer the
    /// transactional helpers ([`with_conn`], [`with_transaction`]).
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.inner.conn.lock()
    }

    /// Path to the underlying file. Returns `None` for `:memory:`.
    pub fn path(&self) -> Option<&Path> {
        if self.inner.path.to_str() == Some(":memory:") {
            None
        } else {
            Some(&self.inner.path)
        }
    }

    /// Run `f` with exclusive access to the connection. The closure may
    /// start its own transaction via [`with_transaction`].
    pub fn with_conn<F, R>(&self, f: F) -> rusqlite::Result<R>
    where
        F: FnOnce(&mut Connection) -> rusqlite::Result<R>,
    {
        let mut guard = self.inner.conn.lock();
        f(&mut guard)
    }

    /// Run `f` inside a SQLite transaction. Rolls back on error.
    pub fn with_transaction<F, R>(&self, f: F) -> rusqlite::Result<R>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<R>,
    {
        let mut guard = self.inner.conn.lock();
        let tx = guard.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    /// `PRAGMA integrity_check` — panics if the DB is corrupt. Returns
    /// the first line of the check output (always "ok" on a healthy DB).
    pub fn integrity_check(&self) -> rusqlite::Result<String> {
        let guard = self.inner.conn.lock();
        let mut stmt = guard.prepare("PRAGMA integrity_check")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut lines = Vec::new();
        for row in rows {
            lines.push(row?);
        }
        Ok(lines.into_iter().next().unwrap_or_else(|| "ok".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_all_tables() {
        let db = SqliteDb::open_in_memory().unwrap();
        let names: Vec<String> = db
            .with_conn(|c| {
                let mut stmt =
                    c.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
                let rows = stmt.query_map([], |row| row.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap();
        for required in [
            "_meta",
            "settings",
            "providerConnections",
            "providerNodes",
            "proxyPools",
            "apiKeys",
            "combos",
            "kv",
            "disabledModels",
            "usageHistory",
            "usageDaily",
            "requestDetails",
        ] {
            assert!(
                names.iter().any(|n| n == required),
                "missing table {required}, got {names:?}"
            );
        }
    }

    #[test]
    fn init_creates_all_indexes() {
        let db = SqliteDb::open_in_memory().unwrap();
        let names: Vec<String> = db
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
                )?;
                let rows = stmt.query_map([], |row| row.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap();
        for required in [
            "idx_pc_provider",
            "idx_pc_provider_active",
            "idx_pc_priority",
            "idx_pn_type",
            "idx_pp_active",
            "idx_pp_status",
            "idx_ak_key",
            "idx_combo_name",
            "idx_kv_scope",
            "idx_uh_ts",
            "idx_uh_provider",
            "idx_uh_model",
            "idx_uh_conn",
            "idx_rd_ts",
            "idx_rd_provider",
            "idx_rd_model",
            "idx_rd_conn",
        ] {
            assert!(
                names.iter().any(|n| n == required),
                "missing index {required}, got {names:?}"
            );
        }
    }

    #[test]
    fn init_is_idempotent() {
        let db = SqliteDb::open_in_memory().unwrap();
        // Second call must not error.
        db.init().unwrap();
        db.init().unwrap();
    }

    #[test]
    fn schema_version_stamped_after_init() {
        let db = SqliteDb::open_in_memory().unwrap();
        let v: i32 = db
            .with_conn(|c| {
                c.query_row(
                    "SELECT CAST(value AS INTEGER) FROM _meta WHERE key='schema_version'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn integrity_check_passes_on_fresh_db() {
        let db = SqliteDb::open_in_memory().unwrap();
        assert_eq!(db.integrity_check().unwrap(), "ok");
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let db = SqliteDb::open_in_memory().unwrap();
        let result: rusqlite::Result<()> = db.with_transaction(|tx| {
            tx.execute(
                "INSERT INTO apiKeys(id, key, createdAt) VALUES(?, ?, ?)",
                rusqlite::params!["a", "k", "2026-01-01"],
            )?;
            tx.execute(
                "INSERT INTO apiKeys(id, key, createdAt) VALUES(?, ?, ?)",
                rusqlite::params!["a", "k", "2026-01-02"], // PK collision
            )?;
            Ok(())
        });
        assert!(result.is_err());
        // The first insert must have rolled back.
        let count: i64 = db
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM apiKeys", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn open_creates_parent_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "openproxy-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = tmp.join("nested").join("test.db");
        SqliteDb::open(&path).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
