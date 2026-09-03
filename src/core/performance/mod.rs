//! Performance tracking and intelligent routing system
//!
//! This module provides:
//! - [`ProviderPerformanceTracker`](tracker::ProviderPerformanceTracker): Tracks provider/model performance metrics
//! - [`ScoringEngine`](scoring::ScoringEngine): Calculates scores based on reliability, speed, and intelligence
//! - [`AutoComboStrategy`](combo::AutoComboStrategy): Auto combo strategy that uses performance scoring
//!
//! ## Scoring Algorithm
//!
//! Overall Score = (Reliability × 0.5) + (Speed × 0.25) + (Intelligence × 0.25)
//!
//! Where:
//! - Reliability: Success rate (0-1)
//! - Speed: Normalized latency score (lower latency = higher score)
//! - Intelligence: Token efficiency score (output/input ratio, capped and normalized)

pub mod combo;
pub mod scoring;
pub mod tracker;

pub use combo::{get_auto_pool_models, resolve_auto_model, AutoComboConfig, AutoComboStrategy};
pub use scoring::{PerformanceTracker, ProviderScore, ScoringEngine, ScoringWeights};
pub use tracker::{ProviderMetrics, ProviderPerformanceTracker};
