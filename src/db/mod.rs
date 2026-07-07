use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use arc_swap::ArcSwap;
use serde_json::Value;
use tokio::fs;
use tokio::sync::RwLock;

use crate::types::{AppDb, Combo, ModelAliasTarget, ProviderConnection, ProviderNode, UsageDb};

pub mod backups;
pub mod crypto;
pub mod sqlite;
pub mod watcher;

#[derive(Debug, Clone, Default)]
pub struct ProviderConnectionFilter {
    pub provider: Option<String>,
    pub is_active: Option<bool>,
}

pub struct Db {
    pub data_dir: PathBuf,
    pub sqlite: sqlite::SqliteDb,
    pub snapshot: ArcSwap<AppDb>,
    pub usage_snapshot: ArcSwap<UsageDb>,
    write_lock: RwLock<()>,
}

impl Db {
    pub async fn load() -> anyhow::Result<Self> {
        let configured = std::env::var_os("DATA_DIR").map(PathBuf::from);
        let default = default_data_dir();

        match &configured {
            Some(dir) => match Self::load_from(dir).await {
                Ok(db) => Ok(db),
                Err(err) if is_permission_denied(&err) && *dir != default => {
                    tracing::warn!(
                        target: "openproxy::db",
                        configured = %dir.display(),
                        fallback = %default.display(),
                        "DATA_DIR not writable (permission denied); falling back to default"
                    );
                    Self::load_from(&default).await
                }
                Err(err) => Err(err),
            },
            None => Self::load_from(&default).await,
        }
    }

    pub async fn load_from(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        fs::create_dir_all(&data_dir).await?;

        // SQLite is the sole runtime store — mandatory, no fallback.
        let sqlite_path = data_dir.join("openproxy.sqlite");
        let sqlite = sqlite::SqliteDb::open(&sqlite_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to open SQLite DB at {}: {}",
                sqlite_path.display(),
                e
            )
        })?;

        // ---- One-time migration from legacy db.json / usage.json ----
        let migrated_marker = data_dir.join(".migrated-from-json");
        let db_json_path = data_dir.join("db.json");
        let usage_json_path = data_dir.join("usage.json");

        if !migrated_marker.exists() && (db_json_path.exists() || usage_json_path.exists()) {
            tracing::info!(
                target: "openproxy::db",
                "Legacy JSON files detected — importing into SQLite once"
            );

            if db_json_path.exists() {
                let bytes = fs::read(&db_json_path)
                    .await
                    .with_context(|| format!("read legacy {}", db_json_path.display()))?;
                // Try decrypted+checksum read first, fall back to plain JSON.
                let app_db_value = match crate::db::crypto::open_db(
                    &bytes,
                    crate::db::crypto::encryption_key().as_deref(),
                ) {
                    Ok(db) => serde_json::to_value(db)?,
                    Err(_) => {
                        let parsed: Value = serde_json::from_slice(&bytes)
                            .with_context(|| format!("parse legacy {}", db_json_path.display()))?;
                        parsed
                    }
                };
                let sq = sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    crate::db::sqlite::import::import_db(&sq, &app_db_value)
                })
                .await
                .context("spawn_blocking for db.json import")??;
                tracing::info!(target: "openproxy::db", "db.json imported into SQLite");
            }

            if usage_json_path.exists() {
                let bytes = fs::read(&usage_json_path)
                    .await
                    .with_context(|| format!("read legacy {}", usage_json_path.display()))?;
                let usage_value: Value = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse legacy {}", usage_json_path.display()))?;
                let sq = sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    crate::db::sqlite::import::import_usage(&sq, &usage_value)
                })
                .await
                .context("spawn_blocking for usage.json import")??;
                tracing::info!(target: "openproxy::db", "usage.json imported into SQLite");
            }

            fs::write(&migrated_marker, b"1").await.with_context(|| {
                format!("write migrated marker at {}", migrated_marker.display())
            })?;
            tracing::info!(
                target: "openproxy::db",
                "Legacy JSON import complete — wrote {}",
                migrated_marker.display()
            );
        }

        // ---- Read snapshot from SQLite ----
        let sq = sqlite.clone();
        let (app_db, usage_db) =
            tokio::task::spawn_blocking(move || -> anyhow::Result<(AppDb, UsageDb)> {
                let app_db = sq.with_conn(|conn| -> rusqlite::Result<AppDb> {
                    let json_val = crate::db::sqlite::export::export_all(conn)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    Ok(AppDb::from_json_value(json_val))
                })?;
                let usage_db = sq.with_conn(|conn| -> rusqlite::Result<UsageDb> {
                    let json_val = crate::db::sqlite::export::export_usage_impl(conn)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    Ok(UsageDb::from_json_value(json_val))
                })?;
                Ok((app_db, usage_db))
            })
            .await
            .context("spawn_blocking for initial SQLite snapshot")??;

        Ok(Self {
            data_dir,
            sqlite,
            snapshot: ArcSwap::from_pointee(app_db),
            usage_snapshot: ArcSwap::from_pointee(usage_db),
            write_lock: RwLock::new(()),
        })
    }

    pub fn snapshot(&self) -> Arc<AppDb> {
        self.snapshot.load_full()
    }

    /// Reload the in-memory AppDb snapshot from SQLite.
    /// Used when an external process (e.g. CLI `combo create`) writes to
    /// the SQLite file directly and the server's snapshot is stale.
    pub async fn reload_snapshot(&self) -> anyhow::Result<Arc<AppDb>> {
        let sq = self.sqlite.clone();
        let app_db = tokio::task::spawn_blocking(move || -> anyhow::Result<AppDb> {
            sq.with_conn(|conn| -> rusqlite::Result<AppDb> {
                let json_val = crate::db::sqlite::export::export_all(conn)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                Ok(AppDb::from_json_value(json_val))
            })
            .map_err(|e| anyhow::anyhow!("SQLite reload failed: {e}"))
        })
        .await
        .context("spawn_blocking for snapshot reload")??;
        let next = Arc::new(app_db);
        // Write-lock is NOT needed here: snapshot.load_full() + snapshot.store()
        // is an atomic single-word CAS via ArcSwap. A concurrent update() would get
        // the same result — the store is the sole writer and always provides a
        // consistent snapshot.
        self.snapshot.store(next.clone());
        Ok(next)
    }

    pub fn usage_snapshot(&self) -> Arc<UsageDb> {
        self.usage_snapshot.load_full()
    }

    /// Returns a reference to the SQLite handle.
    pub fn sqlite_handle(&self) -> &sqlite::SqliteDb {
        &self.sqlite
    }

    pub fn provider_connections(
        &self,
        filter: ProviderConnectionFilter,
    ) -> Vec<ProviderConnection> {
        let snapshot = self.snapshot();
        let mut connections: Vec<_> = snapshot
            .provider_connections
            .iter()
            .filter(|connection| {
                let provider_matches = filter
                    .provider
                    .as_ref()
                    .is_none_or(|provider| &connection.provider == provider);
                let activity_matches = filter
                    .is_active
                    .is_none_or(|is_active| connection.is_active() == is_active);

                provider_matches && activity_matches
            })
            .cloned()
            .collect();

        connections.sort_by_key(|connection| connection.priority.unwrap_or(999));
        connections
    }

    pub fn provider_nodes(&self, node_type: Option<&str>) -> Vec<ProviderNode> {
        let snapshot = self.snapshot();
        snapshot
            .provider_nodes
            .iter()
            .filter(|node| node_type.is_none_or(|expected| node.r#type == expected))
            .cloned()
            .collect()
    }

    pub fn combo_by_name(&self, name: &str) -> Option<Combo> {
        let snapshot = self.snapshot();
        snapshot
            .combos
            .iter()
            .find(|combo| combo.name == name)
            .cloned()
    }

    pub fn model_aliases(&self) -> Arc<std::collections::BTreeMap<String, ModelAliasTarget>> {
        let snapshot = self.snapshot();
        Arc::new(snapshot.model_aliases.clone())
    }

    pub async fn update<F>(&self, updater: F) -> anyhow::Result<Arc<AppDb>>
    where
        F: FnOnce(&mut AppDb),
    {
        let _guard = self.write_lock.write().await;
        let mut next = (*self.snapshot()).clone();
        updater(&mut next);
        next.normalize();
        // Persist to SQLite — the sole runtime store.
        let json_val = serde_json::to_value(&next)?;
        let sq = self.sqlite.clone();
        tokio::task::spawn_blocking(move || crate::db::sqlite::import::import_db(&sq, &json_val))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking for update: {e}"))?
            .map_err(|e| anyhow::anyhow!("SQLite write failed: {e}"))?;
        let next = Arc::new(next);
        self.snapshot.store(next.clone());
        Ok(next)
    }

    pub async fn update_usage<F>(&self, updater: F) -> anyhow::Result<Arc<UsageDb>>
    where
        F: FnOnce(&mut UsageDb),
    {
        let _guard = self.write_lock.write().await;
        let mut next = (*self.usage_snapshot()).clone();
        updater(&mut next);
        next.normalize();
        // Persist to SQLite — the sole runtime store.
        let json_val = serde_json::to_value(&next)?;
        let sq = self.sqlite.clone();
        tokio::task::spawn_blocking(move || {
            crate::db::sqlite::import::import_usage(&sq, &json_val)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking for update_usage: {e}"))?
        .map_err(|e| anyhow::anyhow!("SQLite usage write failed: {e}"))?;
        let next = Arc::new(next);
        self.usage_snapshot.store(next.clone());
        Ok(next)
    }

    /// Atomically replace the in-memory and on-disk app db with the value
    /// produced by `make_next`. Used by the backup-restore / import flows
    /// where the entire payload comes from a foreign snapshot.
    pub async fn replace_app_db<F>(&self, make_next: F) -> anyhow::Result<Arc<AppDb>>
    where
        F: FnOnce() -> AppDb,
    {
        let _guard = self.write_lock.write().await;
        let mut next = make_next();
        next.normalize();
        // Write to SQLite — the sole runtime store.
        let json_val = serde_json::to_value(&next)?;
        let sq = self.sqlite.clone();
        tokio::task::spawn_blocking(move || crate::db::sqlite::import::import_db(&sq, &json_val))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking for replace_app_db: {e}"))?
            .map_err(|e| anyhow::anyhow!("SQLite replace failed: {e}"))?;
        let next = Arc::new(next);
        self.snapshot.store(next.clone());
        Ok(next)
    }

    // ------------------------------------------------------------------
    // Centralized export / import
    // ------------------------------------------------------------------

    /// Serialize the current `AppDb` snapshot to pretty-printed JSON bytes.
    /// Returns a useful filename hint as well.
    pub fn export_db(&self) -> anyhow::Result<(Vec<u8>, String)> {
        let snapshot = self.snapshot.load_full();
        let json = serde_json::to_vec_pretty(snapshot.as_ref())?;
        let filename = format!("openproxy-db-{}.json", chrono_like_stamp());
        Ok((json, filename))
    }

    /// Deserialize JSON bytes into `AppDb` and atomically replace the
    /// in-memory snapshot + SQLite in one write-locked operation.
    pub async fn import_db(&self, json_bytes: &[u8]) -> anyhow::Result<Arc<AppDb>> {
        let parsed: Value = serde_json::from_slice(json_bytes)?;
        if !parsed.is_object() {
            anyhow::bail!("import payload must be a JSON object");
        }
        let next = AppDb::from_json_value(parsed);
        self.replace_app_db(move || next).await
    }

    /// Serialize the current `UsageDb` snapshot to pretty-printed JSON bytes.
    pub fn export_usage_db(&self) -> anyhow::Result<(Vec<u8>, String)> {
        let snapshot = self.usage_snapshot.load_full();
        let json = serde_json::to_vec_pretty(snapshot.as_ref())?;
        let filename = format!("openproxy-usage-{}.json", chrono_like_stamp());
        Ok((json, filename))
    }

    /// Deserialize JSON bytes into `UsageDb` and atomically replace the
    /// in-memory usage snapshot + SQLite in one write-locked operation.
    /// Returns the new snapshot.
    pub async fn import_usage_db(&self, json_bytes: &[u8]) -> anyhow::Result<Arc<UsageDb>> {
        let _guard = self.write_lock.write().await;
        let parsed: Value = serde_json::from_slice(json_bytes)?;
        if !parsed.is_object() {
            anyhow::bail!("import payload must be a JSON object");
        }
        let mut next = UsageDb::from_json_value(parsed);
        next.normalize();
        let json_val = serde_json::to_value(&next)?;
        let sq = self.sqlite.clone();
        tokio::task::spawn_blocking(move || {
            crate::db::sqlite::import::import_usage(&sq, &json_val)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking for import_usage_db: {e}"))?
        .map_err(|e| anyhow::anyhow!("SQLite usage import failed: {e}"))?;
        let next = Arc::new(next);
        self.usage_snapshot.store(next.clone());
        Ok(next)
    }
}

fn default_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let preferred = home.join(".openproxy");
    let legacy = home.join(".openproxy");

    if preferred.exists() || !legacy.exists() {
        preferred
    } else {
        legacy
    }
}

fn is_permission_denied(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

/// Compact UTC timestamp safe for use in filenames (no colons).
fn chrono_like_stamp() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Split into date and time parts, replacing ':' with '-'.
    let secs_of_day = n % 86_400;
    let h = secs_of_day / 3600;
    let m = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;

    let z = n / 86_400;
    let z_i64 = z as i64 + 719_468;
    let era = if z_i64 >= 0 {
        z_i64 / 146_097
    } else {
        (z_i64 - 146_096) / 146_097
    };
    let doe = (z_i64 - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m_civ = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (if m_civ <= 2 { y + 1 } else { y }) as i32;

    format!("{:04}-{:02}-{:02}T{:02}-{:02}-{:02}Z", y, m_civ, d, h, m, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use crate::types::AppDb;

    #[tokio::test]
    async fn db_init_creates_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let sqlite_path = dir.path().join("openproxy.sqlite");

        // SQLite must be created and readable.
        let sqlite = SqliteDb::open(&sqlite_path).unwrap();
        assert!(sqlite_path.exists(), "SQLite file must exist");

        // An empty SQLite should export a default snapshot.
        let app_db = sqlite
            .with_conn(|conn| {
                let val = crate::db::sqlite::export::export_all(conn)?;
                Ok(AppDb::from_json_value(val))
            })
            .unwrap();
        assert_eq!(app_db.settings, Default::default());
        drop(sqlite);
    }

    #[tokio::test]
    async fn db_init_creates_usage_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let sqlite_path = dir.path().join("openproxy-usage.sqlite");

        let sqlite = SqliteDb::open(&sqlite_path).unwrap();
        let usage_db = sqlite
            .with_conn(|conn| {
                let val = crate::db::sqlite::export::export_usage_impl(conn)?;
                Ok(UsageDb::from_json_value(val))
            })
            .unwrap();
        assert!(usage_db.history.is_empty());
    }
}
