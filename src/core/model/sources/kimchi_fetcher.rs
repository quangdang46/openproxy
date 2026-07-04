use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;

use crate::core::model::catalog::ProviderCatalogModel;

/// Kimchi's supported models API endpoint.
const KIMCHI_API_URL: &str = "https://api.cast.ai/v1/llm/openai/supported-providers";

/// Cache TTL for fetched models (5 minutes).
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Internal simple cache for fetched model data.
struct LiveCache {
    models: Vec<ProviderCatalogModel>,
    fetched_at: Instant,
}

static MODEL_CACHE: Lazy<Mutex<Option<LiveCache>>> = Lazy::new(|| Mutex::new(None));

/// Fetch the live Kimchi model list and return their model IDs.
///
/// Results are cached for 5 minutes.  When the API is unreachable or
/// returns a non-success status the cache is served stale if available;
/// otherwise an empty vec is returned.
pub async fn fetch_kimchi_model_ids(api_key: Option<&str>) -> Vec<String> {
    let models = fetch_models_inner(api_key).await;
    models.into_iter().map(|m| m.id).collect()
}

/// Fetch the live Kimchi model list as `ProviderCatalogModel` entries.
///
/// Same caching and fallback semantics as `fetch_kimchi_model_ids`.
pub async fn fetch_kimchi_models(api_key: Option<&str>) -> Vec<ProviderCatalogModel> {
    fetch_models_inner(api_key).await
}

async fn fetch_models_inner(api_key: Option<&str>) -> Vec<ProviderCatalogModel> {
    // ── Cache hit (fresh enough) ──
    if let Ok(guard) = MODEL_CACHE.lock() {
        if let Some(ref cached) = *guard {
            if cached.fetched_at.elapsed() < CACHE_TTL {
                return cached.models.clone();
            }
        }
    }

    // ── Build request ──
    let client = reqwest::Client::new();
    let mut request = client.get(KIMCHI_API_URL);
    if let Some(key) = api_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            request = request.header("Authorization", format!("Bearer {trimmed}"));
        }
    }

    // ── Execute ──
    let models = match request.send().await {
        Ok(response) if response.status().is_success() => response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|json| extract_kimchi_models(&json))
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    // ── Update cache (even empty — negative caching prevents hammering a dead API) ──
    if let Ok(mut guard) = MODEL_CACHE.lock() {
        *guard = Some(LiveCache {
            models: models.clone(),
            fetched_at: Instant::now(),
        });
    }

    models
}

// ───────────────────────────────────────────────
// Response-format parsing
// ───────────────────────────────────────────────

/// Try to extract model entries from the JSON response.
///
/// Handles several plausible shapes the Kimchi API might return:
///
/// 1.  `{"data": [{"id": "...", ...}, ...]}`  — standard OpenAI `/v1/models`
/// 2.  `[{"id": "...", ...}, ...]`             — bare array (unwrapped list)
/// 3.  `{"providers": [{"id": "kimchi", "models": [...]}, ...]}`
/// 4.  `{"kimchi": {"models": [...]}}`         — object keyed by provider id
fn extract_kimchi_models(json: &serde_json::Value) -> Option<Vec<ProviderCatalogModel>> {
    // Strategy 1: bare array
    if let Some(arr) = json.as_array() {
        let models: Vec<ProviderCatalogModel> = arr.iter().filter_map(parse_model_value).collect();
        if !models.is_empty() {
            return Some(models);
        }
    }

    // Strategy 2: OpenAI-style `{ data: [...] }`
    if let Some(data) = json.get("data").and_then(|v| v.as_array()) {
        let models: Vec<ProviderCatalogModel> =
            data.iter().filter_map(parse_model_value).collect();
        if !models.is_empty() {
            return Some(models);
        }
    }

    // Strategy 3: `{ providers: [{ id: "kimchi", models: [...] }, ...] }`
    if let Some(providers) = json.get("providers").and_then(|v| v.as_array()) {
        for provider_val in providers {
            let id = provider_val.get("id").and_then(|v| v.as_str())?;
            if id == "kimchi" {
                if let Some(models_arr) = provider_val.get("models").and_then(|v| v.as_array()) {
                    let models: Vec<ProviderCatalogModel> =
                        models_arr.iter().filter_map(parse_model_value).collect();
                    if !models.is_empty() {
                        return Some(models);
                    }
                }
            }
        }
    }

    // Strategy 4: `{ "kimchi": { models: [...] } }` — keyed by provider id
    for key in &["kimchi"] {
        if let Some(provider_obj) = json.get(*key) {
            if let Some(models_arr) = provider_obj.get("models").and_then(|v| v.as_array()) {
                let models: Vec<ProviderCatalogModel> =
                    models_arr.iter().filter_map(parse_model_value).collect();
                if !models.is_empty() {
                    return Some(models);
                }
            }
        }
    }

    None
}

/// Parse a single JSON value into a `ProviderCatalogModel`.
///
/// The value must have at least an `"id"` field.  `"name"` and `"kind"` are
/// optional; `kind` defaults to `"llm"`.
fn parse_model_value(v: &serde_json::Value) -> Option<ProviderCatalogModel> {
    let id = v.get("id")?.as_str()?.to_string();
    let name = v.get("name").and_then(|n| n.as_str()).map(String::from);
    let kind = v
        .get("kind")
        .and_then(|k| k.as_str())
        .unwrap_or("llm")
        .to_string();

    Some(ProviderCatalogModel {
        id,
        name,
        kind,
        quota_family: None,
        strip: None,
        target_format: None,
        upstream_model_id: None,
        context_window: None,
        capabilities: None,
    })
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_kimchi_models_bare_array() {
        let json = json!([
            {"id": "gpt-4", "name": "GPT-4", "kind": "llm"},
            {"id": "gpt-4o", "name": "GPT-4o", "kind": "llm"},
        ]);
        let models = extract_kimchi_models(&json).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-4");
        assert_eq!(models[1].id, "gpt-4o");
    }

    #[test]
    fn extract_kimchi_models_openai_format() {
        let json = json!({
            "object": "list",
            "data": [
                {"id": "gpt-4", "object": "model", "created": 123, "owned_by": "openai"},
                {"id": "gpt-4o", "object": "model", "created": 124, "owned_by": "openai"},
            ]
        });
        let models = extract_kimchi_models(&json).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-4");
    }

    #[test]
    fn extract_kimchi_models_providers_array() {
        let json = json!({
            "providers": [
                {"id": "anthropic", "models": [{"id": "claude-sonnet-4"}]},
                {"id": "kimchi", "models": [
                    {"id": "minimax-m3", "name": "MiniMax-M3"},
                    {"id": "kimi-k2.7", "name": "Kimi-K2.7"},
                ]},
            ]
        });
        let models = extract_kimchi_models(&json).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "minimax-m3");
        assert_eq!(models[1].id, "kimi-k2.7");
    }

    #[test]
    fn extract_kimchi_models_keyed_by_provider() {
        let json = json!({
            "kimchi": {
                "models": [
                    {"id": "nemotron-3-ultra-fp4"},
                ]
            },
            "openai": {
                "models": [{"id": "gpt-4"}]
            }
        });
        let models = extract_kimchi_models(&json).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "nemotron-3-ultra-fp4");
    }

    #[test]
    fn extract_kimchi_models_empty() {
        let json = json!({"status": "error", "message": "unauthorized"});
        assert!(extract_kimchi_models(&json).is_none());
    }

    #[test]
    fn extract_kimchi_models_missing_id_skipped() {
        let json = json!([
            {"name": "no-id-here"},
            {"id": "valid-model", "name": "Valid"},
        ]);
        let models = extract_kimchi_models(&json).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "valid-model");
    }

    #[test]
    fn parse_model_value_default_kind() {
        let v = json!({"id": "test-model", "name": "Test"});
        let model = parse_model_value(&v).unwrap();
        assert_eq!(model.id, "test-model");
        assert_eq!(model.name, Some("Test".into()));
        assert_eq!(model.kind, "llm");
    }

    #[test]
    fn parse_model_value_explicit_kind() {
        let v = json!({"id": "embed-model", "kind": "embedding"});
        let model = parse_model_value(&v).unwrap();
        assert_eq!(model.kind, "embedding");
        assert!(model.name.is_none());
    }
}
