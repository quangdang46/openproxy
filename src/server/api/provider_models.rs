use std::collections::BTreeMap;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration as ChronoDuration, Utc};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;
use serde_json::{json, Value};

use crate::server::state::AppState;
use crate::types::ProviderConnection;

const OPENAI_COMPATIBLE_PREFIX: &str = "openai-compatible-";
const ANTHROPIC_COMPATIBLE_PREFIX: &str = "anthropic-compatible-";
const OLLAMA_LOCAL_DEFAULT_HOST: &str = "http://localhost:11434";

const GEMINI_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GEMINI_CLIENT_SECRET: &str = "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";
const GEMINI_CLI_MODELS_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

const KIRO_AUTH_SERVICE: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";
const KIRO_MODELS_URL: &str = "https://codewhisperer.us-east-1.amazonaws.com";
const KIRO_MODELS_TARGET: &str = "AmazonCodeWhispererService.ListAvailableModels";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsResponse {
    pub provider: String,
    pub connection_id: String,
    pub models: Vec<ProviderModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

pub(super) async fn list_provider_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(connection) = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
    else {
        return json_error(StatusCode::NOT_FOUND, "Connection not found");
    };

    match fetch_provider_models_response(&state, &connection).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => json_error(error.status, &error.message),
    }
}

pub(super) async fn fetch_compatible_model_ids(connection: &ProviderConnection) -> Vec<String> {
    let models = if is_openai_compatible_provider(&connection.provider) {
        fetch_openai_compatible_models(connection)
            .await
            .ok()
            .map(|payload| payload.models)
            .unwrap_or_default()
    } else if is_anthropic_compatible_provider(&connection.provider) {
        fetch_anthropic_compatible_models(connection)
            .await
            .ok()
            .map(|payload| payload.models)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    dedupe_model_ids(
        models
            .into_iter()
            .map(|model| model.id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect(),
    )
}

pub(super) async fn fetch_models_for_connection(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<Vec<ProviderModel>, (StatusCode, String)> {
    fetch_provider_models_response(state, connection)
        .await
        .map(|payload| payload.models)
        .map_err(|error| (error.status, error.message))
}

#[derive(Debug)]
struct RouteError {
    status: StatusCode,
    message: String,
}

impl RouteError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

#[derive(Debug)]
enum FetchJsonError {
    Http(StatusCode, String),
    Network(String),
    Decode(String),
}

#[derive(Debug)]
struct RefreshResult {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

async fn fetch_provider_models_response(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    if is_openai_compatible_provider(&connection.provider) {
        return fetch_openai_compatible_models(connection).await;
    }

    if is_anthropic_compatible_provider(&connection.provider) {
        return fetch_anthropic_compatible_models(connection).await;
    }

    match connection.provider.as_str() {
        "kiro" => fetch_kiro_models_with_fallback(state, connection).await,
        "gemini-cli" => fetch_gemini_cli_models_with_fallback(state, connection).await,
        "ollama-local" => fetch_ollama_local_models(connection).await,
        "claude" | "anthropic" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_anthropic_models(
                connection,
                "https://api.anthropic.com/v1/models",
                &token,
                Some("2023-06-01"),
            )
            .await
        }
        "gemini" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_gemini_api_models(connection, &token).await
        }
        "qwen" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_openai_style_models_with_bearer(
                connection,
                &resolve_qwen_models_url(connection),
                &token,
            )
            .await
        }
        "codex" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_codex_models(connection, &token).await
        }
        "antigravity" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_antigravity_models(connection, &token).await
        }
        "github" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_github_models(connection, &token).await
        }
        "openai" => fetch_first_party_openai_style_models(
            connection,
            "https://api.openai.com/v1/models",
        )
        .await,
        "openrouter" => fetch_first_party_openai_style_models(
            connection,
            "https://openrouter.ai/api/v1/models",
        )
        .await,
        "alicode" => fetch_first_party_openai_style_models(
            connection,
            "https://coding.dashscope.aliyuncs.com/v1/models",
        )
        .await,
        "alicode-intl" => fetch_first_party_openai_style_models(
            connection,
            "https://coding-intl.dashscope.aliyuncs.com/v1/models",
        )
        .await,
        "volcengine-ark" => fetch_first_party_openai_style_models(
            connection,
            "https://ark.cn-beijing.volces.com/api/coding/v3/models",
        )
        .await,
        "byteplus" => fetch_first_party_openai_style_models(
            connection,
            "https://ark.ap-southeast.bytepluses.com/api/coding/v3/models",
        )
        .await,
        "deepseek" => fetch_first_party_openai_style_models(
            connection,
            "https://api.deepseek.com/models",
        )
        .await,
        "groq" => fetch_first_party_openai_style_models(
            connection,
            "https://api.groq.com/openai/v1/models",
        )
        .await,
        "xai" => fetch_first_party_openai_style_models(
            connection,
            "https://api.x.ai/v1/models",
        )
        .await,
        "mistral" => fetch_first_party_openai_style_models(
            connection,
            "https://api.mistral.ai/v1/models",
        )
        .await,
        "perplexity" => fetch_first_party_openai_style_models(
            connection,
            "https://api.perplexity.ai/models",
        )
        .await,
        "together" => fetch_first_party_openai_style_models(
            connection,
            "https://api.together.xyz/v1/models",
        )
        .await,
        "fireworks" => fetch_first_party_openai_style_models(
            connection,
            "https://api.fireworks.ai/inference/v1/models",
        )
        .await,
        "cerebras" => fetch_first_party_openai_style_models(
            connection,
            "https://api.cerebras.ai/v1/models",
        )
        .await,
        "cohere" => fetch_first_party_openai_style_models(
            connection,
            "https://api.cohere.ai/v1/models",
        )
        .await,
        "nebius" => fetch_first_party_openai_style_models(
            connection,
            "https://api.studio.nebius.ai/v1/models",
        )
        .await,
        "siliconflow" => fetch_first_party_openai_style_models(
            connection,
            "https://api.siliconflow.cn/v1/models",
        )
        .await,
        "hyperbolic" => fetch_first_party_openai_style_models(
            connection,
            "https://api.hyperbolic.xyz/v1/models",
        )
        .await,
        "ollama" => fetch_first_party_openai_style_models(
            connection,
            "https://ollama.com/api/tags",
        )
        .await,
        "nanobanana" => fetch_first_party_openai_style_models(
            connection,
            "https://api.nanobananaapi.ai/v1/models",
        )
        .await,
        "chutes" => fetch_first_party_openai_style_models(
            connection,
            "https://llm.chutes.ai/v1/models",
        )
        .await,
        "nvidia" => fetch_first_party_openai_style_models(
            connection,
            "https://integrate.api.nvidia.com/v1/models",
        )
        .await,
        "assemblyai" => fetch_first_party_openai_style_models(
            connection,
            "https://api.assemblyai.com/v1/models",
        )
        .await,
        other => Err(RouteError::bad_request(format!(
            "Provider {other} does not support models listing"
        ))),
    }
}

async fn fetch_first_party_openai_style_models(
    connection: &ProviderConnection,
    url: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let token =
        primary_token(connection).ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
    fetch_openai_style_models_with_bearer(connection, url, &token).await
}

async fn fetch_openai_compatible_models(
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let base_url = provider_specific_string(connection, "baseUrl").ok_or_else(|| {
        RouteError::bad_request("No base URL configured for OpenAI compatible provider")
    })?;
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let token = connection.api_key.clone().unwrap_or_default();
    fetch_openai_style_models_with_bearer(connection, &url, &token).await
}

async fn fetch_anthropic_compatible_models(
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let base_url = provider_specific_string(connection, "baseUrl").ok_or_else(|| {
        RouteError::bad_request("No base URL configured for Anthropic compatible provider")
    })?;
    let normalized = normalize_anthropic_models_base_url(&base_url);
    let token = connection.api_key.clone().unwrap_or_default();
    fetch_anthropic_models(connection, &normalized, &token, Some("2023-06-01")).await
}

async fn fetch_openai_style_models_with_bearer(
    connection: &ProviderConnection,
    url: &str,
    token: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let request = client.get(url).header(CONTENT_TYPE, "application/json").header(
        AUTHORIZATION,
        format!("Bearer {token}"),
    );
    let payload = fetch_json(request).await.map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_openai_style_models(&payload),
        None,
    ))
}

async fn fetch_anthropic_models(
    connection: &ProviderConnection,
    url: &str,
    token: &str,
    version: Option<&str>,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let mut request = client
        .get(url)
        .header(CONTENT_TYPE, "application/json")
        .header("x-api-key", token)
        .header(AUTHORIZATION, format!("Bearer {token}"));
    if let Some(version) = version {
        request = request.header("anthropic-version", version);
    }
    let payload = fetch_json(request).await.map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_array_models(payload.get("data").or_else(|| payload.get("models"))),
        None,
    ))
}

async fn fetch_gemini_api_models(
    connection: &ProviderConnection,
    token: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let request = client
        .get("https://generativelanguage.googleapis.com/v1beta/models")
        .query(&[("key", token)])
        .header(CONTENT_TYPE, "application/json");
    let payload = fetch_json(request).await.map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_array_models(payload.get("models")),
        None,
    ))
}

async fn fetch_codex_models(
    connection: &ProviderConnection,
    token: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let request = client
        .get("https://chatgpt.com/backend-api/codex/models?client_version=1.0.0")
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .header(AUTHORIZATION, format!("Bearer {token}"));
    let payload = fetch_json(request).await.map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        append_codex_review_models(parse_openai_style_models(&payload)),
        None,
    ))
}

async fn fetch_antigravity_models(
    connection: &ProviderConnection,
    token: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let request = client
        .post("https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:models")
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .json(&json!({}));
    let payload = fetch_json(request).await.map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_array_models(payload.get("models")),
        None,
    ))
}

async fn fetch_github_models(
    connection: &ProviderConnection,
    token: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let request = client
        .get("https://api.githubcopilot.com/models")
        .header(CONTENT_TYPE, "application/json")
        .header("Copilot-Integration-Id", "vscode-chat")
        .header("editor-version", "vscode/1.107.1")
        .header("editor-plugin-version", "copilot-chat/0.26.7")
        .header("user-agent", "GitHubCopilotChat/0.26.7")
        .header(AUTHORIZATION, format!("Bearer {token}"));
    let payload = fetch_json(request).await.map_err(map_upstream_route_error)?;
    let models = payload
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let capabilities = item.get("capabilities")?.as_object()?;
            if capabilities.get("type").and_then(Value::as_str) != Some("chat") {
                return None;
            }
            if item
                .get("policy")
                .and_then(Value::as_object)
                .and_then(|policy| policy.get("state"))
                .and_then(Value::as_str)
                == Some("disabled")
            {
                return None;
            }

            let id = item.get("id").and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }

            let mut extra = BTreeMap::new();
            if let Some(version) = item.get("version") {
                extra.insert("version".to_string(), version.clone());
            }
            if let Some(capabilities) = item.get("capabilities") {
                extra.insert("capabilities".to_string(), capabilities.clone());
            }
            if let Some(is_default) = item.get("model_picker_enabled").and_then(Value::as_bool) {
                extra.insert("isDefault".to_string(), Value::Bool(is_default));
            }

            Some(ProviderModel {
                id: id.to_string(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_string(),
                extra,
            })
        })
        .collect();

    Ok(response_with_models(connection, models, None))
}

async fn fetch_kiro_models_with_fallback(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let profile_arn = provider_specific_string(connection, "profileArn");
    let access_token = connection.access_token.clone();
    let refresh_token = connection.refresh_token.clone();

    let mut warning = None;

    if let (Some(access_token), Some(profile_arn)) = (access_token, profile_arn) {
        match fetch_kiro_models(&access_token, &profile_arn).await {
            Ok(models) => return Ok(response_with_models(connection, models, None)),
            Err(error) if error.contains("AccessDeniedException") && refresh_token.is_some() => {
                if let Some(refresh_token) = refresh_token.as_deref() {
                    if let Ok(refreshed) =
                        refresh_kiro_token(refresh_token, &connection.provider_specific_data).await
                    {
                        persist_refreshed_credentials(state, connection, &refreshed).await;
                        if let Ok(models) = fetch_kiro_models(&refreshed.access_token, &profile_arn).await
                        {
                            return Ok(response_with_models(connection, models, None));
                        }
                    }
                }
                warning = Some(format!("Failed to fetch Kiro models: {error}"));
            }
            Err(error) => {
                warning = Some(format!("Failed to fetch Kiro models: {error}"));
            }
        }
    }

    Ok(response_with_models(connection, Vec::new(), warning))
}

async fn fetch_gemini_cli_models_with_fallback(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let Some(access_token) = connection.access_token.clone() else {
        return Err(RouteError::unauthorized("No valid token found"));
    };

    let project_id = connection
        .project_id
        .clone()
        .or_else(|| provider_specific_string(connection, "projectId"));

    let mut response = send_gemini_cli_models_request(&access_token, project_id.as_deref()).await;

    if matches!(response, Err(FetchJsonError::Http(status, _)) if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN)
    {
        if let Some(refresh_token) = connection.refresh_token.as_deref() {
            if let Ok(refreshed) =
                refresh_google_token(refresh_token, GEMINI_CLIENT_ID, GEMINI_CLIENT_SECRET).await
            {
                persist_refreshed_credentials(state, connection, &refreshed).await;
                response =
                    send_gemini_cli_models_request(&refreshed.access_token, project_id.as_deref())
                        .await;
            }
        }
    }

    match response {
        Ok(payload) => {
            let models = parse_gemini_cli_models(&payload);
            Ok(response_with_models(connection, models, None))
        }
        Err(FetchJsonError::Http(status, body)) => Ok(response_with_models(
            connection,
            Vec::new(),
            Some(format!(
                "Failed to fetch Gemini CLI models: {} {}",
                status.as_u16(),
                body
            )),
        )),
        Err(FetchJsonError::Network(message)) | Err(FetchJsonError::Decode(message)) => Ok(
            response_with_models(
                connection,
                Vec::new(),
                Some(format!("Failed to fetch Gemini CLI models: {message}")),
            ),
        ),
    }
}

async fn fetch_ollama_local_models(
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let url = format!("{}/api/tags", resolve_ollama_local_host(connection));
    let client = http_client()?;
    let request = client.get(url).header(CONTENT_TYPE, "application/json");
    let payload = fetch_json(request).await.map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_openai_style_models(&payload),
        None,
    ))
}

async fn fetch_kiro_models(
    access_token: &str,
    profile_arn: &str,
) -> Result<Vec<ProviderModel>, String> {
    let client = http_client().map_err(|error| error.message)?;
    let request = client
        .post(KIRO_MODELS_URL)
        .header(CONTENT_TYPE, "application/x-amz-json-1.0")
        .header("x-amz-target", KIRO_MODELS_TARGET)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .header(ACCEPT, "application/json")
        .json(&json!({
            "origin": "AI_EDITOR",
            "profileArn": profile_arn,
        }));

    let payload = fetch_json(request).await.map_err(fetch_json_error_message)?;
    Ok(payload
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("modelId").and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }

            let mut extra = BTreeMap::new();
            if let Some(description) = item.get("description") {
                extra.insert("description".to_string(), description.clone());
            }
            if let Some(rate_multiplier) = item.get("rateMultiplier") {
                extra.insert("rateMultiplier".to_string(), rate_multiplier.clone());
            }
            if let Some(rate_unit) = item.get("rateUnit") {
                extra.insert("rateUnit".to_string(), rate_unit.clone());
            }
            if let Some(max_input_tokens) = item
                .get("tokenLimits")
                .and_then(Value::as_object)
                .and_then(|limits| limits.get("maxInputTokens"))
            {
                extra.insert("maxInputTokens".to_string(), max_input_tokens.clone());
            }

            Some(ProviderModel {
                id: id.to_string(),
                name: item
                    .get("modelName")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_string(),
                extra,
            })
        })
        .collect())
}

async fn send_gemini_cli_models_request(
    access_token: &str,
    project_id: Option<&str>,
) -> Result<Value, FetchJsonError> {
    let client = http_client().map_err(|error| FetchJsonError::Network(error.message))?;
    let body = project_id
        .map(|project| json!({ "project": project }))
        .unwrap_or_else(|| json!({}));
    let request = client
        .post(GEMINI_CLI_MODELS_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .header("User-Agent", "google-api-nodejs-client/9.15.1")
        .header("X-Goog-Api-Client", "google-cloud-sdk vscode_cloudshelleditor/0.1")
        .json(&body);
    fetch_json(request).await
}

async fn refresh_google_token(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<RefreshResult, String> {
    let client = http_client().map_err(|error| error.message)?;
    let request = client
        .post(GOOGLE_TOKEN_URL)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ]);

    let payload = fetch_json(request).await.map_err(fetch_json_error_message)?;
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Google refresh response did not include access_token".to_string())?;

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: payload
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in: payload.get("expires_in").and_then(Value::as_i64),
    })
}

async fn refresh_kiro_token(
    refresh_token: &str,
    provider_specific_data: &BTreeMap<String, Value>,
) -> Result<RefreshResult, String> {
    let client = http_client().map_err(|error| error.message)?;

    let request = if let (Some(client_id), Some(client_secret)) = (
        provider_specific_data
            .get("clientId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        provider_specific_data
            .get("clientSecret")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ) {
        let region = provider_specific_data
            .get("region")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("us-east-1");
        client
            .post(format!("https://oidc.{region}.amazonaws.com/token"))
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({
                "clientId": client_id,
                "clientSecret": client_secret,
                "refreshToken": refresh_token,
                "grantType": "refresh_token",
            }))
    } else {
        client
            .post(format!("{KIRO_AUTH_SERVICE}/refreshToken"))
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({ "refreshToken": refresh_token }))
    };

    let payload = fetch_json(request).await.map_err(fetch_json_error_message)?;
    let access_token = payload
        .get("accessToken")
        .or_else(|| payload.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Kiro refresh response did not include access token".to_string())?;

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: payload
            .get("refreshToken")
            .or_else(|| payload.get("refresh_token"))
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in: payload
            .get("expiresIn")
            .or_else(|| payload.get("expires_in"))
            .and_then(Value::as_i64),
    })
}

async fn persist_refreshed_credentials(
    state: &AppState,
    connection: &ProviderConnection,
    refresh: &RefreshResult,
) {
    let connection_id = connection.id.clone();
    let access_token = refresh.access_token.clone();
    let refresh_token = refresh.refresh_token.clone();
    let expires_in = refresh.expires_in;

    let _ = state
        .db
        .update(|db| {
            let Some(target) = db
                .provider_connections
                .iter_mut()
                .find(|candidate| candidate.id == connection_id)
            else {
                return;
            };

            target.access_token = Some(access_token.clone());
            if let Some(refresh_token) = &refresh_token {
                target.refresh_token = Some(refresh_token.clone());
            }
            if let Some(expires_in) = expires_in {
                target.expires_in = Some(expires_in);
                target.expires_at =
                    Some((Utc::now() + ChronoDuration::seconds(expires_in)).to_rfc3339());
            }
            target.updated_at = Some(Utc::now().to_rfc3339());
        })
        .await;
}

fn parse_openai_style_models(payload: &Value) -> Vec<ProviderModel> {
    if let Some(array) = payload.as_array() {
        return parse_array_models(Some(&Value::Array(array.clone())));
    }

    parse_array_models(
        payload
            .get("data")
            .or_else(|| payload.get("models"))
            .or_else(|| payload.get("results")),
    )
}

fn parse_array_models(value: Option<&Value>) -> Vec<ProviderModel> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(provider_model_from_value).collect())
        .unwrap_or_default()
}

fn parse_gemini_cli_models(payload: &Value) -> Vec<ProviderModel> {
    if let Some(items) = payload.get("models").and_then(Value::as_array) {
        return items
            .iter()
            .filter_map(|item| {
                let id = item
                    .get("id")
                    .or_else(|| item.get("model"))
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())?;

                let mut extra = BTreeMap::new();
                if let Some(display_name) = item.get("displayName") {
                    extra.insert("displayName".to_string(), display_name.clone());
                }

                Some(ProviderModel {
                    id: id.to_string(),
                    name: item
                        .get("displayName")
                        .or_else(|| item.get("name"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(id)
                        .to_string(),
                    extra,
                })
            })
            .collect();
    }

    payload
        .get("models")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .filter(|(_, info)| {
                    info.get("isInternal")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        == false
                })
                .map(|(id, info)| ProviderModel {
                    id: id.to_string(),
                    name: info
                        .get("displayName")
                        .or_else(|| info.get("name"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(id)
                        .to_string(),
                    extra: BTreeMap::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn append_codex_review_models(models: Vec<ProviderModel>) -> Vec<ProviderModel> {
    let mut expanded = Vec::with_capacity(models.len() * 2);

    for model in models {
        let is_chat_model = model
            .extra
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("llm")
            != "image"
            && !model.id.to_lowercase().contains("embed");
        let already_review = model.id.ends_with("-review");

        if is_chat_model && !already_review {
            let mut review = model.clone();
            review.id = format!("{}-review", model.id);
            review.name = format!("{} Review", model.name);
            review
                .extra
                .insert("upstreamModelId".to_string(), Value::String(model.id.clone()));
            review
                .extra
                .insert("quotaFamily".to_string(), Value::String("review".to_string()));
            expanded.push(model);
            expanded.push(review);
        } else {
            expanded.push(model);
        }
    }

    expanded
}

fn provider_model_from_value(value: &Value) -> Option<ProviderModel> {
    match value {
        Value::String(text) => {
            let id = text.trim();
            if id.is_empty() {
                return None;
            }
            Some(ProviderModel {
                id: id.to_string(),
                name: id.to_string(),
                extra: BTreeMap::new(),
            })
        }
        Value::Object(object) => {
            let id = object
                .get("id")
                .or_else(|| object.get("slug"))
                .or_else(|| object.get("model"))
                .or_else(|| object.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())?;

            let name = object
                .get("display_name")
                .or_else(|| object.get("displayName"))
                .or_else(|| object.get("name"))
                .or_else(|| object.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(id);

            let extra = object
                .iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "id" | "slug" | "model" | "name" | "display_name" | "displayName"
                    )
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();

            Some(ProviderModel {
                id: id.to_string(),
                name: name.to_string(),
                extra,
            })
        }
        _ => None,
    }
}

fn normalize_anthropic_models_base_url(base_url: &str) -> String {
    let mut normalized = base_url.trim().trim_end_matches('/').to_string();
    if normalized.ends_with("/messages") {
        normalized.truncate(normalized.len() - "/messages".len());
    }
    format!("{normalized}/models")
}

fn resolve_qwen_models_url(connection: &ProviderConnection) -> String {
    let fallback = "https://portal.qwen.ai/v1/models";
    let Some(raw) = provider_specific_string(connection, "resourceUrl") else {
        return fallback.to_string();
    };

    if raw.starts_with("http://") || raw.starts_with("https://") {
        return format!("{}/models", raw.trim_end_matches('/'));
    }

    format!("https://{}/v1/models", raw.trim_end_matches('/'))
}

fn resolve_ollama_local_host(connection: &ProviderConnection) -> String {
    provider_specific_string(connection, "baseUrl")
        .unwrap_or_else(|| OLLAMA_LOCAL_DEFAULT_HOST.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn response_with_models(
    connection: &ProviderConnection,
    models: Vec<ProviderModel>,
    warning: Option<String>,
) -> ProviderModelsResponse {
    ProviderModelsResponse {
        provider: connection.provider.clone(),
        connection_id: connection.id.clone(),
        models,
        warning,
    }
}

fn primary_token(connection: &ProviderConnection) -> Option<String> {
    provider_specific_string(connection, "copilotToken")
        .or_else(|| connection.access_token.clone())
        .or_else(|| connection.api_key.clone())
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
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

fn is_openai_compatible_provider(provider: &str) -> bool {
    provider.starts_with(OPENAI_COMPATIBLE_PREFIX)
}

fn is_anthropic_compatible_provider(provider: &str) -> bool {
    provider.starts_with(ANTHROPIC_COMPATIBLE_PREFIX)
}

fn dedupe_model_ids(mut ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    ids
}

fn http_client() -> Result<reqwest::Client, RouteError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| RouteError::internal("Failed to fetch models"))
}

async fn fetch_json(request: reqwest::RequestBuilder) -> Result<Value, FetchJsonError> {
    let response = request
        .send()
        .await
        .map_err(|error| FetchJsonError::Network(error.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(FetchJsonError::Http(status, body));
    }

    response
        .json::<Value>()
        .await
        .map_err(|error| FetchJsonError::Decode(error.to_string()))
}

fn map_upstream_route_error(error: FetchJsonError) -> RouteError {
    match error {
        FetchJsonError::Http(status, _) => {
            RouteError::new(status, format!("Failed to fetch models: {}", status.as_u16()))
        }
        FetchJsonError::Network(_) | FetchJsonError::Decode(_) => {
            RouteError::internal("Failed to fetch models")
        }
    }
}

fn fetch_json_error_message(error: FetchJsonError) -> String {
    match error {
        FetchJsonError::Http(status, body) => {
            if body.is_empty() {
                status.as_u16().to_string()
            } else {
                body
            }
        }
        FetchJsonError::Network(message) | FetchJsonError::Decode(message) => message,
    }
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(provider: &str) -> ProviderConnection {
        ProviderConnection {
            id: "conn-1".to_string(),
            provider: provider.to_string(),
            auth_type: "oauth".to_string(),
            name: None,
            priority: None,
            is_active: Some(true),
            created_at: None,
            updated_at: None,
            display_name: None,
            email: None,
            global_priority: None,
            default_model: None,
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: None,
            test_status: None,
            last_tested: None,
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: None,
            error_code: None,
            consecutive_use_count: None,
            backoff_level: None,
            consecutive_errors: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            provider_specific_data: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn normalize_anthropic_base_url_strips_messages_suffix() {
        assert_eq!(
            normalize_anthropic_models_base_url("https://example.com/v1/messages"),
            "https://example.com/v1/models"
        );
        assert_eq!(
            normalize_anthropic_models_base_url("https://example.com/v1/messages/"),
            "https://example.com/v1/models"
        );
    }

    #[test]
    fn resolve_qwen_models_url_uses_resource_url_variants() {
        let mut connection = connection("qwen");
        connection.provider_specific_data.insert(
            "resourceUrl".to_string(),
            Value::String("tenant.qwen.ai".to_string()),
        );
        assert_eq!(
            resolve_qwen_models_url(&connection),
            "https://tenant.qwen.ai/v1/models"
        );

        connection.provider_specific_data.insert(
            "resourceUrl".to_string(),
            Value::String("https://tenant.qwen.ai/base".to_string()),
        );
        assert_eq!(
            resolve_qwen_models_url(&connection),
            "https://tenant.qwen.ai/base/models"
        );
    }

    #[test]
    fn parse_gemini_cli_models_filters_internal_entries() {
        let payload = json!({
            "models": {
                "gemini-2.5-pro": { "displayName": "Gemini 2.5 Pro", "isInternal": false },
                "internal-model": { "displayName": "Internal", "isInternal": true }
            }
        });

        let models = parse_gemini_cli_models(&payload);
        assert_eq!(
            models,
            vec![ProviderModel {
                id: "gemini-2.5-pro".to_string(),
                name: "Gemini 2.5 Pro".to_string(),
                extra: BTreeMap::new(),
            }]
        );
    }

    #[test]
    fn codex_parser_appends_review_variants_for_chat_models() {
        let models = append_codex_review_models(vec![
            ProviderModel {
                id: "gpt-5.5".to_string(),
                name: "GPT-5.5".to_string(),
                extra: BTreeMap::new(),
            },
            ProviderModel {
                id: "text-embedding-3-large".to_string(),
                name: "Embedding".to_string(),
                extra: BTreeMap::new(),
            },
            ProviderModel {
                id: "gpt-image-1".to_string(),
                name: "Image".to_string(),
                extra: BTreeMap::from([("type".to_string(), Value::String("image".to_string()))]),
            },
        ]);

        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "gpt-5.5",
                "gpt-5.5-review",
                "text-embedding-3-large",
                "gpt-image-1"
            ]
        );
        assert_eq!(
            models[1].extra.get("upstreamModelId"),
            Some(&Value::String("gpt-5.5".to_string()))
        );
    }
}
