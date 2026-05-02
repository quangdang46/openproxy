use axum::extract::State;
use axum::{
    routing::{delete, get, patch},
    Json, Router,
};
use std::collections::BTreeMap;

use crate::server::state::AppState;

pub type PricingTable = BTreeMap<String, BTreeMap<String, f64>>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/pricing", get(get_pricing).patch(update_pricing).delete(reset_pricing))
        .route("/api/pricing/defaults", get(get_default_pricing_handler))
}

fn get_default_pricing() -> PricingTable {
    let mut pricing = PricingTable::new();

    // OpenAI defaults
    let mut openai_models = BTreeMap::new();
    openai_models.insert("input".to_string(), 0.0025);
    openai_models.insert("output".to_string(), 0.01);
    openai_models.insert("cached".to_string(), 0.00125);
    pricing.insert("openai/gpt-4o".to_string(), openai_models);

    let mut openai_mini = BTreeMap::new();
    openai_mini.insert("input".to_string(), 0.00015);
    openai_mini.insert("output".to_string(), 0.0006);
    openai_mini.insert("cached".to_string(), 0.000075);
    pricing.insert("openai/gpt-4o-mini".to_string(), openai_mini);

    // Anthropic defaults
    let mut anthropic_models = BTreeMap::new();
    anthropic_models.insert("input".to_string(), 0.003);
    anthropic_models.insert("output".to_string(), 0.015);
    anthropic_models.insert("cached".to_string(), 0.0003);
    pricing.insert("anthropic/claude-3-5-sonnet".to_string(), anthropic_models);

    // Google defaults
    let mut google_models = BTreeMap::new();
    google_models.insert("input".to_string(), 0.00125);
    google_models.insert("output".to_string(), 0.005);
    google_models.insert("cached".to_string(), 0.000125);
    pricing.insert("google/gemini-2.5-pro".to_string(), google_models);

    pricing
}

async fn get_pricing(State(state): State<AppState>) -> Json<BTreeMap<String, BTreeMap<String, f64>>> {
    let snapshot = state.db.snapshot();
    // Convert from the types::PricingTable (BTreeMap<String, BTreeMap<String, Value>>) to our simpler type
    let pricing: BTreeMap<String, BTreeMap<String, f64>> = snapshot
        .pricing
        .iter()
        .map(|(provider, models)| {
            let converted: BTreeMap<String, f64> = models
                .iter()
                .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                .collect();
            (provider.clone(), converted)
        })
        .collect();
    Json(pricing)
}

async fn update_pricing(
    State(state): State<AppState>,
    Json(req): Json<BTreeMap<String, BTreeMap<String, f64>>>,
) -> Json<serde_json::Value> {
    let result = state
        .db
        .update(|db| {
            db.pricing = req
                .into_iter()
                .map(|(k, v)| {
                    let converted: BTreeMap<String, serde_json::Value> =
                        v.into_iter().map(|(kk, vv)| (kk, serde_json::json!(vv))).collect();
                    (k, converted)
                })
                .collect();
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ResetPricingQuery {
    pub provider: Option<String>,
    pub model: Option<String>,
}

async fn reset_pricing(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<ResetPricingQuery>,
) -> Json<serde_json::Value> {
    let result = state
        .db
        .update(|db| {
            if let (Some(provider), Some(_model)) = (&params.provider, &params.model) {
                // Reset specific model
                let key = format!("{}/{}", provider, _model);
                db.pricing.remove(&key);
            } else if let Some(provider) = &params.provider {
                // Reset entire provider - remove all keys with this provider prefix
                db.pricing.retain(|key, _| !key.starts_with(provider));
            } else {
                // Reset all pricing to defaults
                db.pricing = get_default_pricing()
                    .into_iter()
                    .map(|(k, v)| {
                        let converted: BTreeMap<String, serde_json::Value> =
                            v.into_iter().map(|(kk, vv)| (kk, serde_json::json!(vv))).collect();
                        (k, converted)
                    })
                    .collect();
            }
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
    }
}

async fn get_default_pricing_handler() -> Json<PricingTable> {
    Json(get_default_pricing())
}