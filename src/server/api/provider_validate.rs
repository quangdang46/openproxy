use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::executor::ProviderFormat;
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
        // 9router registry/mimo-free.js:17 declares `noAuth: true`; the
        // registry's default arm would otherwise send it a bearer probe.
        "mimo-free",
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
            let base_url = snapshot.provider_nodes.iter()
                .find(|n| n.id == p)
                .and_then(|n| n.base_url.as_deref())
                .map(str::trim)
                .unwrap_or_default();
            // Fall back to the provider's well-known Anthropic-compatible
            // endpoint so a user who hasn't configured a custom node still
            // gets a real validation instead of being routed at Anthropic.
            let url = if !base_url.is_empty() {
                anthropic_compatible_messages_url(base_url)
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
                    let code = resp.status().as_u16();
                    if anthropic_compatible_is_valid(code) {
                        (true, None)
                    } else {
                        (false, Some("Invalid API key".into()))
                    }
                },
                Err(e) => (false, Some(e.to_string())),
            }
        }

        _ => {
            // 9router:236-253 — the config-driven web and media probes run
            // ahead of the default arm, so a provider they claim is never
            // answered by it. They sit inside this catch-all rather than
            // before the switch so the explicit arms above keep priority:
            // 9router has no `deepgram` case at all, so its `POST /v1/listen`
            // media probe would otherwise shadow the explicit
            // `GET /v1/projects` probe here.
            if let Some(result) = probe_service_provider(&client, &provider, &api_key).await {
                let (valid, error) = result;
                return Json(json!({
                    "valid": valid,
                    "error": if valid { None::<String> } else { error.or_else(|| Some("Invalid API key".into())) }
                })).into_response();
            }
            match default_arm_for(&provider) {
                DefaultArm::Unsupported => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": "Provider validation not supported" })),
                    )
                        .into_response();
                }
                DefaultArm::NoProbe => (true, None),
                DefaultArm::ProbeOpenAi { base_url } => {
                    // 9router:610-612 — the registry's default auth header is
                    // bearer. 9router:614 — GET /models first because it is a fast
                    // GET; the chat probe only runs when that answer is ambiguous
                    // (a transport error, or a status that is neither 2xx nor
                    // 401/403).
                    let mut probe_ok: Option<bool> = None;
                    if let Ok(resp) = client
                        .get(probe_models_url(&base_url))
                        .header("Authorization", format!("Bearer {api_key}"))
                        .send()
                        .await
                    {
                        let status = resp.status();
                        let code = status.as_u16();
                        if code == 401 || code == 403 {
                            probe_ok = Some(false);
                        } else if status.is_success() {
                            probe_ok = Some(true);
                        }
                    }
                    match probe_ok {
                        Some(valid) => (
                            valid,
                            if valid { None } else { Some("Invalid API key".into()) },
                        ),
                        // 9router:625-631 — minimal chat probe against cfg.baseUrl.
                        None => match client
                            .post(&base_url)
                            .header("Authorization", format!("Bearer {api_key}"))
                            .json(&json!({
                                "model": default_model_for(&provider),
                                "messages": [{"role": "user", "content": "ping"}],
                                "max_tokens": 1,
                            }))
                            .send()
                            .await
                        {
                            Ok(resp) => {
                                let code = resp.status().as_u16();
                                (code != 401 && code != 403, None)
                            }
                            Err(e) => (false, Some(e.to_string())),
                        },
                    }
                }
            }
        }
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

/// 9router normalizeBase + messagesUrl (validate/route.js:150-158): drop a
/// trailing slash, drop a trailing `/messages` an operator may have pasted in
/// full, then append `/v1/messages`. Probing `<base>/messages` instead hits a
/// route these endpoints do not expose, and the 404 it returns used to be read
/// as proof the key was accepted.
fn anthropic_compatible_messages_url(base_url: &str) -> String {
    let mut normalized = base_url.trim().trim_end_matches('/').to_string();
    if normalized.ends_with("/messages") {
        normalized.truncate(normalized.len() - "/messages".len());
    }
    format!("{normalized}/v1/messages")
}

/// "400/529 still confirms key accepted; only 401/403 = bad key"
/// (validate/route.js:180). A dummy body is routinely rejected on its own terms
/// — a 400 or a 529 from an overloaded gateway still proves the credential
/// reached the account, so reporting those as a bad key breaks working nodes.
fn anthropic_compatible_is_valid(status: u16) -> bool {
    status != 401 && status != 403
}

/// 9router parity for the `default:` arm of the provider match
/// (`.tmp/9router/src/app/api/providers/validate/route.js:599-604`).
///
/// The arm is not a stub: it looks the provider up in `PROVIDERS` and probes
/// the entry when its declared transport format is `"openai"` — the registry
/// barrel defaults every format-less entry to it (`open-sse/providers/index.js:14`),
/// so that covers most of the catalog. Only a provider that declares some other
/// transport, or is missing from the registry entirely, reaches the 400.
/// Reporting those as valid instead rubber-stamps every key, and the modal
/// persists a passing result as `testStatus: "active"`
/// (`web/src/components/providers/AddApiKeyModal.tsx:203`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum DefaultArm {
    /// `PROVIDERS[provider]` exists with `format === "openai"` — run the
    /// config-driven probe against this base URL.
    ProbeOpenAi { base_url: String },
    /// A provider this product knows, with no entry to probe against here.
    /// Reporting it unsupported would be wrong — the connection works, and
    /// 9router's wider registry does probe it — but there is no URL to send a
    /// key to, so validation stays advisory as it was before this gate.
    NoProbe,
    /// `!cfg || cfg.format !== "openai" || !cfg.baseUrl` — HTTP 400
    /// `{error: "Provider validation not supported"}`.
    Unsupported,
}

/// Whether this product knows the provider at all. The catalog is the record of
/// that; a provider missing from it and from `PROVIDER_CONFIGS` is a mistyped
/// id, which is the case the 400 is actually for.
fn is_known_provider(provider: &str) -> bool {
    crate::core::model::catalog::provider_catalog()
        .provider_info(provider)
        .is_some()
}

fn default_arm_for(provider: &str) -> DefaultArm {
    // Providers whose 9router registry entry declares a transport format that
    // is not "openai", so `cfg.format !== "openai"` fires. Listed with the
    // declaration each one cites; `kimi-coding` has no registry entry at all,
    // so `!cfg` fires instead.
    const NON_OPENAI_TRANSPORT: &[&str] = &[
        "claude",           // registry/claude.js:22            format: "claude"
        "kimi-coding",      // no registry entry               -> !cfg
        "antigravity",      // registry/antigravity.js:23      format: "antigravity"
        "cursor",           // registry/cursor.js:19           format: "cursor"
        "perplexity-agent", // registry/perplexity-agent.js:24 format: "openai-responses"
    ];
    if NON_OPENAI_TRANSPORT.contains(&provider) {
        return DefaultArm::Unsupported;
    }

    // The `!cfg` and `!cfg.baseUrl` arms, resolved from PROVIDER_CONFIGS — the
    // same map chat/media dispatch reads, so there is no second registry.
    // 9router's `!cfg` tests its own registry, which is broader: media-only
    // providers carry their endpoint in a media adapter instead
    // (`core/media/image/openai_compat.rs`), so a missing entry here is not
    // evidence that the provider is unsupported. The catalog is the record of
    // which providers this product knows, and only a provider absent from both
    // is a typo or an id we do not serve.
    let Some(base_url) = crate::core::executor::provider_config_base_url(provider) else {
        return if is_known_provider(provider) {
            DefaultArm::NoProbe
        } else {
            DefaultArm::Unsupported
        };
    };
    if base_url.trim().is_empty() {
        return DefaultArm::Unsupported;
    }

    // Gate on the resolved transport rather than the raw config string:
    // `ProviderConfig::anthropic()` delegates to `openai()` and so stores
    // "openai", losing the distinction the 9router registry keeps.
    let cfg_format = crate::core::executor::provider_config_format(provider).unwrap_or_default();
    match crate::core::executor::provider_sim_format(provider, &cfg_format) {
        ProviderFormat::OpenAI | ProviderFormat::OpenAICompatible => {
            DefaultArm::ProbeOpenAi { base_url }
        }
        _ => DefaultArm::Unsupported,
    }
}

/// 9router:614 — the `/models` endpoint derived from the chat base URL. 9router
/// uses two `$`-anchored replaces, so only a trailing occurrence is rewritten
/// and each rewrite consumes the suffix for the next; `strip_suffix` keeps that
/// ordering where a plain `str::replace` would also rewrite mid-URL matches.
fn probe_models_url(base_url: &str) -> String {
    let mut url = base_url.to_string();
    if let Some(stem) = url.strip_suffix("/chat/completions") {
        url = format!("{stem}/models");
    }
    if let Some(stem) = url.strip_suffix("/chatbot") {
        url = format!("{stem}/models");
    }
    url
}

/// 9router's `getDefaultModel` (`open-sse/config/providerModels.js:16-19`),
/// including its `|| "test"` fallback for a provider with no catalog models.
fn default_model_for(provider: &str) -> String {
    let catalog = crate::core::model::catalog::provider_catalog();
    catalog
        .static_alias_for_provider(provider)
        .and_then(|alias| catalog.models_for_alias(alias))
        .and_then(|models| models.first())
        .map(|model| model.id.clone())
        .unwrap_or_else(|| "test".to_string())
}

/// How a webSearch/webFetch-only or media-only provider is validated.
///
/// 9router reads the probe config off the provider registry entry
/// (`p.searchConfig || p.fetchConfig`, `p.ttsConfig || ... || p.musicConfig`),
/// which has no Rust-side equivalent, so it is transcribed into
/// [`WEB_PROBES`] and [`MEDIA_PROBES`]. Absence from a table is meaningful:
/// it is the `return null` of a config 9router does not find or cannot
/// authenticate, which hands the provider to the default arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceProbe {
    /// 9router fetches `url` and accepts every status but 401/403.
    Fetch {
        url: &'static str,
        method: ProbeMethod,
        auth: ProbeAuth,
        body: ProbeBody,
    },
    /// 9router short-circuits to `true` and issues no request: `noAuth`,
    /// `authType === "none"`, a media entry with no config at all, or an
    /// auth scheme that needs provider-specific data (`playht`, `aws-sigv4`).
    Accept,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeMethod {
    Get,
    Post,
}

/// How a [`ServiceProbe::Fetch`] carries the key. 9router applies the same
/// switch to both probes, and the two query-string forms are verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeAuth {
    /// A single header, with the scheme prefix 9router writes in front of the
    /// key (`"Bearer "`, `"Token "`, `""` for a bare `x-api-key`).
    Header {
        name: &'static str,
        prefix: &'static str,
    },
    /// google-pse / searchapi take the key in the query string, percent-encoded
    /// by `query_pairs_mut`, followed by 9router's fixed probe parameters.
    Query {
        param: &'static str,
        extra: &'static [(&'static str, &'static str)],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeBody {
    /// A GET sends no body (9router leaves `body` undefined).
    None,
    /// 9router:36 — the web POST body.
    WebPing,
    /// 9router:80 — the media POST body, whose `model` is resolved per provider.
    MediaPing,
}

/// The auth shapes 9router's two switches write. Named so each table row
/// states the scheme it sends rather than burying it in a struct literal.
const BEARER: ProbeAuth = ProbeAuth::Header {
    name: "Authorization",
    prefix: "Bearer ",
};
const TOKEN: ProbeAuth = ProbeAuth::Header {
    name: "Authorization",
    prefix: "Token ",
};
const BASIC: ProbeAuth = ProbeAuth::Header {
    name: "Authorization",
    prefix: "Basic ",
};
const X_API_KEY: ProbeAuth = ProbeAuth::Header {
    name: "x-api-key",
    prefix: "",
};
const XI_API_KEY: ProbeAuth = ProbeAuth::Header {
    name: "xi-api-key",
    prefix: "",
};
const SUBSCRIPTION_TOKEN: ProbeAuth = ProbeAuth::Header {
    name: "x-subscription-token",
    prefix: "",
};

/// 9router:14-45 — webSearch/webFetch-only providers. The switch there has no
/// `default:`, so an unrecognised `authHeader` would still fetch, unauthenticated;
/// every entry below declares a recognised one.
const WEB_PROBES: &[(&str, ServiceProbe)] = &[
    (
        "brave-search",
        ServiceProbe::Fetch {
            url: "https://api.search.brave.com/res/v1",
            method: ProbeMethod::Get,
            auth: SUBSCRIPTION_TOKEN,
            body: ProbeBody::None,
        },
    ),
    (
        "exa",
        ServiceProbe::Fetch {
            url: "https://api.exa.ai/search",
            method: ProbeMethod::Post,
            auth: X_API_KEY,
            body: ProbeBody::WebPing,
        },
    ),
    (
        "firecrawl",
        ServiceProbe::Fetch {
            url: "https://api.firecrawl.dev/v1/scrape",
            method: ProbeMethod::Post,
            auth: BEARER,
            body: ProbeBody::WebPing,
        },
    ),
    (
        "google-pse",
        ServiceProbe::Fetch {
            url: "https://www.googleapis.com/customsearch/v1",
            method: ProbeMethod::Get,
            auth: ProbeAuth::Query {
                param: "key",
                extra: &[("q", "ping"), ("cx", "test")],
            },
            body: ProbeBody::None,
        },
    ),
    (
        "jina-reader",
        ServiceProbe::Fetch {
            url: "https://r.jina.ai",
            method: ProbeMethod::Get,
            auth: BEARER,
            body: ProbeBody::None,
        },
    ),
    (
        "linkup",
        ServiceProbe::Fetch {
            url: "https://api.linkup.so/v1/search",
            method: ProbeMethod::Post,
            auth: BEARER,
            body: ProbeBody::WebPing,
        },
    ),
    (
        "ollama-search",
        ServiceProbe::Fetch {
            url: "https://ollama.com/api/web_search",
            method: ProbeMethod::Post,
            auth: BEARER,
            body: ProbeBody::WebPing,
        },
    ),
    (
        "searchapi",
        ServiceProbe::Fetch {
            url: "https://www.searchapi.io/api/v1/search",
            method: ProbeMethod::Get,
            auth: ProbeAuth::Query {
                param: "api_key",
                extra: &[("q", "ping"), ("engine", "google")],
            },
            body: ProbeBody::None,
        },
    ),
    // registry/searxng.js:18 — `authType: "none"`.
    ("searxng", ServiceProbe::Accept),
    (
        "serper",
        ServiceProbe::Fetch {
            url: "https://google.serper.dev",
            method: ProbeMethod::Post,
            auth: X_API_KEY,
            body: ProbeBody::WebPing,
        },
    ),
    (
        "tavily",
        ServiceProbe::Fetch {
            url: "https://api.tavily.com/search",
            method: ProbeMethod::Post,
            auth: BEARER,
            body: ProbeBody::WebPing,
        },
    ),
    (
        "xquik",
        ServiceProbe::Fetch {
            url: "https://xquik.com/api/v1/credits",
            method: ProbeMethod::Get,
            auth: X_API_KEY,
            body: ProbeBody::None,
        },
    ),
    (
        "youcom",
        ServiceProbe::Fetch {
            url: "https://ydc-index.io/v1/search",
            method: ProbeMethod::Get,
            auth: X_API_KEY,
            body: ProbeBody::None,
        },
    ),
];

/// 9router:47-82 — media-only providers. The switch here *does* have
/// `default: return null`, so a config that declares no recognised
/// `authHeader` is deliberately absent below and falls through to the default
/// arm: `assemblyai` (declares `authorization`, which the switch does not
/// list), `black-forest-labs`, `comfyui`, `fal-ai`, `huggingface`,
/// `nanobanana`, `recraft`, `runwayml`, `sdwebui`, `selfhosted-tts`,
/// `stability-ai`, `voyage-ai`. `serpingapi` is absent from the web table for
/// the same reason — it has no 9router registry entry at all.
const MEDIA_PROBES: &[(&str, ServiceProbe)] = &[
    // `ttsConfig` declares `aws-sigv4`, which needs provider-specific data.
    ("aws-polly", ServiceProbe::Accept),
    (
        "cartesia",
        ServiceProbe::Fetch {
            url: "https://api.cartesia.ai/tts/bytes",
            method: ProbeMethod::Post,
            auth: X_API_KEY,
            body: ProbeBody::MediaPing,
        },
    ),
    ("coqui", ServiceProbe::Accept),
    (
        "deepgram",
        ServiceProbe::Fetch {
            url: "https://api.deepgram.com/v1/listen",
            method: ProbeMethod::Post,
            auth: TOKEN,
            body: ProbeBody::MediaPing,
        },
    ),
    ("edge-tts", ServiceProbe::Accept),
    (
        "elevenlabs",
        ServiceProbe::Fetch {
            url: "https://api.elevenlabs.io/v1/text-to-speech",
            method: ProbeMethod::Post,
            auth: XI_API_KEY,
            body: ProbeBody::MediaPing,
        },
    ),
    (
        "fish-audio",
        ServiceProbe::Fetch {
            url: "https://api.fish.audio/v1/tts",
            method: ProbeMethod::Post,
            auth: BEARER,
            body: ProbeBody::MediaPing,
        },
    ),
    ("google-tts", ServiceProbe::Accept),
    (
        "inworld",
        ServiceProbe::Fetch {
            url: "https://api.inworld.ai/tts/v1/voice",
            method: ProbeMethod::Post,
            auth: BASIC,
            body: ProbeBody::MediaPing,
        },
    ),
    (
        "jina-ai",
        ServiceProbe::Fetch {
            url: "https://api.jina.ai/v1/embeddings",
            method: ProbeMethod::Post,
            auth: BEARER,
            body: ProbeBody::MediaPing,
        },
    ),
    ("local-device", ServiceProbe::Accept),
    // `ttsConfig` declares `playht`, which needs provider-specific data.
    ("playht", ServiceProbe::Accept),
    // No tts/stt/embedding/image/video/music config at all, so 9router's
    // `if (!cfg) return true` applies.
    ("topaz", ServiceProbe::Accept),
    ("tortoise", ServiceProbe::Accept),
];

/// 9router:15-18 — a provider whose every service kind is a web kind is probed
/// as a web provider. A provider with no kinds listed is treated as `["llm"]`
/// (9router's `p.serviceKinds || ["llm"]`) and so is neither web-only nor
/// media-only, which the `is_empty` guard reproduces.
fn is_web_only(kinds: &[String]) -> bool {
    !kinds.is_empty() && kinds.iter().all(|k| k == "webSearch" || k == "webFetch")
}

/// 9router:56-60 — likewise for media kinds.
fn is_media_only(kinds: &[String]) -> bool {
    const MEDIA_KINDS: &[&str] = &[
        "tts",
        "embedding",
        "stt",
        "image",
        "video",
        "music",
        "imageToText",
    ];
    !kinds.is_empty() && kinds.iter().all(|k| MEDIA_KINDS.contains(&k.as_str()))
}

/// 9router:236-253. Returns `None` when neither probe claims the provider,
/// leaving the default arm to decide.
async fn probe_service_provider(
    client: &reqwest::Client,
    provider: &str,
    api_key: &str,
) -> Option<(bool, Option<String>)> {
    let catalog = crate::core::model::catalog::provider_catalog();
    let info = catalog.provider_info(provider)?;
    let kinds = &info.service_kinds;
    if is_web_only(kinds) {
        return run_service_probe(client, provider, api_key, WEB_PROBES).await;
    }
    if is_media_only(kinds) {
        return run_service_probe(client, provider, api_key, MEDIA_PROBES).await;
    }
    None
}

async fn run_service_probe(
    client: &reqwest::Client,
    provider: &str,
    api_key: &str,
    table: &[(&str, ServiceProbe)],
) -> Option<(bool, Option<String>)> {
    let (_, probe) = table.iter().find(|(id, _)| *id == provider)?;
    let ServiceProbe::Fetch {
        url,
        method,
        auth,
        body,
    } = probe
    else {
        return Some((true, None));
    };

    let mut target = (*url).to_string();
    if let ProbeAuth::Query { param, extra } = auth {
        let Ok(mut parsed) = reqwest::Url::parse(url) else {
            return Some((false, Some(format!("invalid probe url: {url}"))));
        };
        parsed.query_pairs_mut().append_pair(param, api_key);
        for (key, value) in *extra {
            parsed.query_pairs_mut().append_pair(key, value);
        }
        target = parsed.into();
    }

    let mut req = match method {
        ProbeMethod::Get => client.get(target),
        ProbeMethod::Post => client.post(target),
    };
    if let ProbeAuth::Header { name, prefix } = auth {
        req = req.header(*name, format!("{prefix}{api_key}"));
    }
    req = match body {
        ProbeBody::None => req,
        ProbeBody::WebPing => req.json(&json!({
            "query": "ping",
            "q": "ping",
            "url": "https://example.com",
        })),
        ProbeBody::MediaPing => req.json(&json!({
            "input": "ping",
            "text": "ping",
            "prompt": "ping",
            "model": default_model_for(provider),
        })),
    };

    match req.send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            Some((code != 401 && code != 403, None))
        }
        Err(e) => Some((false, Some(e.to_string()))),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        anthropic_compatible_is_valid, anthropic_compatible_messages_url, default_arm_for,
        endpoint_is_safe, is_media_only, is_web_only, probe_models_url, DefaultArm, ProbeAuth,
        ServiceProbe, MEDIA_PROBES, WEB_PROBES,
    };

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

    /// Regression: the catch-all arm returned `(true, None)` for every provider
    /// with no explicit case, so a mistyped provider id — or one whose transport
    /// 9router refuses to probe — was reported valid and persisted by the modal
    /// as `testStatus: "active"`. 9router answers those with HTTP 400
    /// `{error: "Provider validation not supported"}` (route.js:601-602).
    #[test]
    fn unknown_provider_is_not_reported_valid() {
        for provider in ["not-a-provider", "typo-opanai", "gpt-4o", ""] {
            assert_eq!(
                default_arm_for(provider),
                DefaultArm::Unsupported,
                "{provider:?} has no PROVIDER_CONFIGS entry and must not be probed"
            );
        }
    }

    /// These reach the 400 in 9router because their registry entry declares a
    /// transport format other than `"openai"`, so `cfg.format !== "openai"`
    /// fires. Without this the anthropic family slips through the raw config
    /// string, which `ProviderConfig::anthropic()` stores as plain "openai".
    #[test]
    fn non_openai_transport_providers_are_unsupported() {
        for provider in [
            "claude",
            "kimi-coding",
            "antigravity",
            "cursor",
            "perplexity-agent",
        ] {
            assert_eq!(
                default_arm_for(provider),
                DefaultArm::Unsupported,
                "{provider:?} declares a non-openai transport in the 9router registry"
            );
        }
    }

    /// The guard against over-gating: these declare `format: "openai"` (the
    /// registry barrel's default) and 9router does probe them, so they must not
    /// be answered with a 400.
    #[test]
    fn openai_transport_providers_still_probe() {
        for (provider, base_url) in [
            (
                "kilocode",
                "https://api.kilo.ai/api/openrouter/chat/completions",
            ),
            ("cline", "https://api.cline.bot/api/v1/chat/completions"),
            ("venice", "https://api.venice.ai/api/v1/chat/completions"),
            (
                "github-models",
                "https://models.github.ai/inference/chat/completions",
            ),
        ] {
            assert_eq!(
                default_arm_for(provider),
                DefaultArm::ProbeOpenAi {
                    base_url: base_url.to_string()
                },
                "{provider:?} is openai-format in the 9router registry and must be probed"
            );
        }
    }

    /// A known media provider with no `PROVIDER_CONFIGS` entry keeps the
    /// advisory answer instead of being told validation is unsupported: 9router
    /// resolves its registry more widely than the chat executor does, and these
    /// carry their endpoint in a media adapter.
    #[test]
    fn known_providers_without_a_probe_url_stay_advisory() {
        for provider in ["recraft", "stability-ai", "voyage-ai"] {
            assert_eq!(
                default_arm_for(provider),
                DefaultArm::NoProbe,
                "{provider:?} is in the catalog, so a 400 would misreport it"
            );
        }
    }

    #[test]
    fn probe_models_url_strips_only_anchored_suffixes() {
        assert_eq!(
            probe_models_url("https://api.kilo.ai/api/openrouter/chat/completions"),
            "https://api.kilo.ai/api/openrouter/models"
        );
        assert_eq!(
            probe_models_url("https://nlpcloud.io/v1/gpu/chatbot"),
            "https://nlpcloud.io/v1/gpu/models"
        );
        // 9router's replaces are `$`-anchored and run in sequence, so only the
        // trailing occurrence is rewritten and the first consumes the suffix.
        assert_eq!(
            probe_models_url("https://h/v1/chat/completions/chat/completions"),
            "https://h/v1/chat/completions/models"
        );
        assert_eq!(probe_models_url("https://h/v1"), "https://h/v1");
    }

    #[test]
    fn web_and_media_probes_only_claim_their_own_kinds() {
        let kinds = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert!(is_web_only(&kinds(&["webSearch"])));
        assert!(is_web_only(&kinds(&["webSearch", "webFetch"])));
        // 9router:17 skips a dual-purpose provider so the LLM probe keeps it.
        assert!(!is_web_only(&kinds(&["llm", "webSearch"])));
        // 9router's `p.serviceKinds || ["llm"]` makes an absent list `["llm"]`.
        assert!(!is_web_only(&kinds(&[])));

        assert!(is_media_only(&kinds(&["tts"])));
        assert!(!is_media_only(&kinds(&["llm", "embedding", "image"])));

        // Asserted against the real catalog so a reclassified provider cannot
        // silently start claiming a probe.
        let catalog = crate::core::model::catalog::provider_catalog();
        let kinds_of = |id: &str| catalog.provider_info(id).unwrap().service_kinds.clone();
        assert!(is_web_only(&kinds_of("tavily")));
        assert!(is_media_only(&kinds_of("elevenlabs")));
        assert!(!is_web_only(&kinds_of("perplexity-agent")));
        assert!(!is_media_only(&kinds_of("tokenrouter")));
    }

    /// The tables are transcribed from the 9router registry, so pin the three
    /// outcomes per provider. Getting one wrong either 400s a provider 9router
    /// probes, or — worse — probes one 9router accepts without a request.
    #[test]
    fn service_probe_tables_match_the_9router_registry() {
        // 9router:47-82 issues no request for these: `noAuth`/`authType: none`,
        // a media entry with no config, or an auth scheme that needs
        // provider-specific data.
        for provider in [
            "searxng",
            "aws-polly",
            "coqui",
            "edge-tts",
            "google-tts",
            "local-device",
            "playht",
            "topaz",
            "tortoise",
        ] {
            let found = WEB_PROBES
                .iter()
                .chain(MEDIA_PROBES.iter())
                .find(|(id, _)| *id == provider);
            assert_eq!(
                found.map(|(_, probe)| *probe),
                Some(ServiceProbe::Accept),
                "{provider:?} is a no-request short-circuit in 9router"
            );
        }

        // 9router hands these back to the default arm instead of probing.
        for provider in [
            "serpingapi",
            "assemblyai",
            "black-forest-labs",
            "comfyui",
            "fal-ai",
            "huggingface",
            "nanobanana",
            "recraft",
            "runwayml",
            "sdwebui",
            "selfhosted-tts",
            "stability-ai",
            "voyage-ai",
        ] {
            let found = WEB_PROBES
                .iter()
                .chain(MEDIA_PROBES.iter())
                .find(|(id, _)| *id == provider);
            assert!(found.is_none(), "{provider:?} must fall through, not probe");
        }

        // Every remaining table entry carries the auth scheme 9router's switch
        // writes for that provider.
        for (provider, probe) in WEB_PROBES.iter().chain(MEDIA_PROBES.iter()) {
            let ServiceProbe::Fetch { url, auth, .. } = probe else {
                continue;
            };
            assert!(
                url.starts_with("https://") || url.starts_with("http://localhost"),
                "{provider}: {url}"
            );
            if let ProbeAuth::Query { param, extra } = auth {
                assert!(
                    !extra.is_empty(),
                    "{provider}: query probe needs its fixed params"
                );
                let _ = param;
            }
        }
    }

    /// The anthropic-compatible arm has to probe the same path 9router probes.
    /// `<base>/messages` is not a route these endpoints expose, so every probe
    /// 404'd — and a 404 read as "key accepted" persisted a broken node.
    #[test]
    fn anthropic_compatible_validate_posts_to_v1_messages() {
        assert_eq!(
            anthropic_compatible_messages_url("https://api.example.com"),
            "https://api.example.com/v1/messages"
        );
        // A pasted-in trailing slash normalizes away.
        assert_eq!(
            anthropic_compatible_messages_url("https://api.example.com/"),
            "https://api.example.com/v1/messages"
        );
        // An operator who pasted the full messages URL gets the suffix stripped
        // rather than doubled.
        assert_eq!(
            anthropic_compatible_messages_url("https://api.example.com/v1/messages"),
            "https://api.example.com/v1/messages"
        );
        assert_eq!(
            anthropic_compatible_messages_url("https://api.example.com/messages"),
            "https://api.example.com/v1/messages"
        );
    }

    #[test]
    fn anthropic_compatible_rejects_only_401_and_403() {
        for status in [200u16, 400, 404, 405, 429, 500, 529] {
            assert!(
                anthropic_compatible_is_valid(status),
                "{status} still proves the key was accepted"
            );
        }
        for status in [401u16, 403] {
            assert!(
                !anthropic_compatible_is_valid(status),
                "{status} is a bad key"
            );
        }
    }
}
