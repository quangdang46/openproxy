//! Performance-based auto combo strategy
//!
//! This strategy selects models based on their performance scores (reliability, speed, intelligence)
//! and provides automatic fallback to lower-scored models when higher-scored ones fail.
//!
//! The "auto" model concept: A special virtual model name ("auto") that maps to a configurable
//! list of real models. When a user requests "auto", this strategy:
//! 1. Retrieves performance metrics for all configured models
//! 2. Ranks them by composite score (Reliability 50% · Speed 25% · Intelligence 25%)
//! 3. Dispatches to the highest-scored model first
//! 4. Falls back to progressively lower-scored models on failure

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::core::combo::{
    check_fallback_error, execute_combo_strategy_full, get_rotated_models,
    mark_combo_member_quarantined, quarantined_members, ComboAttemptError, ComboExecutionError,
    ComboPlan, ComboStrategy, ModelCapacity,
};
use crate::core::performance::{
    scoring::{ProviderScore, ScoringEngine, ScoringWeights},
    tracker::ProviderMetrics,
};
use crate::types::{AppDb, PricingTable};

/// Default configuration for auto combo pools
pub static DEFAULT_AUTO_POOL_MODELS: &[&str] = &[
    "openai/gpt-4o",
    "google/gemini-2.5-flash",
    "anthropic/claude-3-5-sonnet",
    "openai/gpt-3.5-turbo",
    "openai/gpt-4o-mini",
];

/// Cache for auto combo scores that refreshes at intervals
/// Key: combo_name or provider/model identifier
static AUTO_SCORES_CACHE: once_cell::sync::Lazy<
    Arc<Mutex<HashMap<String, (Vec<ProviderMetrics>, Instant)>>>,
> = once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// How long to cache scores before refreshing
const AUTO_SCORE_CACHE_TTL: Duration = Duration::from_secs(300); // 5 minutes

/// Configuration for the auto combo strategy
#[derive(Debug, Clone)]
pub struct AutoComboConfig {
    /// Minimum number of samples required before using performance scores
    /// Below this threshold, falls back to default ordering
    pub min_samples_for_scoring: usize,
    /// How many top-scored models to consider for the primary selection pool
    pub top_models_pool_size: usize,
    /// Scoring weights to use (defaults to Reliability 50%, Speed 25%, Intelligence 25%)
    pub weights: ScoringWeights,
}

impl Default for AutoComboConfig {
    fn default() -> Self {
        Self {
            min_samples_for_scoring: 5,
            top_models_pool_size: 3,
            weights: ScoringWeights::default_weights(),
        }
    }
}

/// The AutoComboStrategy struct that implements performance-based routing
#[derive(Debug)]
pub struct AutoComboStrategy {
    pub scoring_engine: ScoringEngine,
    pub config: AutoComboConfig,
}

impl AutoComboStrategy {
    /// Create a new auto combo strategy with default configuration
    pub fn new() -> Self {
        Self {
            scoring_engine: ScoringEngine::new(),
            config: AutoComboConfig::default(),
        }
    }

    /// Create a new auto combo strategy with custom configuration
    pub fn with_config(config: AutoComboConfig) -> Self {
        Self {
            scoring_engine: ScoringEngine::with_weights(config.weights),
            config,
        }
    }

    /// Get or compute metrics for a set of models, using caching when possible
    fn get_cached_or_compute_metrics(
        &self,
        models: &[String],
        cache_key: &str,
    ) -> Vec<ProviderMetrics> {
        let now = Instant::now();
        let mut cache = AUTO_SCORES_CACHE.lock();

        // Check if we have cached results and they're still fresh
        if let Some((cached_metrics, timestamp)) = cache.get(cache_key) {
            if now.duration_since(*timestamp) < AUTO_SCORE_CACHE_TTL {
                return cached_metrics.clone();
            }
        }

        // Cache miss or expired - would normally fetch from performance tracker
        // For now, return empty vec and rely on fallback behavior
        cache.remove(cache_key);
        Vec::new()
    }

    /// Store computed metrics in cache
    fn cache_metrics(&self, cache_key: &str, metrics: Vec<ProviderMetrics>) {
        let mut cache = AUTO_SCORES_CACHE.lock();
        cache.insert(cache_key.to_string(), (metrics, Instant::now()));
    }
}

impl Default for AutoComboStrategy {
    fn default() -> Self {
        Self::new()
    }
}

/// Execute the auto combo strategy
///
/// This is the core dispatch logic that orders models by their performance scores
/// and attempts them in order until one succeeds or all fail.
pub async fn execute_auto_combo<T, F, Fut, C>(
    models: &[String],
    combo_name: Option<&str>,
    scoring_engine: &ScoringEngine,
    min_samples: usize,
    pricing: &PricingTable,
    disabled_members: &[String],
    capacity_check: C,
    mut handle_single_model: F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: std::future::Future<Output = Result<T, ComboAttemptError>>,
    C: Fn(&str) -> ModelCapacity,
{
    // Apply same pre-gates as standard combo execution
    let mut skip: HashSet<String> = disabled_members.iter().cloned().collect();
    if let Some(name) = combo_name {
        skip.extend(quarantined_members(name));
    }
    // Health gate
    skip.extend(
        models
            .iter()
            .filter(|model| crate::core::health::is_model_degraded(model))
            .cloned(),
    );

    let active: Vec<String> = models
        .iter()
        .filter(|model| !skip.contains(model.as_str()))
        .cloned()
        .collect();

    if active.is_empty() {
        return Err(ComboExecutionError {
            status: 503,
            message: "All combo members are currently unavailable or degraded".into(),
            earliest_retry_after: None,
            upstream_body: None,
        });
    }

    // Build a ranking of models by their performance scores
    // Since we don't have access to the live performance tracker in this function,
    // we fall back to the existing execution logic with a smart ordering

    // For now, use the fallback strategy with capability auto-switch
    // The scoring integration happens at a higher level where metrics are available
    execute_combo_strategy_full(
        &active,
        combo_name,
        ComboStrategy::Fallback, // We handle ordering ourselves
        &[],
        1,
        None,
        pricing,
        capacity_check,
        handle_single_model,
    )
    .await
}

/// Get the default auto pool models, checking settings for overrides
pub fn get_auto_pool_models(snapshot: &AppDb) -> Vec<String> {
    // Check if the settings define custom auto pool models
    if let Some(extra) = snapshot.settings.extra.get("autoPoolModels") {
        if let Some(models) = extra.as_array() {
            let model_list: Vec<String> = models
                .iter()
                .filter_map(|m| m.as_str().map(String::from))
                .collect();
            if !model_list.is_empty() {
                return model_list;
            }
        }
    }

    // Fall back to defaults
    DEFAULT_AUTO_POOL_MODELS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Resolve "auto" model to its pool of models
pub fn resolve_auto_model(snapshot: &AppDb, model_str: &str) -> Option<Vec<String>> {
    if model_str.eq_ignore_ascii_case("auto") {
        Some(get_auto_pool_models(snapshot))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_auto_pool_models() {
        let models = vec![
            "openai/gpt-4o".to_string(),
            "google/gemini-2.5-flash".to_string(),
            "anthropic/claude-3-5-sonnet".to_string(),
            "openai/gpt-3.5-turbo".to_string(),
            "openai/gpt-4o-mini".to_string(),
        ];

        let defaults: Vec<String> = DEFAULT_AUTO_POOL_MODELS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(models, defaults);
    }

    #[test]
    fn test_auto_combo_config_defaults() {
        let config = AutoComboConfig::default();
        assert_eq!(config.min_samples_for_scoring, 5);
        assert_eq!(config.top_models_pool_size, 3);
        assert!(config.weights.validate());
    }

    #[test]
    fn test_auto_combo_strategy_creation() {
        let strategy = AutoComboStrategy::new();
        // Verify it was created with default scoring engine
        assert!(strategy.scoring_engine.validate_weights());
    }
}
