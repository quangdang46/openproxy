//! Progressive lockout for `POST /api/auth/login`.
//!
//! Mirrors the 9router `src/lib/auth/loginLimiter.js` semantics:
//!
//! - `MAX_FAILS` failed attempts triggers a lockout.
//! - On each lockout the `lockout_level` advances; lockout durations follow
//!   `LOCK_STEPS_MS` (30s, 2min, 10min, 30min).
//! - A successful login resets the fail counter.
//! - The fail window is one hour of inactivity: a record that has not been
//!   touched for `FAIL_WINDOW` is treated as fresh.
//!
//! The store is persisted in `attempts.json` under the data directory.
//! On first access the file is loaded from disk; every mutation is written
//! back atomically via an atomic rename.
//!
//! Used by the password login handler in `crate::server::api::auth`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::Mutex;

/// Number of failed attempts before the first lockout kicks in.
const MAX_FAILS: u32 = 5;

/// Lockout durations, applied in order. Once a client has been locked out
/// `LOCK_STEPS_MS.len()` times, the last entry is reused (capped).
///
/// Indices: 0 = first lockout (30s), 1 = second (2min), 2 = third (10min),
/// 3 = fourth-and-later (30min).
const LOCK_STEPS_MS: &[u64] = &[30_000, 120_000, 600_000, 1_800_000];

/// Inactivity window after which a record is reset to zero fails.
const FAIL_WINDOW: Duration = Duration::from_secs(3600);

/// Result of `LoginLimiter::check_and_record`.
///
/// `Ok(())` means the caller may proceed (not currently locked out).
/// `Err(LockoutError::Locked { .. })` means the IP is currently locked and
/// the caller should refuse the request with HTTP 429.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockoutError {
    /// The IP is locked out for the given number of seconds.
    Locked { retry_after_secs: u64 },
}

/// On-disk representation of a single IP's attempt record.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRecord {
    count: u32,
    lockout_level: u32,
    locked_until_unix: Option<u64>,
    window_start_unix: u64,
}

/// In-memory attempt record for one IP.
struct AttemptRecord {
    count: u32,
    /// Index into `LOCK_STEPS_MS`. Capped at `LOCK_STEPS_MS.len() - 1`.
    lockout_level: u32,
    /// When the current lockout expires. `None` means not locked.
    locked_until: Option<SystemTime>,
    /// When the most recent activity occurred. Used to age out stale records.
    window_start: SystemTime,
}

impl AttemptRecord {
    fn new(now: SystemTime) -> Self {
        Self {
            count: 0,
            lockout_level: 0,
            locked_until: None,
            window_start: now,
        }
    }

    fn to_stored(&self) -> StoredRecord {
        StoredRecord {
            count: self.count,
            lockout_level: self.lockout_level,
            locked_until_unix: self
                .locked_until
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
            window_start_unix: self
                .window_start
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    fn from_stored(s: &StoredRecord) -> Self {
        Self {
            count: s.count,
            lockout_level: s.lockout_level,
            locked_until: s
                .locked_until_unix
                .map(|secs| UNIX_EPOCH + Duration::from_secs(secs)),
            window_start: UNIX_EPOCH + Duration::from_secs(s.window_start_unix),
        }
    }
}

/// Progressive lockout store keyed by client IP, persisted to `attempts.json`.
pub struct LoginLimiter {
    path: PathBuf,
    state: Mutex<Option<HashMap<IpAddr, AttemptRecord>>>,
}

impl LoginLimiter {
    /// Create a new limiter whose data lives under `data_dir/attempts.json`.
    ///
    /// The file is loaded lazily from disk on the first call to
    /// [`check_and_record`].
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            path: data_dir.into().join("attempts.json"),
            state: Mutex::new(None),
        }
    }

    /// Load the persisted attempt map from disk. Returns an empty map when the
    /// file does not exist or is corrupt.
    async fn load_inner(&self) -> HashMap<IpAddr, AttemptRecord> {
        let bytes = match fs::read(&self.path).await {
            Ok(b) => b,
            Err(_) => return HashMap::new(),
        };
        let stored: HashMap<String, StoredRecord> = match serde_json::from_slice(&bytes) {
            Ok(m) => m,
            Err(_) => return HashMap::new(),
        };
        stored
            .into_iter()
            .filter_map(|(key, s)| {
                let ip: IpAddr = key.parse().ok()?;
                Some((ip, AttemptRecord::from_stored(&s)))
            })
            .collect()
    }

    /// Persist the attempt map to disk atomically (write-then-rename).
    /// Errors are logged and swallowed so that auth is never blocked by a
    /// write failure.
    async fn persist(&self, map: &HashMap<IpAddr, AttemptRecord>) {
        let stored: HashMap<String, StoredRecord> = map
            .iter()
            .map(|(ip, rec)| (ip.to_string(), rec.to_stored()))
            .collect();

        let bytes = match serde_json::to_vec_pretty(&stored) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(?e, "LoginLimiter: failed to serialize attempts");
                return;
            }
        };

        let root = PathBuf::new();
        let dir = self.path.parent().unwrap_or(&root);
        let tmp = dir.join(format!(".attempts.{}.tmp", uuid::Uuid::new_v4()));

        if let Err(e) = fs::write(&tmp, &bytes).await {
            tracing::warn!(?e, "LoginLimiter: failed to write attempts file");
            let _ = fs::remove_file(&tmp).await;
            return;
        }

        if let Err(e) = fs::rename(&tmp, &self.path).await {
            tracing::warn!(?e, "LoginLimiter: failed to rename attempts file");
            let _ = fs::remove_file(&tmp).await;
        }
    }

    /// Check whether `ip` is currently locked out and record a new attempt.
    ///
    /// If the local cache has not been populated yet, this method loads the
    /// persisted attempt data from disk first (lazy initialization).
    ///
    /// Semantics:
    /// - If the IP is currently locked out, return `LockoutError::Locked` and
    ///   do not record a new fail (so an attacker hammering during a lockout
    ///   does not extend it).
    /// - If the previous activity is older than `FAIL_WINDOW`, reset the
    ///   fail counter and window clock before processing this attempt.
    /// - On `success = true`, reset fail_count / lockout_level / locked_until.
    /// - On `success = false`, increment fail_count; once it reaches
    ///   `MAX_FAILS`, set `locked_until` from `LOCK_STEPS_MS[lockout_level]`
    ///   and advance the level (capped). Return `LockoutError::Locked`.
    pub async fn check_and_record(&self, ip: IpAddr, success: bool) -> Result<(), LockoutError> {
        let mut guard = self.state.lock().await;

        // Lazy-load from disk on first access.
        if guard.is_none() {
            *guard = Some(self.load_inner().await);
        }

        let map = guard.as_mut().unwrap();
        let now = SystemTime::now();

        let record = map.entry(ip).or_insert_with(|| AttemptRecord::new(now));

        // Stale records are treated as fresh.
        if now
            .duration_since(record.window_start)
            .unwrap_or(Duration::ZERO)
            >= FAIL_WINDOW
        {
            record.count = 0;
            record.lockout_level = 0;
            record.locked_until = None;
            record.window_start = now;
        }

        // Currently locked — refuse without recording a new failure.
        if let Some(until) = record.locked_until {
            if now < until {
                let retry_after_secs = until
                    .duration_since(now)
                    .unwrap_or(Duration::from_secs(1))
                    .as_secs()
                    .max(1);
                return Err(LockoutError::Locked { retry_after_secs });
            }
            // Lockout has expired — clear it but keep the fail_count so a
            // brand-new burst of failures still has to climb from zero.
            record.locked_until = None;
        }

        record.window_start = now;

        if success {
            record.count = 0;
            record.lockout_level = 0;
            record.locked_until = None;
            self.persist(map).await;
            return Ok(());
        }

        record.count = record.count.saturating_add(1);

        if record.count >= MAX_FAILS {
            let max_index = LOCK_STEPS_MS.len().saturating_sub(1) as u32;
            let step_index = record.lockout_level.min(max_index) as usize;
            let lockout_ms = LOCK_STEPS_MS[step_index];
            let lockout_duration = Duration::from_millis(lockout_ms);
            record.locked_until = Some(now + lockout_duration);
            record.lockout_level = record.lockout_level.saturating_add(1).min(max_index);
            // Reset the per-window fail counter so the next burst also has
            // to climb from zero; the lockout itself is the penalty.
            record.count = 0;
            self.persist(map).await;
            return Err(LockoutError::Locked {
                retry_after_secs: lockout_duration.as_secs().max(1),
            });
        }

        self.persist(map).await;
        Ok(())
    }
}
