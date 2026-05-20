//! Database backup snapshot/restore operations for openproxy.
//!
//! Ported from OmniRoute's `src/lib/db/backup.ts` and adapted for the JSON
//! `db.json` store used by openproxy. Each snapshot is a single timestamped
//! file under `${DATA_DIR}/db_backups/` of the form
//! `db_<utc-iso-no-colons>_<reason>.json`.
//!
//! Retention is governed by two environment variables:
//! * `DB_BACKUP_MAX_FILES`   — keep at most N newest snapshots (default 20)
//! * `DB_BACKUP_RETENTION_DAYS` — also drop snapshots older than D days
//!   (default 0 = disabled, only the max-files limit applies)
//!
//! The auto-backup loop can be disabled with `DISABLE_AUTO_BACKUP=1`. Manual
//! and pre-restore/pre-import snapshots are never auto-disabled.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;
use tokio::fs;

use crate::types::AppDb;

const BACKUP_THROTTLE_MS: u64 = 60 * 60 * 1000; // 60 minutes
const DEFAULT_MAX_FILES: usize = 20;
const DEFAULT_RETENTION_DAYS: u64 = 0;
/// Smallest plausible db.json — "{}\n" is 3 bytes. Anything smaller is treated
/// as corruption and skipped.
const MIN_BACKUP_BYTES: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupReason {
    Auto,
    Manual,
    PreRestore,
    PreImport,
}

impl BackupReason {
    pub fn as_str(self) -> &'static str {
        match self {
            BackupReason::Auto => "auto",
            BackupReason::Manual => "manual",
            BackupReason::PreRestore => "pre-restore",
            BackupReason::PreImport => "pre-import",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(BackupReason::Auto),
            "manual" => Some(BackupReason::Manual),
            "pre-restore" => Some(BackupReason::PreRestore),
            "pre-import" => Some(BackupReason::PreImport),
            _ => None,
        }
    }

    fn skips_throttle(self) -> bool {
        matches!(
            self,
            BackupReason::Manual | BackupReason::PreRestore | BackupReason::PreImport
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub id: String,
    pub filename: String,
    pub created_at: String,
    pub size: u64,
    pub reason: String,
    pub provider_count: usize,
    pub combo_count: usize,
    pub api_key_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResult {
    pub deleted_files: usize,
    pub kept_files: usize,
    pub max_files: usize,
    pub retention_days: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub restored: bool,
    pub backup_id: String,
    pub provider_count: usize,
    pub combo_count: usize,
    pub api_key_count: usize,
}

pub struct BackupManager {
    backup_dir: PathBuf,
    db_path: PathBuf,
    last_backup_ms: AtomicU64,
}

impl BackupManager {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            backup_dir: data_dir.join("db_backups"),
            db_path: data_dir.join("db.json"),
            last_backup_ms: AtomicU64::new(0),
        }
    }

    pub fn backup_dir(&self) -> &Path {
        &self.backup_dir
    }

    pub fn max_files() -> usize {
        std::env::var("DB_BACKUP_MAX_FILES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_FILES)
    }

    pub fn retention_days() -> u64 {
        std::env::var("DB_BACKUP_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_RETENTION_DAYS)
    }

    pub fn is_auto_disabled() -> bool {
        std::env::var("DISABLE_AUTO_BACKUP")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    /// Create a snapshot of `db.json` under `db_backups/`. Returns `None`
    /// if the backup was throttled or skipped (e.g. db.json is missing or
    /// suspiciously small).
    pub async fn create(&self, reason: BackupReason) -> anyhow::Result<Option<BackupInfo>> {
        if reason == BackupReason::Auto && Self::is_auto_disabled() {
            return Ok(None);
        }

        if !fs::try_exists(&self.db_path).await? {
            return Ok(None);
        }

        let stat = fs::metadata(&self.db_path).await?;
        if stat.len() < MIN_BACKUP_BYTES {
            tracing::warn!(
                target: "openproxy::db::backups",
                size = stat.len(),
                "skipping backup — db.json too small to be valid"
            );
            return Ok(None);
        }

        if !reason.skips_throttle() {
            let now_ms = now_millis();
            let last = self.last_backup_ms.load(Ordering::Relaxed);
            if last > 0 && now_ms.saturating_sub(last) < BACKUP_THROTTLE_MS {
                return Ok(None);
            }
            self.last_backup_ms.store(now_ms, Ordering::Relaxed);
        } else {
            // Manual/pre-* still updates the timestamp so the next auto
            // throttle window starts now.
            self.last_backup_ms.store(now_millis(), Ordering::Relaxed);
        }

        fs::create_dir_all(&self.backup_dir).await?;

        // Shrink-detection guard for auto backups only — never block a manual
        // or pre-* operator action.
        if reason == BackupReason::Auto {
            if let Some(latest) = self.latest_backup_size().await? {
                if latest > MIN_BACKUP_BYTES && stat.len() < latest / 2 {
                    tracing::warn!(
                        target: "openproxy::db::backups",
                        previous = latest,
                        current = stat.len(),
                        "skipping backup — db.json shrank by >50% since last snapshot"
                    );
                    return Ok(None);
                }
            }
        }

        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let safe_timestamp = timestamp.replace([':', '.'], "-");
        let filename = format!("db_{}_{}.json", safe_timestamp, reason.as_str());
        let dest = self.backup_dir.join(&filename);

        fs::copy(&self.db_path, &dest).await?;

        let info = describe_backup(&self.backup_dir, &filename).await?;
        tracing::info!(
            target: "openproxy::db::backups",
            id = %info.id,
            size = info.size,
            reason = %info.reason,
            "created db backup"
        );

        // Best-effort retention pass after each new snapshot.
        if reason != BackupReason::PreRestore && reason != BackupReason::PreImport {
            let _ = self
                .cleanup(Self::max_files(), Self::retention_days())
                .await;
        }

        Ok(Some(info))
    }

    /// List backup files sorted by `created_at` descending (newest first).
    pub async fn list(&self) -> anyhow::Result<Vec<BackupInfo>> {
        if !fs::try_exists(&self.backup_dir).await? {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();
        let mut read_dir = fs::read_dir(&self.backup_dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !is_backup_filename(name_str) {
                continue;
            }
            match describe_backup(&self.backup_dir, name_str).await {
                Ok(info) => entries.push(info),
                Err(err) => {
                    tracing::warn!(
                        target: "openproxy::db::backups",
                        file = %name_str,
                        error = %err,
                        "skipping unreadable backup file"
                    );
                }
            }
        }

        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(entries)
    }

    /// Restore a specific backup. Takes a pre-restore safety snapshot first,
    /// validates the backup is valid JSON, then atomically replaces
    /// `db.json`. The caller is responsible for reloading the in-memory DB.
    pub async fn read_backup(&self, backup_id: &str) -> anyhow::Result<AppDb> {
        validate_backup_id(backup_id)?;
        let backup_path = self.backup_dir.join(backup_id);

        if !fs::try_exists(&backup_path).await? {
            anyhow::bail!("Backup not found: {backup_id}");
        }

        let bytes = fs::read(&backup_path).await?;
        let parsed: Value = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("Backup file is not valid JSON: {e}"))?;
        if !parsed.is_object() {
            anyhow::bail!("Backup root must be a JSON object");
        }
        Ok(AppDb::from_json_value(parsed))
    }

    /// Delete a single backup file.
    pub async fn delete(&self, backup_id: &str) -> anyhow::Result<()> {
        validate_backup_id(backup_id)?;
        let path = self.backup_dir.join(backup_id);
        if fs::try_exists(&path).await? {
            fs::remove_file(&path).await?;
            tracing::info!(
                target: "openproxy::db::backups",
                id = %backup_id,
                "deleted db backup"
            );
        }
        Ok(())
    }

    /// Prune backups using `max_files` newest + `retention_days` cutoff.
    /// Always keeps at least the single newest file even when limits are 0.
    pub async fn cleanup(
        &self,
        max_files: usize,
        retention_days: u64,
    ) -> anyhow::Result<CleanupResult> {
        let entries = self.list().await?;
        let total = entries.len();
        let max_files = max_files.max(1);

        let cutoff_ms: Option<i64> = if retention_days > 0 {
            let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
            Some(cutoff.timestamp_millis())
        } else {
            None
        };

        let mut deleted = 0usize;
        for (idx, info) in entries.iter().enumerate() {
            let exceeds_max = idx >= max_files;
            let expired = cutoff_ms
                .map(|cutoff| {
                    DateTime::parse_from_rfc3339(&info.created_at)
                        .map(|dt| dt.timestamp_millis() < cutoff)
                        .unwrap_or(false)
                })
                .unwrap_or(false);

            if !exceeds_max && !expired {
                continue;
            }
            // Never let the prune pass delete the most recent snapshot.
            if idx == 0 {
                continue;
            }

            let path = self.backup_dir.join(&info.id);
            if let Err(err) = fs::remove_file(&path).await {
                tracing::warn!(
                    target: "openproxy::db::backups",
                    id = %info.id,
                    error = %err,
                    "failed to delete backup"
                );
                continue;
            }
            deleted += 1;
        }

        Ok(CleanupResult {
            deleted_files: deleted,
            kept_files: total.saturating_sub(deleted),
            max_files,
            retention_days,
        })
    }

    async fn latest_backup_size(&self) -> anyhow::Result<Option<u64>> {
        if !fs::try_exists(&self.backup_dir).await? {
            return Ok(None);
        }
        let mut latest_mtime: Option<SystemTime> = None;
        let mut latest_size: Option<u64> = None;
        let mut read_dir = fs::read_dir(&self.backup_dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !is_backup_filename(name_str) {
                continue;
            }
            let meta = match entry.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if latest_mtime.is_none_or(|prev| mtime > prev) {
                latest_mtime = Some(mtime);
                latest_size = Some(meta.len());
            }
        }
        Ok(latest_size)
    }
}

fn is_backup_filename(name: &str) -> bool {
    name.starts_with("db_") && name.ends_with(".json")
}

fn parse_reason_from_filename(name: &str) -> &str {
    // expected: db_<timestamp>_<reason>.json
    let stem = name.trim_end_matches(".json");
    if let Some(rest) = stem.strip_prefix("db_") {
        if let Some(idx) = rest.rfind('_') {
            let candidate = &rest[idx + 1..];
            if BackupReason::from_str(candidate).is_some() {
                return match candidate {
                    "auto" => "auto",
                    "manual" => "manual",
                    "pre-restore" => "pre-restore",
                    "pre-import" => "pre-import",
                    _ => "unknown",
                };
            }
        }
    }
    "unknown"
}

fn validate_backup_id(id: &str) -> anyhow::Result<()> {
    if !is_backup_filename(id) {
        anyhow::bail!("Invalid backup id");
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        anyhow::bail!("Invalid backup id: path traversal detected");
    }
    Ok(())
}

async fn describe_backup(backup_dir: &Path, filename: &str) -> anyhow::Result<BackupInfo> {
    let path = backup_dir.join(filename);
    let meta = fs::metadata(&path).await?;
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let mtime_chrono = DateTime::<Utc>::from(mtime);
    let created_at = mtime_chrono.to_rfc3339_opts(SecondsFormat::Millis, true);
    let reason = parse_reason_from_filename(filename).to_string();

    // Cheap object count: parse JSON only when describing list entries.
    let (provider_count, combo_count, api_key_count) = match fs::read(&path).await {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => count_objects(&value),
            Err(_) => (0, 0, 0),
        },
        Err(_) => (0, 0, 0),
    };

    Ok(BackupInfo {
        id: filename.to_string(),
        filename: filename.to_string(),
        created_at,
        size: meta.len(),
        reason,
        provider_count,
        combo_count,
        api_key_count,
    })
}

fn count_objects(value: &Value) -> (usize, usize, usize) {
    let providers = value
        .get("providerConnections")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let combos = value
        .get("combos")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let api_keys = value
        .get("apiKeys")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    (providers, combos, api_keys)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn seed_app_db() -> AppDb {
        let mut db = AppDb::default();
        // Make it non-trivial so backups aren't skipped.
        db.api_keys = vec![crate::types::ApiKey {
            id: "k1".into(),
            name: "test".into(),
            key: "ak".into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: std::collections::BTreeMap::new(),
        }];
        db
    }

    async fn write_db(path: &Path, db: &AppDb) {
        let bytes = serde_json::to_vec_pretty(db).unwrap();
        fs::write(path, bytes).await.unwrap();
    }

    #[tokio::test]
    async fn create_then_list_then_restore_round_trip() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db.json");
        write_db(&db_path, &seed_app_db()).await;

        let mgr = BackupManager::new(dir.path());
        let info = mgr
            .create(BackupReason::Manual)
            .await
            .unwrap()
            .expect("backup created");
        assert!(info.id.starts_with("db_"));
        assert!(info.id.ends_with("_manual.json"));
        assert_eq!(info.api_key_count, 1);

        let list = mgr.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, info.id);

        let restored = mgr.read_backup(&info.id).await.unwrap();
        assert_eq!(restored.api_keys.len(), 1);
    }

    #[tokio::test]
    async fn auto_backup_is_throttled() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db.json");
        write_db(&db_path, &seed_app_db()).await;

        let mgr = BackupManager::new(dir.path());
        let first = mgr.create(BackupReason::Auto).await.unwrap();
        assert!(first.is_some());
        let second = mgr.create(BackupReason::Auto).await.unwrap();
        assert!(second.is_none(), "auto backup should be throttled");
    }

    #[tokio::test]
    async fn manual_backup_skips_throttle() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db.json");
        write_db(&db_path, &seed_app_db()).await;

        let mgr = BackupManager::new(dir.path());
        let first = mgr.create(BackupReason::Auto).await.unwrap().unwrap();
        // Manual immediately after auto must still succeed.
        let second = mgr
            .create(BackupReason::Manual)
            .await
            .unwrap()
            .expect("manual backup");
        assert_ne!(first.id, second.id);
    }

    #[tokio::test]
    async fn cleanup_respects_max_files() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db.json");
        write_db(&db_path, &seed_app_db()).await;
        let mgr = BackupManager::new(dir.path());

        // Manual backups don't share the auto throttle; create 5 of them.
        for _ in 0..5 {
            mgr.create(BackupReason::Manual).await.unwrap();
            // Tiny sleep so each timestamp differs.
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let pre = mgr.list().await.unwrap();
        assert_eq!(pre.len(), 5);

        let result = mgr.cleanup(2, 0).await.unwrap();
        assert_eq!(result.deleted_files, 3);
        let post = mgr.list().await.unwrap();
        assert_eq!(post.len(), 2);
    }

    #[tokio::test]
    async fn rejects_path_traversal() {
        let dir = tempdir().unwrap();
        let mgr = BackupManager::new(dir.path());
        assert!(mgr.delete("../etc/passwd").await.is_err());
        assert!(mgr.read_backup("../db.json").await.is_err());
    }

    #[tokio::test]
    async fn skips_when_db_too_small() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db.json");
        fs::write(&db_path, b"{").await.unwrap();
        let mgr = BackupManager::new(dir.path());
        let result = mgr.create(BackupReason::Manual).await.unwrap();
        assert!(result.is_none(), "tiny db.json must not be backed up");
    }
}
