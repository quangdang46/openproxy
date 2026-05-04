use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::Incoming as HyperIncoming;
use hyper::http;
use hyper::http::uri::InvalidUri;
use hyper::{Request as HyperRequest, Response as HyperResponse, Uri};
use once_cell::sync::Lazy;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::ClientPool;

static PROVIDER_CONFIGS: Lazy<BTreeMap<&'static str, ProviderConfig>> = Lazy::new(|| {
    BTreeMap::from([
        (
            "openai",
            ProviderConfig::openai("https://api.openai.com/v1/chat/completions"),
        ),
        (
            "openrouter",
            ProviderConfig::openai("https://openrouter.ai/api/v1/chat/completions")
                .with_header("HTTP-Referer", "https://endpoint-proxy.local")
                .with_header("X-Title", "Endpoint Proxy"),
        ),
        (
            "anthropic",
            ProviderConfig::anthropic("https://api.anthropic.com/v1/messages"),
        ),
        (
            "claude",
            ProviderConfig::anthropic("https://api.anthropic.com/v1/messages"),
        ),
        (
            "gemini",
            ProviderConfig::gemini("https://generativelanguage.googleapis.com/v1beta/models"),
        ),
        (
            "glm",
            ProviderConfig::claude_compatible("https://api.z.ai/api/anthropic/v1/messages"),
        ),
        (
            "kimi",
            ProviderConfig::claude_compatible("https://api.kimi.com/coding/v1/messages"),
        ),
        (
            "minimax",
            ProviderConfig::claude_compatible("https://api.minimax.io/anthropic/v1/messages"),
        ),
        (
            "minimax-cn",
            ProviderConfig::claude_compatible("https://api.minimaxi.com/anthropic/v1/messages"),
        ),
        (
            "deepseek",
            ProviderConfig::openai("https://api.deepseek.com/chat/completions"),
        ),
        (
            "groq",
            ProviderConfig::openai("https://api.groq.com/openai/v1/chat/completions"),
        ),
        (
            "xai",
            ProviderConfig::openai("https://api.x.ai/v1/chat/completions"),
        ),
        (
            "mistral",
            ProviderConfig::openai("https://api.mistral.ai/v1/chat/completions"),
        ),
        (
            "together",
            ProviderConfig::openai("https://api.together.xyz/v1/chat/completions"),
        ),
        (
            "fireworks",
            ProviderConfig::openai("https://api.fireworks.ai/inference/v1/chat/completions"),
        ),
        (
            "cerebras",
            ProviderConfig::openai("https://api.cerebras.ai/v1/chat/completions"),
        ),
        (
            "cohere",
            ProviderConfig::openai("https://api.cohere.ai/v1/chat/completions"),
        ),
        (
            "nebius",
            ProviderConfig::openai("https://api.studio.nebius.ai/v1/chat/completions"),
        ),
        (
            "siliconflow",
            ProviderConfig::openai("https://api.siliconflow.cn/v1/chat/completions"),
        ),
        (
            "hyperbolic",
            ProviderConfig::openai("https://api.hyperbolic.xyz/v1/chat/completions"),
        ),
        (
            "perplexity",
            ProviderConfig::openai("https://api.perplexity.ai/chat/completions"),
        ),
        (
            "nanobanana",
            ProviderConfig::openai("https://api.nanobananaapi.ai/v1/chat/completions"),
        ),
        (
            "chutes",
            ProviderConfig::openai("https://llm.chutes.ai/v1/chat/completions"),
        ),
        (
            "gitlab",
            ProviderConfig::openai("https://gitlab.com/api/v4/chat/completions"),
        ),
        (
            "codebuddy",
            ProviderConfig::openai("https://copilot.tencent.com/v1/chat/completions"),
        ),
        (
            "kilocode",
            ProviderConfig::openai("https://api.kilo.ai/api/openrouter/chat/completions"),
        ),
        (
            "cline",
            ProviderConfig::openai("https://api.cline.bot/api/v1/chat/completions")
                .with_header("HTTP-Referer", "https://cline.bot")
                .with_header("X-Title", "Cline"),
        ),
        (
            "opencode-go",
            ProviderConfig::openai("https://opencode.ai/zen/go/v1"),
        ),
        (
            "glm-cn",
            ProviderConfig::openai("https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"),
        ),
        (
            "alicode",
            ProviderConfig::openai("https://coding.dashscope.aliyuncs.com/v1/chat/completions"),
        ),
        (
            "alicode-intl",
            ProviderConfig::openai(
                "https://coding-intl.dashscope.aliyuncs.com/v1/chat/completions",
            ),
        ),
        (
            "volcengine-ark",
            ProviderConfig::openai(
                "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions",
            ),
        ),
        (
            "byteplus",
            ProviderConfig::openai(
                "https://ark.ap-southeast.bytepluses.com/api/coding/v3/chat/completions",
            ),
        ),
        (
            "nvidia",
            ProviderConfig::openai("https://integrate.api.nvidia.com/v1/chat/completions"),
        ),
        (
            "cloudflare-ai",
            ProviderConfig::openai(
                "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/v1/chat/completions",
            ),
        ),
        (
            "azure",
            ProviderConfig::openai("https://{resource}.openai.azure.com/v1/chat/completions"),
        ),
        (
            "blackbox",
            ProviderConfig::openai("https://api.blackbox.ai/api/chat/completions"),
        ),
        (
            "ollama-cloud",
            ProviderConfig::openai("https://api.ollama.com/v1/chat/completions"),
        ),
        (
            "vertex",
            ProviderConfig::gemini("https://generativelanguage.googleapis.com/v1beta/models"),
        ),
        (
            "vertex-partner",
            ProviderConfig::openai("https://{project}.{location}.掉ax.com/v1/chat/completions"),
        ),
        (
            "ollama-local",
            ProviderConfig::openai("http://localhost:11434/v1/chat/completions"),
        ),
    ])
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub base_url: String,
    pub format: String,
    pub default_headers: Vec<(String, String)>,
}

impl ProviderConfig {
    fn openai(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: "openai".into(),
            default_headers: Vec::new(),
        }
    }

    fn gemini(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: "gemini".into(),
            default_headers: Vec::new(),
        }
    }

    fn anthropic(base_url: &str) -> Self {
        Self::openai(base_url)
            .with_header("anthropic-version", "2023-06-01")
            .with_header(
                "anthropic-beta",
                "claude-code-20250219,interleaved-thinking-2025-05-14",
            )
    }

    fn claude_compatible(base_url: &str) -> Self {
        Self::anthropic(base_url)
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.default_headers
            .push((name.to_string(), value.to_string()));
        self
    }
}

pub struct DefaultExecutor {
    provider: String,
    config: ProviderConfig,
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug, Clone)]
pub struct ExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct ExecutionResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Reqwest,
    Hyper,
}

pub enum UpstreamResponse {
    Reqwest(reqwest::Response),
    Hyper(HyperResponse<HyperIncoming>),
}

impl UpstreamResponse {
    pub fn status(&self) -> http::StatusCode {
        match self {
            Self::Reqwest(response) => response.status(),
            Self::Hyper(response) => response.status(),
        }
    }

    pub fn headers(&self) -> &HeaderMap {
        match self {
            Self::Reqwest(response) => response.headers(),
            Self::Hyper(response) => response.headers(),
        }
    }
}

#[derive(Debug)]
pub enum ExecutorError {
    UnsupportedProvider(String),
    MissingCredentials(String),
    MissingProviderSpecificData(String, &'static str),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    InvalidUri(InvalidUri),
    InvalidRequest(http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
}

impl From<reqwest::Error> for ExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for ExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for ExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<http::Error> for ExecutorError {
    fn from(error: http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for ExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<io::Error> for ExecutorError {
    fn from(error: io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for ExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl DefaultExecutor {
    pub fn new(
        provider: impl Into<String>,
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, ExecutorError> {
        let provider = provider.into();
        let config = if let Some(node) = &provider_node {
            if node.r#type == "openai-compatible" || node.r#type == "anthropic-compatible" {
                ProviderConfig::openai("")
            } else {
                PROVIDER_CONFIGS
                    .get(provider.as_str())
                    .cloned()
                    .ok_or_else(|| ExecutorError::UnsupportedProvider(provider.clone()))?
            }
        } else {
            PROVIDER_CONFIGS
                .get(provider.as_str())
                .cloned()
                .ok_or_else(|| ExecutorError::UnsupportedProvider(provider.clone()))?
        };

        Ok(Self {
            provider,
            config,
            pool,
            provider_node,
        })
    }

    pub fn build_url(
        &self,
        model: &str,
        stream: bool,
        credentials: &ProviderConnection,
    ) -> Result<String, ExecutorError> {
        if let Some(node) = &self.provider_node {
            if node.r#type == "openai-compatible" {
                let base_url = compatible_value(credentials.provider_specific_data.get("baseUrl"))
                    .or_else(|| non_empty_option(node.base_url.as_deref()))
                    .unwrap_or("https://api.openai.com/v1");
                let api_type = compatible_value(credentials.provider_specific_data.get("apiType"))
                    .or_else(|| non_empty_option(node.api_type.as_deref()))
                    .unwrap_or("chat");
                let normalized = base_url.trim_end_matches('/');
                let path = if api_type == "responses" {
                    "/responses"
                } else {
                    "/chat/completions"
                };
                return Ok(format!("{normalized}{path}"));
            }

            if node.r#type == "anthropic-compatible" {
                let base_url = compatible_value(credentials.provider_specific_data.get("baseUrl"))
                    .or_else(|| non_empty_option(node.base_url.as_deref()))
                    .unwrap_or("https://api.anthropic.com/v1");
                return Ok(format!("{}/messages", base_url.trim_end_matches('/')));
            }
        }

        if self.provider == "gemini" {
            let action = if stream {
                "streamGenerateContent?alt=sse"
            } else {
                "generateContent"
            };
            return Ok(format!("{}/{model}:{action}", self.config.base_url));
        }

        if self.provider == "opencode-go" {
            let path = if opencode_go_uses_claude_format(model) {
                "messages"
            } else {
                "chat/completions"
            };
            return Ok(format!(
                "{}/{}",
                self.config.base_url.trim_end_matches('/'),
                path
            ));
        }

        if self.config.base_url.contains("{accountId}") {
            let account_id =
                compatible_value(credentials.provider_specific_data.get("accountId")).ok_or(
                    ExecutorError::MissingProviderSpecificData(self.provider.clone(), "accountId"),
                )?;
            return Ok(self.config.base_url.replace("{accountId}", account_id));
        }

        if matches!(
            self.provider.as_str(),
            "claude" | "glm" | "kimi" | "minimax" | "minimax-cn" | "kimi-coding"
        ) {
            return Ok(format!("{}?beta=true", self.config.base_url));
        }

        Ok(self.config.base_url.clone())
    }

    pub fn build_headers(
        &self,
        model: &str,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, ExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        for (name, value) in &self.config.default_headers {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .expect("static header name"),
                HeaderValue::from_str(value)?,
            );
        }

        let is_anthropic_compatible = self
            .provider_node
            .as_ref()
            .is_some_and(|node| node.r#type == "anthropic-compatible");

        if self.provider == "gemini" {
            if let Some(api_key) = credentials.api_key.as_deref() {
                headers.insert("x-goog-api-key", HeaderValue::from_str(api_key)?);
            } else if let Some(access_token) = credentials.access_token.as_deref() {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {access_token}"))?,
                );
            } else {
                return Err(ExecutorError::MissingCredentials(self.provider.clone()));
            }
        } else if self.provider == "anthropic" {
            if let Some(api_key) = credentials.api_key.as_deref() {
                headers.insert("x-api-key", HeaderValue::from_str(api_key)?);
            } else if let Some(access_token) = credentials.access_token.as_deref() {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {access_token}"))?,
                );
            } else {
                return Err(ExecutorError::MissingCredentials(self.provider.clone()));
            }
        } else if self.provider == "opencode-go" && opencode_go_uses_claude_format(model) {
            let token = credentials
                .api_key
                .as_deref()
                .or(credentials.access_token.as_deref())
                .ok_or_else(|| ExecutorError::MissingCredentials(self.provider.clone()))?;
            headers.insert("x-api-key", HeaderValue::from_str(token)?);
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        } else if is_anthropic_compatible {
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            if let Some(api_key) = credentials.api_key.as_deref() {
                headers.insert("x-api-key", HeaderValue::from_str(api_key)?);
            } else if let Some(access_token) = credentials.access_token.as_deref() {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {access_token}"))?,
                );
            } else {
                return Err(ExecutorError::MissingCredentials(self.provider.clone()));
            }
        } else {
            let token = credentials
                .api_key
                .as_deref()
                .or(credentials.access_token.as_deref())
                .ok_or_else(|| ExecutorError::MissingCredentials(self.provider.clone()))?;

            if matches!(
                self.provider.as_str(),
                "glm" | "kimi" | "minimax" | "minimax-cn"
            ) {
                headers.insert("x-api-key", HeaderValue::from_str(token)?);
            } else {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}"))?,
                );
            }

            if self.provider == "kilocode" {
                if let Some(org_id) =
                    compatible_value(credentials.provider_specific_data.get("orgId"))
                {
                    headers.insert("x-kilocode-organizationid", HeaderValue::from_str(org_id)?);
                }
            }
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        Ok(headers)
    }

    pub fn transform_request(&self, body: &Value) -> Value {
        body.clone()
    }

    pub async fn execute(
        &self,
        request: ExecutionRequest,
    ) -> Result<ExecutionResponse, ExecutorError> {
        let url = self.build_url(&request.model, request.stream, &request.credentials)?;
        let headers = self.build_headers(&request.model, &request.credentials, request.stream)?;
        let transformed_body = self.transform_request(&request.body);
        if self.use_hyper_transport(&request, &url) {
            return self.execute_via_hyper(url, headers, transformed_body).await;
        }

        self.execute_via_reqwest(url, headers, transformed_body, request.proxy)
            .await
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn use_hyper_transport(&self, request: &ExecutionRequest, url: &str) -> bool {
        request.proxy.is_none()
            && url
                .split('?')
                .next()
                .is_some_and(|path| path.ends_with("/chat/completions"))
    }

    async fn execute_via_reqwest(
        &self,
        url: String,
        headers: HeaderMap,
        transformed_body: Value,
        proxy: Option<ProxyTarget>,
    ) -> Result<ExecutionResponse, ExecutorError> {
        let client = self.pool.get(&self.provider, proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(ExecutionResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    async fn execute_via_hyper(
        &self,
        url: String,
        headers: HeaderMap,
        transformed_body: Value,
    ) -> Result<ExecutionResponse, ExecutorError> {
        let client = self.pool.get_hyper_direct(&self.provider)?;
        let uri: Uri = url.parse()?;
        let body_bytes = serde_json::to_vec(&transformed_body)?;
        let mut request = HyperRequest::post(uri).body(Full::new(body_bytes.into()))?;
        *request.headers_mut() = headers.clone();
        let response = client.request(request).await?;

        Ok(ExecutionResponse {
            response: UpstreamResponse::Hyper(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Hyper,
        })
    }
}

fn compatible_value(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn non_empty_option(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn opencode_go_uses_claude_format(model: &str) -> bool {
    matches!(model, "minimax-m2.5" | "minimax-m2.7")
}
