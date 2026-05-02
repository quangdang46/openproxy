use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

pub struct ProviderExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
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
                ("anthropic-beta".to_string(), "claude-code-20250219,interleaved-thinking-2025-05-14".to_string()),
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
                ("anthropic-beta".to_string(), "claude-code-20250219,interleaved-thinking-2025-05-14".to_string()),
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
        self.default_headers.push((name.to_string(), value.to_string()));
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

    fn build_url(&self, model: &str, stream: bool) -> String;

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, ProviderExecutorError>;

    fn transform_request(&self, body: &Value) -> Value {
        body.clone()
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

    pub fn build_url(&self, model: &str, stream: bool) -> String {
        let path = if stream {
            &self.config.stream_path
        } else {
            &self.config.chat_path
        };

        match self.config.format {
            ProviderFormat::Gemini => {
                let action = if stream { "streamGenerateContent?alt=sse" } else { "generateContent" };
                format!("{}/{model}:{action}", self.config.base_url.trim_end_matches('/'))
            }
            _ => format!("{}{}", self.config.base_url.trim_end_matches('/'), path),
        }
    }

    /// Build URL with optional API key appended as query param for providers that need it.
    /// Currently used by gemini free tier which has 15 RPM limit.
    pub fn build_url_with_api_key(&self, model: &str, stream: bool, api_key: Option<&str>) -> String {
        let base_url = self.build_url(model, stream);
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
                HeaderValue::from_str(value).map_err(|e| ProviderExecutorError::InvalidHeader(e.to_string()))?,
            );
        }

        let token = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .ok_or_else(|| ProviderExecutorError::MissingCredentials(self.provider.clone()))?;

        let header_name = reqwest::header::HeaderName::from_bytes(self.config.api_key_header.as_bytes())
            .map_err(|_| ProviderExecutorError::InvalidHeader(self.config.api_key_header.to_string()))?;

        match self.config.format {
            ProviderFormat::Anthropic | ProviderFormat::ClaudeCompatible | ProviderFormat::AnthropicCompatible => {
                if self.config.api_key_header == "Authorization" {
                    headers.insert(header_name, HeaderValue::from_str(&format!("Bearer {token}"))?);
                } else {
                    headers.insert(header_name, HeaderValue::from_str(token)?);
                }
            }
            ProviderFormat::Gemini => {
                if header_name == reqwest::header::AUTHORIZATION {
                    headers.insert(header_name, HeaderValue::from_str(&format!("Bearer {token}"))?);
                } else {
                    headers.insert(header_name, HeaderValue::from_str(token)?);
                }
            }
            _ => {
                if header_name == reqwest::header::AUTHORIZATION {
                    headers.insert(header_name, HeaderValue::from_str(&format!("Bearer {token}"))?);
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

    pub async fn execute(
        &self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionResponse, ProviderExecutorError> {
        let api_key = request.credentials.api_key.as_deref();
        let url = if self.provider == "gemini" && api_key.is_some() {
            self.build_url_with_api_key(&request.model, request.stream, api_key)
        } else {
            self.build_url(&request.model, request.stream)
        };
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

        Ok(ProviderExecutionResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
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

    fn build_url(&self, model: &str, stream: bool) -> String {
        self.build_url(model, stream)
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, ProviderExecutorError> {
        self.build_headers(credentials, stream)
    }
}

static PROVIDER_REGISTRY: once_cell::sync::Lazy<BTreeMap<&'static str, ProviderExecutorConfig>> =
    once_cell::sync::Lazy::new(|| {
        BTreeMap::from([
            ("claude", ProviderExecutorConfig::anthropic("https://api.anthropic.com/v1")),
            ("codex", ProviderExecutorConfig::openai("https://api.openai.com/v1")),
            ("github", ProviderExecutorConfig::openai("https://api.github.com/copilot")),
            ("gitlab", ProviderExecutorConfig::openai("https://gitlab.com/api/v4")),
            ("kiro", ProviderExecutorConfig::anthropic("https://api.kiro.ai/v1")),
            ("cursor", ProviderExecutorConfig::openai("https://api.cursor.com/v1")),
            ("kiro-free", ProviderExecutorConfig::anthropic("https://api.kiro.ai/v1")),
            ("opencode", ProviderExecutorConfig::openai("https://opencode.ai/zen/v1")),
            ("vertex", ProviderExecutorConfig::gemini("https://generativelanguage.googleapis.com/v1beta/models")),
            ("openai", ProviderExecutorConfig::openai("https://api.openai.com/v1")),
            ("deepseek", ProviderExecutorConfig::openai("https://api.deepseek.com/v1")),
            ("groq", ProviderExecutorConfig::openai("https://api.groq.com/openai/v1")),
            ("together", ProviderExecutorConfig::openai("https://api.together.xyz/v1")),
            ("fireworks", ProviderExecutorConfig::openai("https://api.fireworks.ai/inference/v1")),
            ("cerebras", ProviderExecutorConfig::openai("https://api.cerebras.ai/v1")),
            ("mistral", ProviderExecutorConfig::openai("https://api.mistral.ai/v1")),
            ("cohere", ProviderExecutorConfig::openai("https://api.cohere.ai/v1")),
            ("perplexity", ProviderExecutorConfig::openai("https://api.perplexity.ai/chat/completions")),
            ("xai", ProviderExecutorConfig::openai("https://api.x.ai/v1")),
            ("nvidia", ProviderExecutorConfig::openai("https://integrate.api.nvidia.com/v1")),
            ("cloudflare-ai", ProviderExecutorConfig::openai("https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/v1")),
            ("blackbox", ProviderExecutorConfig::openai("https://api.blackbox.ai/api/chat/completions")),
            ("ai21", ProviderExecutorConfig::openai("https://api.ai21.com/v1")),
            ("lepton", ProviderExecutorConfig::openai("https://api.lepton.ai/v1")),
            ("novita", ProviderExecutorConfig::openai("https://api.novita.ai/v1")),
            ("deepinfra", ProviderExecutorConfig::openai("https://api.deepinfra.com/v1")),
            ("focus", ProviderExecutorConfig::openai("https://api.focusforce.io/v1")),
            ("navigators", ProviderExecutorConfig::openai("https://api.navigators.ai/v1")),
            ("polyscale", ProviderExecutorConfig::openai("https://api.polyscale.ai/v1")),
            ("rampt", ProviderExecutorConfig::openai("https://api.rampt.ai/v1")),
            ("skip", ProviderExecutorConfig::openai("https://api.skip.cloud/v1")),
            ("unbound", ProviderExecutorConfig::openai("https://api.unbound.ai/v1")),
            ("workers", ProviderExecutorConfig::openai("https://api.cloudflare.ai/v1")),
            ("zerogpt", ProviderExecutorConfig::openai("https://api.zerogpt.com/v1")),
            ("nebius", ProviderExecutorConfig::openai("https://api.studio.nebius.ai/v1")),
            ("siliconflow", ProviderExecutorConfig::openai("https://api.siliconflow.cn/v1")),
            ("hyperbolic", ProviderExecutorConfig::openai("https://api.hyperbolic.xyz/v1")),
            ("chutes", ProviderExecutorConfig::openai("https://llm.chutes.ai/v1")),
            ("anthropic", ProviderExecutorConfig::anthropic("https://api.anthropic.com/v1")),
            ("glm", ProviderExecutorConfig::claude_compatible("https://api.z.ai/api/anthropic/v1")),
            ("kimi", ProviderExecutorConfig::claude_compatible("https://api.kimi.com/coding/v1")),
            ("kimi-coding", ProviderExecutorConfig::claude_compatible("https://api.kimi.com/coding/v1")),
            ("minimax", ProviderExecutorConfig::claude_compatible("https://api.minimax.io/anthropic/v1")),
            ("minimax-cn", ProviderExecutorConfig::claude_compatible("https://api.minimaxi.com/anthropic/v1")),
            ("gemini", ProviderExecutorConfig::gemini("https://generativelanguage.googleapis.com/v1beta/models")),
            ("codebuddy", ProviderExecutorConfig::openai("https://copilot.tencent.com/v1")),
            ("kilocode", ProviderExecutorConfig::openai("https://api.kilo.ai/api/openrouter/v1")),
            ("alicode", ProviderExecutorConfig::openai("https://coding.dashscope.aliyuncs.com/v1")),
            ("alicode-intl", ProviderExecutorConfig::openai("https://coding-intl.dashscope.aliyuncs.com/v1")),
            ("volcengine-ark", ProviderExecutorConfig::openai("https://ark.cn-beijing.volces.com/api/coding/v3")),
            ("byteplus", ProviderExecutorConfig::openai("https://ark.ap-southeast.bytepluses.com/api/coding/v3")),
            ("openrouter", ProviderExecutorConfig::openai("https://openrouter.ai/api/v1/chat/completions")
                .with_header("HTTP-Referer", "https://endpoint-proxy.local")
                .with_header("X-Title", "Endpoint Proxy")),
            ("ollama-cloud", ProviderExecutorConfig::openai("https://api.ollama.com/v1")),
            ("ollama-local", ProviderExecutorConfig::openai("http://localhost:11434/v1")),
            ("stability-ai", ProviderExecutorConfig::openai("https://api.stability.ai/v1")),
            ("replicate", ProviderExecutorConfig::openai("https://api.replicate.com/v1")),
            ("cline", ProviderExecutorConfig::openai("https://api.cline.bot/api/v1/chat/completions")
                .with_header("HTTP-Referer", "https://cline.bot")
                .with_header("X-Title", "Cline")),
            ("opencode-go", ProviderExecutorConfig::openai("https://opencode.ai/zen/go/v1")),
            ("glm-cn", ProviderExecutorConfig::openai("https://open.bigmodel.cn/api/coding/paas/v4/chat/completions")),
            ("vertex-partner", ProviderExecutorConfig::openai("https://{project}.{location}.掉ax.com/v1/chat/completions")),
            ("azure", ProviderExecutorConfig::openai("https://{resource}.openai.azure.com/v1/chat/completions")),
("grok", ProviderExecutorConfig::openai("https://api.x.ai/v1")),
            ("elevenlabs", ProviderExecutorConfig::openai("https://api.elevenlabs.io/v1")),
            ("cartesia", ProviderExecutorConfig::openai("https://api.cartesia.ai/v1")),
            ("playht", ProviderExecutorConfig::openai("https://api.play.ht/api/v1")),
            ("deepgram", ProviderExecutorConfig::openai("https://api.deepgram.com/v1")),
            ("google-tts", ProviderExecutorConfig::openai("https://texttospeech.googleapis.com/v1")),
            ("edge-tts", ProviderExecutorConfig::openai("https://edge.tts.api.speech.microsoft.com/cognitiveservices/v1")),
            ("openai-embedding", ProviderExecutorConfig::openai("https://api.openai.com/v1")),
            ("cohere-embedding", ProviderExecutorConfig::openai("https://api.cohere.ai/v1")),
            ("voyage-ai", ProviderExecutorConfig::openai("https://api.voyageai.com/v1")),
            ("dalle", ProviderExecutorConfig::openai("https://api.openai.com/v1")),
            ("stable-diffusion", ProviderExecutorConfig::openai("https://api.stability.ai/v1")),
            ("tavily", ProviderExecutorConfig::openai("https://api.tavily.com/v1")),
            ("brave-search", ProviderExecutorConfig::openai("https://api.search.brave.com/v1")),
            ("serper", ProviderExecutorConfig::openai("https://google.serper.dev/search")),
            ("exa", ProviderExecutorConfig::openai("https://api.exa.ai/v1")),
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
        "openai", "anthropic", "deepseek", "groq", "together", "fireworks", "cerebras",
        "mistral", "cohere", "perplexity", "xai", "nvidia", "cloudflare-ai", "blackbox",
        "ai21", "lepton", "novita", "deepinfra", "focus", "navigators", "polyscale",
        "rampt", "skip", "unbound", "workers", "zerogpt", "nebius", "siliconflow",
        "hyperbolic", "chutes", "glm", "kimi", "kimi-coding", "minimax", "minimax-cn",
        "gemini", "codebuddy", "kilocode", "alicode", "alicode-intl", "volcengine-ark",
        "byteplus", "openrouter", "ollama-cloud", "ollama-local", "stability-ai",
        "replicate", "cline", "opencode-go", "glm-cn", "vertex-partner", "azure",
        "grok", "elevenlabs", "cartesia", "playht", "deepgram",
        "google-tts", "edge-tts", "openai-embedding", "cohere-embedding", "voyage-ai",
        "dalle", "stable-diffusion", "tavily", "brave-search", "serper", "exa",
    ]
}

pub fn get_free_providers() -> Vec<&'static str> {
    vec!["kiro", "opencode", "vertex", "openrouter", "nvidia", "ollama-cloud", "gemini"]
}

pub fn get_specialty_providers() -> Vec<&'static str> {
    vec![
        "claude", "codex", "github", "kiro", "vertex", "cursor", "ollama", "grok",
    ]
}
