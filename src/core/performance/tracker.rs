//! Performance metrics tracking
//!
//! Collects and aggregates performance data from usage tracking to enable
//! intelligent routing decisions based on reliability, speed, and intelligence metrics.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::usage::tracker::{UsageSummary, UsageTracker};
use crate::types::{TokenUsage, UsageEntry};

/// Performance metrics for a provider/model combination
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderMetrics {
    /// Provider name (e.g., "openai", "anthropic")
    pub provider: String,
    /// Model name (e.g., "gpt-4", "claude-3-opus")
    pub model: String,

    /// Reliability metrics
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub success_rate: f64, // 0.0 to 1.0

    /// Speed metrics (in milliseconds)
    pub avg_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub min_latency_ms: u64,
    pub max_latency_ms: u64,

    /// Token/Intelligence metrics
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub avg_input_tokens: f64,
    pub avg_output_tokens: f64,
    pub token_efficiency_score: f64, // output/input ratio normalized

    /// Composite scores (0.0 to 1.0)
    pub reliability_score: f64,
    pub speed_score: f64,
    pub intelligence_score: f64,
    pub overall_score: f64,

    /// Last updated timestamp
    pub last_updated: DateTime<Utc>,

    /// Recent latency samples for p95 calculation (keep last 100 samples)
    #[serde(skip)]
    pub recent_latencies: VecDeque<u64>,
}

/// Sliding window performance tracker that maintains metrics over time
#[derive(Clone)]
pub struct ProviderPerformanceTracker {
    /// In-memory metrics cache keyed by "provider/model"
    metrics: Arc<RwLock<HashMap<String, ProviderMetrics>>>,
    /// Reference to usage tracker for raw data
    usage_tracker: Arc<UsageTracker>,
    /// Configuration for sliding window size
    window_size: usize,
    /// Minimum samples required before scoring
    min_samples: usize,
}

impl std::fmt::Debug for ProviderPerformanceTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderPerformanceTracker")
            .field("window_size", &self.window_size)
            .field("min_samples", &self.min_samples)
            .finish()
    }
}

impl ProviderPerformanceTracker {
    /// Create a new performance tracker
    pub fn new(usage_tracker: Arc<UsageTracker>, window_size: usize, min_samples: usize) -> Self {
        Self {
            metrics: Arc::new(RwLock::new(HashMap::new())),
            usage_tracker,
            window_size,
            min_samples,
        }
    }

    /// Update metrics based on recent usage data
    pub async fn update_metrics(&self) {
        let usage_db = self.usage_tracker.get_usage_db();
        let mut metrics_lock = self.metrics.write().unwrap();

        // Process each usage entry to update metrics
        for entry in &usage_db.history {
            // Skip entries without token data for reliability
            if entry.tokens.is_none() {
                continue;
            }

            let provider = entry
                .provider
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("unknown");
            let model = entry.model.clone();
            let key = format!("{}/{}", provider, model);

            // Get or create metrics for this provider/model
            let metrics = metrics_lock
                .entry(key.clone())
                .or_insert_with(|| ProviderMetrics {
                    provider: provider.to_string(),
                    model: model.clone(),
                    ..Default::default()
                });

            // Update basic counters
            metrics.total_requests += 1;

            // Determine if request was successful (status 2xx or no status = assumed success)
            let is_success = match &entry.status {
                Some(status) => status
                    .parse::<i32>()
                    .map(|s| s >= 200 && s < 300)
                    .unwrap_or(false),
                None => true, // No status typically means success
            };

            if is_success {
                metrics.successful_requests += 1;
            } else {
                metrics.failed_requests += 1;
            }

            // Update success rate
            if metrics.total_requests > 0 {
                metrics.success_rate =
                    metrics.successful_requests as f64 / metrics.total_requests as f64;
            }

            // Update token metrics
            if let Some(tokens) = &entry.tokens {
                let input_tokens = tokens.prompt_tokens.or(tokens.input_tokens).unwrap_or(0);
                let output_tokens = tokens
                    .completion_tokens
                    .or(tokens.output_tokens)
                    .unwrap_or(0);

                metrics.total_input_tokens += input_tokens;
                metrics.total_output_tokens += output_tokens;

                if metrics.total_requests > 0 {
                    metrics.avg_input_tokens =
                        metrics.total_input_tokens as f64 / metrics.total_requests as f64;
                    metrics.avg_output_tokens =
                        metrics.total_output_tokens as f64 / metrics.total_requests as f64;
                }
            }

            // Update latency (we'd need to track this separately in practice)
            // For now, we'll simulate or extract from metadata if available
            // In a real implementation, we'd track request/response timing

            // Update last updated timestamp
            metrics.last_updated = Utc::now();
        }

        // Calculate derived scores for all metrics
        self.calculate_scores(&mut metrics_lock);
    }

    /// Calculate reliability, speed, and intelligence scores
    fn calculate_scores(
        &self,
        metrics_lock: &mut std::sync::RwLockWriteGuard<'_, HashMap<String, ProviderMetrics>>,
    ) {
        // First pass: collect min/max values for normalization
        let mut min_latency = u64::MAX;
        let mut max_latency = u64::MIN;

        for metrics in metrics_lock.values() {
            // Update latency bounds
            if metrics.min_latency_ms < min_latency && metrics.min_latency_ms > 0 {
                min_latency = metrics.min_latency_ms;
            }
            if metrics.max_latency_ms > max_latency {
                max_latency = metrics.max_latency_ms;
            }
        }

        // Avoid division by zero
        if min_latency == u64::MAX {
            min_latency = 0;
        }
        if max_latency == u64::MIN {
            max_latency = 1000;
        } // Default 1 second

        // Second pass: calculate normalized scores
        for metrics in metrics_lock.values_mut() {
            // Reliability score is just success rate (0-1)
            metrics.reliability_score = metrics.success_rate;

            // Speed score: lower latency = higher score (inverted and normalized)
            if metrics.avg_latency_ms > 0.0 && max_latency > min_latency {
                let normalized_latency = (metrics.avg_latency_ms as u64 - min_latency) as f64
                    / (max_latency - min_latency) as f64;
                metrics.speed_score = 1.0 - normalized_latency.clamp(0.0, 1.0); // Invert so lower latency = higher score
            } else {
                metrics.speed_score = 0.5; // Default middle score
            }

            // Intelligence score: based on token efficiency (output/input ratio)
            // Higher output/input ratio generally indicates better utilization
            if metrics.avg_input_tokens > 0.0 {
                let efficiency = metrics.avg_output_tokens / metrics.avg_input_tokens;
                // Normalize efficiency score (0-1 range, where 1.0 is perfect efficiency)
                metrics.token_efficiency_score = efficiency.clamp(0.0, 2.0); // Cap at 2.0 for extreme cases
                metrics.intelligence_score = (metrics.token_efficiency_score / 2.0).clamp(0.0, 1.0);
            } else {
                metrics.intelligence_score = 0.5; // Default middle score
            }

            // Overall score: weighted average
            metrics.overall_score = (metrics.reliability_score * 0.5)
                + (metrics.speed_score * 0.25)
                + (metrics.intelligence_score * 0.25);
        }
    }

    /// Get metrics for a specific provider/model
    pub fn get_metrics(&self, provider: &str, model: &str) -> Option<ProviderMetrics> {
        let key = format!("{}/{}", provider, model);
        self.metrics.read().unwrap().get(&key).cloned()
    }

    /// Get all metrics sorted by overall score (descending)
    pub fn get_ranked_models(&self) -> Vec<ProviderMetrics> {
        let mut metrics: Vec<ProviderMetrics> =
            self.metrics.read().unwrap().values().cloned().collect();
        metrics.sort_by(|a, b| b.overall_score.partial_cmp(&a.overall_score).unwrap());
        metrics
    }

    /// Get top N models by score
    pub fn get_top_models(&self, count: usize) -> Vec<ProviderMetrics> {
        let mut ranked = self.get_ranked_models();
        ranked.truncate(count.min(ranked.len()));
        ranked
    }

    /// Check if we have enough samples to provide reliable scores
    pub fn has_sufficient_data(&self, provider: &str, model: &str) -> bool {
        let key = format!("{}/{}", provider, model);
        if let Some(metrics) = self.metrics.read().unwrap().get(&key) {
            metrics.total_requests >= self.min_samples as u64
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::types::TokenUsage;
    use std::sync::Arc;

    fn create_test_entry(
        provider: &str,
        model: &str,
        success: bool,
        latency_ms: u64,
        input: u64,
        output: u64,
    ) -> UsageEntry {
        UsageEntry {
            timestamp: Some(Utc::now().to_rfc3339()),
            provider: Some(provider.to_string()),
            model: model.to_string(),
            tokens: Some(TokenUsage {
                prompt_tokens: Some(input),
                input_tokens: None,
                completion_tokens: Some(output),
                output_tokens: None,
                total_tokens: Some(input + output),
                reasoning_tokens: None,
                cached_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                extra: Default::default(),
            }),
            connection_id: Some("test".to_string()),
            api_key: Some("test".to_string()),
            endpoint: Some("/v1/chat/completions".to_string()),
            cost: Some(0.001),
            status: if success {
                Some("200".to_string())
            } else {
                Some("500".to_string())
            },
            bytes_before: 0,
            bytes_after: 0,
            bytes_saved: 0,
            image_prompts: 0,
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_performance_tracker_basic() {
        // Create a mock usage tracker
        let temp_dir = std::env::temp_dir().join("openproxy_test_perf");
        let _ = std::fs::remove_dir_all(&temp_dir);
        let db = crate::db::Db::load_from(&temp_dir).await.unwrap();
        let db = Arc::new(db);
        let usage_tracker = Arc::new(crate::core::usage::tracker::UsageTracker::new(db.clone()));

        // Create performance tracker
        let perf_tracker = ProviderPerformanceTracker::new(usage_tracker.clone(), 100, 5);

        // Add some test data
        let entries = vec![
            create_test_entry("openai", "gpt-4", true, 1000, 50, 100),
            create_test_entry("openai", "gpt-4", true, 1200, 60, 120),
            create_test_entry("openai", "gpt-4", false, 3000, 30, 0),
            create_test_entry("anthropic", "claude-3", true, 800, 40, 80),
            create_test_entry("anthropic", "claude-3", true, 900, 45, 90),
        ];

        // Insert entries into the usage database
        for entry in &entries {
            let entry_clone = entry.clone();
            let _ = db
                .update_usage(move |db| {
                    db.history.push(entry_clone.clone());
                    if db.total_requests_lifetime < db.history.len() as u64 {
                        db.total_requests_lifetime = db.history.len() as u64;
                    }
                })
                .await;
        }

        // Update metrics from usage data
        perf_tracker.update_metrics().await;

        // Check that we have metrics
        let openai_gpt4 = perf_tracker.get_metrics("openai", "gpt-4").unwrap();
        assert_eq!(openai_gpt4.provider, "openai");
        assert_eq!(openai_gpt4.model, "gpt-4");
        assert_eq!(openai_gpt4.total_requests, 3);
        assert_eq!(openai_gpt4.successful_requests, 2);
        assert_eq!(openai_gpt4.failed_requests, 1);

        // Success rate should be 2/3 = 0.666...
        assert!((openai_gpt4.success_rate - 0.666).abs() < 0.01);

        // Check that scores are calculated
        assert!(openai_gpt4.reliability_score >= 0.0 && openai_gpt4.reliability_score <= 1.0);
        assert!(openai_gpt4.speed_score >= 0.0 && openai_gpt4.speed_score <= 1.0);
        assert!(openai_gpt4.intelligence_score >= 0.0 && openai_gpt4.intelligence_score <= 1.0);
        assert!(openai_gpt4.overall_score >= 0.0 && openai_gpt4.overall_score <= 1.0);
    }
}
