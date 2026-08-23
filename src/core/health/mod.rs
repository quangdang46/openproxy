//! Provider health tracking with status-aware degrade windows.
//!
//! A background daemon ([`daemon::spawn_health_daemon`]) probes every active
//! API-key provider connection on a fixed interval and feeds the observed HTTP
//! status into a process-global [`registry::HealthRegistry`]. The registry maps
//! the status onto a *degrade window*: while a connection is degraded it is
//! skipped by account fallback (via `degradedUntil` persisted on the
//! connection) and — when every connection of a provider is degraded — by the
//! combo dispatcher.
//!
//! # Degrade timing (OmniRoute `credentialHealth` parity)
//!
//! | Probe result        | Status          | Degrade window |
//! |---------------------|-----------------|----------------|
//! | `200`–`299`         | `healthy`       | cleared        |
//! | `401` / `403`       | `auth_failed`   | none (marked)  |
//! | `429`               | `rate_limited`  | 120 s (2 min)  |
//! | `503`               | `unavailable`   | 600 s (10 min) |
//! | `500` `502` `504`   | `server_error`  | 300 s (5 min)  |
//! | other `4xx`         | `unknown`       | none (reachable) |
//! | transport / timeout | `server_error`  | 300 s (5 min)  |
//!
//! `auth_failed` deliberately does *not* degrade: a bad key is an operator
//! problem that a cooldown cannot fix, and degrading would silently remove the
//! account from every combo without surfacing the cause.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;

pub mod daemon;
pub mod probe;
pub mod registry;

#[cfg(test)]
mod tests;

pub use daemon::{run_health_tick, spawn_health_daemon};
pub use probe::{probe_connection, ProbeOutcome};
pub use registry::{HealthRegistry, HealthSummary, ProviderHealthSummary};

/// `ProviderConnection.extra` key holding the RFC3339 instant until which the
/// connection is degraded. Read by `account_fallback::is_account_unavailable`.
pub const DEGRADED_UNTIL_KEY: &str = "degradedUntil";
/// `ProviderConnection.extra` key holding the last observed [`HealthStatus`].
pub const HEALTH_STATUS_KEY: &str = "healthStatus";
/// `ProviderConnection.extra` key holding the last probe timestamp (RFC3339).
pub const HEALTH_CHECKED_AT_KEY: &str = "healthCheckedAt";

/// Degrade window applied on `429 Too Many Requests`.
pub const DEGRADE_RATE_LIMITED: Duration = Duration::from_secs(120);
/// Degrade window applied on `503 Service Unavailable`.
pub const DEGRADE_UNAVAILABLE: Duration = Duration::from_secs(600);
/// Degrade window applied on `500` / `502` / `504` and transport failures.
pub const DEGRADE_SERVER_ERROR: Duration = Duration::from_secs(300);

/// Health classification of a single probe result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// `2xx` — provider reachable and authorized.
    Healthy,
    /// `401` / `403` — credentials rejected. Marked, never degraded.
    AuthFailed,
    /// `429` — quota / rate limit hit.
    RateLimited,
    /// `503` — upstream explicitly unavailable.
    Unavailable,
    /// `500` / `502` / `504` / transport failure.
    ServerError,
    /// Any other response (e.g. `404` on a provider without `/models`).
    Unknown,
}

impl HealthStatus {
    /// Classify an HTTP status code.
    pub fn from_http(status: u16) -> Self {
        match status {
            200..=299 => Self::Healthy,
            401 | 403 => Self::AuthFailed,
            429 => Self::RateLimited,
            503 => Self::Unavailable,
            500 | 502 | 504 => Self::ServerError,
            // Remaining 5xx behave like a server error; remaining 4xx mean the
            // endpoint answered, so the provider itself is reachable.
            500..=599 => Self::ServerError,
            _ => Self::Unknown,
        }
    }

    /// Classification for a transport-level failure (DNS, TLS, timeout).
    pub fn from_transport_failure() -> Self {
        Self::ServerError
    }

    /// Degrade window for this status, or `None` when the status must not
    /// remove the connection from rotation.
    pub fn degrade_duration(self) -> Option<Duration> {
        match self {
            Self::RateLimited => Some(DEGRADE_RATE_LIMITED),
            Self::Unavailable => Some(DEGRADE_UNAVAILABLE),
            Self::ServerError => Some(DEGRADE_SERVER_ERROR),
            Self::Healthy | Self::AuthFailed | Self::Unknown => None,
        }
    }

    /// Stable snake_case wire name (used in `extra` and the `/health` payload).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::AuthFailed => "auth_failed",
            Self::RateLimited => "rate_limited",
            Self::Unavailable => "unavailable",
            Self::ServerError => "server_error",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this status counts as a failure for consecutive-failure
    /// tracking (auth failures do count — the account is unusable).
    pub fn is_failure(self) -> bool {
        !matches!(self, Self::Healthy | Self::Unknown)
    }
}

/// Latest known health of one provider connection.
#[derive(Debug, Clone)]
pub struct HealthRecord {
    pub connection_id: String,
    pub provider: String,
    pub status: HealthStatus,
    /// Observed HTTP status, `None` for transport failures.
    pub http_status: Option<u16>,
    pub checked_at: DateTime<Utc>,
    /// Absolute instant the degrade window ends. `None` when not degraded.
    pub degraded_until: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub error: Option<String>,
}

impl HealthRecord {
    /// Whether the degrade window is still open at `now`.
    pub fn is_degraded_at(&self, now: DateTime<Utc>) -> bool {
        self.degraded_until.is_some_and(|until| until > now)
    }
}

static GLOBAL_HEALTH_REGISTRY: Lazy<Arc<HealthRegistry>> =
    Lazy::new(|| Arc::new(HealthRegistry::new()));

/// Process-global health registry.
///
/// `AppState::health` holds a clone of this `Arc` so HTTP handlers, the daemon,
/// the combo dispatcher, and account fallback all observe the same records
/// without threading state through pure helpers.
pub fn health_registry() -> Arc<HealthRegistry> {
    GLOBAL_HEALTH_REGISTRY.clone()
}

/// Whether every known connection of the provider serving `model` is currently
/// degraded. `model` may be `"<alias>/<model-id>"` or a bare provider id.
///
/// Returns `false` when the provider has no health records yet (unknown =
/// allowed), so a fresh process never blocks traffic.
pub fn is_model_degraded(model: &str) -> bool {
    GLOBAL_HEALTH_REGISTRY.is_model_degraded(model)
}
