//! In-memory health registry keyed by provider-connection id.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;

use super::{HealthRecord, HealthStatus};
use crate::core::model::resolve_provider_alias;

/// Thread-safe map of `connection_id -> ` latest [`HealthRecord`].
///
/// Mirrors the `DashMap` pattern used by [`crate::core::circuit_breaker`]:
/// lock-free reads on the dispatch path, no per-request DB I/O.
#[derive(Default)]
pub struct HealthRegistry {
    records: DashMap<String, HealthRecord>,
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self {
            records: DashMap::new(),
        }
    }

    /// Record a probe result and return the stored record.
    ///
    /// `http_status` is `None` for transport failures, which are classified as
    /// [`HealthStatus::ServerError`].
    pub fn record_probe(
        &self,
        connection_id: &str,
        provider: &str,
        http_status: Option<u16>,
        error: Option<String>,
    ) -> HealthRecord {
        self.record_probe_at(connection_id, provider, http_status, error, Utc::now())
    }

    /// [`Self::record_probe`] with an explicit clock — used by tests to assert
    /// exact degrade windows without sleeping.
    pub fn record_probe_at(
        &self,
        connection_id: &str,
        provider: &str,
        http_status: Option<u16>,
        error: Option<String>,
        now: DateTime<Utc>,
    ) -> HealthRecord {
        let status = match http_status {
            Some(code) => HealthStatus::from_http(code),
            None => HealthStatus::from_transport_failure(),
        };

        let previous_failures = self
            .records
            .get(connection_id)
            .map(|record| record.consecutive_failures)
            .unwrap_or(0);
        let consecutive_failures = if status.is_failure() {
            previous_failures.saturating_add(1)
        } else {
            0
        };

        let degraded_until = status.degrade_duration().and_then(|window| {
            chrono::Duration::from_std(window)
                .ok()
                .map(|delta| now + delta)
        });

        let record = HealthRecord {
            connection_id: connection_id.to_string(),
            provider: provider.to_string(),
            status,
            http_status,
            checked_at: now,
            degraded_until,
            consecutive_failures,
            error,
        };

        self.records
            .insert(connection_id.to_string(), record.clone());
        record
    }

    /// Latest record for a connection, if any.
    pub fn get(&self, connection_id: &str) -> Option<HealthRecord> {
        self.records
            .get(connection_id)
            .map(|record| record.value().clone())
    }

    /// Whether the connection is inside an open degrade window.
    pub fn is_connection_degraded(&self, connection_id: &str) -> bool {
        self.is_connection_degraded_at(connection_id, Utc::now())
    }

    pub fn is_connection_degraded_at(&self, connection_id: &str, now: DateTime<Utc>) -> bool {
        self.records
            .get(connection_id)
            .is_some_and(|record| record.is_degraded_at(now))
    }

    /// End of the degrade window for a connection, when still open.
    pub fn degraded_until(&self, connection_id: &str) -> Option<DateTime<Utc>> {
        let now = Utc::now();
        self.records
            .get(connection_id)
            .and_then(|record| record.degraded_until.filter(|until| *until > now))
    }

    /// Whether *every* tracked connection of `provider` is degraded.
    ///
    /// `false` when the provider has no records — unknown providers must never
    /// be blocked (a freshly started process knows nothing yet).
    pub fn is_provider_degraded(&self, provider: &str) -> bool {
        self.is_provider_degraded_at(provider, Utc::now())
    }

    pub fn is_provider_degraded_at(&self, provider: &str, now: DateTime<Utc>) -> bool {
        let mut seen = false;
        for entry in self.records.iter() {
            if entry.value().provider != provider {
                continue;
            }
            seen = true;
            if !entry.value().is_degraded_at(now) {
                return false;
            }
        }
        seen
    }

    /// Whether the provider serving `model` (`"<alias>/<model-id>"` or a bare
    /// provider id) is fully degraded.
    pub fn is_model_degraded(&self, model: &str) -> bool {
        let prefix = model.split('/').next().unwrap_or(model).trim();
        if prefix.is_empty() {
            return false;
        }
        let provider = resolve_provider_alias(prefix);
        self.is_provider_degraded(&provider) || self.is_provider_degraded(prefix)
    }

    /// Drop the record for a connection (e.g. connection deleted).
    pub fn clear_connection(&self, connection_id: &str) {
        self.records.remove(connection_id);
    }

    /// Drop every record. Test / manual-reset helper.
    pub fn clear_all(&self) {
        self.records.clear();
    }

    /// Snapshot of all records.
    pub fn records(&self) -> Vec<HealthRecord> {
        self.records
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Aggregate view used by `GET /health`.
    pub fn summary(&self) -> HealthSummary {
        let now = Utc::now();
        let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
        let mut per_provider: BTreeMap<String, ProviderHealthSummary> = BTreeMap::new();
        let mut degraded = 0usize;
        let mut connections = 0usize;

        for entry in self.records.iter() {
            let record = entry.value();
            connections += 1;
            *by_status
                .entry(record.status.as_str().to_string())
                .or_insert(0) += 1;

            let provider = per_provider
                .entry(record.provider.clone())
                .or_insert_with(|| ProviderHealthSummary {
                    provider: record.provider.clone(),
                    connections: 0,
                    healthy: 0,
                    degraded: 0,
                    degraded_until: None,
                    statuses: BTreeMap::new(),
                });
            provider.connections += 1;
            *provider
                .statuses
                .entry(record.status.as_str().to_string())
                .or_insert(0) += 1;

            if record.is_degraded_at(now) {
                degraded += 1;
                provider.degraded += 1;
                provider.degraded_until = match (provider.degraded_until, record.degraded_until) {
                    (Some(current), Some(candidate)) if current >= candidate => Some(current),
                    (_, candidate @ Some(_)) => candidate,
                    (current, None) => current,
                };
            } else if record.status == HealthStatus::Healthy {
                provider.healthy += 1;
            }
        }

        let healthy = by_status.get("healthy").copied().unwrap_or(0);
        HealthSummary {
            connections,
            healthy,
            degraded,
            by_status,
            providers: per_provider.into_values().collect(),
        }
    }
}

/// Aggregate health across all tracked connections.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSummary {
    /// Number of connections with a health record.
    pub connections: usize,
    /// Connections whose last probe was `2xx`.
    pub healthy: usize,
    /// Connections inside an open degrade window.
    pub degraded: usize,
    /// Count per [`HealthStatus::as_str`].
    pub by_status: BTreeMap<String, usize>,
    /// Per-provider breakdown, sorted by provider id.
    pub providers: Vec<ProviderHealthSummary>,
}

/// Per-provider slice of [`HealthSummary`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealthSummary {
    pub provider: String,
    pub connections: usize,
    pub healthy: usize,
    pub degraded: usize,
    /// Latest degrade expiry across the provider's connections.
    pub degraded_until: Option<DateTime<Utc>>,
    pub statuses: BTreeMap<String, usize>,
}
