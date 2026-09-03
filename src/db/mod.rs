use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use arc_swap::ArcSwap;
use serde_json::Value;
use tokio::fs;
use tokio::sync::RwLock;

use crate::core::model::catalog::ProviderCatalog;
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

/// Decrypt every provider connection in an `AppDb` built from a SQLite
/// `export_all`. The in-memory snapshot must hold plaintext credentials;
/// SQLite's `data` column holds ciphertext. Decryption is a no-op when
/// `OPENPROXY_ENCRYPTION_KEY` is unset (plaintext mode), and `decrypt_opt`
/// fails-loud (clears) ciphertext that can't be decrypted.
fn decrypt_snapshot_connections(app_db: &mut AppDb) {
    let key = crate::db::crypto::encryption_key().unwrap_or_default();
    if key.is_empty() {
        return;
    }
    for conn in &mut app_db.provider_connections {
        crate::db::crypto::decrypt_connection(conn, &key);
    }
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
        let (mut app_db, usage_db) =
            tokio::task::spawn_blocking(move || -> anyhow::Result<(AppDb, UsageDb)> {
                let app_db = sq.with_conn(|conn| -> rusqlite::Result<AppDb> {
                    let json_val = crate::db::sqlite::export::export_all(conn)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    Ok(AppDb::from_json_value(json_val))
                })?;
                // SQLite stores ciphertext in the `data` column; the in-memory
                // snapshot must hold plaintext so credential checks and the
                // executors see real tokens (H20 boundary invariant).
                let mut app_db = app_db;
                decrypt_snapshot_connections(&mut app_db);
                let usage_db = sq.with_conn(|conn| -> rusqlite::Result<UsageDb> {
                    let json_val = crate::db::sqlite::export::export_usage_impl(conn)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    Ok(UsageDb::from_json_value(json_val))
                })?;
                Ok((app_db, usage_db))
            })
            .await
            .context("spawn_blocking for initial SQLite snapshot")??;

        // Seed default provider connections from the static catalog for any
        // providers that are registered in provider_catalog.json but not present
        // in the persisted SQLite store. This ensures the dashboard provider
        // list and API always reflect the full catalog even after a fresh install.
        let catalog = crate::core::model::catalog::provider_catalog();
        let existing_providers: std::collections::HashSet<&str> = app_db
            .provider_connections
            .iter()
            .map(|c| c.provider.as_str())
            .collect();

        let mut seeded_count = 0usize;
        let now = chrono::Utc::now().to_rfc3339();
        let enc_key = crate::db::crypto::encryption_key().unwrap_or_default();

        for provider_id in catalog.provider_ids() {
            if existing_providers.contains(provider_id) {
                continue;
            }

            let id = uuid::Uuid::new_v4().to_string();
            let mut conn = crate::types::ProviderConnection::default();
            conn.id = id.clone();
            conn.provider = provider_id.to_string();
            conn.auth_type = "apikey".to_string();
            conn.name = Some(format!("{} (default)", provider_id));
            conn.priority = Some(1);
            conn.is_active = Some(false); // inactive placeholder — not usable without a real key
            conn.created_at = Some(now.clone());
            conn.updated_at = Some(now.clone());
            conn.test_status = Some("unknown".to_string());
            crate::db::crypto::encrypt_connection(&mut conn, &enc_key);
            let data_json = serde_json::to_string(&conn).unwrap_or_default();

            let result = sqlite.with_conn(|c: &mut rusqlite::Connection| -> rusqlite::Result<()> {
                c.execute(
                    "INSERT OR IGNORE INTO providerConnections(id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    rusqlite::params![
                        conn.id,
                        conn.provider,
                        conn.auth_type,
                        conn.name,
                        conn.email,
                        conn.priority,
                        conn.is_active.map(|v| v as i32).unwrap_or(0),
                        data_json,
                        conn.created_at.unwrap_or_default(),
                        conn.updated_at.unwrap_or_default(),
                    ],
                )?;
                Ok(())
            });

            match result {
                Ok(_) => {
                    seeded_count += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "openproxy::db",
                        "Failed to seed provider connection for {}: {}",
                        provider_id, e
                    );
                }
            }
        }

        if seeded_count > 0 {
            tracing::info!(
                target: "openproxy::db",
                "Seeded {} default provider connection(s) from provider_catalog.json",
                seeded_count
            );
            // Re-read the snapshot to include newly inserted connections
            app_db = sqlite
                .with_conn(
                    |conn: &mut rusqlite::Connection| -> rusqlite::Result<AppDb> {
                        let json_val = crate::db::sqlite::export::export_all(conn)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                        let mut app_db = AppDb::from_json_value(json_val);
                        decrypt_snapshot_connections(&mut app_db);
                        Ok(app_db)
                    },
                )
                .map_err(|e| anyhow::anyhow!("SQLite read failed: {e}"))?;
        }

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
            let mut app_db = sq
                .with_conn(|conn| -> rusqlite::Result<AppDb> {
                    let json_val = crate::db::sqlite::export::export_all(conn)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    Ok(AppDb::from_json_value(json_val))
                })
                .map_err(|e| anyhow::anyhow!("SQLite reload failed: {e}"))?;
            decrypt_snapshot_connections(&mut app_db);
            Ok(app_db)
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

    /// Incremental write — applies only the difference between the current
    /// snapshot and the mutated one to SQLite (H19). Unchanged rows are never
    /// rewritten, and the append-only `usageHistory`/`usageDaily`/
    /// `requestDetails` tables are left untouched (previously `import_db`
    /// wiped them on every config change even though the AppDb payload never
    /// contains usage data).
    ///
    /// Prefer [`update_settings`] when only the settings have changed to
    /// avoid the diff overhead entirely.
    pub async fn update<F>(&self, updater: F) -> anyhow::Result<Arc<AppDb>>
    where
        F: FnOnce(&mut AppDb),
    {
        let _guard = self.write_lock.write().await;
        let prev = (*self.snapshot()).clone();
        let mut next = prev.clone();
        updater(&mut next);
        next.normalize();

        if next != prev {
            let sq = self.sqlite.clone();
            let prev = prev.clone();
            let next = next.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                sq.with_transaction(|conn| {
                    crate::db::sqlite::patch::apply_app_db_diff(conn, &prev, &next)
                })
                .map_err(|e| anyhow::anyhow!("SQLite incremental write failed: {e}"))?;
                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking for update: {e}"))??;
        }
        let next = Arc::new(next);
        self.snapshot.store(next.clone());
        Ok(next)
    }

    /// Targeted settings update — mutates only the `settings` row in SQLite
    /// without rewriting the entire database (H19). Use this when only
    /// server settings have changed (e.g. a dashboard config toggle).
    pub async fn update_settings<F>(&self, updater: F) -> anyhow::Result<Arc<AppDb>>
    where
        F: FnOnce(&mut crate::types::Settings),
    {
        let _guard = self.write_lock.write().await;
        let mut next = (*self.snapshot()).clone();
        updater(&mut next.settings);
        next.normalize();
        // Persist only the settings row to SQLite.
        let settings_str = serde_json::to_string(&next.settings)?;
        let sq = self.sqlite.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            sq.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO settings(id, data) VALUES(1, ?1) ON CONFLICT(id) DO UPDATE SET data = excluded.data",
                    rusqlite::params![settings_str],
                )
            })
            .map_err(|e| anyhow::anyhow!("SQLite settings write failed: {e}"))?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking for update_settings: {e}"))??;
        let next = Arc::new(next);
        self.snapshot.store(next.clone());
        Ok(next)
    }

    pub async fn update_usage<F>(&self, updater: F) -> anyhow::Result<Arc<UsageDb>>
    where
        F: FnOnce(&mut UsageDb),
    {
        let _guard = self.write_lock.write().await;
        let prev = (*self.usage_snapshot()).clone();
        let mut next = prev.clone();
        updater(&mut next);
        next.normalize();

        if next != prev {
            // Incremental append (9router usageRepo.saveRequestUsage): only
            // the new rows are INSERTed — the DELETE-all + re-INSERT rewrite
            // previously done here (import_usage) is reserved for imports.
            let appended: Vec<crate::types::UsageEntry> = next
                .history
                .iter()
                .skip(prev.history.len())
                .cloned()
                .collect();
            if !appended.is_empty() {
                let sq = self.sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    sq.with_transaction(|conn| {
                        for entry in &appended {
                            crate::db::sqlite::repo::usage_repo::insert(conn, entry)?;
                        }
                        Ok(())
                    })
                    .map_err(|e| anyhow::anyhow!("SQLite usage append failed: {e}"))
                })
                .await
                .map_err(|e| anyhow::anyhow!("spawn_blocking for update_usage: {e}"))??;
            }
        }

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

/// Seed default provider connections from the static provider catalog.
///
/// For every provider registered in `provider_catalog.json` that does NOT already
/// have a corresponding `providerConnection` row in SQLite, insert a placeholder
/// connection so that the dashboard and API always reflect the full catalog.
///
/// This is additive only: existing connections (real credentials) are preserved,
/// and the new placeholder connections are marked inactive so they do not
/// interfere with request routing or health checks.
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

    /// Regression test for the catalog-seeding behavior added so the dashboard
    /// provider list reflects every provider registered in
    /// `provider_catalog.json` even on a fresh install (no SQLite pre-seeded).
    #[tokio::test]
    async fn load_from_seeds_catalog_providers_inactive() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::load_from(dir.path()).await.expect("load_from");
        let snapshot = db.snapshot();

        let catalog = crate::core::model::catalog::provider_catalog();
        let catalog_ids: Vec<&str> = catalog.provider_ids().collect();

        // Every catalog provider must have a connection after a fresh load.
        assert!(
            !catalog_ids.is_empty(),
            "catalog must contain providers for this test to be meaningful"
        );
        for id in &catalog_ids {
            let conn = snapshot
                .provider_connections
                .iter()
                .find(|c| c.provider == *id);
            assert!(
                conn.is_some(),
                "catalog provider {id} should be seeded as a connection"
            );
            // Placeholders are inactive — they cannot route without a real key.
            assert_eq!(
                conn.unwrap().is_active,
                Some(false),
                "seeded placeholder for {id} must be inactive"
            );
        }
    }

    /// Seeding must be idempotent: reloading the same data dir must not create
    /// duplicate placeholder connections for already-seeded providers.
    #[tokio::test]
    async fn load_from_is_idempotent_for_catalog_seeding() {
        let dir = tempfile::tempdir().unwrap();

        let db1 = Db::load_from(dir.path()).await.expect("first load");
        let before = db1.snapshot().provider_connections.len();

        // Reload from the same directory; the SQLite now already has the seeds.
        let db2 = Db::load_from(dir.path()).await.expect("second load");
        let after = db2.snapshot().provider_connections.len();

        assert_eq!(
            before, after,
            "reloading must not duplicate seeded catalog connections"
        );

        let catalog = crate::core::model::catalog::provider_catalog();
        let catalog_ids: Vec<&str> = catalog.provider_ids().collect();
        let snap2 = db2.snapshot();
        for id in &catalog_ids {
            let matches: Vec<_> = snap2
                .provider_connections
                .iter()
                .filter(|c| c.provider == *id)
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "catalog provider {id} must have exactly one connection after reload (idempotent)"
            );
        }
    }

    /// A pre-existing real connection must survive seeding untouched — seeding
    /// only inserts placeholders for providers NOT already present.
    #[tokio::test]
    async fn load_from_preserves_existing_connections() {
        let dir = tempfile::tempdir().unwrap();

        // First load seeds placeholders.
        let db = Db::load_from(dir.path()).await.expect("load_from");

        // Inject a real (active) connection for a catalog provider, then reload.
        let real_provider = crate::core::model::catalog::provider_catalog()
            .provider_ids()
            .next()
            .expect("at least one catalog provider")
            .to_string();
        let mut conn = crate::types::ProviderConnection::default();
        conn.id = "real-conn-1".into();
        conn.provider = real_provider.clone();
        conn.auth_type = "apikey".into();
        conn.name = Some("Real Key".into());
        conn.is_active = Some(true);
        db.update(|state| {
            state.provider_connections.push(conn.clone());
        })
        .await
        .expect("update");

        // Reload: the real connection must remain, still active.
        let db2 = Db::load_from(dir.path()).await.expect("reload");
        let snap2 = db2.snapshot();
        let reloaded = snap2
            .provider_connections
            .iter()
            .find(|c| c.id == "real-conn-1")
            .expect("real connection preserved");
        assert_eq!(reloaded.is_active, Some(true));
        assert_eq!(reloaded.provider, real_provider);
    }
}
