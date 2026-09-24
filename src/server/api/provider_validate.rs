use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::state::AppState;

/// Reject an operator-supplied endpoint that points back at the host or a
/// private network.
///
/// `azureEndpoint` is fully caller-controlled and is interpolated straight into
/// a URL the server then POSTs to. Without this check the route is an SSRF
/// primitive: the response distinguishes connection-refused, timeout and HTTP
/// status, which is a three-way oracle over any host the proxy can reach —
/// including `169.254.169.254` (cloud metadata) and the proxy's own admin
/// port. The route also has a permissive CORS layer, so a web page the
/// operator merely visits can drive it.
///
/// Resolution is deliberately NOT performed here: a DNS answer can change
/// between this check and the connect (TOCTOU). We gate on scheme and on
/// literal-IP / known-internal hostnames, which is the part an attacker
/// controls directly.
fn endpoint_is_safe(endpoint: &str) -> Result<(), String> {
    let parsed: reqwest::Url = endpoint
        .parse()
        .map_err(|_| "azureEndpoint is not a valid URL".to_string())?;

    if parsed.scheme() != "https" {
        return Err("azureEndpoint must use https".to_string());
    }

    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() {
        return Err("azureEndpoint has no host".to_string());
    }

    // Names that always mean "this machine or this network".
    const INTERNAL_NAMES: &[&str] = &[
        "localhost",
        "metadata.google.internal",
        "metadata",
        "instance-data",
    ];
    if INTERNAL_NAMES.contains(&host.as_str())
        || host.ends_with(".localhost")
        || host.ends_with(".internal")
    {
        return Err("azureEndpoint points at an internal host".to_string());
    }

    // `Url::host_str` keeps the brackets around an IPv6 literal
    // ("[::1]"), which will not parse as an IpAddr until they are stripped.
    let host_for_ip = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host_for_ip.parse::<std::net::IpAddr>() {
        let blocked = match ip {
            std::net::IpAddr::V4(v4) => {
                let o = v4.octets();
                o[0] == 10
                    || o[0] == 127
                    || (o[0] == 172 && (o[1] & 0xF0) == 0x10)
                    || (o[0] == 192 && o[1] == 168)
                    || (o[0] == 169 && o[1] == 254)
                    || (o[0] == 100 && (o[1] & 0xC0) == 0x40)
                    || o[0] == 0
                    || o[0] >= 240
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00
            }
        };
        if blocked {
            return Err("azureEndpoint points at a private or reserved address".to_string());
        }
    }

    Ok(())
}

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
    headers: HeaderMap,
    Json(req): Json<ValidateRequest>,
) -> Response {
    // This route POSTs to an operator-supplied endpoint using an
    // operator-supplied API key. That is a management action, and without a
    // guard it is reachable by anything that can reach the port — including
    // cross-origin from a web page, because the router adds a permissive CORS
    // layer. Auth before touching the network.
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    let provider = req.provider.trim().to_string();
    let api_key = req.api_key.as_deref().unwrap_or("").trim().to_string();

    // No-auth providers
    let no_auth = [
        "edge-tts",
        "local-device",
        "sdwebui",
        "comfyui",
        "ollama-local",
        "opencode-zen",
    ];
    if no_auth.contains(&provider.as_str()) {
        return Json(json!({ "valid": true })).into_response();
    }
    if provider.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Provider is required" })),
        )
            .into_response();
    }
    if api_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "API key is required" })),
        )
            .into_response();
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
        // deepseek-web: web-cookie provider — validate the userToken via
        // GET /api/v0/users/current (OmniRoute webProvidersA.ts parity).
        "deepseek-web" | "ds-web" => validate_deepseek_web(&client, &api_key).await,
        "groq" => validate_bearer(&client, "https://api.groq.com/openai/v1/models", &api_key).await,
        "openrouter" => validate_bearer(&client, "https://openrouter.ai/api/v1/models", &api_key).await,
        "mistral" => validate_bearer(&client, "https://api.mistral.ai/v1/models", &api_key).await,
        "perplexity" => validate_bearer(&client, "https://api.perplexity.ai/models", &api_key).await,
        "together" => validate_bearer(&client, "https://api.together.xyz/v1/models", &api_key).await,
        "fireworks" => validate_bearer(&client, "https://api.fireworks.ai/inference/v1/models", &api_key).await,
        "cerebras" => validate_bearer(&client, "https://api.cerebras.ai/v1/models", &api_key).await,
        "cohere" => validate_bearer(&client, "https://api.cohere.ai/v1/models", &api_key).await,
        "nebius" => validate_bearer(&client, "https://api.studio.nebius.ai/v1/models", &api_key).await,
        "siliconflow" => validate_bearer(&client, "https://api.siliconflow.com/v1/models", &api_key).await,
        "hyperbolic" => validate_bearer(&client, "https://api.hyperbolic.xyz/v1/models", &api_key).await,
        "chutes" => validate_bearer(&client, "https://llm.chutes.ai/v1/models", &api_key).await,
        "nvidia" => validate_bearer(&client, "https://integrate.api.nvidia.com/v1/models", &api_key).await,
        "xiaomi-mimo" => validate_bearer(&client, "https://api.xiaomimimo.com/v1/models", &api_key).await,
        "xiaomi-tokenplan" => {
            let region = req.provider_specific_data.as_ref()
                .and_then(|d| d.get("region"))
                .and_then(|v| v.as_str())
                .unwrap_or("sgp");
            let base = match region {
                "cn" => "https://token-plan-cn.xiaomimimo.com/v1",
                "ams" => "https://token-plan-ams.xiaomimimo.com/v1",
                _ => "https://token-plan-sgp.xiaomimimo.com/v1",
            };
            validate_bearer(&client, &format!("{base}/models"), &api_key).await
        }
        "nanobanana" => validate_bearer(&client, "https://api.nanobananaapi.ai/v1/models", &api_key).await,
        "assemblyai" => validate_bearer(&client, "https://api.assemblyai.com/v1/account", &api_key).await,
        "ollama" => validate_bearer(&client, "https://ollama.com/api/tags", &api_key).await,
        "aimlapi" => validate_bearer(&client, "https://api.aimlapi.com/v1/models", &api_key).await,
        "modal" => validate_bearer(&client, "https://api.modal.com/v1/models", &api_key).await,
        "reka" => validate_bearer(&client, "https://api.reka.ai/v1/models", &api_key).await,
        "nlpcloud" => validate_bearer(&client, "https://api.nlpcloud.io/v1/gpu/chatbot", &api_key).await,
        "bazaarlink" => validate_bearer(&client, "https://bazaarlink.ai/api/v1/models", &api_key).await,
        "completions" => validate_bearer(&client, "https://completions.me/api/v1/models", &api_key).await,
        "freetheai" => validate_bearer(&client, "https://api.freetheai.xyz/v1/models", &api_key).await,
        "llm7" => validate_bearer(&client, "https://api.llm7.io/v1/models", &api_key).await,
        "kluster" => validate_bearer(&client, "https://api.kluster.ai/v1/models", &api_key).await,
        "predibase" => validate_bearer(&client, "https://serving.app.predibase.com/v1/models", &api_key).await,
        "bytez" => validate_bearer(&client, "https://api.bytez.com/models/v2", &api_key).await,
        "morph" => validate_bearer(&client, "https://api.morphllm.com/v1/models", &api_key).await,
        "longcat" => validate_bearer(&client, "https://api.longcat.chat/openai/v1/models", &api_key).await,
        "puter" => validate_bearer(&client, "https://api.puter.com/puterai/openai/v1/models", &api_key).await,
        "scaleway" => validate_bearer(&client, "https://api.scaleway.ai/v1/models", &api_key).await,
        "sambanova" => validate_bearer(&client, "https://api.sambanova.ai/v1/models", &api_key).await,
        "nscale" => validate_bearer(&client, "https://inference.api.nscale.com/v1/models", &api_key).await,
        "baseten" => validate_bearer(&client, "https://inference.baseten.co/v1/models", &api_key).await,
        "publicai" => validate_bearer(&client, "https://api.publicai.co/v1/models", &api_key).await,
        "nous-research" => validate_bearer(&client, "https://inference-api.nousresearch.com/v1/models", &api_key).await,
        "glhf" => validate_bearer(&client, "https://glhf.chat/api/openai/v1/models", &api_key).await,
        "uncloseai" => (true, None),
        "enally" => {
            match client.get("https://ai.enally.in/v1/models").header("x-api-key", &api_key).send().await {
                Ok(resp) => (resp.status().is_success(), None),
                Err(e) => (false, Some(e.to_string())),
            }
        }
        "agentrouter" => {
            match client.post("https://agentrouter.org/v1/messages")
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .json(&json!({"model": "test", "max_tokens": 1, "messages": [{"role": "user", "content": "test"}]}))
                .send().await
            {
                Ok(resp) => (resp.status().as_u16() != 401, None),
                Err(e) => (false, Some(e.to_string())),
            }
        }

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
            match client.post("https://api.blackbox.ai/v1/chat/completions")
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
            if let Err(why) = endpoint_is_safe(endpoint) {
                return (StatusCode::BAD_REQUEST, Json(json!({ "valid": false, "error": why })))
                    .into_response();
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

        "opencode-go" => {
            validate_bearer(&client, "https://opencode.ai/zen/go/v1/models", &api_key).await
        }

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
            // Fall back to the provider's well-known Anthropic-compatible
            // endpoint so a user who hasn't configured a custom node still
            // gets a real validation instead of being routed at Anthropic.
            let url = if !base_url.is_empty() {
                format!("{}/messages", base_url)
            } else {
                match p {
                    "glm" => "https://api.z.ai/api/anthropic/v1/messages".to_string(),
                    "kimi" => "https://api.kimi.com/coding/v1/messages".to_string(),
                    "minimax" => "https://api.minimax.io/anthropic/v1/messages".to_string(),
                    "minimax-cn" => "https://api.minimaxi.com/anthropic/v1/messages".to_string(),
                    _ => "https://api.anthropic.com/v1/messages".to_string(),
                }
            };
            // GLM/Kimi/MiniMax accept either x-api-key or Bearer; send a
            // minimal `ping` so the upstream actually executes auth (a HEAD
            // or empty POST tends to return 4xx that isn't auth-related).
            let body = json!({
                "model": match p {
                    "glm" => "glm-4.5-flash",
                    "kimi" => "kimi-k2.5",
                    "minimax" | "minimax-cn" => "minimax-m2",
                    _ => "claude-3-5-haiku-20241022",
                },
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}],
            });
            match client.post(&url)
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&body)
                .send().await {
                Ok(resp) => {
                    let status = resp.status();
                    // Treat any non-auth 2xx/4xx as proof the key works:
                    // some upstreams 400 on the dummy body but still validate
                    // the key. Only 401/403 mean the key is wrong.
                    let code = status.as_u16();
                    if code == 401 || code == 403 { (false, Some("Invalid API key".into())) }
                    else if status.is_success() || (400..500).contains(&code) && code != 429 {
                        let body_text = resp.text().await.unwrap_or_default();
                        let body_lower = body_text.to_lowercase();
                        let looks_auth = body_lower.contains("invalid api key")
                            || body_lower.contains("unauthorized")
                            || body_lower.contains("authentication failed");
                        (!looks_auth, if looks_auth { Some("Invalid API key".into()) } else { None })
                    } else {
                        (status.is_success(), None)
                    }
                },
                Err(e) => (false, Some(e.to_string())),
            }
        }

        _ => (true, None),
    };

    Json(json!({
        "valid": valid,
        "error": if valid { None::<String> } else { error.or_else(|| Some("Invalid API key".into())) }
    })).into_response()
}

async fn validate_bearer(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
) -> (bool, Option<String>) {
    match client
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(resp) => (resp.status().is_success(), None),
        Err(e) => (false, Some(e.to_string())),
    }
}

/// Validate a deepseek-web userToken (OmniRoute
/// `webProvidersA.ts validateDeepSeekWebProvider` parity): unwrap a
/// JSON-wrapped token, then `GET /api/v0/users/current` with browser
/// headers. 401/403 means the token is wrong; any other status proves the
/// token was accepted.
async fn validate_deepseek_web(client: &reqwest::Client, api_key: &str) -> (bool, Option<String>) {
    let mut token = api_key.trim().to_string();
    if token.starts_with('{') {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&token) {
            if let Some(v) = map.get("value").and_then(Value::as_str) {
                token = v.to_string();
            }
        }
    }
    match client
        .get("https://chat.deepseek.com/api/v0/users/current")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "*/*")
        .header("Origin", "https://chat.deepseek.com")
        .header("Referer", "https://chat.deepseek.com/")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36",
        )
        .header("X-Client-Bundle-Id", "com.deepseek.chat")
        .header("X-Client-Platform", "web")
        .header("X-Client-Version", "2.0.0")
        .send()
        .await
    {
        Ok(resp) => {
            let code = resp.status().as_u16();
            if code == 401 || code == 403 {
                (false, Some("Invalid userToken".into()))
            } else {
                (resp.status().is_success(), None)
            }
        }
        Err(e) => (false, Some(e.to_string())),
    }
}

fn is_openai_compatible(provider: &str) -> bool {
    matches!(
        provider,
        "custom-openai"
            | "custom-embedding"
            | "volcengine-ark"
            | "byteplus"
            | "glm-cn"
            | "alicode"
            | "alicode-intl"
            | "opencode-go"
    )
}

fn is_anthropic_compatible(provider: &str) -> bool {
    matches!(
        provider,
        "custom-anthropic" | "glm" | "kimi" | "minimax" | "minimax-cn"
    )
}

#[cfg(test)]
mod tests {
    use super::endpoint_is_safe;

    /// Regression (audit finding #30): `azureEndpoint` is interpolated into a
    /// URL the server then POSTs to. The response distinguished
    /// connection-refused / timeout / HTTP status, giving a three-way oracle
    /// over anything the proxy could reach — including the cloud metadata
    /// address and the proxy's own admin port.
    #[test]
    fn azure_endpoint_rejects_internal_targets() {
        for bad in [
            "http://example.com",               // not https
            "https://localhost",                // loopback by name
            "https://LOCALHOST:4623",           // case-insensitive
            "https://api.localhost",            // .localhost suffix
            "https://metadata.google.internal", // GCP metadata
            "https://169.254.169.254",          // link-local / IMDS
            "https://127.0.0.1",                // loopback literal
            "https://10.1.2.3",                 // RFC1918
            "https://172.16.0.1",               // RFC1918
            "https://192.168.1.1",              // RFC1918
            "https://100.64.0.1",               // CGNAT
            "https://0.0.0.0",                  // unspecified
            "https://[::1]",                    // IPv6 loopback
            "https://[fd00::1]",                // IPv6 ULA
            "not a url",
            "",
        ] {
            assert!(endpoint_is_safe(bad).is_err(), "must reject {bad:?}");
        }
    }

    #[test]
    fn azure_endpoint_accepts_a_real_azure_host() {
        for good in [
            "https://my-resource.openai.azure.com",
            "https://my-resource.cognitiveservices.azure.com/openai",
        ] {
            assert!(
                endpoint_is_safe(good).is_ok(),
                "must accept {good:?}: {:?}",
                endpoint_is_safe(good)
            );
        }
    }
}
