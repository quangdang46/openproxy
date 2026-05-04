use std::collections::BTreeMap;
use std::sync::Arc;

use once_cell::sync::Lazy;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

pub struct ApiKeyExecutor {
    pub provider: String,
    pub base_url: String,
    pub api_key_header: String,
    pool: Arc<ClientPool>,
}

#[derive(Debug)]
pub enum ApiKeyExecutorError {
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

impl From<reqwest::Error> for ApiKeyExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for ApiKeyExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error.to_string())
    }
}

impl From<hyper::http::uri::InvalidUri> for ApiKeyExecutorError {
    fn from(error: hyper::http::uri::InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for ApiKeyExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for ApiKeyExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for ApiKeyExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for ApiKeyExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl std::fmt::Display for ApiKeyExecutorError {
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

impl std::error::Error for ApiKeyExecutorError {}

pub struct ApiKeyExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct ApiKeyExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for ApiKeyExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

impl ApiKeyExecutor {
    pub fn new(provider: &str, base_url: &str, api_key_header: &str) -> Self {
        Self {
            provider: provider.to_string(),
            base_url: base_url.to_string(),
            api_key_header: api_key_header.to_string(),
            pool: Arc::new(ClientPool::default()),
        }
    }

    pub fn with_pool(
        provider: &str,
        base_url: &str,
        api_key_header: &str,
        pool: Arc<ClientPool>,
    ) -> Self {
        Self {
            provider: provider.to_string(),
            base_url: base_url.to_string(),
            api_key_header: api_key_header.to_string(),
            pool,
        }
    }

    pub async fn execute(
        &self,
        request: ApiKeyExecutionRequest,
    ) -> Result<ApiKeyExecutorResponse, ApiKeyExecutorError> {
        let url = self.build_url(&request.model, request.stream);
        let headers = self.build_headers(&request.credentials, request.stream)?;
        let transformed_body = self.transform_request(&request.body);

        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get(&self.provider, request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        Ok(ApiKeyExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    fn build_url(&self, _model: &str, stream: bool) -> String {
        let path = if stream {
            "/chat/completions"
        } else {
            "/chat/completions"
        };
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, ApiKeyExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let token = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .ok_or_else(|| ApiKeyExecutorError::MissingCredentials(self.provider.clone()))?;

        let header_name = reqwest::header::HeaderName::from_bytes(self.api_key_header.as_bytes())
            .map_err(|_| {
            ApiKeyExecutorError::InvalidHeader(format!(
                "invalid header name: {}",
                self.api_key_header
            ))
        })?;

        if self.api_key_header == "Authorization" {
            headers.insert(
                header_name,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        } else {
            headers.insert(header_name, HeaderValue::from_str(token)?);
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        Ok(headers)
    }

    fn transform_request(&self, body: &Value) -> Value {
        body.clone()
    }
}

static API_KEY_PROVIDERS: Lazy<BTreeMap<&'static str, (&'static str, &'static str)>> =
    Lazy::new(|| {
        BTreeMap::from([
            ("openai", ("https://api.openai.com/v1", "Authorization")),
            ("anthropic", ("https://api.anthropic.com/v1", "x-api-key")),
            ("deepseek", ("https://api.deepseek.com/v1", "Authorization")),
            ("mistral", ("https://api.mistral.ai/v1", "Authorization")),
            ("cohere", ("https://api.cohere.ai/v1", "Authorization")),
            (
                "fireworks",
                ("https://api.fireworks.ai/inference/v1", "Authorization"),
            ),
            ("together", ("https://api.together.xyz/v1", "Authorization")),
            ("perplexity", ("https://api.perplexity.ai", "Authorization")),
            (
                "nebius",
                ("https://api.studio.nebius.ai/v1", "Authorization"),
            ),
            ("xai", ("https://api.x.ai/v1", "Authorization")),
            ("ai21", ("https://api.ai21.com/v1", "Authorization")),
            (
                "stability-ai",
                ("https://api.stability.ai/v1", "Authorization"),
            ),
            (
                "replicate",
                ("https://api.replicate.com/v1", "Authorization"),
            ),
            ("lepton", ("https://api.lepton.ai/v1", "Authorization")),
            ("novita", ("https://api.novita.ai/v1", "Authorization")),
            (
                "deepinfra",
                ("https://api.deepinfra.com/v1", "Authorization"),
            ),
            ("focus", ("https://api.focusforce.io/v1", "Authorization")),
            (
                "navigators",
                ("https://api.navigators.ai/v1", "Authorization"),
            ),
            (
                "polyscale",
                ("https://api.polyscale.ai/v1", "Authorization"),
            ),
            ("rampt", ("https://api.rampt.ai/v1", "Authorization")),
            ("skip", ("https://api.skip.cloud/v1", "Authorization")),
            ("unbound", ("https://api.unbound.ai/v1", "Authorization")),
            ("workers", ("https://api.cloudflare.ai/v1", "Authorization")),
            ("zerogpt", ("https://api.zerogpt.com/v1", "Authorization")),
            ("groq", ("https://api.groq.com/openai/v1", "Authorization")),
            ("cerebras", ("https://api.cerebras.ai/v1", "Authorization")),
            (
                "siliconflow",
                ("https://api.siliconflow.cn/v1", "Authorization"),
            ),
            (
                "hyperbolic",
                ("https://api.hyperbolic.xyz/v1", "Authorization"),
            ),
            ("chutes", ("https://llm.chutes.ai/v1", "Authorization")),
            (
                "nanobanana",
                ("https://api.nanobananaapi.ai/v1", "Authorization"),
            ),
            (
                "nvidia",
                ("https://integrate.api.nvidia.com/v1", "Authorization"),
            ),
            (
                "cloudflare-ai",
                (
                    "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/v1",
                    "Authorization",
                ),
            ),
            ("blackbox", ("https://api.blackbox.ai/api", "Authorization")),
            (
                "gitlab",
                (
                    "https://gitlab.com/api/v4/chat/completions",
                    "Authorization",
                ),
            ),
            (
                "codebuddy",
                ("https://copilot.tencent.com/v1", "Authorization"),
            ),
            (
                "kilocode",
                ("https://api.kilo.ai/api/openrouter/v1", "Authorization"),
            ),
            (
                "alicode",
                ("https://coding.dashscope.aliyuncs.com/v1", "Authorization"),
            ),
            (
                "alicode-intl",
                (
                    "https://coding-intl.dashscope.aliyuncs.com/v1",
                    "Authorization",
                ),
            ),
            (
                "volcengine-ark",
                (
                    "https://ark.cn-beijing.volces.com/api/coding/v3",
                    "Authorization",
                ),
            ),
            (
                "byteplus",
                (
                    "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
                    "Authorization",
                ),
            ),
            (
                "ollama-cloud",
                ("https://api.ollama.com/v1", "Authorization"),
            ),
        ])
    });

pub fn get_api_key_provider_config(provider: &str) -> Option<(&'static str, &'static str)> {
    API_KEY_PROVIDERS.get(provider).copied()
}

pub fn is_api_key_provider(provider: &str) -> bool {
    API_KEY_PROVIDERS.contains_key(provider)
}
