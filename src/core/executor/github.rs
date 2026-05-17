use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const GITHUB_COPILOT_VSCODE_VERSION: &str = "1.110.0";
const GITHUB_COPILOT_CHAT_VERSION: &str = "0.38.0";
const GITHUB_COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.38.0";
const GITHUB_COPILOT_API_VERSION: &str = "2025-04-01";
const GITHUB_COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const GITHUB_OAUTH_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

#[derive(Clone)]
pub struct GithubExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum GithubExecutorError {
    MissingCredentials(String),
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for GithubExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for GithubExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for GithubExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for GithubExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for GithubExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct GithubExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct GithubExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct GithubCopilotTokenResponse {
    pub token: String,
    #[serde(rename = "expires_at")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct GithubOAuthTokenResponse {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

impl GithubExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, GithubExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(&self) -> String {
        // GitHub Copilot uses a fixed base URL for chat completions
        "https://api.githubcopilot.com/chat/completions".to_string()
    }

    fn build_headers(&self, credentials: &ProviderConnection, stream: bool) -> HeaderMap {
        let token = credentials
            .provider_specific_data
            .get("copilotToken")
            .and_then(|v| v.as_str())
            .or(credentials.access_token.as_deref())
            .unwrap_or("");

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token))
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "copilot-integration-id",
            HeaderValue::from_static("vscode-chat"),
        );
        headers.insert(
            "editor-version",
            HeaderValue::from_str(&format!("vscode/{}", GITHUB_COPILOT_VSCODE_VERSION))
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "editor-plugin-version",
            HeaderValue::from_str(&format!("copilot-chat/{}", GITHUB_COPILOT_CHAT_VERSION))
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(GITHUB_COPILOT_USER_AGENT),
        );
        headers.insert(
            "openai-intent",
            HeaderValue::from_static("conversation-panel"),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(GITHUB_COPILOT_API_VERSION),
        );
        headers.insert(
            "x-request-id",
            HeaderValue::from_str(&Uuid::new_v4().to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "x-vscode-user-agent-library-version",
            HeaderValue::from_static("electron-fetch"),
        );
        headers.insert("X-Initiator", HeaderValue::from_static("user"));

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        headers
    }

    /// Check if model requires max_completion_tokens instead of max_tokens
    fn requires_max_completion_tokens(model: &str) -> bool {
        let lower = model.to_lowercase();
        lower.contains("gpt-5")
            || lower.contains("o1-")
            || lower.contains("o3-")
            || lower.contains("o4-")
    }

    /// Check if model supports temperature parameter
    fn supports_temperature(model: &str) -> bool {
        !model.to_lowercase().contains("gpt-5.4")
    }

    /// Check if model supports thinking (Claude models on Copilot don't)
    fn supports_thinking(model: &str) -> bool {
        !model.to_lowercase().contains("claude")
    }

    /// Check if model supports reasoning_effort
    fn supports_reasoning_effort(model: &str) -> bool {
        let lower = model.to_lowercase();
        // Claude Opus 4.6 and Sonnet 4.6 support it
        if lower.contains("claude")
            && (lower.contains("opus") || lower.contains("sonnet"))
            && lower.contains("4.6")
        {
            return true;
        }
        // All other Claude models: strip
        if lower.contains("claude") {
            return false;
        }
        // GPT-5 family, Gemini, etc.: keep
        true
    }

    /// Sanitize messages for GitHub Copilot /chat/completions endpoint.
    /// Only 'text' and 'image_url' content part types are accepted.
    fn sanitize_messages(body: &mut Value) {
        // Extract response_format and model info first (before mutable borrow of messages)
        let needs_json_instruction = {
            let has_claude = body
                .get("model")
                .and_then(|m| m.as_str())
                .map(|m| m.to_lowercase().contains("claude"))
                .unwrap_or(false);
            let has_response_format = body.get("response_format").is_some();
            has_claude && has_response_format
        };

        let system_instruction = if needs_json_instruction {
            let response_format = body.get("response_format").cloned().unwrap_or(Value::Null);
            if let Some(schema) = response_format
                .get("json_schema")
                .and_then(|j| j.get("schema"))
            {
                Some("CRITICAL: You must ONLY output raw JSON. Never use markdown code blocks. Never use backticks. Never wrap JSON in triple backticks. Output ONLY the raw JSON object.".to_string())
            } else if response_format.get("type").and_then(|t| t.as_str()) == Some("json_object") {
                Some("CRITICAL: You must ONLY output raw JSON. Never use markdown code blocks. Never use backticks.".to_string())
            } else {
                None
            }
        } else {
            None
        };

        if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            if let Some(instruction) = system_instruction {
                let mut system_idx = None;
                for (i, msg) in messages.iter().enumerate() {
                    if msg.get("role").and_then(|r| r.as_str()) == Some("system") {
                        system_idx = Some(i);
                        break;
                    }
                }

                if let Some(idx) = system_idx {
                    if let Some(content) = messages[idx].get_mut("content") {
                        let existing = content.as_str().unwrap_or("").to_string();
                        *content = json!(format!("{}\n\n{}", instruction, existing));
                    }
                } else {
                    messages.insert(
                        0,
                        json!({
                            "role": "system",
                            "content": instruction
                        }),
                    );
                }

                let mut last_user_idx = None;
                for (i, msg) in messages.iter().enumerate() {
                    if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                        last_user_idx = Some(i);
                    }
                }
                if let Some(idx) = last_user_idx {
                    if let Some(content) = messages[idx].get_mut("content") {
                        let existing = if content.is_string() {
                            content.as_str().unwrap_or("").to_string()
                        } else {
                            serde_json::to_string(content).unwrap_or_default()
                        };
                        *content = json!(format!("Respond with ONLY raw JSON (no markdown, no backticks, no code blocks): {}", existing));
                    }
                }
            }

            for msg in messages.iter_mut() {
                if let Some(content) = msg.get_mut("content") {
                    if content.is_null() {
                        continue;
                    }

                    if content.is_string() {
                        continue;
                    }

                    if let Some(arr) = content.as_array_mut() {
                        let mut clean_content = Vec::new();
                        for part in arr.iter() {
                            if let Some(part_type) = part.get("type").and_then(|t| t.as_str()) {
                                if part_type == "text" || part_type == "image_url" {
                                    clean_content.push(part.clone());
                                } else {
                                    let serialized =
                                        serde_json::to_string(part).unwrap_or_default();
                                    let text = part
                                        .get("text")
                                        .or_else(|| part.get("content"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or(&serialized);
                                    clean_content.push(json!({
                                        "type": "text",
                                        "text": text
                                    }));
                                }
                            }
                        }

                        if clean_content.is_empty() {
                            *content = Value::Null;
                        } else {
                            *content = json!(clean_content);
                        }
                    }
                }
            }
        }
    }

    fn transform_request(&self, body: &Value, model: &str, stream: bool) -> Value {
        let mut transformed = body.clone();

        // max_completion_tokens for newer models
        if Self::requires_max_completion_tokens(model) {
            if let Some(max_tokens) = transformed.get("max_tokens") {
                transformed["max_completion_tokens"] = max_tokens.clone();
                transformed.as_object_mut().unwrap().remove("max_tokens");
            }
        }

        // Strip temperature for unsupported models
        if !Self::supports_temperature(model) {
            transformed.as_object_mut().unwrap().remove("temperature");
        }

        // Strip Claude-style thinking for non-thinking models
        if !Self::supports_thinking(model) {
            transformed.as_object_mut().unwrap().remove("thinking");
        }

        // Strip reasoning_effort "none"
        if transformed.get("reasoning_effort").and_then(|v| v.as_str()) == Some("none") {
            transformed
                .as_object_mut()
                .unwrap()
                .remove("reasoning_effort");
        }

        // Strip reasoning_effort for unsupported models
        if !Self::supports_reasoning_effort(model) {
            transformed
                .as_object_mut()
                .unwrap()
                .remove("reasoning_effort");
        }

        transformed
    }

    pub async fn execute_request(
        &self,
        request: GithubExecutionRequest,
    ) -> Result<GithubExecutorResponse, GithubExecutorError> {
        let url = self.build_url();
        let mut body = self.transform_request(&request.body, &request.model, request.stream);
        Self::sanitize_messages(&mut body);

        let headers = self.build_headers(&request.credentials, request.stream);

        let client = self.pool.get("github", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&body)
            .send()
            .await?;

        Ok(GithubExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body: body,
            transport: TransportKind::Reqwest,
        })
    }

    /// Refresh GitHub Copilot token
    pub async fn refresh_copilot_token(
        &self,
        github_access_token: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Option<GithubCopilotTokenResponse> {
        let client = reqwest::Client::builder().build().ok()?;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("token {}", github_access_token)).ok()?,
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(GITHUB_COPILOT_USER_AGENT),
        );
        headers.insert(
            "Editor-Version",
            HeaderValue::from_str(&format!("vscode/{}", GITHUB_COPILOT_VSCODE_VERSION)).ok()?,
        );
        headers.insert(
            "Editor-Plugin-Version",
            HeaderValue::from_str(&format!("copilot-chat/{}", GITHUB_COPILOT_CHAT_VERSION)).ok()?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(GITHUB_COPILOT_API_VERSION),
        );

        let response = client
            .get(GITHUB_COPILOT_TOKEN_URL)
            .headers(headers)
            .send()
            .await
            .ok()?;

        if !response.status().is_success() {
            return None;
        }

        response.json::<GithubCopilotTokenResponse>().await.ok()
    }

    /// Refresh GitHub OAuth token
    pub async fn refresh_github_token(
        &self,
        refresh_token: &str,
        client_id: &str,
        client_secret: Option<&str>,
        proxy: Option<&ProxyTarget>,
    ) -> Option<GithubOAuthTokenResponse> {
        let client = reqwest::Client::builder().build().ok()?;

        let mut params = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ];
        if let Some(secret) = client_secret {
            params.push(("client_secret", secret));
        }

        let response = client
            .post(GITHUB_OAUTH_TOKEN_URL)
            .form(&params)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .send()
            .await
            .ok()?;

        if !response.status().is_success() {
            return None;
        }

        response.json::<GithubOAuthTokenResponse>().await.ok()
    }
}
