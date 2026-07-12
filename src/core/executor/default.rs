use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::Incoming as HyperIncoming;
use hyper::http;
use hyper::http::uri::InvalidUri;
use hyper::{Request as HyperRequest, Response as HyperResponse, Uri};
use once_cell::sync::Lazy;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::core::utils::reasoning_content_injector::inject_reasoning_content;
use crate::oauth::token_refresh::dispatch_oauth_refresh;
use crate::types::{ProviderConnection, ProviderNode};

use super::strip_unsupported::strip_unsupported_params;
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
            ProviderConfig::openai("https://opencode.ai/zen/v1"),
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
            ProviderConfig::openai("https://ollama.com/v1/chat/completions"),
        ),
        (
            "vertex",
            ProviderConfig::gemini("https://generativelanguage.googleapis.com/v1beta/models"),
        ),
        (
            "vertex-partner",
            ProviderConfig::gemini("https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}"),
        ),
        (
            "ollama-local",
            ProviderConfig::openai("http://localhost:11434/v1/chat/completions"),
        ),
        (
            "antigravity",
            ProviderConfig::gemini("https://cloudcode-pa.googleapis.com/v1internal"),
        ),
        (
            "grok-web",
            ProviderConfig::openai("https://grok.com/app-chat/conversations/new"),
        ),
        (
            "perplexity-web",
            ProviderConfig::openai("https://www.perplexity.ai"),
        ),
        (
            "xiaomi-mimo",
            ProviderConfig::openai("https://api.xiaomimimo.com/v1/chat/completions"),
        ),
        (
            "black-forest-labs",
            ProviderConfig::openai("https://api.blackforestlabs.ai/v1"),
        ),
        ("fal-ai", ProviderConfig::openai("https://fal.run/fal-ai")),
        (
            "runwayml",
            ProviderConfig::openai("https://api.runwayml.com/v1"),
        ),
        (
            "sdwebui",
            ProviderConfig::openai("http://127.0.0.1:7860/sdapi/v1"),
        ),
        ("comfyui", ProviderConfig::openai("http://127.0.0.1:8188")),
        (
            "lm-studio",
            ProviderConfig::openai("http://localhost:1234/v1/chat/completions"),
        ),
        (
            "vllm",
            ProviderConfig::openai("http://localhost:8000/v1/chat/completions"),
        ),
        (
            "huggingface",
            ProviderConfig::openai("https://api-inference.huggingface.co"),
        ),
        ("jina-ai", ProviderConfig::openai("https://api.jina.ai/v1")),
        ("linkup", ProviderConfig::openai("https://api.linkup.so/v1")),
        ("searxng", ProviderConfig::openai("http://localhost:8080")),
        ("youcom", ProviderConfig::openai("https://api.you.com/v1")),
        (
            "google-pse",
            ProviderConfig::openai("https://www.googleapis.com/customsearch/v1"),
        ),
        (
            "searchapi",
            ProviderConfig::openai("https://www.searchapi.io/api/v1"),
        ),
        (
            "firecrawl",
            ProviderConfig::openai("https://api.firecrawl.dev/v1"),
        ),
        (
            "topaz",
            ProviderConfig::openai("https://api.topazlabs.com/v1"),
        ),
        (
            "inference-net",
            ProviderConfig::openai("https://api.inference.net/v1/chat/completions"),
        ),
        (
            "vercel-ai-gateway",
            ProviderConfig::openai("https://ai-gateway.vercel.sh/v1/chat/completions"),
        ),
        (
            "xiaomi-tokenplan",
            ProviderConfig::openai("https://token-plan-sgp.xiaomimimo.com/v1/chat/completions"),
        ),
        (
            "github-models",
            ProviderConfig::openai("https://models.github.ai/inference/chat/completions"),
        ),
        (
            "hackclub",
            ProviderConfig::openai("https://ai.hackclub.com/proxy/v1/chat/completions"),
        ),
        (
            "ollama",
            ProviderConfig::openai("https://ollama.com/v1/chat/completions"),
        ),
        (
            "assemblyai",
            ProviderConfig::openai("https://api.assemblyai.com/v2"),
        ),
        (
            "agentrouter",
            ProviderConfig::anthropic("https://agentrouter.org/v1/messages"),
        ),
        (
            "aimlapi",
            ProviderConfig::openai("https://api.aimlapi.com/v1/chat/completions"),
        ),
        (
            "modal",
            ProviderConfig::openai("https://api.modal.com/v1/chat/completions"),
        ),
        (
            "reka",
            ProviderConfig::openai("https://api.reka.ai/v1/chat/completions"),
        ),
        (
            "nlpcloud",
            ProviderConfig::openai("https://api.nlpcloud.io/v1/gpu/chatbot"),
        ),
        (
            "bazaarlink",
            ProviderConfig::openai("https://bazaarlink.ai/api/v1/chat/completions"),
        ),
        (
            "completions",
            ProviderConfig::openai("https://completions.me/api/v1/chat/completions"),
        ),
        (
            "enally",
            ProviderConfig::openai("https://ai.enally.in/v1/chat/completions"),
        ),
        (
            "freetheai",
            ProviderConfig::openai("https://api.freetheai.xyz/v1/chat/completions"),
        ),
        (
            "llm7",
            ProviderConfig::openai("https://api.llm7.io/v1/chat/completions"),
        ),
        (
            "kluster",
            ProviderConfig::openai("https://api.kluster.ai/v1/chat/completions"),
        ),
        (
            "predibase",
            ProviderConfig::openai("https://serving.app.predibase.com/v1/chat/completions"),
        ),
        (
            "bytez",
            ProviderConfig::openai("https://api.bytez.com/models/v2"),
        ),
        (
            "morph",
            ProviderConfig::openai("https://api.morphllm.com/v1/chat/completions"),
        ),
        (
            "longcat",
            ProviderConfig::openai("https://api.longcat.chat/openai/v1/chat/completions"),
        ),
        (
            "puter",
            ProviderConfig::openai("https://api.puter.com/puterai/openai/v1/chat/completions"),
        ),
        (
            "uncloseai",
            ProviderConfig::openai("https://hermes.ai.unturf.com/v1/chat/completions"),
        ),
        (
            "scaleway",
            ProviderConfig::openai("https://api.scaleway.ai/v1/chat/completions"),
        ),
        (
            "sambanova",
            ProviderConfig::openai("https://api.sambanova.ai/v1/chat/completions"),
        ),
        (
            "nscale",
            ProviderConfig::openai("https://inference.api.nscale.com/v1/chat/completions"),
        ),
        (
            "baseten",
            ProviderConfig::openai("https://inference.baseten.co/v1/chat/completions"),
        ),
        (
            "publicai",
            ProviderConfig::openai("https://api.publicai.co/v1/chat/completions"),
        ),
        (
            "nous-research",
            ProviderConfig::openai("https://inference-api.nousresearch.com/v1/chat/completions"),
        ),
        (
            "glhf",
            ProviderConfig::openai("https://glhf.chat/api/openai/v1/chat/completions"),
        ),
        (
            "cursor",
            ProviderConfig::openai("https://api.cursor.sh/v1/chat/completions"),
        ),
        (
            "cu",
            ProviderConfig::openai("https://api.cursor.sh/v1/chat/completions"),
        ),
        (
            "codebuddy-cn",
            ProviderConfig::openai("https://api.codebuddy.cn/v1/chat/completions"),
        ),
        (
            "mimo-free",
            ProviderConfig::openai("https://mimo.kiro.dev/v1/chat/completions"),
        ),
        (
            "xiaomi-tokenplan",
            ProviderConfig::openai("https://tokenplan.xiaomi.com/v1/chat/completions"),
        ),
    ])
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub base_url: String,
    pub format: String,
    pub default_headers: Vec<(String, String)>,
    pub fallback_urls: Vec<String>,
}

impl ProviderConfig {
    fn openai(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: "openai".into(),
            default_headers: Vec::new(),
            fallback_urls: Vec::new(),
        }
    }

    fn gemini(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: "gemini".into(),
            default_headers: Vec::new(),
            fallback_urls: Vec::new(),
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

    #[allow(dead_code)]
    fn with_fallback(mut self, url: &str) -> Self {
        self.fallback_urls.push(url.to_string());
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
    CredentialRefreshFailed(String),
    MaxRetriesExhausted(String),
    UpstreamStatus(http::StatusCode, String),
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

    /// Full endpoint URL already (path present); optional query is ignored for matching.
    fn is_already_endpoint(url: &str) -> bool {
        let path = url.split('?').next().unwrap_or(url);
        path.contains("/chat/completions")
            || path.ends_with("/messages")
            || path.contains("/anthropic/v1/messages")
            || path.contains("/responses")
    }

    /// Providers that use Claude-compatible `?beta=true` (9r transport urlSuffix).
    fn provider_wants_claude_beta(provider: &str) -> bool {
        matches!(
            provider,
            "claude"
                | "anthropic"
                | "glm"
                | "kimi"
                | "kimi-coding"
                | "minimax"
                | "minimax-cn"
                | "agentrouter"
        )
    }

    /// Ensure Claude multi-endpoint absolute URLs keep `?beta=true` when missing.
    fn ensure_claude_beta_suffix(url: &str, provider: &str) -> String {
        if !Self::provider_wants_claude_beta(provider) {
            return url.to_string();
        }
        let path = url.split('?').next().unwrap_or(url);
        let is_messages = path.ends_with("/messages") || path.contains("/anthropic/v1/messages");
        if !is_messages {
            return url.to_string();
        }
        if url.contains("beta=") {
            return url.to_string();
        }
        if url.contains('?') {
            format!("{url}&beta=true")
        } else {
            format!("{url}?beta=true")
        }
    }

    /// Xiaomi Token Plan: region host + dual OpenAI/Claude path (9router XiaomiTokenplanExecutor).
    fn xiaomi_tokenplan_url(credentials: &ProviderConnection) -> Result<String, ExecutorError> {
        let region =
            compatible_value(credentials.provider_specific_data.get("region")).unwrap_or("sgp");
        let base = match region {
            "cn" => "https://token-plan-cn.xiaomimimo.com/v1",
            "ams" => "https://token-plan-ams.xiaomimimo.com/v1",
            _ => "https://token-plan-sgp.xiaomimimo.com/v1",
        };
        let wants_claude = credentials
            .runtime_transport
            .as_ref()
            .and_then(|rt| rt.base_url.as_deref())
            .map(|u| u.contains("/anthropic/") || u.ends_with("/messages"))
            .unwrap_or(false);
        if wants_claude {
            let host = base.trim_end_matches('/').trim_end_matches("/v1");
            return Ok(format!("{host}/anthropic/v1/messages"));
        }
        Ok(format!("{base}/chat/completions"))
    }

    pub fn build_url(
        &self,
        model: &str,
        stream: bool,
        credentials: &ProviderConnection,
    ) -> Result<String, ExecutorError> {
        // Region-specific providers must win over resolve_transport's default-region URL.
        if self.provider == "xiaomi-tokenplan" || self.provider == "xmtp" {
            return Self::xiaomi_tokenplan_url(credentials);
        }

        // Check runtime_transport base_url override on the connection first.
        // 9router multi-endpoint transports store a full endpoint URL
        // (…/chat/completions or …/messages[?beta=true]). Use as-is when path is present;
        // otherwise append the provider-default path. Claude beta is baked into the
        // multi-endpoint table (or appended here when missing) so already_endpoint
        // never silently drops urlSuffix.
        if let Some(rt) = &credentials.runtime_transport {
            if let Some(rt_base_url) = &rt.base_url {
                let normalized = rt_base_url.trim_end_matches('/');
                let already_endpoint = Self::is_already_endpoint(normalized);
                if already_endpoint {
                    return Ok(Self::ensure_claude_beta_suffix(normalized, &self.provider));
                }
                if let Some(node) = &self.provider_node {
                    if node.r#type == "anthropic-compatible" {
                        return Ok(format!("{}/messages", normalized));
                    }
                }
                if matches!(
                    self.provider.as_str(),
                    "claude"
                        | "anthropic"
                        | "glm"
                        | "kimi"
                        | "kimi-coding"
                        | "minimax"
                        | "minimax-cn"
                        | "agentrouter"
                        | "xiaomi-mimo"
                        | "mimo"
                ) {
                    let messages = format!("{}/messages", normalized);
                    return Ok(Self::ensure_claude_beta_suffix(&messages, &self.provider));
                }
                return Ok(format!("{}/chat/completions", normalized));
            }
        }

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
            "claude" | "glm" | "kimi" | "minimax" | "minimax-cn" | "kimi-coding" | "agentrouter"
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
        } else if matches!(
            self.provider.as_str(),
            "xiaomi-tokenplan" | "xmtp" | "xiaomi-mimo" | "mimo"
        ) && credentials
            .runtime_transport
            .as_ref()
            .and_then(|rt| rt.base_url.as_deref())
            .is_some_and(|u| u.contains("/anthropic/") || u.ends_with("/messages"))
        {
            // Claude native transport: x-api-key (9router xiaomi-tokenplan / xiaomi-mimo)
            let token = credentials
                .api_key
                .as_deref()
                .or(credentials.access_token.as_deref())
                .ok_or_else(|| ExecutorError::MissingCredentials(self.provider.clone()))?;
            headers.insert("x-api-key", HeaderValue::from_str(token)?);
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        } else if is_anthropic_compatible || self.provider.starts_with("anthropic-compatible") {
            // 9router: anthropic-version + dual auth (x-api-key and/or Bearer)
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            if let Some(api_key) = credentials.api_key.as_deref() {
                headers.insert("x-api-key", HeaderValue::from_str(api_key)?);
                // Dual-auth: also send Bearer for third-party gateways (9router)
                if !headers.contains_key(AUTHORIZATION) {
                    headers.insert(
                        AUTHORIZATION,
                        HeaderValue::from_str(&format!("Bearer {api_key}"))?,
                    );
                }
            }
            if let Some(access_token) = credentials.access_token.as_deref() {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {access_token}"))?,
                );
            }
            if !headers.contains_key("x-api-key") && !headers.contains_key(AUTHORIZATION) {
                return Err(ExecutorError::MissingCredentials(self.provider.clone()));
            }
            // Strip first-party Claude Code identity headers for non-Anthropic upstreams
            for h in [
                "x-stainless-package-version",
                "x-stainless-runtime",
                "x-stainless-runtime-version",
                "anthropic-beta",
            ] {
                headers.remove(h);
            }
        } else {
            // Prefer access_token over api_key for Bearer (9router BaseExecutor)
            let token = credentials
                .access_token
                .as_deref()
                .or(credentials.api_key.as_deref())
                .ok_or_else(|| ExecutorError::MissingCredentials(self.provider.clone()))?;

            if matches!(
                self.provider.as_str(),
                "glm" | "kimi" | "agentrouter" | "enally"
            ) {
                headers.insert("x-api-key", HeaderValue::from_str(token)?);
            } else if matches!(self.provider.as_str(), "minimax" | "minimax-cn") {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}"))?,
                );
            } else {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}"))?,
                );
            }

            // Header hooks: kimi / cline / claude overlay (9router default.js)
            if self.provider == "kimi" || self.provider == "kimi-coding" {
                headers.insert(
                    "User-Agent",
                    HeaderValue::from_static("Mozilla/5.0 KimiCoding"),
                );
            }
            if self.provider == "cline" || self.provider == "clinepass" {
                // Cline often needs workos: prefix handled elsewhere; keep Bearer
            }
            // Claude header cache overlay for anthropic/claude providers
            if matches!(self.provider.as_str(), "claude" | "anthropic") {
                if let Some(overlay) =
                    crate::core::utils::claude_header_cache::get_cached_claude_headers()
                {
                    for (k, v) in overlay {
                        if let (Ok(name), Ok(val)) = (
                            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                            HeaderValue::from_str(&v),
                        ) {
                            if !headers.contains_key(&name) {
                                headers.insert(name, val);
                            }
                        }
                    }
                }
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

    pub fn transform_request(&self, body: &Value, model: &str) -> Value {
        let mut body = self.apply_json_schema_fallback(body);

        // Normalize developer→system role (many providers reject role:developer)
        normalize_developer_role(&mut body);

        // Convert OpenAI-format tools to Claude format when the provider
        // uses a Claude-compatible endpoint (minimax, glm, kimi, etc.)
        if matches!(
            self.provider.as_str(),
            "minimax" | "minimax-cn" | "glm" | "kimi" | "kimi-coding" | "agentrouter"
        ) {
            convert_openai_tools_to_claude(&mut body);
        }

        // Strip unsupported tool types for Fireworks/OCg upstream
        if self.provider == "opencode-go" {
            strip_fireworks_unsupported_tools(&mut body);
        }

        // Inject reasoning_content placeholder for DeepSeek/Kimi providers
        inject_reasoning_content(&self.provider, model, &mut body);

        // Strip unsupported request params for providers that don't support them
        strip_unsupported_params(&self.provider, model, &mut body);

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
        let is_openai_compatible = self
            .provider_node
            .as_ref()
            .is_some_and(|node| node.r#type == "openai-compatible");

        if !is_openai_compatible {
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
        mut request: ExecutionRequest,
    ) -> Result<ExecutionResponse, ExecutorError> {
        // Build headers and transformed body once, reused across retries and
        // fallback URLs.
        let mut headers =
            self.build_headers(&request.model, &request.credentials, request.stream)?;
        let transformed_body = self.transform_request(&request.body, &request.model);

        // Try primary then fallback URLs.
        let urls = self.resolve_urls(&request.model, request.stream, &request.credentials);

        for url in &urls {
            let use_hyper = self.use_hyper_transport(&request, url);

            // The retry loop for this URL.
            for retry in 0..3 {
                let upstream = self
                    .send_one(url, &headers, &transformed_body, &request, use_hyper)
                    .await?;
                let status = upstream.status();

                // Success: return immediately.
                if status.is_success() {
                    return Ok(ExecutionResponse {
                        response: upstream,
                        url: url.clone(),
                        headers,
                        transformed_body,
                        transport: if use_hyper {
                            TransportKind::Hyper
                        } else {
                            TransportKind::Reqwest
                        },
                    });
                }

                // 401 / 403: try credential refresh and retry once with new creds.
                if status == http::StatusCode::UNAUTHORIZED || status == http::StatusCode::FORBIDDEN
                {
                    if retry == 0 {
                        if let Some(new_creds) =
                            self.try_refresh_credentials(&request.credentials).await
                        {
                            request.credentials = new_creds;
                            headers = self.build_headers(
                                &request.model,
                                &request.credentials,
                                request.stream,
                            )?;
                            // Retry immediately with refreshed credentials.
                            let retry_resp = self
                                .send_one(url, &headers, &transformed_body, &request, use_hyper)
                                .await?;
                            if retry_resp.status().is_success() {
                                return Ok(ExecutionResponse {
                                    response: retry_resp,
                                    url: url.clone(),
                                    headers,
                                    transformed_body,
                                    transport: if use_hyper {
                                        TransportKind::Hyper
                                    } else {
                                        TransportKind::Reqwest
                                    },
                                });
                            }
                        }
                    }
                    // No refresh or refresh didn't help — try next fallback URL.
                    break;
                }

                // 429: try next fallback URL (9router BaseExecutor.shouldRetry)
                if status == http::StatusCode::TOO_MANY_REQUESTS {
                    break;
                }

                // 502 Bad Gateway: 3 retries x 3s
                if status == http::StatusCode::BAD_GATEWAY {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }

                // 503 Service Unavailable: 3 retries x 2s
                if status == http::StatusCode::SERVICE_UNAVAILABLE {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }

                // 504 Gateway Timeout: 2 retries x 3s
                if status == http::StatusCode::GATEWAY_TIMEOUT {
                    if retry < 1 {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    }
                    // After 2 retries, fall through to next fallback URL.
                    break;
                }

                // Other non-success status: propagate upstream error.
                return Err(ExecutorError::UpstreamStatus(
                    status,
                    format!("upstream returned {} for URL {}", status.as_u16(), url),
                ));
            }
        }

        Err(ExecutorError::MaxRetriesExhausted(
            "all retries and fallback URLs exhausted".into(),
        ))
    }

    /// Send a single request without retries, returning the raw upstream response.
    async fn send_one(
        &self,
        url: &str,
        headers: &HeaderMap,
        transformed_body: &Value,
        request: &ExecutionRequest,
        use_hyper: bool,
    ) -> Result<UpstreamResponse, ExecutorError> {
        if use_hyper {
            let client = self.pool.get_hyper_direct(&self.provider)?;
            let uri: Uri = url.parse()?;
            let body_bytes = serde_json::to_vec(transformed_body)?;
            let mut req = HyperRequest::post(uri).body(Full::new(body_bytes.into()))?;
            *req.headers_mut() = headers.clone();
            client
                .request(req)
                .await
                .map_err(ExecutorError::Hyper)
                .map(UpstreamResponse::Hyper)
        } else {
            let client = self.pool.get(&self.provider, request.proxy.as_ref())?;
            client
                .post(url)
                .headers(headers.clone())
                .json(transformed_body)
                .send()
                .await
                .map_err(ExecutorError::Request)
                .map(UpstreamResponse::Reqwest)
        }
    }

    /// Resolve primary and fallback URLs for the given request.
    fn resolve_urls(
        &self,
        model: &str,
        stream: bool,
        credentials: &ProviderConnection,
    ) -> Vec<String> {
        let primary = match self.build_url(model, stream, credentials) {
            Ok(url) => url,
            Err(_) => return Vec::new(),
        };
        let mut urls = vec![primary];
        urls.extend(self.config.fallback_urls.clone());
        urls
    }

    /// Try to refresh OAuth credentials when the upstream returns 401/403.
    /// Returns `Some(updated_creds)` on success, `None` on failure.
    async fn try_refresh_credentials(
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

/// Convert OpenAI-format tools to Claude format.
///
/// OpenAI: `{"type":"function", "function": {"name":"x", "description":"d", "parameters":{...}}}`
/// Claude: `{"name":"x", "description":"d", "input_schema": {...}}`
///
/// Also strips `tool_choice` from OpenAI format and converts it.
fn convert_openai_tools_to_claude(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    // Convert tools[]
    if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
        let mut claude_tools = Vec::new();
        for tool in tools.drain(..) {
            let Some(tool_obj) = tool.as_object() else {
                continue;
            };
            let type_ = tool_obj.get("type").and_then(Value::as_str).unwrap_or("");
            if type_ != "function" {
                // Skip non-function tools
                continue;
            }
            let Some(func) = tool_obj.get("function") else {
                continue;
            };
            let name = func
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = func
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let input_schema = func
                .get("parameters")
                .cloned()
                .or_else(|| func.get("input_schema").cloned())
                .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));

            claude_tools.push(serde_json::json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            }));
        }
        if !claude_tools.is_empty() {
            // Add cache_control to last tool
            if let Some(last) = claude_tools.last_mut() {
                if let Some(last_obj) = last.as_object_mut() {
                    last_obj.insert(
                        "cache_control".to_string(),
                        serde_json::json!({"type": "ephemeral"}),
                    );
                }
            }
            tools.clear();
            tools.extend(claude_tools);
        } else {
            obj.remove("tools");
        }
    }

    // Convert tool_choice
    // OpenAI: {"type": "function", "function": {"name": "..."}} → Claude: {"type": "tool", "name": "..."}
    // OpenAI: "auto" → Claude: {"type": "auto"}
    // OpenAI: "required" → Claude: {"type": "any"}
    // OpenAI: "none" → Claude: {"type": "none"}
    if let Some(tc) = obj.get("tool_choice") {
        let new_tc = match tc {
            Value::String(s) => match s.as_str() {
                "required" => Some(serde_json::json!({"type": "any"})),
                "none" => Some(serde_json::json!({"type": "none"})),
                "auto" => Some(serde_json::json!({"type": "auto"})),
                _ => Some(serde_json::json!({"type": "auto"})),
            },
            Value::Object(m) => {
                if let Some(name) = m
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    Some(serde_json::json!({"type": "tool", "name": name}))
                } else {
                    Some(serde_json::json!({"type": "auto"}))
                }
            }
            _ => None,
        };
        if let Some(new_tc) = new_tc {
            obj.insert("tool_choice".to_string(), new_tc);
        }
    }
}

/// Strip tools that Fireworks AI / OCg upstream doesn't support.
/// - Only keeps tools with type "function"
/// - Strips "strict" field from function definitions
fn strip_fireworks_unsupported_tools(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
        // Keep only function-type tools that also have a `function` object
        // (type "function" without function:{} breaks DeepSeek upstream)
        tools.retain(|tool| {
            let t = tool.get("type").and_then(Value::as_str).unwrap_or("");
            t == "function" && tool.get("function").and_then(Value::as_object).is_some()
                || t == "custom"
                || t.is_empty()
        });
        for tool in tools.iter_mut() {
            if let Some(tool_obj) = tool.as_object_mut() {
                tool_obj.remove("strict");
            }
        }
    }
}
