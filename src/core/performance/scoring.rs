//! Scoring engine for intelligent routing
//!
//! Implements the scoring algorithm: Reliability 50% · Speed 25% · Intelligence 25%
//!
//! ## Scoring Formula
//!
//! Overall Score = (Reliability × 0.5) + (Speed × 0.25) + (Intelligence × 0.25)
//!
//! Where each component is normalized to [0,1] range:
//! - Reliability: Success rate (0-1)
//! - Speed: Inverse latency score (lower latency = higher score)
//! - Intelligence: Token efficiency score (output/input ratio, capped and normalized)

use crate::core::performance::tracker::ProviderMetrics;
use serde::{Deserialize, Serialize};

/// Scoring configuration with weights
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ScoringWeights {
    /// Weight for reliability component (success rate)
    pub reliability: f64,
    /// Weight for speed component (inverse latency)
    pub speed: f64,
    /// Weight for intelligence component (token efficiency)
    pub intelligence: f64,
}

impl ScoringWeights {
    /// Default weights: Reliability 50% · Speed 25% · Intelligence 25%
    pub fn default_weights() -> Self {
        Self {
            reliability: 0.5,
            speed: 0.25,
            intelligence: 0.25,
        }
    }

    /// Validate that weights sum to 1.0
    pub fn validate(&self) -> bool {
        let sum = self.reliability + self.speed + self.intelligence;
        (sum - 1.0).abs() < 0.001
    }
}

/// Calculate normalized scores for a provider/model based on metrics
#[derive(Debug, Clone)]
pub struct ScoringEngine {
    weights: ScoringWeights,
}

impl Default for ScoringEngine {
    fn default() -> Self {
        Self {
            weights: ScoringWeights::default_weights(),
        }
    }
}

impl ScoringEngine {
    /// Create a new scoring engine with default weights
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new scoring engine with custom weights
    pub fn with_weights(weights: ScoringWeights) -> Self {
        assert!(weights.validate(), "Weights must sum to 1.0");
        Self { weights }
    }

    /// Validate that the current weights sum to 1.0
    pub fn validate_weights(&self) -> bool {
        self.weights.validate()
    }

    /// Calculate reliability score from success rate
    pub fn calculate_reliability_score(success_rate: f64) -> f64 {
        // Success rate is already in [0,1] range
        success_rate.clamp(0.0, 1.0)
    }

    /// Calculate speed score from latency
    /// Lower latency = higher score
    pub fn calculate_speed_score(latency_ms: f64, max_latency_ms: f64) -> f64 {
        if max_latency_ms <= 0.0 {
            return 0.5; // Default middle score
        }
        // Normalize and invert: 1.0 - (latency / max_latency)
        let normalized = (latency_ms / max_latency_ms).clamp(0.0, 1.0);
        1.0 - normalized
    }

    /// Calculate intelligence score from token efficiency
    /// Higher output/input ratio = better utilization = higher score
    pub fn calculate_intelligence_score(input_tokens: f64, output_tokens: f64) -> f64 {
        if input_tokens <= 0.0 {
            return 0.5; // Default middle score when no input
        }

        let efficiency = output_tokens / input_tokens;
        // Cap extreme values and normalize to [0,1]
        // Assuming ideal efficiency is around 1.0 (equal input/output)
        // But we allow for higher efficiency up to 2.0
        let capped_efficiency = efficiency.min(2.0).max(0.0);
        capped_efficiency / 2.0
    }

    /// Calculate overall score using weighted components
    pub fn calculate_overall_score(&self, reliability: f64, speed: f64, intelligence: f64) -> f64 {
        (reliability * self.weights.reliability)
            + (speed * self.weights.speed)
            + (intelligence * self.weights.intelligence)
    }

    /// Calculate all scores for a provider/model based on raw metrics
    pub fn score_provider(
        &self,
        success_rate: f64,
        avg_latency_ms: f64,
        max_latency_ms: f64,
        avg_input_tokens: f64,
        avg_output_tokens: f64,
    ) -> ProviderScore {
        let reliability = Self::calculate_reliability_score(success_rate);
        let speed = Self::calculate_speed_score(avg_latency_ms, max_latency_ms);
        let intelligence = Self::calculate_intelligence_score(avg_input_tokens, avg_output_tokens);
        let overall = Self::calculate_overall_score(self, reliability, speed, intelligence);

        ProviderScore {
            reliability,
            speed,
            intelligence,
            overall,
        }
    }
}

/// Container for individual score components and overall score
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderScore {
    /// Reliability score [0,1] (success rate)
    pub reliability: f64,
    /// Speed score [0,1] (inverse latency)
    pub speed: f64,
    /// Intelligence score [0,1] (token efficiency)
    pub intelligence: f64,
    /// Overall weighted score [0,1]
    pub overall: f64,
}

impl ProviderScore {
    /// Create a default middle-score ProviderScore
    pub fn default_middle() -> Self {
        Self {
            reliability: 0.5,
            speed: 0.5,
            intelligence: 0.5,
            overall: 0.5,
        }
    }

    /// Check if score is valid (all components in [0,1])
    pub fn is_valid(&self) -> bool {
        self.reliability >= 0.0
            && self.reliability <= 1.0
            && self.speed >= 0.0
            && self.speed <= 1.0
            && self.intelligence >= 0.0
            && self.intelligence <= 1.0
            && self.overall >= 0.0
            && self.overall <= 1.0
    }
}

/// Trait defining the interface for performance tracking
#[async_trait::async_trait]
pub trait PerformanceTracker: Send + Sync {
    /// Get metrics for a specific provider/model combination
    fn get_metrics(&self, provider: &str, model: &str) -> Option<ProviderMetrics>;

    /// Get all models ranked by performance score
    async fn get_all_ranked_models(&self) -> Vec<ProviderMetrics>;

    /// Check if we have sufficient data for scoring a provider/model
    fn has_sufficient_data(&self, provider: &str, model: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scoring_weights_default() {
        let weights = ScoringWeights::default_weights();
        assert!(weights.validate());
        assert_eq!(weights.reliability, 0.5);
        assert_eq!(weights.speed, 0.25);
        assert_eq!(weights.intelligence, 0.25);
    }

    #[test]
    fn test_reliability_scoring() {
        // Perfect success rate
        assert_eq!(ScoringEngine::calculate_reliability_score(1.0), 1.0);
        // Zero success rate
        assert_eq!(ScoringEngine::calculate_reliability_score(0.0), 0.0);
        // Middle success rate
        assert_eq!(ScoringEngine::calculate_reliability_score(0.8), 0.8);
        // Clamping
        assert_eq!(ScoringEngine::calculate_reliability_score(1.5), 1.0);
        assert_eq!(ScoringEngine::calculate_reliability_score(-0.5), 0.0);
    }

    #[test]
    fn test_speed_scoring() {
        // Best case: zero latency
        assert_eq!(ScoringEngine::calculate_speed_score(0.0, 1000.0), 1.0);
        // Worst case: max latency
        assert_eq!(ScoringEngine::calculate_speed_score(1000.0, 1000.0), 0.0);
        // Middle case: half max latency
        assert_eq!(ScoringEngine::calculate_speed_score(500.0, 1000.0), 0.5);
        // Beyond max latency
        assert_eq!(ScoringEngine::calculate_speed_score(2000.0, 1000.0), 0.0);
        // Edge case: zero max latency
        assert_eq!(ScoringEngine::calculate_speed_score(500.0, 0.0), 0.5);
    }

    #[test]
    fn test_intelligence_scoring() {
        // Equal input/output tokens
        assert_eq!(
            ScoringEngine::calculate_intelligence_score(100.0, 100.0),
            0.5
        );
        // More output than input
        assert_eq!(
            ScoringEngine::calculate_intelligence_score(100.0, 200.0),
            1.0
        );
        // Much more output (capped at 2.0 ratio)
        assert_eq!(
            ScoringEngine::calculate_intelligence_score(100.0, 300.0),
            1.0
        );
        // No input tokens
        assert_eq!(ScoringEngine::calculate_intelligence_score(0.0, 100.0), 0.5);
        // Zero output
        assert_eq!(ScoringEngine::calculate_intelligence_score(100.0, 0.0), 0.0);
    }

    #[test]
    fn test_overall_scoring() {
        let engine = ScoringEngine::new();

        // Perfect scores in all categories
        let score = engine.calculate_overall_score(1.0, 1.0, 1.0);
        assert_eq!(score, 1.0);

        // Zero scores in all categories
        let score = engine.calculate_overall_score(0.0, 0.0, 0.0);
        assert_eq!(score, 0.0);

        // Mixed scores
        let score = engine.calculate_overall_score(0.8, 0.6, 0.9);
        // Expected: 0.8*0.5 + 0.6*0.25 + 0.9*0.25 = 0.4 + 0.15 + 0.225 = 0.775
        assert!((score - 0.775).abs() < 0.001);
    }

    #[test]
    fn test_provider_scoring() {
        let engine = ScoringEngine::new();

        // Score a provider with good metrics
        let score = engine.score_provider(
            0.9,    // 90% success rate
            200.0,  // 200ms avg latency
            1000.0, // 1000ms max latency (for normalization)
            50.0,   // 50 avg input tokens
            100.0,  // 100 avg output tokens
        );

        // Reliability: 0.9
        // Speed: 1.0 - (200/1000) = 0.8
        // Intelligence: min(100/50, 2.0)/2.0 = min(2.0, 2.0)/2.0 = 1.0
        // Overall: 0.9*0.5 + 0.8*0.25 + 1.0*0.25 = 0.45 + 0.2 + 0.25 = 0.9

        assert!((score.reliability - 0.9).abs() < 0.001);
        assert!((score.speed - 0.8).abs() < 0.001);
        assert!((score.intelligence - 1.0).abs() < 0.001);
        assert!((score.overall - 0.9).abs() < 0.001);
    }
}
