use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{Duration as ChronoDuration, Utc};
use rand::RngCore;
use reqwest::{Client, Proxy, RequestBuilder};
use serde::Serialize;
use serde_json::{json, Value};
use url::Url;
use uuid::Uuid;

use crate::core::model::catalog::provider_catalog;
use crate::server::state::AppState;
use crate::types::ProviderConnection;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const PROXY_TEST_TIMEOUT: Duration = Duration::from_secs(8);
const TOKEN_EXPIRY_BUFFER_SECS: i64 = 5 * 60;
const OLLAMA_LOCAL_DEFAULT_HOST: &str = "http://localhost:11434";

const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const GEMINI_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GEMINI_CLIENT_SECRET: &str = "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";
const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const ANTIGRAVITY_CLIENT_SECRET: &str = "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";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const KIRO_SOCIAL_REFRESH_URL: &str = "https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken";
const QWEN_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
const QWEN_TOKEN_URL: &str = "https://chat.qwen.ai/api/v1/oauth2/token";
const CLINE_REFRESH_URL: &str = "https://api.cline.bot/api/v1/auth/refresh";
const CLINE_USERS_ME_URL: &str = "https://api.cline.bot/api/v1/users/me";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderTestResponse {
    valid: bool,
    error: Option<String>,
    refreshed: bool,
}

#[derive(Debug, Clone, Default)]
struct EffectiveProxy {
    connection_proxy_enabled: bool,
    connection_proxy_url: String,
    connection_no_proxy: String,
    vercel_relay_url: Option<String>,
}

#[derive(Debug, Clone)]
struct RefreshResult {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug)]
struct ConnectionTestResult {
    valid: bool,
    error: Option<String>,
    refreshed: bool,
    new_tokens: Option<RefreshResult>,
}

#[derive(Debug)]
struct PreparedRequest {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<PreparedBody>,
}

#[derive(Debug)]
enum PreparedBody {
    Json(Value),
    Form(Vec<(String, String)>),
}

pub(super) async fn test_provider_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(connection) = state
        .db
        .snapshot()
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Connection not found" })),
        )
            .into_response();
    };

    let effective_proxy = resolve_effective_proxy(&state, &connection);
    if effective_proxy.connection_proxy_enabled
        && !effective_proxy.connection_proxy_url.is_empty()
        && effective_proxy.vercel_relay_url.is_none()
    {
        if let Err(error) = test_proxy_url(&effective_proxy.connection_proxy_url).await {
            persist_test_result(
                &state,
                &connection.id,
                ConnectionTestResult {
                    valid: false,
                    error: Some(error.clone()),
                    refreshed: false,
                    new_tokens: None,
                },
            )
            .await;

            return Json(ProviderTestResponse {
                valid: false,
                error: Some(error),
                refreshed: false,
            })
            .into_response();
        }
    }

    let auth_type = connection.auth_type.trim().to_ascii_lowercase();
    let result = if matches!(auth_type.as_str(), "apikey" | "api_key" | "cookie") {
        test_api_key_connection(&state, &connection, &effective_proxy).await
    } else {
        test_oauth_connection(&state, &connection, &effective_proxy).await
    };

    persist_test_result(&state, &connection.id, result).await
}

async fn persist_test_result(
    state: &AppState,
    connection_id: &str,
    result: ConnectionTestResult,
) -> Response {
    let error = result.error.clone();
    let refreshed = result.refreshed;
    let new_tokens = result.new_tokens.clone();

    let connection_id = connection_id.to_string();
    let _ = state
        .db
        .update(|db| {
            let Some(connection) = db
                .provider_connections
                .iter_mut()
                .find(|connection| connection.id == connection_id)
            else {
                return;
            };

            connection.test_status =
                Some(if result.valid { "active" } else { "error" }.to_string());
            connection.last_error = if result.valid { None } else { error.clone() };
            connection.last_error_at = if result.valid {
                None
            } else {
                Some(Utc::now().to_rfc3339())
            };
            connection.updated_at = Some(Utc::now().to_rfc3339());

            if let Some(tokens) = &new_tokens {
                connection.access_token = Some(tokens.access_token.clone());
                if let Some(refresh_token) = &tokens.refresh_token {
                    connection.refresh_token = Some(refresh_token.clone());
                }
                if let Some(expires_in) = tokens.expires_in {
                    connection.expires_in = Some(expires_in);
                    connection.expires_at =
                        Some((Utc::now() + ChronoDuration::seconds(expires_in)).to_rfc3339());
                }
            }
        })
        .await;

    Json(ProviderTestResponse {
        valid: result.valid,
        error,
        refreshed,
    })
    .into_response()
}

async fn test_oauth_connection(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
) -> ConnectionTestResult {
    if connection
        .access_token
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        return invalid("No access token");
    }

    let token_expired = is_token_expired(connection);
    let mut access_token = connection.access_token.clone().unwrap_or_default();
    let mut refreshed = false;
    let mut new_tokens = None;

    if is_refreshable_provider(&connection.provider) && token_expired {
        if let Some(refresh_token) = connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            match refresh_oauth_token(connection, refresh_token, effective_proxy).await {
                Ok(tokens) => {
                    access_token = tokens.access_token.clone();
                    refreshed = true;
                    new_tokens = Some(tokens);
                }
                Err(_) if is_check_expiry_provider(&connection.provider) => {
                    return invalid("Token expired and refresh failed");
                }
                Err(_) => {}
            }
        } else if is_check_expiry_provider(&connection.provider) {
            return invalid("Token expired");
        }
    }

    if is_check_expiry_provider(&connection.provider) {
        if refreshed {
            return ConnectionTestResult {
                valid: true,
                error: None,
                refreshed,
                new_tokens,
            };
        }

        if token_expired {
            return invalid("Token expired");
        }

        return ConnectionTestResult {
            valid: true,
            error: None,
            refreshed: false,
            new_tokens: None,
        };
    }

    if connection.provider == "cursor" || connection.provider == "codebuddy" {
        return ConnectionTestResult {
            valid: true,
            error: None,
            refreshed,
            new_tokens,
        };
    }

    if connection.provider == "cline" {
        let initial = probe_cline_access_token(state, effective_proxy, &access_token).await;
        if initial.valid || initial.error.as_deref() != Some("Token invalid or revoked") {
            return ConnectionTestResult {
                valid: initial.valid,
                error: initial.error,
                refreshed,
                new_tokens,
            };
        }

        let Some(refresh_token) = connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return ConnectionTestResult {
                valid: false,
                error: Some("Token invalid or revoked".to_string()),
                refreshed,
                new_tokens,
            };
        };

        match refresh_cline_token(refresh_token, effective_proxy).await {
            Ok(tokens) => {
                let access_token = tokens.access_token.clone();
                let retry = probe_cline_access_token(state, effective_proxy, &access_token).await;
                ConnectionTestResult {
                    valid: retry.valid,
                    error: retry.error,
                    refreshed: retry.valid,
                    new_tokens: if retry.valid { Some(tokens) } else { None },
                }
            }
            Err(_) => invalid("Token invalid or revoked"),
        }
    } else {
        let request = oauth_probe_request(&connection.provider, &access_token);
        match request {
            Some(request) => {
                let response =
                    execute_request(state, &connection.provider, effective_proxy, request).await;
                let accepted = match response {
                    Ok(response) => response,
                    Err(error) => return invalid(&error),
                };

                if accepted.status().is_success()
                    || matches!(
                        connection.provider.as_str(),
                        "codex" if accepted.status().as_u16() == 400
                    )
                {
                    return ConnectionTestResult {
                        valid: true,
                        error: None,
                        refreshed,
                        new_tokens,
                    };
                }

                let error = match accepted.status() {
                    StatusCode::UNAUTHORIZED => "Token invalid or revoked".to_string(),
                    StatusCode::FORBIDDEN => "Access denied".to_string(),
                    status => format!("API returned {}", status.as_u16()),
                };

                ConnectionTestResult {
                    valid: false,
                    error: Some(error),
                    refreshed,
                    new_tokens,
                }
            }
            None => invalid("Provider test not supported"),
        }
    }
}

async fn test_api_key_connection(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
) -> ConnectionTestResult {
    if is_openai_compatible_provider(&connection.provider) {
        let Some(base_url) = provider_specific_string(connection, "baseUrl") else {
            return invalid("Missing base URL");
        };
        let response = execute_request(
            state,
            &connection.provider,
            effective_proxy,
            PreparedRequest {
                method: Method::GET,
                url: format!("{}/models", base_url.trim_end_matches('/')),
                headers: vec![(
                    "Authorization".to_string(),
                    format!("Bearer {}", connection.api_key.clone().unwrap_or_default()),
                )],
                body: None,
            },
        )
        .await;
        return compatible_result(response, "Invalid API key or base URL");
    }

    if is_anthropic_compatible_provider(&connection.provider) {
        let Some(base_url) = provider_specific_string(connection, "baseUrl") else {
            return invalid("Missing base URL");
        };
        let normalized = normalize_anthropic_models_url(&base_url);
        let api_key = connection.api_key.clone().unwrap_or_default();
        let response = execute_request(
            state,
            &connection.provider,
            effective_proxy,
            PreparedRequest {
                method: Method::GET,
                url: normalized,
                headers: vec![
                    ("x-api-key".to_string(), api_key.clone()),
                    ("anthropic-version".to_string(), "2023-06-01".to_string()),
                    ("Authorization".to_string(), format!("Bearer {api_key}")),
                ],
                body: None,
            },
        )
        .await;
        return compatible_result(response, "Invalid API key or base URL");
    }

    let response = match connection.provider.as_str() {
        "cloudflare-ai" => test_cloudflare_ai_connection(state, connection, effective_proxy).await,
        "azure" => test_azure_connection(state, connection, effective_proxy).await,
        "openai" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.openai.com/v1/models",
                "Invalid API key",
            )
            .await
        }
        "anthropic" => {
            anthropic_first_party_test(
                state,
                connection,
                effective_proxy,
                "https://api.anthropic.com/v1/messages",
                "claude-3-haiku-20240307",
            )
            .await
        }
        "gemini" => test_gemini_api_key_connection(state, connection, effective_proxy).await,
        "openrouter" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://openrouter.ai/api/v1/auth/key",
                "Invalid API key",
            )
            .await
        }
        "glm" => {
            anthropic_like_status_test(
                state,
                connection,
                effective_proxy,
                "https://api.z.ai/api/anthropic/v1/messages",
                "glm-4.7",
                "Invalid API key",
            )
            .await
        }
        "glm-cn" => {
            openai_chat_status_test(
                state,
                connection,
                effective_proxy,
                "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
                "glm-4.7",
                "Invalid API key",
            )
            .await
        }
        "minimax" => {
            anthropic_like_status_test(
                state,
                connection,
                effective_proxy,
                "https://api.minimax.io/anthropic/v1/messages",
                "minimax-m2",
                "Invalid API key",
            )
            .await
        }
        "minimax-cn" => {
            anthropic_like_status_test(
                state,
                connection,
                effective_proxy,
                "https://api.minimaxi.com/anthropic/v1/messages",
                "minimax-m2",
                "Invalid API key",
            )
            .await
        }
        "kimi" => {
            anthropic_like_status_test(
                state,
                connection,
                effective_proxy,
                "https://api.kimi.com/coding/v1/messages",
                "kimi-latest",
                "Invalid API key",
            )
            .await
        }
        "alicode" => {
            openai_chat_status_test(
                state,
                connection,
                effective_proxy,
                "https://coding.dashscope.aliyuncs.com/v1/chat/completions",
                &default_catalog_model("alicode"),
                "Invalid API key",
            )
            .await
        }
        "alicode-intl" => {
            openai_chat_status_test(
                state,
                connection,
                effective_proxy,
                "https://coding-intl.dashscope.aliyuncs.com/v1/chat/completions",
                &default_catalog_model("alicode-intl"),
                "Invalid API key",
            )
            .await
        }
        "volcengine-ark" => {
            openai_chat_status_test(
                state,
                connection,
                effective_proxy,
                "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions",
                &default_catalog_model("volcengine-ark"),
                "Invalid API key",
            )
            .await
        }
        "byteplus" => {
            openai_chat_status_test(
                state,
                connection,
                effective_proxy,
                "https://ark.ap-southeast.bytepluses.com/api/coding/v3/chat/completions",
                &default_catalog_model("byteplus"),
                "Invalid API key",
            )
            .await
        }
        "deepseek" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.deepseek.com/models",
                "Invalid API key",
            )
            .await
        }
        "groq" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.groq.com/openai/v1/models",
                "Invalid API key",
            )
            .await
        }
        "mistral" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.mistral.ai/v1/models",
                "Invalid API key",
            )
            .await
        }
        "xai" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.x.ai/v1/models",
                "Invalid API key",
            )
            .await
        }
        "nvidia" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://integrate.api.nvidia.com/v1/models",
                "Invalid API key",
            )
            .await
        }
        "perplexity" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.perplexity.ai/models",
                "Invalid API key",
            )
            .await
        }
        "together" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.together.xyz/v1/models",
                "Invalid API key",
            )
            .await
        }
        "fireworks" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.fireworks.ai/inference/v1/models",
                "Invalid API key",
            )
            .await
        }
        "cerebras" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.cerebras.ai/v1/models",
                "Invalid API key",
            )
            .await
        }
        "cohere" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.cohere.ai/v1/models",
                "Invalid API key",
            )
            .await
        }
        "nebius" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.studio.nebius.ai/v1/models",
                "Invalid API key",
            )
            .await
        }
        "siliconflow" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.siliconflow.cn/v1/models",
                "Invalid API key",
            )
            .await
        }
        "hyperbolic" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.hyperbolic.xyz/v1/models",
                "Invalid API key",
            )
            .await
        }
        "ollama" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://ollama.com/api/tags",
                "Invalid API key",
            )
            .await
        }
        "ollama-local" => test_ollama_local_connection(state, connection, effective_proxy).await,
        "deepgram" => {
            simple_get_token_test(
                state,
                connection,
                effective_proxy,
                "https://api.deepgram.com/v1/projects",
                "Token",
                "Invalid API key",
            )
            .await
        }
        "assemblyai" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.assemblyai.com/v1/account",
                "Invalid API key",
            )
            .await
        }
        "nanobanana" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://api.nanobananaapi.ai/v1/models",
                "Invalid API key",
            )
            .await
        }
        "chutes" => {
            simple_get_bearer_test(
                state,
                connection,
                effective_proxy,
                "https://llm.chutes.ai/v1/models",
                "Invalid API key",
            )
            .await
        }
        "grok-web" => test_grok_web_connection(state, connection, effective_proxy).await,
        "perplexity-web" => {
            test_perplexity_web_connection(state, connection, effective_proxy).await
        }
        _ => invalid("Provider test not supported"),
    };

    response
}

async fn test_cloudflare_ai_connection(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
) -> ConnectionTestResult {
    let Some(account_id) = provider_specific_string(connection, "accountId") else {
        return invalid("Missing Account ID");
    };

    let request = PreparedRequest {
        method: Method::POST,
        url: format!(
            "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1/chat/completions"
        ),
        headers: authorization_headers(connection),
        body: Some(PreparedBody::Json(json!({
            "model": default_catalog_model("cloudflare-ai"),
            "messages": [{ "role": "user", "content": "test" }],
            "max_tokens": 1
        }))),
    };

    match execute_request(state, &connection.provider, effective_proxy, request).await {
        Ok(response) => {
            let valid = !matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            );
            ConnectionTestResult {
                valid,
                error: if valid {
                    None
                } else {
                    Some("Invalid API token or Account ID".to_string())
                },
                refreshed: false,
                new_tokens: None,
            }
        }
        Err(error) => invalid(&error),
    }
}

async fn test_azure_connection(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
) -> ConnectionTestResult {
    let endpoint = provider_specific_string(connection, "azureEndpoint").unwrap_or_default();
    let deployment = provider_specific_string(connection, "deployment")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "gpt-4".to_string());
    let api_version = provider_specific_string(connection, "apiVersion")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "2024-10-01-preview".to_string());
    let url = format!(
        "{}/openai/deployments/{deployment}/chat/completions?api-version={api_version}",
        endpoint.trim_end_matches('/')
    );

    let mut headers = vec![
        (
            "api-key".to_string(),
            connection.api_key.clone().unwrap_or_default(),
        ),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    if let Some(organization) = provider_specific_string(connection, "organization") {
        headers.push(("OpenAI-Organization".to_string(), organization));
    }

    let request = PreparedRequest {
        method: Method::POST,
        url,
        headers,
        body: Some(PreparedBody::Json(json!({
            "messages": [{ "role": "user", "content": "test" }],
            "max_completion_tokens": 1
        }))),
    };

    match execute_request(state, &connection.provider, effective_proxy, request).await {
        Ok(response) => {
            let valid = !matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            );
            ConnectionTestResult {
                valid,
                error: if valid {
                    None
                } else {
                    Some("Invalid API key or Azure configuration".to_string())
                },
                refreshed: false,
                new_tokens: None,
            }
        }
        Err(error) => invalid(&error),
    }
}

async fn test_gemini_api_key_connection(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
) -> ConnectionTestResult {
    let api_key = connection.api_key.clone().unwrap_or_default();
    let request = PreparedRequest {
        method: Method::GET,
        url: format!("https://generativelanguage.googleapis.com/v1/models?key={api_key}"),
        headers: vec![],
        body: None,
    };
    match execute_request(state, &connection.provider, effective_proxy, request).await {
        Ok(response) => ConnectionTestResult {
            valid: response.status().is_success(),
            error: if response.status().is_success() {
                None
            } else {
                Some("Invalid API key".to_string())
            },
            refreshed: false,
            new_tokens: None,
        },
        Err(error) => invalid(&error),
    }
}

async fn test_ollama_local_connection(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
) -> ConnectionTestResult {
    let host = provider_specific_string(connection, "baseUrl")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OLLAMA_LOCAL_DEFAULT_HOST.to_string());

    let request = PreparedRequest {
        method: Method::GET,
        url: format!("{}/api/tags", host.trim_end_matches('/')),
        headers: vec![],
        body: None,
    };

    match execute_request(state, &connection.provider, effective_proxy, request).await {
        Ok(response) => ConnectionTestResult {
            valid: response.status().is_success(),
            error: if response.status().is_success() {
                None
            } else {
                Some(format!(
                    "Ollama not reachable at {}",
                    host.trim_end_matches('/')
                ))
            },
            refreshed: false,
            new_tokens: None,
        },
        Err(_) => invalid(&format!(
            "Ollama not reachable at {}",
            host.trim_end_matches('/')
        )),
    }
}

async fn test_grok_web_connection(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
) -> ConnectionTestResult {
    let mut token = connection.api_key.clone().unwrap_or_default();
    if let Some(value) = token.strip_prefix("sso=") {
        token = value.to_string();
    }

    let statsig_id =
        STANDARD.encode("e:TypeError: Cannot read properties of null (reading 'children')");
    let request = PreparedRequest {
        method: Method::POST,
        url: "https://grok.com/rest/app-chat/conversations/new".to_string(),
        headers: vec![
            ("Accept".to_string(), "*/*".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Cookie".to_string(), format!("sso={token}")),
            ("Origin".to_string(), "https://grok.com".to_string()),
            ("Referer".to_string(), "https://grok.com/".to_string()),
            (
                "User-Agent".to_string(),
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36".to_string(),
            ),
            ("x-statsig-id".to_string(), statsig_id),
            ("x-xai-request-id".to_string(), Uuid::new_v4().to_string()),
            (
                "traceparent".to_string(),
                format!("00-{}-{}-00", random_hex(16), random_hex(8)),
            ),
        ],
        body: Some(PreparedBody::Json(json!({
            "temporary": true,
            "modelName": "grok-4",
            "message": "ping",
            "fileAttachments": [],
            "imageAttachments": [],
            "disableSearch": false,
            "enableImageGeneration": false,
            "sendFinalMetadata": true
        }))),
    };

    match execute_request(state, &connection.provider, effective_proxy, request).await {
        Ok(response) => {
            let valid = !matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            );
            ConnectionTestResult {
                valid,
                error: if valid {
                    None
                } else {
                    Some("Invalid SSO cookie".to_string())
                },
                refreshed: false,
                new_tokens: None,
            }
        }
        Err(error) => invalid(&error),
    }
}

async fn test_perplexity_web_connection(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
) -> ConnectionTestResult {
    let mut session_token = connection.api_key.clone().unwrap_or_default();
    if let Some(value) = session_token.strip_prefix("__Secure-next-auth.session-token=") {
        session_token = value.to_string();
    }

    let request = PreparedRequest {
        method: Method::GET,
        url: "https://www.perplexity.ai/api/auth/session".to_string(),
        headers: vec![
            (
                "User-Agent".to_string(),
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36".to_string(),
            ),
            (
                "Cookie".to_string(),
                format!("__Secure-next-auth.session-token={session_token}"),
            ),
        ],
        body: None,
    };

    match execute_request(state, &connection.provider, effective_proxy, request).await {
        Ok(response) => {
            if !response.status().is_success() {
                return invalid("Invalid session cookie");
            }

            match response.json::<Value>().await {
                Ok(payload) if payload.get("user").is_some() => ConnectionTestResult {
                    valid: true,
                    error: None,
                    refreshed: false,
                    new_tokens: None,
                },
                Ok(_) => invalid("Session expired — re-paste cookie"),
                Err(error) => invalid(&error.to_string()),
            }
        }
        Err(error) => invalid(&error),
    }
}

async fn simple_get_bearer_test(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
    url: &str,
    error_message: &str,
) -> ConnectionTestResult {
    simple_get_token_test(
        state,
        connection,
        effective_proxy,
        url,
        "Bearer",
        error_message,
    )
    .await
}

async fn simple_get_token_test(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
    url: &str,
    auth_prefix: &str,
    error_message: &str,
) -> ConnectionTestResult {
    let request = PreparedRequest {
        method: Method::GET,
        url: url.to_string(),
        headers: vec![(
            "Authorization".to_string(),
            format!(
                "{auth_prefix} {}",
                connection.api_key.clone().unwrap_or_default()
            ),
        )],
        body: None,
    };

    match execute_request(state, &connection.provider, effective_proxy, request).await {
        Ok(response) => ConnectionTestResult {
            valid: response.status().is_success(),
            error: if response.status().is_success() {
                None
            } else {
                Some(error_message.to_string())
            },
            refreshed: false,
            new_tokens: None,
        },
        Err(error) => invalid(&error),
    }
}

async fn anthropic_first_party_test(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
    url: &str,
    model: &str,
) -> ConnectionTestResult {
    let api_key = connection.api_key.clone().unwrap_or_default();
    let request = PreparedRequest {
        method: Method::POST,
        url: url.to_string(),
        headers: vec![
            ("x-api-key".to_string(), api_key),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ],
        body: Some(PreparedBody::Json(json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "test" }]
        }))),
    };

    match execute_request(state, &connection.provider, effective_proxy, request).await {
        Ok(response) => {
            let valid = response.status() != StatusCode::UNAUTHORIZED;
            ConnectionTestResult {
                valid,
                error: if valid {
                    None
                } else {
                    Some("Invalid API key".to_string())
                },
                refreshed: false,
                new_tokens: None,
            }
        }
        Err(error) => invalid(&error),
    }
}

async fn anthropic_like_status_test(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
    url: &str,
    model: &str,
    error_message: &str,
) -> ConnectionTestResult {
    let api_key = connection.api_key.clone().unwrap_or_default();
    let request = PreparedRequest {
        method: Method::POST,
        url: url.to_string(),
        headers: vec![
            ("x-api-key".to_string(), api_key),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ],
        body: Some(PreparedBody::Json(json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "test" }]
        }))),
    };
    status_test_excluding(
        state,
        connection,
        effective_proxy,
        request,
        &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN],
        error_message,
    )
    .await
}

async fn openai_chat_status_test(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
    url: &str,
    model: &str,
    error_message: &str,
) -> ConnectionTestResult {
    let request = PreparedRequest {
        method: Method::POST,
        url: url.to_string(),
        headers: authorization_headers(connection),
        body: Some(PreparedBody::Json(json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "test" }]
        }))),
    };
    status_test_excluding(
        state,
        connection,
        effective_proxy,
        request,
        &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN],
        error_message,
    )
    .await
}

async fn status_test_excluding(
    state: &AppState,
    connection: &ProviderConnection,
    effective_proxy: &EffectiveProxy,
    request: PreparedRequest,
    invalid_statuses: &[StatusCode],
    error_message: &str,
) -> ConnectionTestResult {
    match execute_request(state, &connection.provider, effective_proxy, request).await {
        Ok(response) => {
            let valid = !invalid_statuses.contains(&response.status());
            ConnectionTestResult {
                valid,
                error: if valid {
                    None
                } else {
                    Some(error_message.to_string())
                },
                refreshed: false,
                new_tokens: None,
            }
        }
        Err(error) => invalid(&error),
    }
}

async fn refresh_oauth_token(
    connection: &ProviderConnection,
    refresh_token: &str,
    effective_proxy: &EffectiveProxy,
) -> Result<RefreshResult, String> {
    match connection.provider.as_str() {
        "gemini-cli" => {
            refresh_google_token(
                refresh_token,
                GEMINI_CLIENT_ID,
                GEMINI_CLIENT_SECRET,
                effective_proxy,
            )
            .await
        }
        "antigravity" => {
            refresh_google_token(
                refresh_token,
                ANTIGRAVITY_CLIENT_ID,
                ANTIGRAVITY_CLIENT_SECRET,
                effective_proxy,
            )
            .await
        }
        "codex" => {
            refresh_form_token(
                CODEX_TOKEN_URL,
                vec![
                    ("grant_type".to_string(), "refresh_token".to_string()),
                    ("client_id".to_string(), CODEX_CLIENT_ID.to_string()),
                    ("refresh_token".to_string(), refresh_token.to_string()),
                ],
                effective_proxy,
            )
            .await
        }
        "claude" => {
            refresh_json_token(
                CLAUDE_TOKEN_URL,
                json!({
                    "grant_type": "refresh_token",
                    "refresh_token": refresh_token,
                    "client_id": CLAUDE_CLIENT_ID,
                }),
                effective_proxy,
            )
            .await
        }
        "kiro" => refresh_kiro_token(connection, refresh_token, effective_proxy).await,
        "qwen" => {
            refresh_form_token(
                QWEN_TOKEN_URL,
                vec![
                    ("grant_type".to_string(), "refresh_token".to_string()),
                    ("refresh_token".to_string(), refresh_token.to_string()),
                    ("client_id".to_string(), QWEN_CLIENT_ID.to_string()),
                ],
                effective_proxy,
            )
            .await
        }
        "cline" => refresh_cline_token(refresh_token, effective_proxy).await,
        _ => Err("Provider refresh not supported".to_string()),
    }
}

async fn refresh_google_token(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
    effective_proxy: &EffectiveProxy,
) -> Result<RefreshResult, String> {
    refresh_form_token(
        GOOGLE_TOKEN_URL,
        vec![
            ("client_id".to_string(), client_id.to_string()),
            ("client_secret".to_string(), client_secret.to_string()),
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), refresh_token.to_string()),
        ],
        effective_proxy,
    )
    .await
}

async fn refresh_kiro_token(
    connection: &ProviderConnection,
    refresh_token: &str,
    effective_proxy: &EffectiveProxy,
) -> Result<RefreshResult, String> {
    if let (Some(client_id), Some(client_secret)) = (
        connection_value(connection, "clientId"),
        connection_value(connection, "clientSecret"),
    ) {
        let region =
            connection_value(connection, "region").unwrap_or_else(|| "us-east-1".to_string());
        refresh_json_token(
            &format!("https://oidc.{region}.amazonaws.com/token"),
            json!({
                "clientId": client_id,
                "clientSecret": client_secret,
                "refreshToken": refresh_token,
                "grantType": "refresh_token",
            }),
            effective_proxy,
        )
        .await
    } else {
        refresh_json_token(
            KIRO_SOCIAL_REFRESH_URL,
            json!({ "refreshToken": refresh_token }),
            effective_proxy,
        )
        .await
    }
}

async fn refresh_cline_token(
    refresh_token: &str,
    effective_proxy: &EffectiveProxy,
) -> Result<RefreshResult, String> {
    let request = PreparedRequest {
        method: Method::POST,
        url: CLINE_REFRESH_URL.to_string(),
        headers: vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ],
        body: Some(PreparedBody::Json(json!({
            "refreshToken": refresh_token,
            "grantType": "refresh_token",
            "clientType": "extension"
        }))),
    };

    let client = reqwest::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?;
    let response = execute_prepared_request(&client, effective_proxy, request)
        .await
        .map_err(|error| error.to_string())?;
    let payload: Value = response.json().await.map_err(|error| error.to_string())?;
    let data = payload.get("data").unwrap_or(&payload);
    let access_token = data
        .get("accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Cline refresh response did not include access token".to_string())?;

    let expires_in = data
        .get("expiresAt")
        .and_then(Value::as_str)
        .and_then(|expires_at| chrono::DateTime::parse_from_rfc3339(expires_at).ok())
        .map(|expires_at| (expires_at.timestamp() - Utc::now().timestamp()).max(1));

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: data
            .get("refreshToken")
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in,
    })
}

async fn refresh_form_token(
    url: &str,
    fields: Vec<(String, String)>,
    effective_proxy: &EffectiveProxy,
) -> Result<RefreshResult, String> {
    let request = PreparedRequest {
        method: Method::POST,
        url: url.to_string(),
        headers: vec![
            (
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            ),
            ("Accept".to_string(), "application/json".to_string()),
        ],
        body: Some(PreparedBody::Form(fields)),
    };
    decode_refresh_response(execute_simple_request(effective_proxy, request).await?).await
}

async fn refresh_json_token(
    url: &str,
    body: Value,
    effective_proxy: &EffectiveProxy,
) -> Result<RefreshResult, String> {
    let request = PreparedRequest {
        method: Method::POST,
        url: url.to_string(),
        headers: vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ],
        body: Some(PreparedBody::Json(body)),
    };
    decode_refresh_response(execute_simple_request(effective_proxy, request).await?).await
}

async fn decode_refresh_response(response: reqwest::Response) -> Result<RefreshResult, String> {
    let payload: Value = response.json().await.map_err(|error| error.to_string())?;
    let access_token = payload
        .get("access_token")
        .or_else(|| payload.get("accessToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Refresh response did not include access token".to_string())?;

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: payload
            .get("refresh_token")
            .or_else(|| payload.get("refreshToken"))
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in: payload
            .get("expires_in")
            .or_else(|| payload.get("expiresIn"))
            .and_then(Value::as_i64),
    })
}

async fn probe_cline_access_token(
    state: &AppState,
    effective_proxy: &EffectiveProxy,
    access_token: &str,
) -> ConnectionTestResult {
    let request = PreparedRequest {
        method: Method::GET,
        url: CLINE_USERS_ME_URL.to_string(),
        headers: cline_headers(
            access_token,
            vec![("Accept".to_string(), "application/json".to_string())],
        ),
        body: None,
    };

    match execute_request(state, "cline", effective_proxy, request).await {
        Ok(response) if response.status().is_success() => ConnectionTestResult {
            valid: true,
            error: None,
            refreshed: false,
            new_tokens: None,
        },
        Ok(response) if response.status() == StatusCode::UNAUTHORIZED => {
            invalid("Token invalid or revoked")
        }
        Ok(response) if response.status() == StatusCode::FORBIDDEN => invalid("Access denied"),
        Ok(response) => invalid(&format!("API returned {}", response.status().as_u16())),
        Err(error) => invalid(&error),
    }
}

fn oauth_probe_request(provider: &str, access_token: &str) -> Option<PreparedRequest> {
    match provider {
        "codex" => Some(PreparedRequest {
            method: Method::POST,
            url: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            headers: vec![
                (
                    "Authorization".to_string(),
                    format!("Bearer {access_token}"),
                ),
                ("Content-Type".to_string(), "application/json".to_string()),
                ("originator".to_string(), "codex-cli".to_string()),
                (
                    "User-Agent".to_string(),
                    "codex-cli/1.0.18 (macOS; arm64)".to_string(),
                ),
            ],
            body: Some(PreparedBody::Json(json!({
                "model": "gpt-5.3-codex",
                "input": [],
                "stream": false,
                "store": false
            }))),
        }),
        "gemini-cli" | "antigravity" => Some(PreparedRequest {
            method: Method::GET,
            url: "https://www.googleapis.com/oauth2/v1/userinfo?alt=json".to_string(),
            headers: vec![(
                "Authorization".to_string(),
                format!("Bearer {access_token}"),
            )],
            body: None,
        }),
        "github" => Some(PreparedRequest {
            method: Method::GET,
            url: "https://api.github.com/user".to_string(),
            headers: vec![
                (
                    "Authorization".to_string(),
                    format!("Bearer {access_token}"),
                ),
                ("User-Agent".to_string(), "9Router".to_string()),
                (
                    "Accept".to_string(),
                    "application/vnd.github+json".to_string(),
                ),
            ],
            body: None,
        }),
        "iflow" => Some(PreparedRequest {
            method: Method::GET,
            url: format!(
                "https://iflow.cn/api/oauth/getUserInfo?accessToken={}",
                url::form_urlencoded::byte_serialize(access_token.as_bytes()).collect::<String>()
            ),
            headers: vec![],
            body: None,
        }),
        "kilocode" => Some(PreparedRequest {
            method: Method::GET,
            url: "https://api.kilo.ai/api/profile".to_string(),
            headers: vec![(
                "Authorization".to_string(),
                format!("Bearer {access_token}"),
            )],
            body: None,
        }),
        "gitlab" => Some(PreparedRequest {
            method: Method::GET,
            url: "https://gitlab.com/api/v4/user".to_string(),
            headers: vec![(
                "Authorization".to_string(),
                format!("Bearer {access_token}"),
            )],
            body: None,
        }),
        _ => None,
    }
}

async fn execute_request(
    state: &AppState,
    provider_key: &str,
    effective_proxy: &EffectiveProxy,
    request: PreparedRequest,
) -> Result<reqwest::Response, String> {
    if effective_proxy.vercel_relay_url.is_some() {
        let client = Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|error| error.to_string())?;
        return execute_prepared_request(&client, effective_proxy, request).await;
    }

    let proxy_target = if effective_proxy.connection_proxy_enabled
        && !effective_proxy.connection_proxy_url.is_empty()
    {
        Some(crate::core::proxy::ProxyTarget {
            url: effective_proxy.connection_proxy_url.clone(),
            no_proxy: effective_proxy.connection_no_proxy.clone(),
            strict_proxy: false,
            pool_id: None,
            label: None,
            rtt_ms: None,
        })
    } else {
        None
    };

    let client = state
        .client_pool
        .get(provider_key, proxy_target.as_ref())
        .map_err(|error| error.to_string())?;

    execute_prepared_request(client.as_ref(), effective_proxy, request).await
}

async fn execute_simple_request(
    effective_proxy: &EffectiveProxy,
    request: PreparedRequest,
) -> Result<reqwest::Response, String> {
    let client = build_proxy_capable_client(effective_proxy)?;
    execute_prepared_request(&client, effective_proxy, request).await
}

fn build_proxy_capable_client(effective_proxy: &EffectiveProxy) -> Result<Client, String> {
    let mut builder = Client::builder().timeout(DEFAULT_TIMEOUT);
    if effective_proxy.connection_proxy_enabled && !effective_proxy.connection_proxy_url.is_empty()
    {
        let proxy = Proxy::all(&effective_proxy.connection_proxy_url)
            .map_err(|error| error.to_string())?
            .no_proxy(reqwest::NoProxy::from_string(
                &effective_proxy.connection_no_proxy,
            ));
        builder = builder.proxy(proxy);
    }
    builder.build().map_err(|error| error.to_string())
}

async fn execute_prepared_request(
    client: &Client,
    effective_proxy: &EffectiveProxy,
    request: PreparedRequest,
) -> Result<reqwest::Response, String> {
    let mut builder = if let Some(relay_url) = effective_proxy.vercel_relay_url.as_deref() {
        build_relay_request(client, relay_url, request)?
    } else {
        build_direct_request(client, request)
    };

    builder = builder.timeout(DEFAULT_TIMEOUT);
    builder.send().await.map_err(|error| error.to_string())
}

fn build_direct_request(client: &Client, request: PreparedRequest) -> RequestBuilder {
    let mut builder = client.request(request.method, request.url);
    for (name, value) in request.headers {
        builder = builder.header(name, value);
    }
    match request.body {
        Some(PreparedBody::Json(body)) => builder.json(&body),
        Some(PreparedBody::Form(body)) => builder.form(&body),
        None => builder,
    }
}

fn build_relay_request(
    client: &Client,
    relay_url: &str,
    request: PreparedRequest,
) -> Result<RequestBuilder, String> {
    let parsed = Url::parse(&request.url).map_err(|error| error.to_string())?;
    let target = parsed.origin().ascii_serialization();
    let relay_path = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    };

    let mut builder = client
        .request(request.method, relay_url)
        .header("x-relay-target", target)
        .header("x-relay-path", relay_path);
    for (name, value) in request.headers {
        builder = builder.header(name, value);
    }
    builder = match request.body {
        Some(PreparedBody::Json(body)) => builder.json(&body),
        Some(PreparedBody::Form(body)) => builder.form(&body),
        None => builder,
    };
    Ok(builder)
}

async fn test_proxy_url(proxy_url: &str) -> Result<(), String> {
    let proxy = Proxy::all(proxy_url).map_err(|error| format!("Invalid proxy URL: {error}"))?;
    let client = Client::builder()
        .proxy(proxy)
        .timeout(PROXY_TEST_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?;

    let response = client
        .head("https://google.com/")
        .header("User-Agent", "9Router")
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "Proxy test timed out".to_string()
            } else {
                error.to_string()
            }
        })?;

    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "Proxy test failed with status {}",
            response.status().as_u16()
        ))
    }
}

fn resolve_effective_proxy(state: &AppState, connection: &ProviderConnection) -> EffectiveProxy {
    let snapshot = state.db.snapshot();
    let proxy_pool_id = provider_specific_string(connection, "proxyPoolId")
        .filter(|value| value != "__none__")
        .unwrap_or_default();

    if !proxy_pool_id.is_empty() {
        if let Some(proxy_pool) = snapshot
            .proxy_pools
            .iter()
            .find(|proxy_pool| proxy_pool.id == proxy_pool_id)
        {
            if proxy_pool.is_active.unwrap_or(false) {
                let proxy_url = proxy_pool.proxy_url.trim();
                if !proxy_url.is_empty() {
                    if proxy_pool.r#type == "vercel" {
                        return EffectiveProxy {
                            connection_proxy_enabled: false,
                            connection_proxy_url: String::new(),
                            connection_no_proxy: proxy_pool.no_proxy.trim().to_string(),
                            vercel_relay_url: Some(proxy_url.to_string()),
                        };
                    }

                    return EffectiveProxy {
                        connection_proxy_enabled: true,
                        connection_proxy_url: proxy_url.to_string(),
                        connection_no_proxy: proxy_pool.no_proxy.trim().to_string(),
                        vercel_relay_url: None,
                    };
                }
            }
        }
    }

    let connection_proxy_enabled = connection
        .provider_specific_data
        .get("connectionProxyEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let connection_proxy_url =
        provider_specific_string(connection, "connectionProxyUrl").unwrap_or_default();
    let connection_no_proxy =
        provider_specific_string(connection, "connectionNoProxy").unwrap_or_default();

    EffectiveProxy {
        connection_proxy_enabled,
        connection_proxy_url,
        connection_no_proxy,
        vercel_relay_url: None,
    }
}

fn authorization_headers(connection: &ProviderConnection) -> Vec<(String, String)> {
    vec![
        (
            "Authorization".to_string(),
            format!("Bearer {}", connection.api_key.clone().unwrap_or_default()),
        ),
        ("content-type".to_string(), "application/json".to_string()),
    ]
}

fn provider_specific_string(connection: &ProviderConnection, key: &str) -> Option<String> {
    connection
        .provider_specific_data
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn connection_value(connection: &ProviderConnection, key: &str) -> Option<String> {
    provider_specific_string(connection, key).or_else(|| {
        connection
            .extra
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn is_openai_compatible_provider(provider: &str) -> bool {
    provider.starts_with("openai-compatible-")
}

fn is_anthropic_compatible_provider(provider: &str) -> bool {
    provider.starts_with("anthropic-compatible-")
}

fn normalize_anthropic_models_url(base_url: &str) -> String {
    let mut normalized = base_url.trim().trim_end_matches('/').to_string();
    if normalized.ends_with("/messages") {
        normalized.truncate(normalized.len() - "/messages".len());
    }
    format!("{normalized}/models")
}

fn default_catalog_model(provider: &str) -> String {
    let catalog = provider_catalog();
    let alias = catalog
        .static_alias_for_provider(provider)
        .unwrap_or(provider);
    catalog
        .models_for_alias(alias)
        .and_then(|models| models.iter().find(|model| model.kind == "llm"))
        .or_else(|| {
            catalog
                .models_for_alias(alias)
                .and_then(|models| models.first())
        })
        .map(|model| model.id.clone())
        .unwrap_or_else(|| "gpt-4o-mini".to_string())
}

fn is_token_expired(connection: &ProviderConnection) -> bool {
    let Some(expires_at) = connection.expires_at.as_deref() else {
        return false;
    };

    chrono::DateTime::parse_from_rfc3339(expires_at)
        .map(|expires_at| {
            expires_at.timestamp() <= Utc::now().timestamp() + TOKEN_EXPIRY_BUFFER_SECS
        })
        .unwrap_or(false)
}

fn is_refreshable_provider(provider: &str) -> bool {
    matches!(
        provider,
        "claude" | "codex" | "gemini-cli" | "antigravity" | "qwen" | "kiro" | "cline"
    )
}

fn is_check_expiry_provider(provider: &str) -> bool {
    matches!(provider, "claude" | "qwen" | "kiro" | "kimi-coding")
}

fn cline_headers(token: &str, extra_headers: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut headers = vec![
        ("HTTP-Referer".to_string(), "https://cline.bot".to_string()),
        ("X-Title".to_string(), "Cline".to_string()),
        (
            "User-Agent".to_string(),
            format!("9Router/{}", env!("CARGO_PKG_VERSION")),
        ),
        ("X-PLATFORM".to_string(), std::env::consts::OS.to_string()),
        ("X-PLATFORM-VERSION".to_string(), "rust".to_string()),
        ("X-CLIENT-TYPE".to_string(), "9router".to_string()),
        (
            "X-CLIENT-VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        ),
        (
            "X-CORE-VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        ),
        ("X-IS-MULTIROOT".to_string(), "false".to_string()),
    ];
    if !token.trim().is_empty() {
        headers.push((
            "Authorization".to_string(),
            format!("Bearer {}", cline_access_token(token)),
        ));
    }
    headers.extend(extra_headers);
    headers
}

fn cline_access_token(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.starts_with("workos:") {
        trimmed.to_string()
    } else {
        format!("workos:{trimmed}")
    }
}

fn random_hex(len_bytes: usize) -> String {
    let mut bytes = vec![0u8; len_bytes];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn compatible_result(
    response: Result<reqwest::Response, String>,
    error_message: &str,
) -> ConnectionTestResult {
    match response {
        Ok(response) => ConnectionTestResult {
            valid: response.status().is_success(),
            error: if response.status().is_success() {
                None
            } else {
                Some(error_message.to_string())
            },
            refreshed: false,
            new_tokens: None,
        },
        Err(error) => invalid(&error),
    }
}

fn invalid(error: &str) -> ConnectionTestResult {
    ConnectionTestResult {
        valid: false,
        error: Some(error.to_string()),
        refreshed: false,
        new_tokens: None,
    }
}
