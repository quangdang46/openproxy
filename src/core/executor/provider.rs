use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::oauth::token_refresh::{dispatch_oauth_refresh, needs_refresh as oauth_needs_refresh};
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

/// Log severity level for per-request log messages.
#[derive(Debug, Clone)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// A single log entry attached to a request.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
}

/// Options that control proxy/retry behaviour for a single execution.
#[derive(Debug, Clone, Default)]
pub struct ProxyOptions {
    /// URL index to try (for round-robin / fallback rotation).
    pub url_index: Option<usize>,
}

pub struct ProviderExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
    /// Signal for aborting an in-flight request.
    pub signal: Option<CancellationToken>,
    /// Request-scoped log entries.
    pub log: Option<Vec<LogEntry>>,
    /// Options controlling proxy/retry behaviour.
    pub proxy_options: Option<ProxyOptions>,
}

pub struct ProviderExecutionResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

#[derive(Debug)]
pub enum ProviderExecutorError {
    UnsupportedProvider(String),
    MissingCredentials(String),
    InvalidHeader(String),
    InvalidUri(hyper::http::uri::InvalidUri),
    InvalidRequest(hyper::http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
}

impl From<reqwest::Error> for ProviderExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for ProviderExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error.to_string())
    }
}

impl From<hyper::http::uri::InvalidUri> for ProviderExecutorError {
    fn from(error: hyper::http::uri::InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for ProviderExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for ProviderExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for ProviderExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for ProviderExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl std::fmt::Display for ProviderExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProvider(p) => write!(f, "Unsupported provider: {}", p),
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::InvalidHeader(e) => write!(f, "Invalid header: {}", e),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
        }
    }
}

impl std::error::Error for ProviderExecutorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFormat {
    OpenAI,
    Anthropic,
    Gemini,
    ClaudeCompatible,
    OpenAICompatible,
    AnthropicCompatible,
}

impl ProviderFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::ClaudeCompatible => "claude_compatible",
            Self::OpenAICompatible => "openai_compatible",
            Self::AnthropicCompatible => "anthropic_compatible",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderExecutorConfig {
    pub base_url: String,
    pub format: ProviderFormat,
    pub api_key_header: &'static str,
    pub default_headers: Vec<(String, String)>,
    pub stream_path: String,
    pub chat_path: String,
}

impl ProviderExecutorConfig {
    pub fn openai(base_url: &'static str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: ProviderFormat::OpenAI,
            api_key_header: "Authorization",
            default_headers: Vec::new(),
            stream_path: "/chat/completions".to_string(),
            chat_path: "/chat/completions".to_string(),
        }
    }

    pub fn anthropic(base_url: &'static str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: ProviderFormat::Anthropic,
            api_key_header: "x-api-key",
            default_headers: vec![
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
                (
                    "anthropic-beta".to_string(),
                    "claude-code-20250219,interleaved-thinking-2025-05-14".to_string(),
                ),
            ],
            stream_path: "/v1/messages".to_string(),
            chat_path: "/v1/messages".to_string(),
        }
    }

    pub fn claude_compatible(base_url: &'static str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: ProviderFormat::ClaudeCompatible,
            api_key_header: "x-api-key",
            default_headers: vec![
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
                (
                    "anthropic-beta".to_string(),
                    "claude-code-20250219,interleaved-thinking-2025-05-14".to_string(),
                ),
            ],
            stream_path: "/v1/messages".to_string(),
            chat_path: "/v1/messages".to_string(),
        }
    }

    pub fn gemini(base_url: &'static str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: ProviderFormat::Gemini,
            api_key_header: "x-goog-api-key",
            default_headers: Vec::new(),
            stream_path: "/v1beta/models".to_string(),
            chat_path: "/v1beta/models".to_string(),
        }
    }

    pub fn openai_compatible(base_url: &'static str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: ProviderFormat::OpenAICompatible,
            api_key_header: "Authorization",
            default_headers: Vec::new(),
            stream_path: "/chat/completions".to_string(),
            chat_path: "/chat/completions".to_string(),
        }
    }

    pub fn anthropic_compatible(base_url: &'static str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: ProviderFormat::AnthropicCompatible,
            api_key_header: "x-api-key",
            default_headers: vec![("anthropic-version".to_string(), "2023-06-01".to_string())],
            stream_path: "/v1/messages".to_string(),
            chat_path: "/v1/messages".to_string(),
        }
    }

    pub fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.default_headers
            .push((name.to_string(), value.to_string()));
        self
    }
}

#[async_trait]
pub trait ProviderExecutor: Send + Sync {
    fn provider_name(&self) -> &str;

    async fn execute(
        &self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionResponse, ProviderExecutorError>;

    fn build_url(
        &self,
        model: &str,
        stream: bool,
        url_index: Option<usize>,
        credentials: Option<&ProviderConnection>,
    ) -> String;

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, ProviderExecutorError>;

    fn transform_request(
        &self,
        body: &Value,
        _model: &str,
        _stream: bool,
        _credentials: &ProviderConnection,
    ) -> Value {
        body.clone()
    }

    /// Refresh the OAuth / access-token credentials for this provider.
    ///
    /// Returns `Some(updated_connection)` on success, or `None` if the
    /// provider does not support credential refresh or the refresh failed.
    async fn refresh_credentials(
        &self,
        credentials: &ProviderConnection,
    ) -> Option<ProviderConnection> {
        let _ = credentials;
        None
    }

    /// Returns `true` if the credentials are expired (or close to expiring)
    /// and should be refreshed before the next request.
    fn needs_refresh(&self, credentials: &ProviderConnection) -> bool {
        let _ = credentials;
        false
    }
}

pub struct UnifiedExecutor {
    provider: String,
    config: ProviderExecutorConfig,
    pool: Arc<ClientPool>,
}

impl UnifiedExecutor {
    pub fn new(provider: &str, config: ProviderExecutorConfig, pool: Arc<ClientPool>) -> Self {
        Self {
            provider: provider.to_string(),
            config,
            pool,
        }
    }

    pub fn for_provider(provider: &str, pool: Arc<ClientPool>) -> Option<Self> {
        let config = get_provider_config(provider)?;
        Some(Self::new(provider, config, pool))
    }

    pub fn provider_name(&self) -> &str {
        &self.provider
    }

    pub fn build_url(
        &self,
        model: &str,
        stream: bool,
        _url_index: Option<usize>,
        _credentials: Option<&ProviderConnection>,
    ) -> String {
        let path = if stream {
            &self.config.stream_path
        } else {
            &self.config.chat_path
        };

        match self.config.format {
            ProviderFormat::Gemini => {
                let action = if stream {
                    "streamGenerateContent?alt=sse"
                } else {
                    "generateContent"
                };
                format!(
                    "{}/{model}:{action}",
                    self.config.base_url.trim_end_matches('/')
                )
            }
            _ => format!("{}{}", self.config.base_url.trim_end_matches('/'), path),
        }
    }

    /// Build URL with optional API key appended as query param for providers that need it.
    /// Currently used by gemini free tier which has 15 RPM limit.
    pub fn build_url_with_api_key(
        &self,
        model: &str,
        stream: bool,
        api_key: Option<&str>,
    ) -> String {
        let base_url = self.build_url(model, stream, None, None);
        if let Some(key) = api_key {
            format!("{base_url}&key={key}")
        } else {
            base_url
        }
    }

    pub fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, ProviderExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        for (name, value) in &self.config.default_headers {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| ProviderExecutorError::InvalidHeader(name.clone()))?,
                HeaderValue::from_str(value)
                    .map_err(|e| ProviderExecutorError::InvalidHeader(e.to_string()))?,
            );
        }

        let token = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .ok_or_else(|| ProviderExecutorError::MissingCredentials(self.provider.clone()))?;

        let header_name =
            reqwest::header::HeaderName::from_bytes(self.config.api_key_header.as_bytes())
                .map_err(|_| {
                    ProviderExecutorError::InvalidHeader(self.config.api_key_header.to_string())
                })?;

        match self.config.format {
            ProviderFormat::Anthropic
            | ProviderFormat::ClaudeCompatible
            | ProviderFormat::AnthropicCompatible => {
                if self.config.api_key_header == "Authorization" {
                    headers.insert(
                        header_name,
                        HeaderValue::from_str(&format!("Bearer {token}"))?,
                    );
                } else {
                    headers.insert(header_name, HeaderValue::from_str(token)?);
                }
            }
            ProviderFormat::Gemini => {
                if header_name == reqwest::header::AUTHORIZATION {
                    headers.insert(
                        header_name,
                        HeaderValue::from_str(&format!("Bearer {token}"))?,
                    );
                } else {
                    headers.insert(header_name, HeaderValue::from_str(token)?);
                }
            }
            _ => {
                if header_name == reqwest::header::AUTHORIZATION {
                    headers.insert(
                        header_name,
                        HeaderValue::from_str(&format!("Bearer {token}"))?,
                    );
                } else {
                    headers.insert(header_name, HeaderValue::from_str(token)?);
                }
            }
        }

        if stream {
            headers.insert(
                reqwest::header::ACCEPT,
                HeaderValue::from_static("text/event-stream"),
            );
        }

        Ok(headers)
    }

    pub fn transform_request(
        &self,
        body: &Value,
        _model: &str,
        _stream: bool,
        _credentials: &ProviderConnection,
    ) -> Value {
        let mut body = self.apply_json_schema_fallback(body);

        normalize_developer_role(&mut body);

        body
    }

    /// Fallback json_schema -> json_object for openai-compatible providers
    /// without native Structured Output support.
    ///
    /// When `response_format.type` is `"json_schema"`, this method:
    /// 1. Extracts the JSON schema
    /// 2. Injects schema instructions into the system message
    /// 3. Downgrades `response_format` to `{"type": "json_object"}`
    fn apply_json_schema_fallback(&self, body: &Value) -> Value {
        let is_openai = matches!(
            self.config.format,
            ProviderFormat::OpenAI | ProviderFormat::OpenAICompatible
        );

        if !is_openai {
            return body.clone();
        }

        let response_format = match body.get("response_format") {
            Some(rf) => rf,
            None => return body.clone(),
        };

        if response_format.get("type").and_then(Value::as_str) != Some("json_schema") {
            return body.clone();
        }

        let schema = match response_format
            .get("json_schema")
            .and_then(|s| s.get("schema"))
        {
            Some(s) => s,
            None => return body.clone(),
        };

        let schema_json = serde_json::to_string_pretty(schema).unwrap_or_default();
        let prompt = format!(
            "You must respond with valid JSON that strictly follows this JSON schema:\n```json\n{schema_json}\n```\nRespond ONLY with the JSON object, no other text."
        );

        let mut new_body = body.clone();

        if let Some(messages) = new_body.get_mut("messages").and_then(Value::as_array_mut) {
            let sys_idx = messages
                .iter()
                .position(|m| m.get("role").and_then(Value::as_str) == Some("system"));

            if let Some(idx) = sys_idx {
                let sys = &mut messages[idx];
                if let Some(content) = sys.get_mut("content") {
                    if content.is_string() {
                        let existing = content.as_str().unwrap_or("");
                        *content = Value::String(format!("{existing}\n\n{prompt}"));
                    } else if let Some(arr) = content.as_array_mut() {
                        arr.push(serde_json::json!({
                            "type": "text",
                            "text": format!("\n\n{prompt}")
                        }));
                    }
                }
            } else {
                messages.insert(
                    0,
                    serde_json::json!({
                        "role": "system",
                        "content": prompt
                    }),
                );
            }
        }

        new_body["response_format"] = serde_json::json!({"type": "json_object"});
        new_body
    }

    pub async fn execute(
        &self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionResponse, ProviderExecutorError> {
        let api_key = request.credentials.api_key.as_deref();
        let url_index = request.proxy_options.as_ref().and_then(|o| o.url_index);
        let url = if self.provider == "gemini" && api_key.is_some() {
            self.build_url_with_api_key(&request.model, request.stream, api_key)
        } else {
            self.build_url(
                &request.model,
                request.stream,
                url_index,
                Some(&request.credentials),
            )
        };
        let headers = self.build_headers(&request.credentials, request.stream)?;
        let transformed_body = self.transform_request(
            &request.body,
            &request.model,
            request.stream,
            &request.credentials,
        );

        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get(&self.provider, request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        Ok(ProviderExecutionResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    /// Refresh OAuth/access-token credentials.
    async fn refresh_credentials(
        &self,
        credentials: &ProviderConnection,
    ) -> Option<ProviderConnection> {
        let refresh_token = credentials.refresh_token.as_deref()?;
        if refresh_token.is_empty() {
            return None;
        }

        match dispatch_oauth_refresh(
            &self.provider,
            refresh_token,
            &credentials.provider_specific_data,
        )
        .await
        {
            Ok(result) => {
                let mut updated = credentials.clone();
                updated.access_token = Some(result.access_token);
                if let Some(new_refresh) = result.refresh_token {
                    updated.refresh_token = Some(new_refresh);
                }
                if let Some(expires_in) = result.expires_in {
                    let expiry = chrono::Utc::now() + chrono::Duration::seconds(expires_in);
                    updated.expires_at = Some(expiry.to_rfc3339());
                }
                Some(updated)
            }
            Err(e) => {
                tracing::warn!(
                    "credential refresh failed for provider {}: {}",
                    self.provider,
                    e
                );
                None
            }
        }
    }

    /// Returns true if the credentials are expired or near-expiration.
    fn needs_refresh(&self, credentials: &ProviderConnection) -> bool {
        oauth_needs_refresh(&credentials.expires_at)
    }
}

#[async_trait]
impl ProviderExecutor for UnifiedExecutor {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn execute(
        &self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionResponse, ProviderExecutorError> {
        UnifiedExecutor::execute(self, request).await
    }

    fn build_url(
        &self,
        model: &str,
        stream: bool,
        url_index: Option<usize>,
        credentials: Option<&ProviderConnection>,
    ) -> String {
        self.build_url(model, stream, url_index, credentials)
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, ProviderExecutorError> {
        self.build_headers(credentials, stream)
    }

    fn transform_request(
        &self,
        body: &Value,
        model: &str,
        stream: bool,
        credentials: &ProviderConnection,
    ) -> Value {
        self.transform_request(body, model, stream, credentials)
    }

    async fn refresh_credentials(
        &self,
        credentials: &ProviderConnection,
    ) -> Option<ProviderConnection> {
        UnifiedExecutor::refresh_credentials(self, credentials).await
    }

    fn needs_refresh(&self, credentials: &ProviderConnection) -> bool {
        UnifiedExecutor::needs_refresh(self, credentials)
    }
}

static PROVIDER_REGISTRY: once_cell::sync::Lazy<BTreeMap<&'static str, ProviderExecutorConfig>> =
    once_cell::sync::Lazy::new(|| {
        BTreeMap::from([
            (
                "claude",
                ProviderExecutorConfig::anthropic("https://api.anthropic.com/v1"),
            ),
            (
                "codex",
                ProviderExecutorConfig::openai("https://api.openai.com/v1"),
            ),
            (
                "github",
                ProviderExecutorConfig::openai("https://api.github.com/copilot"),
            ),
            (
                "gitlab",
                ProviderExecutorConfig::openai("https://gitlab.com/api/v4"),
            ),
            (
                "kiro",
                ProviderExecutorConfig::anthropic("https://api.kiro.ai/v1"),
            ),
            (
                "cursor",
                ProviderExecutorConfig::openai("https://api.cursor.com/v1"),
            ),
            (
                "kiro-free",
                ProviderExecutorConfig::anthropic("https://api.kiro.ai/v1"),
            ),
            (
                "opencode",
                ProviderExecutorConfig::openai("https://opencode.ai/zen/v1"),
            ),
            (
                "vertex",
                ProviderExecutorConfig::gemini(
                    "https://generativelanguage.googleapis.com/v1beta/models",
                ),
            ),
            (
                "openai",
                ProviderExecutorConfig::openai("https://api.openai.com/v1"),
            ),
            (
                "deepseek",
                ProviderExecutorConfig::openai("https://api.deepseek.com/v1"),
            ),
            (
                "groq",
                ProviderExecutorConfig::openai("https://api.groq.com/openai/v1"),
            ),
            (
                "together",
                ProviderExecutorConfig::openai("https://api.together.xyz/v1"),
            ),
            (
                "fireworks",
                ProviderExecutorConfig::openai("https://api.fireworks.ai/inference/v1"),
            ),
            (
                "cerebras",
                ProviderExecutorConfig::openai("https://api.cerebras.ai/v1"),
            ),
            (
                "mistral",
                ProviderExecutorConfig::openai("https://api.mistral.ai/v1"),
            ),
            (
                "cohere",
                ProviderExecutorConfig::openai("https://api.cohere.ai/v1"),
            ),
            (
                "perplexity",
                ProviderExecutorConfig::openai("https://api.perplexity.ai/chat/completions"),
            ),
            (
                "xai",
                ProviderExecutorConfig::openai("https://api.x.ai/v1")
                    .with_header("User-Agent", "grok-cli/9router"),
            ),
            (
                "nvidia",
                ProviderExecutorConfig::openai("https://integrate.api.nvidia.com/v1"),
            ),
            (
                "cloudflare-ai",
                ProviderExecutorConfig::openai(
                    "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/v1",
                ),
            ),
            (
                "blackbox",
                ProviderExecutorConfig::openai("https://api.blackbox.ai/api/chat/completions"),
            ),
            (
                "ai21",
                ProviderExecutorConfig::openai("https://api.ai21.com/v1"),
            ),
            (
                "lepton",
                ProviderExecutorConfig::openai("https://api.lepton.ai/v1"),
            ),
            (
                "novita",
                ProviderExecutorConfig::openai("https://api.novita.ai/v1"),
            ),
            (
                "deepinfra",
                ProviderExecutorConfig::openai("https://api.deepinfra.com/v1"),
            ),
            (
                "focus",
                ProviderExecutorConfig::openai("https://api.focusforce.io/v1"),
            ),
            (
                "navigators",
                ProviderExecutorConfig::openai("https://api.navigators.ai/v1"),
            ),
            (
                "polyscale",
                ProviderExecutorConfig::openai("https://api.polyscale.ai/v1"),
            ),
            (
                "rampt",
                ProviderExecutorConfig::openai("https://api.rampt.ai/v1"),
            ),
            (
                "skip",
                ProviderExecutorConfig::openai("https://api.skip.cloud/v1"),
            ),
            (
                "unbound",
                ProviderExecutorConfig::openai("https://api.unbound.ai/v1"),
            ),
            (
                "workers",
                ProviderExecutorConfig::openai("https://api.cloudflare.ai/v1"),
            ),
            (
                "zerogpt",
                ProviderExecutorConfig::openai("https://api.zerogpt.com/v1"),
            ),
            (
                "nebius",
                ProviderExecutorConfig::openai("https://api.studio.nebius.ai/v1"),
            ),
            (
                "siliconflow",
                ProviderExecutorConfig::openai("https://api.siliconflow.cn/v1"),
            ),
            (
                "hyperbolic",
                ProviderExecutorConfig::openai("https://api.hyperbolic.xyz/v1"),
            ),
            (
                "chutes",
                ProviderExecutorConfig::openai("https://llm.chutes.ai/v1"),
            ),
            (
                "anthropic",
                ProviderExecutorConfig::anthropic("https://api.anthropic.com/v1"),
            ),
            (
                "glm",
                ProviderExecutorConfig::claude_compatible("https://api.z.ai/api/anthropic/v1"),
            ),
            (
                "kimi",
                ProviderExecutorConfig::claude_compatible("https://api.kimi.com/coding/v1"),
            ),
            (
                "kimi-coding",
                ProviderExecutorConfig::claude_compatible("https://api.kimi.com/coding/v1"),
            ),
            (
                "minimax",
                ProviderExecutorConfig::claude_compatible("https://api.minimax.io/anthropic/v1"),
            ),
            (
                "minimax-cn",
                ProviderExecutorConfig::claude_compatible("https://api.minimaxi.com/anthropic/v1"),
            ),
            (
                "gemini",
                ProviderExecutorConfig::gemini(
                    "https://generativelanguage.googleapis.com/v1beta/models",
                ),
            ),
            (
                "codebuddy",
                ProviderExecutorConfig::openai("https://copilot.tencent.com/v1"),
            ),
            (
                "codebuddy-cn",
                ProviderExecutorConfig::openai("https://api.codebuddy.cn/v1"),
            ),
            (
                "gemini-cli",
                ProviderExecutorConfig::gemini("https://cloudcode-pa.googleapis.com/v1internal"),
            ),
            (
                "iflow",
                ProviderExecutorConfig::openai("https://apis.iflow.cn/v1"),
            ),
            (
                "mimo-free",
                ProviderExecutorConfig::openai("https://mimo.kiro.dev/v1"),
            ),
            (
                "qoder",
                ProviderExecutorConfig::openai("https://api.qoder.com/v1"),
            ),
            (
                "qwen",
                ProviderExecutorConfig::openai("https://portal.qwen.ai/v1"),
            ),
            (
                "kilocode",
                ProviderExecutorConfig::openai("https://api.kilo.ai/api/openrouter/v1"),
            ),
            (
                "alicode",
                ProviderExecutorConfig::openai("https://coding.dashscope.aliyuncs.com/v1"),
            ),
            (
                "alicode-intl",
                ProviderExecutorConfig::openai("https://coding-intl.dashscope.aliyuncs.com/v1"),
            ),
            (
                "volcengine-ark",
                ProviderExecutorConfig::openai("https://ark.cn-beijing.volces.com/api/coding/v3"),
            ),
            (
                "byteplus",
                ProviderExecutorConfig::openai(
                    "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
                ),
            ),
            (
                "openrouter",
                ProviderExecutorConfig::openai("https://openrouter.ai/api/v1/chat/completions")
                    .with_header("HTTP-Referer", "https://endpoint-proxy.local")
                    .with_header("X-Title", "Endpoint Proxy"),
            ),
            (
                "ollama-cloud",
                ProviderExecutorConfig::openai("https://ollama.com/v1"),
            ),
            (
                "ollama-local",
                ProviderExecutorConfig::openai("http://localhost:11434/v1"),
            ),
            (
                "stability-ai",
                ProviderExecutorConfig::openai("https://api.stability.ai/v1"),
            ),
            (
                "replicate",
                ProviderExecutorConfig::openai("https://api.replicate.com/v1"),
            ),
            (
                "cline",
                ProviderExecutorConfig::openai("https://api.cline.bot/api/v1/chat/completions")
                    .with_header("HTTP-Referer", "https://cline.bot")
                    .with_header("X-Title", "Cline"),
            ),
            (
                "opencode-go",
                ProviderExecutorConfig::openai("https://opencode.ai/zen/v1"),
            ),
            (
                "glm-cn",
                ProviderExecutorConfig::openai(
                    "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
                ),
            ),
            (
                "vertex-partner",
                ProviderExecutorConfig::gemini(
                    "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}",
                ),
            ),
            (
                "azure",
                ProviderExecutorConfig::openai(
                    "https://{resource}.openai.azure.com/v1/chat/completions",
                ),
            ),
            (
                "grok",
                ProviderExecutorConfig::openai("https://api.x.ai/v1"),
            ),
            (
                "elevenlabs",
                ProviderExecutorConfig::openai("https://api.elevenlabs.io/v1"),
            ),
            (
                "cartesia",
                ProviderExecutorConfig::openai("https://api.cartesia.ai/v1"),
            ),
            (
                "playht",
                ProviderExecutorConfig::openai("https://api.play.ht/api/v1"),
            ),
            (
                "deepgram",
                ProviderExecutorConfig::openai("https://api.deepgram.com/v1"),
            ),
            (
                "google-tts",
                ProviderExecutorConfig::openai("https://texttospeech.googleapis.com/v1"),
            ),
            (
                "edge-tts",
                ProviderExecutorConfig::openai(
                    "https://edge.tts.api.speech.microsoft.com/cognitiveservices/v1",
                ),
            ),
            (
                "openai-embedding",
                ProviderExecutorConfig::openai("https://api.openai.com/v1"),
            ),
            (
                "cohere-embedding",
                ProviderExecutorConfig::openai("https://api.cohere.ai/v1"),
            ),
            (
                "voyage-ai",
                ProviderExecutorConfig::openai("https://api.voyageai.com/v1"),
            ),
            (
                "dalle",
                ProviderExecutorConfig::openai("https://api.openai.com/v1"),
            ),
            (
                "stable-diffusion",
                ProviderExecutorConfig::openai("https://api.stability.ai/v1"),
            ),
            (
                "tavily",
                ProviderExecutorConfig::openai("https://api.tavily.com/v1"),
            ),
            (
                "brave-search",
                ProviderExecutorConfig::openai("https://api.search.brave.com/v1"),
            ),
            (
                "serper",
                ProviderExecutorConfig::openai("https://google.serper.dev/search"),
            ),
            (
                "exa",
                ProviderExecutorConfig::openai("https://api.exa.ai/v1"),
            ),
            (
                "antigravity",
                ProviderExecutorConfig::gemini("https://cloudcode-pa.googleapis.com/v1internal"),
            ),
            (
                "grok-web",
                ProviderExecutorConfig::openai("https://grok.com/app-chat/conversations/new"),
            ),
            (
                "perplexity-web",
                ProviderExecutorConfig::openai("https://www.perplexity.ai"),
            ),
            (
                "xiaomi-mimo",
                ProviderExecutorConfig::openai("https://api.xiaomimimo.com/v1"),
            ),
            (
                "assemblyai",
                ProviderExecutorConfig::openai("https://api.assemblyai.com/v2"),
            ),
            (
                "black-forest-labs",
                ProviderExecutorConfig::openai("https://api.blackforestlabs.ai/v1"),
            ),
            (
                "fal-ai",
                ProviderExecutorConfig::openai("https://fal.run/fal-ai"),
            ),
            (
                "runwayml",
                ProviderExecutorConfig::openai("https://api.runwayml.com/v1"),
            ),
            (
                "sdwebui",
                ProviderExecutorConfig::openai("http://127.0.0.1:7860"),
            ),
            (
                "comfyui",
                ProviderExecutorConfig::openai("http://127.0.0.1:8188"),
            ),
            (
                "lm-studio",
                ProviderExecutorConfig::openai("http://localhost:1234/v1"),
            ),
            (
                "vllm",
                ProviderExecutorConfig::openai("http://localhost:8000/v1"),
            ),
            (
                "trae",
                ProviderExecutorConfig::openai("https://core-normal.trae.ai/api/remote/v1"),
            ),
            (
                "kimchi",
                ProviderExecutorConfig::openai("https://llm.kimchi.dev/openai/v1"),
            ),
            (
                "huggingface",
                ProviderExecutorConfig::openai("https://api-inference.huggingface.co"),
            ),
            (
                "jina-ai",
                ProviderExecutorConfig::openai("https://api.jina.ai/v1"),
            ),
            (
                "linkup",
                ProviderExecutorConfig::openai("https://api.linkup.so/v1"),
            ),
            (
                "searxng",
                ProviderExecutorConfig::openai("http://localhost:8080"),
            ),
            (
                "youcom",
                ProviderExecutorConfig::openai("https://api.you.com/v1"),
            ),
            (
                "google-pse",
                ProviderExecutorConfig::openai("https://www.googleapis.com/customsearch/v1"),
            ),
            (
                "searchapi",
                ProviderExecutorConfig::openai("https://www.searchapi.io/api/v1"),
            ),
            (
                "firecrawl",
                ProviderExecutorConfig::openai("https://api.firecrawl.dev/v1"),
            ),
            (
                "topaz",
                ProviderExecutorConfig::openai("https://api.topazlabs.com/v1"),
            ),
            (
                "ollama",
                ProviderExecutorConfig::openai("https://ollama.com/v1"),
            ),
            (
                "inference-net",
                ProviderExecutorConfig::openai("https://api.inference.net/v1"),
            ),
            (
                "vercel-ai-gateway",
                ProviderExecutorConfig::openai("https://ai-gateway.vercel.sh/v1"),
            ),
            (
                "xiaomi-tokenplan",
                ProviderExecutorConfig::openai("https://token-plan-sgp.xiaomimimo.com/v1"),
            ),
            (
                "agentrouter",
                ProviderExecutorConfig::anthropic("https://agentrouter.org/v1"),
            ),
            (
                "aimlapi",
                ProviderExecutorConfig::openai("https://api.aimlapi.com/v1"),
            ),
            (
                "modal",
                ProviderExecutorConfig::openai("https://api.modal.com/v1"),
            ),
            (
                "reka",
                ProviderExecutorConfig::openai("https://api.reka.ai/v1"),
            ),
            (
                "nlpcloud",
                ProviderExecutorConfig::openai("https://api.nlpcloud.io/v1/gpu"),
            ),
            (
                "bazaarlink",
                ProviderExecutorConfig::openai("https://bazaarlink.ai/api/v1"),
            ),
            (
                "completions",
                ProviderExecutorConfig::openai("https://completions.me/api/v1"),
            ),
            (
                "enally",
                ProviderExecutorConfig::openai("https://ai.enally.in/v1"),
            ),
            (
                "freetheai",
                ProviderExecutorConfig::openai("https://api.freetheai.xyz/v1"),
            ),
            (
                "llm7",
                ProviderExecutorConfig::openai("https://api.llm7.io/v1"),
            ),
            (
                "kluster",
                ProviderExecutorConfig::openai("https://api.kluster.ai/v1"),
            ),
            (
                "predibase",
                ProviderExecutorConfig::openai("https://serving.app.predibase.com/v1"),
            ),
            (
                "bytez",
                ProviderExecutorConfig::openai("https://api.bytez.com"),
            ),
            (
                "morph",
                ProviderExecutorConfig::openai("https://api.morphllm.com/v1"),
            ),
            (
                "longcat",
                ProviderExecutorConfig::openai("https://api.longcat.chat/openai/v1"),
            ),
            (
                "puter",
                ProviderExecutorConfig::openai("https://api.puter.com/puterai/openai/v1"),
            ),
            (
                "uncloseai",
                ProviderExecutorConfig::openai("https://hermes.ai.unturf.com/v1"),
            ),
            (
                "scaleway",
                ProviderExecutorConfig::openai("https://api.scaleway.ai/v1"),
            ),
            (
                "sambanova",
                ProviderExecutorConfig::openai("https://api.sambanova.ai/v1"),
            ),
            (
                "nscale",
                ProviderExecutorConfig::openai("https://inference.api.nscale.com/v1"),
            ),
            (
                "baseten",
                ProviderExecutorConfig::openai("https://inference.baseten.co/v1"),
            ),
            (
                "publicai",
                ProviderExecutorConfig::openai("https://api.publicai.co/v1"),
            ),
            (
                "nous-research",
                ProviderExecutorConfig::openai("https://inference-api.nousresearch.com/v1"),
            ),
            (
                "glhf",
                ProviderExecutorConfig::openai("https://glhf.chat/api/openai/v1"),
            ),
            (
                "github-models",
                ProviderExecutorConfig::openai("https://models.github.ai/inference"),
            ),
            (
                "hackclub",
                ProviderExecutorConfig::openai("https://ai.hackclub.com/proxy/v1"),
            ),
            // ── Enterprise & Cloud ──────────────────────────────────────
            (
                "databricks",
                ProviderExecutorConfig::openai("https://adb-0000000000000000.0.azuredatabricks.net/serving-endpoints"),
            ),
            (
                "snowflake",
                ProviderExecutorConfig::openai("https://{account}.snowflakecomputing.com/api/v2"),
            ),
            (
                "heroku",
                ProviderExecutorConfig::openai("https://us.inference.heroku.com/v1"),
            ),
            (
                "lambda-ai",
                ProviderExecutorConfig::openai("https://api.lambda.ai/v1"),
            ),
            (
                "ovhcloud",
                ProviderExecutorConfig::openai("https://oai.endpoints.kepler.ai.cloud.ovh.net/v1"),
            ),
            (
                "wandb",
                ProviderExecutorConfig::openai("https://api.inference.wandb.ai/v1"),
            ),
            // ── Gateway / Bridge ──────────────────────────────────────────
            (
                "kilo-gateway",
                ProviderExecutorConfig::openai("https://api.kilo.ai/api/gateway"),
            ),
            (
                "v0-vercel",
                ProviderExecutorConfig::openai("https://api.v0.dev/v1"),
            ),
            // ── Regional CN providers ─────────────────────────────────────
            (
                "alibaba",
                ProviderExecutorConfig::openai("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
            ),
            (
                "alibaba-cn",
                ProviderExecutorConfig::openai("https://dashscope.aliyuncs.com/compatible-mode/v1"),
            ),
            (
                "moonshot",
                ProviderExecutorConfig::openai("https://api.moonshot.ai/v1"),
            ),
            (
                "qianfan",
                ProviderExecutorConfig::openai("https://qianfan.baidubce.com/v2"),
            ),
            (
                "volcengine",
                ProviderExecutorConfig::openai("https://ark.cn-beijing.volces.com/api/v3"),
            ),
            (
                "zai",
                ProviderExecutorConfig::claude_compatible("https://api.z.ai/api/anthropic/v1"),
            ),
            // ── Regional international ────────────────────────────────────
            (
                "gigachat",
                ProviderExecutorConfig::openai("https://gigachat.devices.sberbank.ru/api/v1"),
            ),
            (
                "upstage",
                ProviderExecutorConfig::openai("https://api.upstage.ai/v1"),
            ),
            (
                "maritalk",
                ProviderExecutorConfig::openai("https://chat.maritaca.ai/api"),
            ),
            // ── Inference APIs (openai-compatible) ────────────────────────
            (
                "venice",
                ProviderExecutorConfig::openai("https://api.venice.ai/api/v1"),
            ),
            (
                "nanobanana",
                ProviderExecutorConfig::openai("https://api.nanobananaapi.ai/v1"),
            ),
            (
                "featherless-ai",
                ProviderExecutorConfig::openai("https://api.featherless.ai/v1"),
            ),
            (
                "friendliai",
                ProviderExecutorConfig::openai("https://api.friendli.ai/serverless/v1"),
            ),
            (
                "galadriel",
                ProviderExecutorConfig::openai("https://api.galadriel.ai/v1"),
            ),
            (
                "llamagate",
                ProviderExecutorConfig::openai("https://llamagate.ai/v1"),
            ),
            (
                "nanogpt",
                ProviderExecutorConfig::openai("https://nano-gpt.com/api/v1"),
            ),
            (
                "synthetic",
                ProviderExecutorConfig::openai("https://api.synthetic.new/openai/v1"),
            ),
            (
                "pollinations",
                ProviderExecutorConfig::openai("https://gen.pollinations.ai/v1"),
            ),
            (
                "meta-llama",
                ProviderExecutorConfig::openai("https://api.llama.com/compat/v1"),
            ),
            // ── Coding / CLI tool providers ───────────────────────────────
            (
                "opencode-zen",
                ProviderExecutorConfig::openai("https://opencode.ai/zen/v1"),
            ),
            (
                "kimi-coding-apikey",
                ProviderExecutorConfig::claude_compatible("https://api.kimi.com/coding/v1"),
            ),
            (
                "devin-cli",
                ProviderExecutorConfig::openai("devin://acp/stdio"),
            ),
            (
                "windsurf",
                ProviderExecutorConfig::openai("https://server.self-serve.windsurf.com"),
            ),
            (
                "crof",
                ProviderExecutorConfig::openai("https://crof.ai/v1"),
            ),
            // ── Media providers ───────────────────────────────────────────
            (
                "haiper",
                ProviderExecutorConfig::openai("https://api.haiper.ai/v1"),
            ),
            (
                "leonardo",
                ProviderExecutorConfig::openai("https://cloud.leonardo.ai/api/rest/v1"),
            ),
            (
                "ideogram",
                ProviderExecutorConfig::openai("https://api.ideogram.ai"),
            ),
            (
                "suno",
                ProviderExecutorConfig::openai("https://studio-api.suno.ai/api"),
            ),
            (
                "udio",
                ProviderExecutorConfig::openai("https://www.udio.com/api"),
            ),
            // ── Web / Chat providers ──────────────────────────────────────
            (
                "chatgpt-web",
                ProviderExecutorConfig::openai("https://chatgpt.com/backend-api"),
            ),
            (
                "gemini-web",
                ProviderExecutorConfig::gemini("https://gemini.google.com/app"),
            ),
            (
                "muse-spark-web",
                ProviderExecutorConfig::openai("https://www.meta.ai/api"),
            ),
            (
                "custom",
                ProviderExecutorConfig::openai("https://api.openai.com/v1"),
            ),
        ])
    });

pub fn get_provider_config(provider: &str) -> Option<ProviderExecutorConfig> {
    PROVIDER_REGISTRY.get(provider).cloned()
}

pub fn is_supported_provider(provider: &str) -> bool {
    PROVIDER_REGISTRY.contains_key(provider)
}

pub fn all_providers() -> Vec<&'static str> {
    PROVIDER_REGISTRY.keys().cloned().collect()
}

pub fn get_oauth_providers() -> Vec<&'static str> {
    vec!["claude", "codex", "github", "gitlab", "kiro", "cursor"]
}

pub fn get_api_key_providers() -> Vec<&'static str> {
    vec![
        "openai",
        "anthropic",
        "deepseek",
        "groq",
        "together",
        "fireworks",
        "cerebras",
        "mistral",
        "cohere",
        "perplexity",
        "xai",
        "nvidia",
        "cloudflare-ai",
        "blackbox",
        "ai21",
        "lepton",
        "novita",
        "deepinfra",
        "focus",
        "navigators",
        "polyscale",
        "rampt",
        "skip",
        "unbound",
        "workers",
        "zerogpt",
        "nebius",
        "siliconflow",
        "hyperbolic",
        "chutes",
        "glm",
        "kimi",
        "kimi-coding",
        "minimax",
        "minimax-cn",
        "gemini",
        "codebuddy",
        "kilocode",
        "alicode",
        "alicode-intl",
        "volcengine-ark",
        "byteplus",
        "openrouter",
        "ollama-cloud",
        "ollama-local",
        "stability-ai",
        "replicate",
        "cline",
        "opencode-go",
        "glm-cn",
        "vertex-partner",
        "azure",
        "grok",
        "elevenlabs",
        "cartesia",
        "playht",
        "deepgram",
        "google-tts",
        "edge-tts",
        "openai-embedding",
        "cohere-embedding",
        "voyage-ai",
        "dalle",
        "stable-diffusion",
        "tavily",
        "brave-search",
        "serper",
        "exa",
        "antigravity",
        "grok-web",
        "perplexity-web",
        "xiaomi-mimo",
        "assemblyai",
        "black-forest-labs",
        "fal-ai",
        "runwayml",
        "sdwebui",
        "comfyui",
        "lm-studio",
        "vllm",
        "codebuddy-cn",
        "gemini-cli",
        "iflow",
        "mimo-free",
        "qoder",
        "qwen",
        "trae",
        "kimchi",
        "huggingface",
        "jina-ai",
        "linkup",
        "searxng",
        "youcom",
        "google-pse",
        "searchapi",
        "firecrawl",
        "topaz",
        "ollama",
        "inference-net",
        "vercel-ai-gateway",
        "xiaomi-tokenplan",
        "agentrouter",
        "aimlapi",
        "modal",
        "reka",
        "nlpcloud",
        "bazaarlink",
        "completions",
        "enally",
        "freetheai",
        "llm7",
        "kluster",
        "predibase",
        "bytez",
        "morph",
        "longcat",
        "puter",
        "uncloseai",
        "scaleway",
        "sambanova",
        "nscale",
        "baseten",
        "publicai",
        "nous-research",
        "glhf",
        "github-models",
        // ── Newly added (enterprise, cloud, gateway, regional CN) ──
        "databricks",
        "snowflake",
        "heroku",
        "lambda-ai",
        "ovhcloud",
        "wandb",
        "kilo-gateway",
        "v0-vercel",
        "alibaba",
        "alibaba-cn",
        "moonshot",
        "qianfan",
        "volcengine",
        "zai",
        "gigachat",
        "upstage",
        "maritalk",
        "venice",
        "nanobanana",
        "featherless-ai",
        "friendliai",
        "galadriel",
        "llamagate",
        "nanogpt",
        "synthetic",
        "pollinations",
        "meta-llama",
        "opencode-zen",
        "kimi-coding-apikey",
        "devin-cli",
        "windsurf",
        "crof",
        "haiper",
        "leonardo",
        "ideogram",
        "suno",
        "udio",
        "chatgpt-web",
        "gemini-web",
        "muse-spark-web",
    ]
}

pub fn get_free_providers() -> Vec<&'static str> {
    vec![
        "kiro",
        "opencode",
        "vertex",
        "openrouter",
        "nvidia",
        "ollama-cloud",
        "gemini",
        "hackclub",
    ]
}

pub fn get_specialty_providers() -> Vec<&'static str> {
    vec![
        "claude",
        "codex",
        "github",
        "kiro",
        "vertex",
        "cursor",
        "ollama",
        "grok",
        "azure",
        "qwen",
        "iflow",
        "gemini-cli",
        "opencode",
        "opencode-go",
        "qoder",
        "commandcode",
        "antigravity",
        "grok-web",
        "perplexity-web",
        "xiaomi-mimo",
    ]
}
