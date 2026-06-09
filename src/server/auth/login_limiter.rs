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
//! The store is process-local and guarded by a `Mutex`. The dashboard binds
//! `127.0.0.1` by default, so this is not a multi-node problem.
//!
//! Used by the password login handler in `crate::server::api::auth`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

struct AttemptRecord {
    fail_count: u32,
    /// Index into `LOCK_STEPS_MS`. Capped at `LOCK_STEPS_MS.len() - 1`.
    lockout_level: u32,
    /// When the current lockout expires. `None` means not locked.
    locked_until: Option<Instant>,
    /// When the most recent activity occurred. Used to age out stale records.
    window_start: Instant,
}

impl AttemptRecord {
    fn new(now: Instant) -> Self {
        Self {
            fail_count: 0,
            lockout_level: 0,
            locked_until: None,
            window_start: now,
        }
    }
}

/// In-memory progressive lockout store keyed by client IP.
pub struct LoginLimiter {
    attempts: Mutex<HashMap<IpAddr, AttemptRecord>>,
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self {
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Check whether `ip` is currently locked out and record a new attempt.
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
    pub fn check_and_record(&self, ip: IpAddr, success: bool) -> Result<(), LockoutError> {
        let now = Instant::now();
        let mut attempts = self
            .attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let record = attempts
            .entry(ip)
            .or_insert_with(|| AttemptRecord::new(now));

        // Stale records are treated as fresh.
        if now.duration_since(record.window_start) >= FAIL_WINDOW {
            record.fail_count = 0;
            record.lockout_level = 0;
            record.locked_until = None;
            record.window_start = now;
        }

        // Currently locked — refuse without recording a new failure.
        if let Some(until) = record.locked_until {
            if until > now {
                let retry_after_secs = (until - now).as_secs().max(1);
                return Err(LockoutError::Locked { retry_after_secs });
            }
            // Lockout has expired — clear it but keep the fail_count so a
            // brand-new burst of failures still has to climb from zero.
            record.locked_until = None;
        }

        record.window_start = now;

        if success {
            record.fail_count = 0;
            record.lockout_level = 0;
            record.locked_until = None;
            return Ok(());
        }

        record.fail_count = record.fail_count.saturating_add(1);

        if record.fail_count >= MAX_FAILS {
            let max_index = LOCK_STEPS_MS.len().saturating_sub(1) as u32;
            let step_index = record.lockout_level.min(max_index) as usize;
            let lockout_ms = LOCK_STEPS_MS[step_index];
            let lockout_duration = Duration::from_millis(lockout_ms);
            record.locked_until = Some(now + lockout_duration);
            record.lockout_level = record.lockout_level.saturating_add(1).min(max_index);
            // Reset the per-window fail counter so the next burst also has
            // to climb from zero; the lockout itself is the penalty.
            record.fail_count = 0;
            return Err(LockoutError::Locked {
                retry_after_secs: lockout_duration.as_secs().max(1),
            });
        }

        Ok(())
    }
}
