use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ValidateRequest {
    provider: String,
    api_key: Option<String>,
    provider_specific_data: Option<serde_json::Map<String, Value>>,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/providers/validate", post(validate_provider))
}

async fn validate_provider(
    State(state): State<AppState>,
    Json(req): Json<ValidateRequest>,
) -> Response {
    let provider = req.provider.trim().to_string();
    let api_key = req.api_key.as_deref().unwrap_or("").trim().to_string();

    // No-auth providers
    let no_auth = ["edge-tts", "local-device", "sdwebui", "comfyui", "ollama-local"];
    if no_auth.contains(&provider.as_str()) {
        return Json(json!({ "valid": true })).into_response();
    }
    if provider.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Provider is required" }))).into_response();
    }
    if api_key.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "API key is required" }))).into_response();
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Json(json!({ "valid": false, "error": e.to_string() })).into_response(),
    };

    let (valid, error) = match provider.as_str() {
        "openai" => validate_bearer(&client, "https://api.openai.com/v1/models", &api_key).await,
        "deepseek" => validate_bearer(&client, "https://api.deepseek.com/models", &api_key).await,
        "groq" => validate_bearer(&client, "https://api.groq.com/openai/v1/models", &api_key).await,
        "openrouter" => validate_bearer(&client, "https://openrouter.ai/api/v1/models", &api_key).await,
        "mistral" => validate_bearer(&client, "https://api.mistral.ai/v1/models", &api_key).await,
        "perplexity" => validate_bearer(&client, "https://api.perplexity.ai/models", &api_key).await,
        "together" => validate_bearer(&client, "https://api.together.xyz/v1/models", &api_key).await,
        "fireworks" => validate_bearer(&client, "https://api.fireworks.ai/inference/v1/models", &api_key).await,
        "cerebras" => validate_bearer(&client, "https://api.cerebras.ai/v1/models", &api_key).await,
        "cohere" => validate_bearer(&client, "https://api.cohere.ai/v1/models", &api_key).await,
        "nebius" => validate_bearer(&client, "https://api.studio.nebius.ai/v1/models", &api_key).await,
        "siliconflow" => validate_bearer(&client, "https://api.siliconflow.cn/v1/models", &api_key).await,
        "hyperbolic" => validate_bearer(&client, "https://api.hyperbolic.xyz/v1/models", &api_key).await,
        "chutes" => validate_bearer(&client, "https://llm.chutes.ai/v1/models", &api_key).await,
        "nvidia" => validate_bearer(&client, "https://integrate.api.nvidia.com/v1/models", &api_key).await,
        "xiaomi-mimo" => validate_bearer(&client, "https://api.xiaomimimo.com/v1/models", &api_key).await,
        "nanobanana" => validate_bearer(&client, "https://api.nanobananaapi.ai/v1/models", &api_key).await,
        "assemblyai" => validate_bearer(&client, "https://api.assemblyai.com/v1/account", &api_key).await,
        "ollama" => validate_bearer(&client, "https://ollama.com/api/tags", &api_key).await,

        "xai" => {
            match client.get("https://api.x.ai/v1/models").header("Authorization", format!("Bearer {api_key}")).send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    (status == 200 || status == 403, None)
                }
                Err(e) => (false, Some(e.to_string())),
            }
        }

        "gemini" => {
            match client.get(format!("https://generativelanguage.googleapis.com/v1/models?key={api_key}")).send().await {
                Ok(resp) => (resp.status().is_success(), None),
                Err(e) => (false, Some(e.to_string())),
            }
        }

        "anthropic" => {
            match client.post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .json(&json!({"model": "claude-3-haiku-20240307", "max_tokens": 1, "messages": [{"role": "user", "content": "test"}]}))
                .send().await
            {
                Ok(resp) => (resp.status().as_u16() != 401, None),
                Err(e) => (false, Some(e.to_string())),
            }
        }

        "deepgram" => {
            match client.get("https://api.deepgram.com/v1/projects").header("Authorization", format!("Token {api_key}")).send().await {
                Ok(resp) => (resp.status().is_success(), None),
                Err(e) => (false, Some(e.to_string())),
            }
        }

        "blackbox" => {
            match client.post("https://api.blackbox.ai/chat/completions")
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Content-Type", "application/json")
                .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "test"}], "max_tokens": 10}))
                .send().await
            {
                Ok(resp) => {
                    let s = resp.status().as_u16();
                    (s == 200 || s == 400, None)
                }
                Err(e) => (false, Some(e.to_string())),
            }
        }

        "azure" => {
            let psd = req.provider_specific_data.as_ref();
            let endpoint = psd.and_then(|d| d.get("azureEndpoint")).and_then(Value::as_str).unwrap_or("").trim().trim_end_matches('/');
            let deployment = psd.and_then(|d| d.get("deployment")).and_then(Value::as_str).unwrap_or("gpt-4");
            let api_version = psd.and_then(|d| d.get("apiVersion")).and_then(Value::as_str).unwrap_or("2024-10-01-preview");
            if endpoint.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Azure endpoint required" }))).into_response();
            }
            let url = format!("{}/openai/deployments/{}/chat/completions?api-version={}", endpoint, deployment, api_version);
            match client.post(&url).header("api-key", &api_key).header("Content-Type", "application/json")
                .json(&json!({"messages": [{"role": "user", "content": "test"}], "max_tokens": 1})).send().await
            {
                Ok(resp) => {
                    let s = resp.status().as_u16();
                    (s != 401 && s != 403, None)
                }
                Err(e) => (false, Some(e.to_string())),
            }
        }

        "vertex" | "vertex-partner" => {
            let is_sa = api_key.starts_with('{');
            if is_sa {
                let parsed: Value = serde_json::from_str(&api_key).unwrap_or_default();
                let valid = parsed.get("client_email").is_some() && parsed.get("private_key").is_some() && parsed.get("project_id").is_some();
                (valid, if valid { None } else { Some("Invalid SA JSON".into()) })
            } else {
                match client.post(format!("https://aiplatform.googleapis.com/v1/publishers/google/models/__probe__:generateContent?key={api_key}"))
                    .header("Content-Type", "application/json").body("{}").send().await
                {
                    Ok(resp) => {
                        let s = resp.status().as_u16();
                        (s != 401 && s != 403, None)
                    }
                    Err(e) => (false, Some(e.to_string())),
                }
            }
        }

        "cloudflare-ai" => {
            let psd = req.provider_specific_data.as_ref();
            let account_id = psd.and_then(|d| d.get("accountId")).and_then(Value::as_str).unwrap_or("");
            if account_id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({ "valid": false, "error": "Missing Account ID" }))).into_response();
            }
            let url = format!("https://api.cloudflare.com/client/v4/accounts/{}/ai/v1/chat/completions", account_id);
            match client.post(&url).header("Authorization", format!("Bearer {api_key}")).header("Content-Type", "application/json")
                .json(&json!({"model": "@cf/meta/llama-3.1-8b-instruct", "messages": [{"role": "user", "content": "test"}], "max_tokens": 1}))
                .send().await
            {
                Ok(resp) => {
                    let s = resp.status().as_u16();
                    (s != 401 && s != 403 && s != 404, None)
                }
                Err(e) => (false, Some(e.to_string())),
            }
        }

        // OpenAI-compatible: use provider node baseUrl
        p if is_openai_compatible(p) => {
            let snapshot = state.db.snapshot();
            let base_url = snapshot.provider_nodes.iter()
                .find(|n| n.id == p)
                .and_then(|n| n.base_url.as_deref())
                .map(str::trim)
                .map(|u| u.trim_end_matches('/').to_string());
            match base_url {
                Some(base) => validate_bearer(&client, &format!("{}/models", base), &api_key).await,
                None => return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("{} node not found", p) }))).into_response(),
            }
        }

        // Anthropic-compatible
        p if is_anthropic_compatible(p) => {
            let snapshot = state.db.snapshot();
            let mut base_url = snapshot.provider_nodes.iter()
                .find(|n| n.id == p)
                .and_then(|n| n.base_url.as_deref())
                .map(str::trim)
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or_default();
            if base_url.ends_with("/messages") {
                base_url = base_url[..base_url.len()-9].to_string();
            }
            let url = if base_url.is_empty() { "https://api.anthropic.com/v1/messages".to_string() } else { format!("{}/messages", base_url) };
            match client.post(&url).header("x-api-key", &api_key).header("anthropic-version", "2023-06-01").header("Authorization", format!("Bearer {api_key}")).send().await {
                Ok(resp) => (resp.status().is_success(), None),
                Err(e) => (false, Some(e.to_string())),
            }
        }

        _ => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Provider validation not supported" }))).into_response();
        }
    };

    Json(json!({
        "valid": valid,
        "error": if valid { None::<String> } else { error.or_else(|| Some("Invalid API key".into())) }
    })).into_response()
}

async fn validate_bearer(client: &reqwest::Client, url: &str, api_key: &str) -> (bool, Option<String>) {
    match client.get(url).header("Authorization", format!("Bearer {api_key}")).send().await {
        Ok(resp) => (resp.status().is_success(), None),
        Err(e) => (false, Some(e.to_string())),
    }
}

fn is_openai_compatible(provider: &str) -> bool {
    matches!(provider, "custom-openai" | "custom-embedding" | "volcengine-ark" | "byteplus" | "glm-cn" | "alicode" | "alicode-intl" | "opencode-go")
}

fn is_anthropic_compatible(provider: &str) -> bool {
    matches!(provider, "custom-anthropic" | "glm" | "kimi" | "minimax" | "minimax-cn")
}
