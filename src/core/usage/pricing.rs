use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::PricingTable;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostModel {
    #[serde(rename = "per_token")]
    PerToken,
    #[serde(rename = "flat_monthly")]
    FlatMonthly,
    #[serde(rename = "free")]
    Free,
    #[serde(rename = "credits")]
    Credits,
}

impl CostModel {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "per_token" | "per-token" => Self::PerToken,
            "flat_monthly" | "flat-monthly" => Self::FlatMonthly,
            "free" => Self::Free,
            "credits" => Self::Credits,
            _ => Self::PerToken,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub model: String,
    pub provider: String,
    pub cost_model: CostModel,
    #[serde(default)]
    pub price_per_million: f64,
    #[serde(default)]
    pub flat_monthly_price: f64,
    #[serde(default)]
    pub credits: f64,
}

impl ModelPricing {
    pub fn new(model: &str, provider: &str, cost_model: CostModel) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model,
            price_per_million: 0.0,
            flat_monthly_price: 0.0,
            credits: 0.0,
        }
    }

    pub fn per_token(model: &str, provider: &str, price_per_million: f64) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model: CostModel::PerToken,
            price_per_million,
            flat_monthly_price: 0.0,
            credits: 0.0,
        }
    }

    pub fn flat_monthly(model: &str, provider: &str, price: f64) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model: CostModel::FlatMonthly,
            price_per_million: 0.0,
            flat_monthly_price: price,
            credits: 0.0,
        }
    }

    pub fn free(model: &str, provider: &str) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model: CostModel::Free,
            price_per_million: 0.0,
            flat_monthly_price: 0.0,
            credits: 0.0,
        }
    }

    pub fn credits(model: &str, provider: &str, amount: f64) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model: CostModel::Credits,
            price_per_million: 0.0,
            flat_monthly_price: 0.0,
            credits: amount,
        }
    }

    pub fn calculate_cost(&self, prompt_tokens: u64, completion_tokens: u64) -> f64 {
        match self.cost_model {
            CostModel::PerToken => {
                let total_tokens = prompt_tokens + completion_tokens;
                (total_tokens as f64 / 1_000_000.0) * self.price_per_million
            }
            CostModel::FlatMonthly | CostModel::Free | CostModel::Credits => 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pricing {
    rates: BTreeMap<String, BTreeMap<String, ModelPricing>>,
}

impl Pricing {
    pub fn new() -> Self {
        let mut rates = BTreeMap::new();

        rates.insert(
            "glm".to_string(),
            BTreeMap::from([
                (
                    "glm-5.1".to_string(),
                    ModelPricing::per_token("glm-5.1", "glm", 0.6),
                ),
                (
                    "glm-5".to_string(),
                    ModelPricing::per_token("glm-5", "glm", 0.6),
                ),
                (
                    "glm-4.7".to_string(),
                    ModelPricing::per_token("glm-4.7", "glm", 0.6),
                ),
            ]),
        );

        rates.insert(
            "minimax".to_string(),
            BTreeMap::from([
                (
                    "MiniMax-M2.7".to_string(),
                    ModelPricing::per_token("MiniMax-M2.7", "minimax", 0.2),
                ),
                (
                    "MiniMax-M2.5".to_string(),
                    ModelPricing::per_token("MiniMax-M2.5", "minimax", 0.2),
                ),
            ]),
        );

        rates.insert(
            "kimi".to_string(),
            BTreeMap::from([
                (
                    "kimi-k2.5".to_string(),
                    ModelPricing::flat_monthly("kimi-k2.5", "kimi", 9.0),
                ),
                (
                    "kimi-k2.5-thinking".to_string(),
                    ModelPricing::flat_monthly("kimi-k2.5-thinking", "kimi", 9.0),
                ),
            ]),
        );

        rates.insert(
            "kiro".to_string(),
            BTreeMap::from([("all".to_string(), ModelPricing::free("all", "kiro"))]),
        );

        rates.insert(
            "opencode".to_string(),
            BTreeMap::from([("all".to_string(), ModelPricing::free("all", "opencode"))]),
        );

        rates.insert(
            "vertex".to_string(),
            BTreeMap::from([(
                "all".to_string(),
                ModelPricing::credits("all", "vertex", 300.0),
            )]),
        );

        Self { rates }
    }

    pub fn from_db(db_pricing: &PricingTable) -> Self {
        let mut rates = BTreeMap::new();

        for (provider, models) in db_pricing {
            let mut model_rates = BTreeMap::new();
            for (model, value) in models {
                let pricing = parse_model_pricing(provider, model, value);
                model_rates.insert(model.clone(), pricing);
            }
            if !model_rates.is_empty() {
                rates.insert(provider.clone(), model_rates);
            }
        }

        if rates.is_empty() {
            return Self::new();
        }

        Self { rates }
    }

    pub fn get(&self, provider: &str, model: &str) -> Option<&ModelPricing> {
        if let Some(models) = self.rates.get(provider) {
            if let Some(pricing) = models.get(model) {
                return Some(pricing);
            }
            return models.get("all");
        }
        None
    }

    pub fn calculate_cost(
        &self,
        provider: &str,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
    ) -> f64 {
        self.get(provider, model)
            .map(|p| p.calculate_cost(prompt_tokens, completion_tokens))
            .unwrap_or(0.0)
    }
}

impl Default for Pricing {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_model_pricing(provider: &str, model: &str, value: &Value) -> ModelPricing {
    if let Some(obj) = value.as_object() {
        let cost_model = obj
            .get("costModel")
            .and_then(Value::as_str)
            .map(CostModel::from_str)
            .unwrap_or(CostModel::PerToken);

        let price_per_million = obj
            .get("pricePerMillion")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);

        let flat_monthly_price = obj
            .get("flatMonthlyPrice")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);

        let credits = obj.get("credits").and_then(Value::as_f64).unwrap_or(0.0);

        return ModelPricing {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model,
            price_per_million,
            flat_monthly_price,
            credits,
        };
    }

    if let Some(num) = value.as_f64() {
        return ModelPricing::per_token(model, provider, num);
    }

    ModelPricing {
        model: model.to_string(),
        provider: provider.to_string(),
        cost_model: CostModel::PerToken,
        price_per_million: 0.0,
        flat_monthly_price: 0.0,
        credits: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glm_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("glm", "glm-5.1").unwrap();
        assert_eq!(p.cost_model, CostModel::PerToken);
        assert_eq!(p.price_per_million, 0.6);
        let cost = p.calculate_cost(1_000_000, 500_000);
        assert!((cost - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_minimax_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("minimax", "MiniMax-M2.7").unwrap();
        assert_eq!(p.cost_model, CostModel::PerToken);
        assert_eq!(p.price_per_million, 0.2);
    }

    #[test]
    fn test_kimi_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("kimi", "kimi-k2.5").unwrap();
        assert_eq!(p.cost_model, CostModel::FlatMonthly);
        assert_eq!(p.flat_monthly_price, 9.0);
    }

    #[test]
    fn test_kiro_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("kiro", "all").unwrap();
        assert_eq!(p.cost_model, CostModel::Free);
        assert_eq!(p.calculate_cost(1_000_000, 500_000), 0.0);
    }

    #[test]
    fn test_vertex_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("vertex", "all").unwrap();
        assert_eq!(p.cost_model, CostModel::Credits);
        assert_eq!(p.credits, 300.0);
    }

    #[test]
    fn test_calculate_cost_helper() {
        let pricing = Pricing::new();
        let cost = pricing.calculate_cost("glm", "glm-5.1", 1_000_000, 0);
        assert!((cost - 0.6).abs() < 0.001);
    }

    #[test]
    fn test_free_model_returns_zero() {
        let pricing = Pricing::new();
        assert_eq!(
            pricing.calculate_cost("kiro", "claude-sonnet-4.5", 100_000_000, 50_000_000),
            0.0
        );
    }

    #[test]
    fn test_unknown_model_returns_zero() {
        let pricing = Pricing::new();
        assert_eq!(
            pricing.calculate_cost("unknown", "unknown-model", 1_000_000, 1_000_000),
            0.0
        );
    }
}
