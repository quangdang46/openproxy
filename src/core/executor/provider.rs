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
///
/// Used to categorize log entries attached to provider execution requests
/// for debugging and monitoring purposes.
#[derive(Debug, Clone)]
pub enum LogLevel {
    /// Detailed diagnostic information for troubleshooting.
    Debug,
    /// General operational information about request flow.
    Info,
    /// Potential issues that don't prevent request completion.
    Warn,
    /// Errors that may affect request outcome but don't halt execution.
    Error,
}

/// A single log entry attached to a request.
///
/// Log entries provide structured, request-scoped logging that travels with
/// the execution request through the provider executor. This enables detailed
/// per-request debugging without relying on global log correlation.
///
/// # Example
///
/// ```rust
/// let log_entry = LogEntry {
///     level: LogLevel::Info,
///     message: "Sending request to OpenAI with model gpt-4o".to_string(),
/// };
/// ```
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// The severity level of this log entry.
    pub level: LogLevel,
    /// Human-readable log message describing the event.
    pub message: String,
}

/// Options that control proxy/retry behaviour for a single execution.
#[derive(Debug, Clone, Default)]
pub struct ProxyOptions {
    /// URL index to try (for round-robin / fallback rotation).
    pub url_index: Option<usize>,
}

/// Request to execute against a single provider/model.
///
/// Contains all the information needed to execute a single request against
/// a specific provider and model. This is the primary input type for all
/// provider executors.
///
/// # Fields
///
/// * `model` - The model identifier string (e.g., "gpt-4o", "claude-3-haiku")
/// * `body` - The JSON request body to send to the provider
/// * `stream` - Whether to use SSE streaming for the response
/// * `credentials` - Provider connection details including API key, etc.
/// * `proxy` - Optional proxy target for routing the request
/// * `signal` - Optional cancellation signal for aborting in-flight requests
/// * `log` - Optional request-scoped log entries for debugging
/// * `proxy_options` - Optional options controlling proxy/retry behavior
pub struct ProviderExecutionRequest {
    /// The model identifier string (e.g., "gpt-4o", "claude-3-haiku")
    pub model: String,
    /// The JSON request body to send to the provider
    pub body: Value,
    /// Whether to use SSE streaming for the response
    pub stream: bool,
    /// Provider connection details including API key, access token, etc.
    pub credentials: ProviderConnection,
    /// Optional proxy target for routing the request through a proxy
    pub proxy: Option<ProxyTarget>,
    /// Signal for aborting an in-flight request.
    pub signal: Option<CancellationToken>,
    /// Request-scoped log entries for debugging and monitoring.
    pub log: Option<Vec<LogEntry>>,
    /// Options controlling proxy/retry behaviour.
    pub proxy_options: Option<ProxyOptions>,
}

/// Response from a single provider execution.
///
/// Contains the upstream response along with metadata about how the request
/// was handled. This is the primary output type for all provider executors.
///
/// # Fields
///
/// * `response` - The upstream HTTP response (reqwest or hyper response)
/// * `url` - The actual URL the request was sent to
/// * `headers` - HTTP response headers from the upstream provider
/// * `transformed_body` - The request body after format transformation
/// * `transport` - The transport mechanism used (reqwest, hyper, etc.)
pub struct ProviderExecutionResponse {
    /// The upstream HTTP response (reqwest or hyper response)
    pub response: UpstreamResponse,
    /// The actual URL the request was sent to
    pub url: String,
    /// HTTP response headers from the upstream provider
    pub headers: HeaderMap,
    /// The request body after format transformation
    pub transformed_body: Value,
    /// The transport mechanism used (reqwest, hyper, etc.)
    pub transport: TransportKind,
}

/// Error type for provider execution failures.
///
/// Covers all possible errors that can occur when executing a request against
/// a provider, from unsupported providers to network errors.
#[derive(Debug)]
pub enum ProviderExecutorError {
    /// The requested provider is not supported by this proxy.
    UnsupportedProvider(String),
    /// Required credentials are missing or invalid for the provider.
    MissingCredentials(String),
    /// An HTTP header value is invalid.
    InvalidHeader(String),
    /// The request URI is malformed.
    InvalidUri(hyper::http::uri::InvalidUri),
    /// The HTTP request could not be constructed.
    InvalidRequest(hyper::http::Error),
    /// JSON serialization failed.
    Serialize(serde_json::Error),
    /// Failed to initialize the HTTP client.
    HyperClientInit(std::io::Error),
    /// HTTP transport layer error (hyper).
    Hyper(hyper_util::client::legacy::Error),
    /// Network-level error from reqwest.
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

/// Configuration for a provider executor.
///
/// Contains all the information needed to configure how requests are sent to
/// a specific provider, including endpoint URLs, authentication method, and
/// protocol-specific settings.
///
/// # Fields
///
/// * `base_url` - The base URL for the provider's API (e.g., "https://api.openai.com")
/// * `format` - The protocol format this provider uses (OpenAI, Anthropic, etc.)
/// * `api_key_header` - The HTTP header name used for API key authentication
/// * `default_headers` - Default headers to include with every request
/// * `stream_path` - Path appended to base_url for streaming endpoints
/// * `chat_path` - Path appended to base_url for chat/completion endpoints
#[derive(Debug, Clone)]
pub struct ProviderExecutorConfig {
    /// The base URL for the provider's API (e.g., "https://api.openai.com")
    pub base_url: String,
    /// The protocol format this provider uses (OpenAI, Anthropic, etc.)
    pub format: ProviderFormat,
    /// The HTTP header name used for API key authentication
    pub api_key_header: &'static str,
    /// Default headers to include with every request
    pub default_headers: Vec<(String, String)>,
    /// Path appended to base_url for streaming endpoints
    pub stream_path: String,
    /// Path appended to base_url for chat/completion endpoints
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

/// Core trait that all provider executors must implement.
///
/// This trait defines the interface for executing requests against various
/// AI providers (OpenAI, Anthropic, Gemini, etc.). Each provider type
/// implements this trait to handle protocol-specific request/response
/// transformation, authentication, and error handling.
///
/// # Type Requirements
///
/// * `Send` - Safe to transfer between threads
/// * `Sync` - Safe to share references between threads
///
/// # Implementations
///
/// See the executors/ directory for specific implementations:
/// * `default::UnifiedExecutor` - Handles most OpenAI-compatible providers
/// * `anthropic::AzureExecutor` - Special handling for Azure OpenAI
/// * `gemini_cli::GeminiCliExecutor` - Gemini via CLI tool
/// * And many others in the executors/ directory
#[async_trait]
pub trait ProviderExecutor: Send + Sync {
    /// Returns the name of the provider this executor handles.
    ///
    /// This should match the provider identifier used in configuration
    /// and model strings (e.g., "openai", "anthropic", "gemini").
    fn provider_name(&self) -> &str;

    /// Execute a request against the provider.
    ///
    /// This is the main entry point for provider execution. It handles:
    /// * Building the request URL and headers
    /// * Transforming the request body according to provider format
    /// * Sending the HTTP request
    /// * Receiving and transforming the response
    /// * Handling provider-specific error cases
    ///
    /// # Arguments
    ///
    /// * `request` - Contains all information needed for the execution
    ///
    /// # Returns
    ///
    /// * `Ok(ProviderExecutionResponse)` - Successful execution with response
    /// * `Err(ProviderExecutorError)` - Execution failed with specific error
    async fn execute(
        &self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionResponse, ProviderExecutorError>;

    /// Build the URL for a request to this provider.
    ///
    /// Constructs the full endpoint URL based on the provider's base URL,
    /// model name, and whether streaming is requested.
    ///
    /// # Arguments
    ///
    /// * `model` - The model identifier (e.g., "gpt-4o")
    /// * `stream` - Whether this is a streaming request
    /// * `url_index` - Index for round-robin URL selection (if multiple URLs)
    /// * `credentials` - Optional credentials for authentication
    ///
    /// # Returns
    ///
    /// * String - The complete URL to request
    fn build_url(
        &self,
        model: &str,
        stream: bool,
        url_index: Option<usize>,
        credentials: Option<&ProviderConnection>,
    ) -> String;

    /// Build HTTP headers for a request to this provider.
    ///
    /// Constructs the appropriate headers including authentication,
    /// content type, and provider-specific headers.
    ///
    /// # Arguments
    ///
    /// * `credentials` - Provider connection containing API key/token
    /// * `stream` - Whether this is a streaming request (affects Accept header)
    ///
    /// # Returns
    ///
    /// * `Ok(HeaderMap)` - Successfully built headers
    /// * `Err(ProviderExecutorError)` - Failed to build headers (invalid credentials, etc.)
    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, ProviderExecutorError>;

    /// Transform the request body before sending to the provider.
    ///
    /// Performs provider-specific transformation of the request body,
    /// such as converting between OpenAI and Anthropic message formats,
    /// handling developer roles, or applying JSON schema fallbacks.
    ///
    /// # Arguments
    ///
    /// * `body` - The original request body
    /// * `_model` - The model identifier (unused in base implementation)
    /// * `_stream` - Whether this is a streaming request (unused in base)
    /// * `_credentials` - Provider credentials (unused in base implementation)
    ///
    /// # Returns
    ///
    /// * Value - The transformed request body ready to send
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
    ///
    /// # Arguments
    ///
    /// * `credentials` - Current credentials that need refreshing
    ///
    /// # Returns
    ///
    /// * `Some(ProviderConnection)` - Updated credentials with fresh token
    /// * `None` - Provider doesn't support refresh or refresh failed
    async fn refresh_credentials(
        &self,
        credentials: &ProviderConnection,
    ) -> Option<ProviderConnection> {
        let _ = credentials;
        None
    }

    /// Returns `true` if the credentials are expired (or close to expiring)
    /// and should be refreshed before the next request.
    ///
    /// # Arguments
    ///
    /// * `credentials` - Credentials to check for expiration
    ///
    /// # Returns
    ///
    /// * `bool` - True if credentials need refresh, false otherwise
    fn needs_refresh(&self, credentials: &ProviderConnection) -> bool {
        let _ = credentials;
        false
    }
}

/// Unified executor that handles most OpenAI-compatible providers.
///
/// This is the default executor implementation that can handle any provider
/// using OpenAI-compatible, Anthropic, Gemini, or similar APIs. It uses
/// a configuration-driven approach rather than provider-specific code.
///
/// # Fields
///
/// * `provider` - The provider name (e.g., "openai", "anthropic")
/// * `config` - The provider configuration (URLs, format, headers)
/// * `pool` - Shared HTTP client pool for connection reuse
pub struct UnifiedExecutor {
    /// The provider name (e.g., "openai", "anthropic")
    provider: String,
    /// The provider configuration (URLs, format, headers)
    config: ProviderExecutorConfig,
    /// Shared HTTP client pool for connection reuse
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

    /// Append the API key as a query param for providers that cannot use a
    /// header. The separator depends on whether `build_url` already emitted a
    /// query string (`:streamGenerateContent?alt=sse` does, `:generateContent`
    /// does not — appending `&key=` to the latter yields a 404, verified live).
    ///
    /// Gemini does not use this: it authenticates with the `x-goog-api-key`
    /// header, which keeps the key out of logged URLs.
    pub fn build_url_with_api_key(
        &self,
        model: &str,
        stream: bool,
        api_key: Option<&str>,
    ) -> String {
        let base_url = self.build_url(model, stream, None, None);
        match api_key {
            Some(key) if base_url.contains('?') => format!("{base_url}&key={key}"),
            Some(key) => format!("{base_url}?key={key}"),
            None => base_url,
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
        let url_index = request.proxy_options.as_ref().and_then(|o| o.url_index);
        let url = self.build_url(
            &request.model,
            request.stream,
            url_index,
            Some(&request.credentials),
        );
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
