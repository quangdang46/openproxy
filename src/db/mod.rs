use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde_json::Value;
use tokio::fs;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::types::{AppDb, Combo, ModelAliasTarget, ProviderConnection, ProviderNode, UsageDb};

pub mod backups;
pub mod crypto;
pub mod watcher;
pub mod sqlite;

#[derive(Debug, Clone, Default)]
pub struct ProviderConnectionFilter {
    pub provider: Option<String>,
    pub is_active: Option<bool>,
}

pub struct Db {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub usage_path: PathBuf,
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

        let db_path = data_dir.join("db.json");
        let usage_path = data_dir.join("usage.json");

        let app_db = load_or_init_app_db(&db_path).await?;
        let usage_db = load_or_init_usage_db(&usage_path).await?;

        Ok(Self {
            data_dir,
            db_path,
            usage_path,
            snapshot: ArcSwap::from_pointee(app_db),
            usage_snapshot: ArcSwap::from_pointee(usage_db),
            write_lock: RwLock::new(()),
        })
    }

    pub fn snapshot(&self) -> Arc<AppDb> {
        self.snapshot.load_full()
    }

    pub fn usage_snapshot(&self) -> Arc<UsageDb> {
        self.usage_snapshot.load_full()
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
        let key = crate::db::crypto::encryption_key();
        write_app_db_atomic(&mut next, &self.db_path, key.as_deref()).await?;
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
        write_usage_db_atomic(&mut next, &self.usage_path).await?;
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
        let key = crate::db::crypto::encryption_key();
        write_app_db_atomic(&mut next, &self.db_path, key.as_deref()).await?;
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
    /// in-memory snapshot + on-disk file in one write-locked operation.
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
    /// in-memory usage snapshot + on-disk file in one write-locked operation.
    /// Returns the new snapshot.
    pub async fn import_usage_db(&self, json_bytes: &[u8]) -> anyhow::Result<Arc<UsageDb>> {
        let _guard = self.write_lock.write().await;
        let parsed: Value = serde_json::from_slice(json_bytes)?;
        if !parsed.is_object() {
            anyhow::bail!("import payload must be a JSON object");
        }
        let mut next = UsageDb::from_json_value(parsed);
        next.normalize();
        write_usage_db_atomic(&mut next, &self.usage_path).await?;
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

async fn load_or_init_app_db(path: &Path) -> anyhow::Result<AppDb> {
    if !fs::try_exists(path).await? {
        let mut value = AppDb::default();
        let key = crate::db::crypto::encryption_key();
        write_app_db_atomic(&mut value, path, key.as_deref()).await?;
        return Ok(value);
    }

    let bytes = fs::read(path).await?;

    // Try reading with checksum verification and decryption.
    let maybe_value = read_app_db(&bytes).await;

    let (mut value, created_new) = match maybe_value {
        Ok(v) => (v, false),
        Err(e) => {
            tracing::warn!(
                target: "openproxy::db",
                "failed to read db.json with crypto ({e}); falling back to plain parse"
            );
            // Fallback: parse the file as a plain (unchecksummed, unencrypted)
            // JSON object.
            let parsed: Value = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(_) => {
                    let mut v = AppDb::default();
                    let key = crate::db::crypto::encryption_key();
                    write_app_db_atomic(&mut v, path, key.as_deref()).await?;
                    return Ok(v);
                }
            };
            (AppDb::from_json_value(parsed), false)
        }
    };

    // Migration v0.1.3: default require_login to false for better local UX.
    let mut mutated = false;
    if value.settings.require_login {
        value.settings.require_login = false;
        mutated = true;
    }

    // Schema version check: if the loaded value has a lower version, migrate.
    if value.schema_version < crate::db::crypto::SCHEMA_VERSION {
        value.schema_version = crate::db::crypto::SCHEMA_VERSION;
        mutated = true;
    }

    if mutated || created_new {
        let key = crate::db::crypto::encryption_key();
        write_app_db_atomic(&mut value, path, key.as_deref()).await?;
    }
    Ok(value)
}

async fn load_or_init_usage_db(path: &Path) -> anyhow::Result<UsageDb> {
    if !fs::try_exists(path).await? {
        let mut value = UsageDb::default();
        write_usage_db_atomic(&mut value, path).await?;
        return Ok(value);
    }

    let bytes = fs::read(path).await?;

    // Try checksum-verified read; fall back to plain parse for legacy files.
    match read_usage_db(&bytes).await {
        Ok(db) => Ok(db),
        Err(e) => {
            tracing::warn!(
                target: "openproxy::db",
                "failed to read usage.json with checksum ({e}); falling back to plain parse"
            );
            let parsed: Value = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(_) => {
                    let mut v = UsageDb::default();
                    write_usage_db_atomic(&mut v, path).await?;
                    return Ok(v);
                }
            };
            let value = UsageDb::from_json_value(parsed);
            Ok(value)
        }
    }
}

/// Re-read `db.json` from disk without writing anything back. Used by the
/// file watcher to pick up CLI mutations made by another process. Tolerates
/// a brief read race during atomic-rename writes by retrying once.
pub async fn reload_app_db(path: &Path) -> anyhow::Result<AppDb> {
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(_) => {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            fs::read(path).await?
        }
    };

    // Try checksum-verified + decrypted read first; fall back to plain parse.
    match read_app_db(&bytes).await {
        Ok(db) => Ok(db),
        Err(e) => {
            tracing::warn!(
                target: "openproxy::db",
                "reload_app_db: crypto read failed ({e}), using plain parse"
            );
            let parsed: Value = serde_json::from_slice(&bytes)?;
            Ok(AppDb::from_json_value(parsed))
        }
    }
}

/// Re-read `usage.json` from disk without writing anything back.
pub async fn reload_usage_db(path: &Path) -> anyhow::Result<UsageDb> {
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(_) => {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            fs::read(path).await?
        }
    };

    // Try checksum-verified read; fall back to plain parse.
    match read_usage_db(&bytes).await {
        Ok(db) => Ok(db),
        Err(e) => {
            tracing::warn!(
                target: "openproxy::db",
                "reload_usage_db: checksum read failed ({e}), using plain parse"
            );
            let parsed: Value = serde_json::from_slice(&bytes)?;
            Ok(UsageDb::from_json_value(parsed))
        }
    }
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

/// Atomic write: serialize `value` to pretty JSON at `path` via temp + rename,
/// embedding a `_checksum` SHA-256 field for integrity verification.
async fn write_value_with_checksum(value: &serde_json::Value, path: &Path) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).await?;

    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("db"),
        Uuid::new_v4()
    ));

    let body = serde_json::to_vec_pretty(value)?;
    let checksum = crate::db::crypto::sha256_checksum(&body);
    let mut with_checksum = value.clone();
    with_checksum
        .as_object_mut()
        .expect("top-level Value must be an object")
        .insert("_checksum".into(), serde_json::Value::String(checksum));

    let bytes = serde_json::to_vec_pretty(&with_checksum)?;
    fs::write(&temp_path, bytes).await?;
    fs::rename(&temp_path, path).await?;
    Ok(())
}

/// Read a `serde_json::Value`, verify `_checksum` if present, return the
/// clean Value (with `_checksum` stripped).
fn read_verified_value(bytes: &[u8]) -> anyhow::Result<serde_json::Value> {
    let mut root: serde_json::Value = serde_json::from_slice(bytes)?;
    let Value::Object(ref mut map) = root else {
        return Ok(root);
    };

    let stored_checksum = map
        .remove("_checksum")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    if let Some(ref expected) = stored_checksum {
        if let Ok(body) = serde_json::to_vec_pretty(&root) {
            let actual = crate::db::crypto::sha256_checksum(&body);
            if *expected != actual {
                tracing::warn!(
                    target: "openproxy::db",
                    "SHA-256 checksum mismatch (expected={expected}, actual={actual}); continuing"
                );
            }
        }
    }

    Ok(root)
}

/// Write an `AppDb` to disk: encrypt sensitive connection fields, embed
/// `schema_version`, serialize with a SHA-256 checksum, then restore
/// plaintext on `value` so the in-memory copy remains usable.
async fn write_app_db_atomic(value: &mut AppDb, path: &Path, key: Option<&str>) -> anyhow::Result<()> {
    if let Some(k) = key {
        for conn in &mut value.provider_connections {
            crate::db::crypto::encrypt_connection(conn, k);
        }
    }
    value.schema_version = crate::db::crypto::SCHEMA_VERSION;
    value.checksum.clear();

    let json_val = serde_json::to_value(&*value).expect("AppDb is always serializable");
    write_value_with_checksum(&json_val, path).await?;

    // Restore plaintext in-memory.
    if let Some(k) = key {
        for conn in &mut value.provider_connections {
            crate::db::crypto::decrypt_connection(conn, k);
        }
    }
    Ok(())
}

/// Read an `AppDb` from disk, verify the SHA-256 checksum, and decrypt
/// sensitive connection fields.
async fn read_app_db(bytes: &[u8]) -> anyhow::Result<AppDb> {
    let parsed = read_verified_value(bytes)?;
    let mut app_db = AppDb::from_json_value(parsed);

    if let Some(ref key) = crate::db::crypto::encryption_key() {
        for conn in &mut app_db.provider_connections {
            crate::db::crypto::decrypt_connection(conn, key);
        }
    }

    Ok(app_db)
}

/// Write a `UsageDb` to disk with a SHA-256 checksum. Usage records are not
/// encrypted (they contain no secrets), but they get the same integrity check.
async fn write_usage_db_atomic(value: &mut UsageDb, path: &Path) -> anyhow::Result<()> {
    let json_val = serde_json::to_value(&*value).expect("UsageDb is always serializable");
    write_value_with_checksum(&json_val, path).await?;
    Ok(())
}

/// Read a `UsageDb` from disk, verifying the SHA-256 checksum if present.
async fn read_usage_db(bytes: &[u8]) -> anyhow::Result<UsageDb> {
    let parsed = read_verified_value(bytes)?;
    Ok(UsageDb::from_json_value(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AppDb, ProviderConnection};
    use serde_json::Value;

    /// Issue #155 regression: when OPENPROXY_ENCRYPTION_KEY is unset, the
    /// write path must NOT encrypt secrets. Otherwise the read path (which
    /// skips decryption when no key is set) would return ciphertext as if
    /// it were a plaintext API key.
    #[tokio::test]
    async fn write_app_db_no_key_does_not_encrypt() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db.json");

        let mut db = AppDb::default();
        db.provider_connections = vec![ProviderConnection {
            id: "c1".into(),
            provider: "openai".into(),
            api_key: Some("sk-plaintext-12345".into()),
            access_token: Some("tok-plaintext".into()),
            refresh_token: None,
            id_token: None,
            ..Default::default()
        }];

        // No key => write_app_db_atomic must skip encryption.
        write_app_db_atomic(&mut db, &path, None).await.unwrap();

        let bytes = tokio::fs::read(&path).await.unwrap();
        let raw: Value = serde_json::from_slice(&bytes).unwrap();
        let conn = &raw["providerConnections"][0];
        assert_eq!(
            conn["apiKey"].as_str(),
            Some("sk-plaintext-12345"),
            "with no key, apiKey must be stored as plaintext (not encrypted)"
        );
        assert_eq!(
            conn["accessToken"].as_str(),
            Some("tok-plaintext"),
            "with no key, accessToken must be stored as plaintext (not encrypted)"
        );

        // (no need to read back — write_app_db_atomic with None skips encryption)
    }

    /// Issue #155 regression: with a key, secrets are encrypted on disk.
    #[tokio::test]
    async fn write_app_db_with_key_encrypts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db.json");

        let mut db = AppDb::default();
        db.provider_connections = vec![ProviderConnection {
            id: "c1".into(),
            provider: "openai".into(),
            api_key: Some("sk-secret-key".into()),
            access_token: None,
            refresh_token: None,
            id_token: None,
            ..Default::default()
        }];

        write_app_db_atomic(&mut db, &path, Some("test-key")).await.unwrap();

        let bytes = tokio::fs::read(&path).await.unwrap();
        let raw: Value = serde_json::from_slice(&bytes).unwrap();
        let conn = &raw["providerConnections"][0];
        assert_ne!(
            conn["apiKey"].as_str(),
            Some("sk-secret-key"),
            "with a key, apiKey must be encrypted on disk"
        );
    }
}
