use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::server::state::AppState;

type ModelPricing = BTreeMap<String, f64>;
type ProviderPricing = BTreeMap<String, ModelPricing>;
type PricingTable = BTreeMap<String, ProviderPricing>;

const VALID_PRICING_FIELDS: &[&str] = &["input", "output", "cached", "reasoning", "cache_creation"];

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/pricing",
        get(get_pricing).patch(update_pricing).delete(reset_pricing),
    )
}

fn default_pricing() -> PricingTable {
    let mut table = PricingTable::new();
    let mut provider = ProviderPricing::new();
    provider.insert(
        "gpt-5.3-codex".to_string(),
        BTreeMap::from([
            ("input".to_string(), 1.75),
            ("output".to_string(), 14.0),
            ("cached".to_string(), 0.175),
            ("reasoning".to_string(), 14.0),
            ("cache_creation".to_string(), 1.75),
        ]),
    );
    table.insert("gh".to_string(), provider);
    table
}

fn user_pricing(snapshot: &crate::types::AppDb) -> PricingTable {
    snapshot
        .pricing
        .iter()
        .map(|(provider, models)| {
            let converted = models
                .iter()
                .filter_map(|(model, pricing)| {
                    pricing.as_object().map(|pricing_fields| {
                        let fields = pricing_fields
                            .iter()
                            .filter_map(|(field, value)| {
                                value.as_f64().map(|value| (field.clone(), value))
                            })
                            .collect::<ModelPricing>();
                        (model.clone(), fields)
                    })
                })
                .collect::<ProviderPricing>();
            (provider.clone(), converted)
        })
        .collect()
}

fn merged_pricing(user_pricing: &PricingTable) -> PricingTable {
    let mut merged = default_pricing();

    for (provider, models) in default_pricing() {
        let entry = merged.entry(provider.clone()).or_default();
        if let Some(user_models) = user_pricing.get(&provider) {
            for (model, pricing) in user_models {
                if let Some(existing) = entry.get_mut(model) {
                    for (field, value) in pricing {
                        existing.insert(field.clone(), *value);
                    }
                } else {
                    entry.insert(model.clone(), pricing.clone());
                }
            }
        }
        for (model, pricing) in models {
            entry.entry(model).or_insert(pricing);
        }
    }

    for (provider, models) in user_pricing {
        let entry = merged.entry(provider.clone()).or_default();
        for (model, pricing) in models {
            entry
                .entry(model.clone())
                .or_insert_with(|| pricing.clone());
        }
    }

    merged
}

fn pricing_to_db(table: PricingTable) -> BTreeMap<String, BTreeMap<String, Value>> {
    table
        .into_iter()
        .map(|(provider, models)| {
            let models = models
                .into_iter()
                .map(|(model, pricing)| {
                    let pricing = serde_json::to_value(pricing)
                        .unwrap_or_else(|_| Value::Object(Default::default()));
                    (model, pricing)
                })
                .collect();
            (provider, models)
        })
        .collect()
}

fn validate_pricing_payload(payload: &Value) -> Result<PricingTable, String> {
    let providers = payload
        .as_object()
        .ok_or_else(|| "Invalid pricing data format".to_string())?;

    let mut table = PricingTable::new();
    for (provider, models) in providers {
        let models = models
            .as_object()
            .ok_or_else(|| format!("Invalid pricing for provider: {provider}"))?;

        let mut converted_models = ProviderPricing::new();
        for (model, pricing) in models {
            let pricing = pricing
                .as_object()
                .ok_or_else(|| format!("Invalid pricing for model: {provider}/{model}"))?;

            let mut converted_pricing = ModelPricing::new();
            for (field, value) in pricing {
                if !VALID_PRICING_FIELDS.contains(&field.as_str()) {
                    return Err(format!(
                        "Invalid pricing field: {field} for {provider}/{model}"
                    ));
                }
                let Some(value) = value.as_f64() else {
                    return Err(format!(
                        "Invalid pricing value for {field} in {provider}/{model}: must be non-negative number"
                    ));
                };
                if value.is_sign_negative() {
                    return Err(format!(
                        "Invalid pricing value for {field} in {provider}/{model}: must be non-negative number"
                    ));
                }
                converted_pricing.insert(field.clone(), value);
            }

            converted_models.insert(model.clone(), converted_pricing);
        }
        table.insert(provider.clone(), converted_models);
    }

    Ok(table)
}

async fn get_pricing(State(state): State<AppState>) -> Response {
    let snapshot = state.db.snapshot();
    Json(merged_pricing(&user_pricing(&snapshot))).into_response()
}

async fn update_pricing(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let pricing = match validate_pricing_payload(&payload) {
        Ok(pricing) => pricing,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response()
        }
    };

    let result = state
        .db
        .update(|db| {
            let current = user_pricing(db);
            let mut merged = current;
            for (provider, models) in pricing.clone() {
                let entry = merged.entry(provider).or_default();
                for (model, model_pricing) in models {
                    entry.insert(model, model_pricing);
                }
            }
            db.pricing = pricing_to_db(merged);
        })
        .await;

    match result {
        Ok(snapshot) => Json(user_pricing(&snapshot)).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "Failed to update pricing" })),
        )
            .into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct ResetPricingQuery {
    provider: Option<String>,
    model: Option<String>,
}

async fn reset_pricing(
    State(state): State<AppState>,
    Query(query): Query<ResetPricingQuery>,
) -> Response {
    let result = state
        .db
        .update(|db| {
            let mut pricing = user_pricing(db);

            match (query.provider.as_deref(), query.model.as_deref()) {
                (Some(provider), Some(model)) => {
                    if let Some(provider_pricing) = pricing.get_mut(provider) {
                        provider_pricing.remove(model);
                        if provider_pricing.is_empty() {
                            pricing.remove(provider);
                        }
                    }
                }
                (Some(provider), None) => {
                    pricing.remove(provider);
                }
                _ => {
                    pricing.clear();
                }
            }

            db.pricing = pricing_to_db(pricing);
        })
        .await;

    match result {
        Ok(snapshot) => Json(merged_pricing(&user_pricing(&snapshot))).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "Failed to reset pricing" })),
        )
            .into_response(),
    }
}
