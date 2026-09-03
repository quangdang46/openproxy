//! HTTP API endpoints for performance tracking and auto pool management
//!
//! This module provides HTTP endpoints for:
//! - Viewing model rankings by performance score
//! - Managing auto pool model configurations
//! - Getting detailed performance metrics for specific models
//! - Configuring scoring weights

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};

use serde::{Deserialize, Serialize};

use crate::core::performance::combo::get_auto_pool_models;
use crate::core::performance::{
    AutoComboConfig, AutoComboStrategy, ProviderMetrics, ProviderScore, ScoringEngine,
    ScoringWeights,
};
use crate::server::state::AppState;
use crate::types::AppDb;

/// Response containing a ranked list of models
#[derive(Debug, Serialize, Deserialize)]
pub struct RankingsResponse {
    pub rankings: Vec<ModelRanking>,
    pub total_models: usize,
    pub scoring_weights: serde_json::Value,
}

/// Individual model ranking entry
#[derive(Debug, Serialize, Deserialize)]
pub struct ModelRanking {
    pub rank: usize,
    pub provider: String,
    pub model: String,
    pub overall_score: f64,
    pub reliability_score: f64,
    pub speed_score: f64,
    pub intelligence_score: f64,
    pub total_requests: u64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
}

/// Request to update auto pool configuration
#[derive(Debug, Deserialize)]
pub struct UpdateAutoPoolRequest {
    /// New list of models for the auto pool
    pub models: Option<Vec<String>>,
    /// Minimum samples required before scoring
    pub min_samples_for_scoring: Option<usize>,
    /// Number of top models to consider
    pub top_models_pool_size: Option<usize>,
}

/// Request to update scoring weights
#[derive(Debug, Deserialize)]
pub struct UpdateScoringWeightsRequest {
    pub reliability: f64,
    pub speed: f64,
    pub intelligence: f64,
}

/// Response for auto pool configuration
#[derive(Debug, Serialize)]
pub struct AutoPoolConfigResponse {
    pub models: Vec<String>,
    pub min_samples_for_scoring: usize,
    pub top_models_pool_size: usize,
    pub scoring_weights: serde_json::Value,
}

/// GET /api/performance/rankings
/// Returns all models ranked by their overall performance score
pub async fn get_rankings(State(_state): State<AppState>) -> impl IntoResponse {
    // In a real implementation, this would query the performance tracker
    // For now, return a placeholder response
    let response = RankingsResponse {
        rankings: vec![],
        total_models: 0,
        scoring_weights: serde_json::to_value(ScoringWeights::default_weights())
            .unwrap_or_default(),
    };

    (StatusCode::OK, Json(response))
}

/// GET /api/performance/rankings/{provider}/{model}
/// Returns detailed performance metrics for a specific model
pub async fn get_model_metrics(
    State(_state): State<AppState>,
    Path((provider, model)): Path<(String, String)>,
) -> impl IntoResponse {
    // In a real implementation, this would query the performance tracker
    // For now, return a placeholder response
    let metrics = ModelRanking {
        rank: 0,
        provider,
        model,
        overall_score: 0.0,
        reliability_score: 0.0,
        speed_score: 0.0,
        intelligence_score: 0.0,
        total_requests: 0,
        success_rate: 0.0,
        avg_latency_ms: 0.0,
    };

    (StatusCode::OK, Json(metrics))
}

/// GET /api/performance/auto-pool
/// Returns the current auto pool configuration
pub async fn get_auto_pool(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.db.snapshot();
    let models = get_auto_pool_models(&snapshot);

    let response = AutoPoolConfigResponse {
        models,
        min_samples_for_scoring: 5,
        top_models_pool_size: 3,
        scoring_weights: serde_json::to_value(ScoringWeights::default_weights())
            .unwrap_or_default(),
    };

    (StatusCode::OK, Json(response))
}

/// POST /api/performance/auto-pool
/// Updates the auto pool configuration
pub async fn update_auto_pool(
    State(state): State<AppState>,
    Json(payload): Json<UpdateAutoPoolRequest>,
) -> impl IntoResponse {
    let snapshot = state.db.snapshot();

    // In a real implementation, this would update the configuration
    // and persist it to the database

    let response = AutoPoolConfigResponse {
        models: payload
            .models
            .unwrap_or_else(|| get_auto_pool_models(&snapshot)),
        min_samples_for_scoring: payload.min_samples_for_scoring.unwrap_or(5),
        top_models_pool_size: payload.top_models_pool_size.unwrap_or(3),
        scoring_weights: serde_json::to_value(ScoringWeights::default_weights())
            .unwrap_or_default(),
    };

    (StatusCode::OK, Json(response))
}

/// GET /api/performance/scoring-weights
/// Returns the current scoring weights
pub async fn get_scoring_weights(State(_state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "reliability": 0.5,
        "speed": 0.25,
        "intelligence": 0.25
    }))
}

/// POST /api/performance/scoring-weights
/// Updates the scoring weights
pub async fn update_scoring_weights(
    State(_state): State<AppState>,
    Json(payload): Json<UpdateScoringWeightsRequest>,
) -> impl IntoResponse {
    let weights = ScoringWeights {
        reliability: payload.reliability,
        speed: payload.speed,
        intelligence: payload.intelligence,
    };

    if !weights.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Weights must sum to 1.0"
            })),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::to_value(weights).unwrap_or_default()),
    )
}

/// Create the performance API router
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/rankings", get(get_rankings))
        .route("/rankings/{provider}/{model}", get(get_model_metrics))
        .route("/auto-pool", get(get_auto_pool))
        .route("/auto-pool", post(update_auto_pool))
        .route("/scoring-weights", get(get_scoring_weights))
        .route("/scoring-weights", post(update_scoring_weights))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scoring_weights_validation() {
        let valid = ScoringWeights::default_weights();
        assert!(valid.validate());

        let invalid = ScoringWeights {
            reliability: 0.6,
            speed: 0.3,
            intelligence: 0.3,
        };
        assert!(!invalid.validate());
    }

    #[test]
    fn test_auto_pool_config_response_serialization() {
        let response = AutoPoolConfigResponse {
            models: vec![
                "openai/gpt-4o".to_string(),
                "anthropic/claude-3-5-sonnet".to_string(),
            ],
            min_samples_for_scoring: 5,
            top_models_pool_size: 3,
            scoring_weights: serde_json::to_value(ScoringWeights::default_weights())
                .unwrap_or_default(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("openai/gpt-4o"));
        assert!(json.contains("anthropic/claude-3-5-sonnet"));
    }
}
