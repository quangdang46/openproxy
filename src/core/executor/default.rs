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
use tokio::sync::{self, Semaphore};

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::core::utils::reasoning_content_injector::inject_reasoning_content;
use crate::oauth::token_refresh::dispatch_oauth_refresh;
use crate::types::{ProviderConnection, ProviderNode};

use crate::core::simulation::env_force_all;

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
            "api-airforce",
            ProviderConfig::openai("https://api.airforce/v1/chat/completions")
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
        // kimi-coding is the dual-auth alias of kimi (providers.rs: kimi() is
        // kimi_coding() with a renamed id). Connections created through the
        // device-code OAuth path carry no provider_node, so without this entry
        // DefaultExecutor::new returned UnsupportedProvider -> HTTP 500.
        (
            "kimi-coding",
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
            ProviderConfig::openai("https://api.siliconflow.com/v1/chat/completions"),
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
        // Zen answers 200 for unauthenticated POSTs, so auth is optional
        // (`PROVIDER_OPTIONAL_AUTH`) and model ids pass through verbatim.
        (
            "opencode-zen",
            ProviderConfig::openai("https://opencode.ai/zen/v1/chat/completions"),
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
            "alims-intl",
            ProviderConfig::openai(
                "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
            ),
        ),
        (
            "alitp-intl",
            ProviderConfig::openai(
                "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/chat/completions",
            ),
        ),
        (
            "baidu",
            ProviderConfig::openai("https://qianfan.baidubce.com/v2/chat/completions"),
        ),
        (
            "bluesminds",
            ProviderConfig::openai("https://api.bluesminds.com/v1/chat/completions"),
        ),
        (
            "clinepass",
            ProviderConfig::openai("https://api.cline.bot/api/v1/chat/completions")
                .with_header("HTTP-Referer", "https://cline.bot")
                .with_header("X-Title", "Cline"),
        ),
        (
            "codebuddy-intl",
            ProviderConfig::openai("https://www.codebuddy.ai/v2/chat/completions")
                .with_header("User-Agent", "IDE/2.108.1 CodeBuddy/2.108.1")
                .with_header("X-Product", "SaaS")
                .with_header("X-IDE-Type", "IDE")
                .with_header("X-IDE-Name", "IDE")
                .with_header("x-requested-with", "XMLHttpRequest")
                .with_header("x-codebuddy-request", "1"),
        ),
        (
            "featherless",
            ProviderConfig::openai("https://api.featherless.ai/v1/chat/completions"),
        ),
        (
            "kilo-gateway",
            ProviderConfig::openai("https://api.kilo.ai/api/gateway/chat/completions"),
        ),
        (
            "perplexity-agent",
            // OpenAI Responses API — do NOT normalize to /chat/completions.
            ProviderConfig::openai("https://api.perplexity.ai/v1/responses"),
        ),
        (
            "poolside",
            ProviderConfig::openai("https://inference.poolside.ai/v1/chat/completions"),
        ),
        (
            "tencent",
            ProviderConfig::openai("https://api.hunyuan.cloud.tencent.com/v1/chat/completions"),
        ),
        (
            "tokenrouter",
            ProviderConfig::openai("https://api.tokenrouter.com/v1/chat/completions"),
        ),
        (
            "venice",
            // /api/v1 (double path) — do NOT "fix" to /v1.
            ProviderConfig::openai("https://api.venice.ai/api/v1/chat/completions"),
        ),
        (
            "zed",
            // MINIMAL chat-path fix: zed's non-standard auth header
            // ("Authorization: <user_id> <access_token>", no Bearer) and NDJSON
            // wire protocol are a separate executor task (parity A3). This entry
            // clears UnsupportedProvider so a pre-obtained token can route.
            ProviderConfig::openai("https://cloud.zed.dev/completions"),
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
            // 9router registry/blackbox.js:26 — /v1/chat/completions.
            ProviderConfig::openai("https://api.blackbox.ai/v1/chat/completions"),
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
            ProviderConfig::openai("https://grok.com/rest/app-chat/conversations/new"),
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
            "serpingapi",
            ProviderConfig::openai("https://api.serpingapi.com/v1"),
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
            // 9router registry/codebuddy-cn.js:22 — NOT api.codebuddy.cn.
            "codebuddy-cn",
            ProviderConfig::openai("https://copilot.tencent.com/v2/chat/completions"),
        ),
        (
            "mimo-free",
            ProviderConfig::openai("https://api.xiaomimimo.com/api/free-ai/openai/chat"),
        ),
        // NOTE: no second "xiaomi-tokenplan" entry — BTreeMap::from keeps the
        // LAST duplicate, which previously shadowed the sgp region URL with
        // tokenplan.xiaomi.com. Region routing lives in xiaomi_tokenplan_url
        // (registry/xiaomi-tokenplan.js transport.regions); the map entry is
        // only a fallback and must stay the sgp default (:359).
    ])
});

// Semaphore for tokenrouter free models to limit concurrent requests to 1
static TOKENROUTER_SEMAPHORE: Lazy<Semaphore> = Lazy::new(|| Semaphore::new(1));

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

/// Anthropic beta flags, ported from `selectAnthropicBeta` in
/// `open-sse/providers/shared.js:51-69`. Heavy-agent flags are gated to
/// opus/sonnet — cheaper models don't need them.
const ANTHROPIC_BETA_BASE: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28";
const ANTHROPIC_BETA_HEAVY_AGENT: &str = "advanced-tool-use-2025-11-20,effort-2025-11-24";

pub fn select_anthropic_beta(model: &str) -> String {
    if model.starts_with("claude-opus") || model.starts_with("claude-sonnet") {
        format!("{ANTHROPIC_BETA_BASE},{ANTHROPIC_BETA_HEAVY_AGENT}")
    } else {
        ANTHROPIC_BETA_BASE.to_string()
    }
}

pub struct DefaultExecutor {
    provider: String,
    config: ProviderConfig,
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
    /// When set, `execute` runs in simulation mode and renders its envelope
    /// in this format regardless of the provider's registry config.
    ///
    /// Bead openproxy-umtq: the dispatch short-circuit routes EVERY provider
    /// that resolves to mock through this executor, including providers with
    /// a dedicated arm (kiro, codex, cursor, …) that have no `PROVIDER_CONFIGS`
    /// entry. `sim_override` is the plan's *source* format, so the simulated
    /// envelope is rendered in the client's own dialect and the response
    /// translator (which is bypassed for mock) stays coherent.
    sim_override: Option<SimulationMode>,
}

/// The simulation override carried by a mock-only `DefaultExecutor`.
///
/// The mock branch is a pure function of the request (no credentials, no
/// network, no `base_url`), so a simulated executor can be built for ANY
/// provider — including ones with a dedicated dispatch arm and no
/// `PROVIDER_CONFIGS` entry — without the real-transport config those
/// providers would need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimulationMode {
    /// Render the simulated envelope in this format (the client's dialect).
    pub format: crate::core::executor::ProviderFormat,
    /// Force the simulation branch even with no sim header and no stub
    /// connection (i.e. when the provider is *configured* mock, not just
    /// credentialless). Mirrors `ExecutionRequest::force_mock`.
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct ExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
    /// Incoming client headers for simulation control only
    /// (`x-openproxy-sim-*`, bead sim-04). Defaults empty; never forwarded.
    #[allow(dead_code)]
    pub sim_headers: HeaderMap,
    /// Resolver-wiring (follow-up openproxy-1ycq): when true, the executor
    /// activates the mock branch for configured-mock providers even without
    /// the per-request header. Set by dispatch sites that performed a DB
    /// lookup via `status_for` (chat stub gate, CLI). Unit/integration tests
    /// that build ExecutionRequest literally keep the default `false` and
    /// drive mock mode via `sim_headers` — unchanged behavior.
    pub force_mock: bool,
}

impl Default for ExecutionRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            body: Value::Null,
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: HeaderMap::new(),
            force_mock: false,
        }
    }
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

    /// Read the full body as text (lossy), consuming the response.
    pub async fn text(self) -> String {
        match self {
            Self::Reqwest(response) => response.text().await.unwrap_or_default(),
            Self::Hyper(response) => {
                let bytes = http_body_util::BodyExt::collect(response.into_body())
                    .await
                    .map(|c| c.to_bytes())
                    .unwrap_or_default();
                String::from_utf8_lossy(&bytes).to_string()
            }
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
    SemaphoreAcquisitionFailed,
    CredentialRefreshFailed(String),
    MaxRetriesExhausted(String),
    UpstreamStatus(http::StatusCode, String),
    /// Mock requested for a provider format with no registered simulator
    /// (bead sim-04/sim-05: surfaces explicitly, never silent).
    SimulationUnsupported {
        provider: String,
        format: String,
    },
    /// Provider-correct simulated rejection (bead sim-08): renders as the
    /// exact HTTP status + envelope, never masked as 500.
    SimulationValidation {
        status: http::StatusCode,
        body: serde_json::Value,
        retry_after: Option<u64>,
    },
}
impl ExecutorError {
    /// Map an executor failure into a ComboAttemptError preserving the raw
    /// upstream status when available (429 rate limits must surface with
    /// retry-after, not be masked as 500).
    pub fn into_combo_attempt_error(self) -> crate::core::combo::ComboAttemptError {
        use crate::core::combo::ComboAttemptError;
        match &self {
            Self::UpstreamStatus(status, message) => ComboAttemptError {
                status: status.as_u16(),
                message: message.clone(),
                retry_after: None,
                upstream_body: None,
            },
            Self::MissingCredentials(p) => ComboAttemptError {
                status: 400,
                message: format!("Missing credentials for provider: {p}"),
                retry_after: None,
                upstream_body: None,
            },
            other => ComboAttemptError {
                status: 500,
                message: format!("Execution failed: {other:?}"),
                retry_after: None,
                upstream_body: None,
            },
        }
    }
}

impl From<crate::core::simulation::SimulationError> for ExecutorError {
    fn from(error: crate::core::simulation::SimulationError) -> Self {
        match error {
            crate::core::simulation::SimulationError::Unsupported { provider, format } => {
                Self::SimulationUnsupported { provider, format }
            }
            crate::core::simulation::SimulationError::Internal(detail) => {
                Self::MaxRetriesExhausted(detail)
            }
            crate::core::simulation::SimulationError::Validation {
                status,
                body,
                retry_after,
            } => {
                let code =
                    http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::BAD_REQUEST);
                Self::SimulationValidation {
                    status: code,
                    body,
                    retry_after,
                }
            }
        }
    }
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

impl From<tokio::sync::AcquireError> for ExecutorError {
    fn from(_: tokio::sync::AcquireError) -> Self {
        Self::SemaphoreAcquisitionFailed
    }
}

/// Resolve a provider's upstream base URL from the live map
/// (`PROVIDER_CONFIGS`). This is the single source of truth for chat and
/// media; it replaces the deleted `provider.rs` PROVIDER_REGISTRY, which
/// had silently drifted (e.g. wrong blackbox URL, wrong `featherless-ai`
/// key). Returns `None` for unknown providers.
pub fn provider_config_base_url(provider: &str) -> Option<String> {
    PROVIDER_CONFIGS
        .get(provider)
        .map(|config| config.base_url.clone())
}

/// Resolve the simulation [`ProviderFormat`](crate::core::executor::ProviderFormat)
/// for a provider name + config format string (bead sim-19: shared by the
/// executor method and the status API so both agree).
/// NOTE: DefaultExecutor ProviderConfig.format is a plain string and
/// anthropic()/claude_compatible() constructors delegate to openai(),
/// so config.format alone misroutes the anthropic family. Provider name
/// takes precedence for family resolution (mirrors provider_wants_claude_beta).
pub fn provider_sim_format(
    provider: &str,
    config_format: &str,
) -> crate::core::executor::ProviderFormat {
    const ANTHROPIC_FAMILY: &[&str] = &[
        "anthropic",
        "claude",
        "glm",
        "kimi",
        "kimi-coding",
        "minimax",
        "minimax-cn",
        "agentrouter",
    ];
    if ANTHROPIC_FAMILY.contains(&provider) {
        return crate::core::executor::ProviderFormat::Anthropic;
    }
    match config_format {
        "openai-compatible" => crate::core::executor::ProviderFormat::OpenAICompatible,
        "anthropic" => crate::core::executor::ProviderFormat::Anthropic,
        "anthropic-compatible" | "claude-compatible" => {
            crate::core::executor::ProviderFormat::AnthropicCompatible
        }
        "gemini" => crate::core::executor::ProviderFormat::Gemini,
        _ => crate::core::executor::ProviderFormat::OpenAI,
    }
}

/// Config format string for a provider (for status/dispatch surfaces).
/// Returns `None` for unknown providers.
pub fn provider_config_format(provider: &str) -> Option<String> {
    PROVIDER_CONFIGS
        .get(provider)
        .map(|config| config.format.clone())
}

/// All known provider names (keys of `PROVIDER_CONFIGS`), sorted.
/// Used by the simulation status surface (bead sim-19) so every supported
/// provider reports a mode even without a kv override stored.
pub fn provider_config_names() -> Vec<String> {
    let mut names: Vec<String> = PROVIDER_CONFIGS.keys().map(|k| k.to_string()).collect();
    names.sort();
    names
}

/// Per-status same-URL retry policy, ported from 9router
/// `config/runtimeConfig.js:78-83` (`DEFAULT_RETRY_CONFIG`):
///
///   429 -> attempts 0, delay 0      never retried on the SAME url
///   502 -> attempts 3, delay 3000   also the bucket for network exceptions
///   503 -> attempts 3, delay 2000
///   504 -> attempts 2, delay 3000
///   anything else -> attempts 0
///
/// 9router maps fetch/network exceptions onto the 502 entry (base.js:174:
/// `tryRetry(urlIndex, HTTP_STATUS.BAD_GATEWAY, ...)`), so a connection refusal
/// or TLS error is retried 3 times with a 3s gap. OpenProxy propagated those
/// with `?` on the very first failure, so one refused connection failed the
/// request outright while a 502 on the same url got three attempts.
///
/// "attempts" counts TOTAL attempts, not extra retries.
fn retry_policy(status: http::StatusCode) -> (u32, u64) {
    match status {
        http::StatusCode::BAD_GATEWAY => (3, 3_000),
        http::StatusCode::SERVICE_UNAVAILABLE => (3, 2_000),
        http::StatusCode::GATEWAY_TIMEOUT => (2, 3_000),
        // 429 is deliberately absent: retrying a rate-limited request
        // immediately makes the limit worse. 9router moves to the NEXT url
        // instead (base.js:84, shouldRetry fires only when another url exists).
        _ => (0, 0),
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
            sim_override: None,
        })
    }

    /// Build a simulation-only executor for any provider (bead openproxy-umtq).
    ///
    /// The mock branch in `execute` never reads `base_url`, credentials, or the
    /// client pool, so it does not need the real-transport `PROVIDER_CONFIGS`
    /// entry. This lets the dispatch short-circuit route providers with a
    /// dedicated executor (kiro, codex, cursor, …) — which have no such entry
    /// — through the simulator, so mock mode is honored for every provider
    /// instead of only the OpenAI-shaped `else` arm.
    ///
    /// `format` is the client's dialect (the plan's source format): the
    /// simulated envelope is rendered in it, so the caller can return it
    /// verbatim without response translation.
    pub fn new_for_mock(
        provider: impl Into<String>,
        format: crate::core::executor::ProviderFormat,
        pool: Arc<ClientPool>,
    ) -> Self {
        Self {
            provider: provider.into(),
            config: ProviderConfig {
                base_url: String::new(),
                format: format.as_str().to_string(),
                default_headers: Vec::new(),
                fallback_urls: Vec::new(),
            },
            pool,
            provider_node: None,
            sim_override: Some(SimulationMode {
                format,
                force: true,
            }),
        }
    }

    /// Whether simulation mock mode is active for this request.
    ///
    /// Active iff: global env force is set, the per-request `x-openproxy-sim:
    /// mock` header is present, or `force_mock` was set by a dispatch site
    /// that already performed the DB lookup (resolver-wiring follow-up
    /// openproxy-1ycq: chat stub gate, CLI stub gate). The executor itself
    /// stays DB-free (hot path) — resolution happens once at dispatch.
    /// Format support is enforced at dispatch (bead sim-06+).
    fn simulation_active(request: &ExecutionRequest) -> bool {
        use crate::core::simulation::SIM_HEADER;
        if env_force_all() {
            return true;
        }
        if request.force_mock {
            return true;
        }
        request
            .sim_headers
            .get(SIM_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("mock"))
    }

    /// Resolve the simulation [`ProviderFormat`] for this executor.
    /// Shared by the mock branch (execute_simulated) and the REAL-branch
    /// fault path (sim-15) so both agree on the envelope shape.
    fn sim_format(&self) -> crate::core::executor::ProviderFormat {
        if let Some(override_mode) = self.sim_override {
            return override_mode.format;
        }
        provider_sim_format(&self.provider, self.config.format.as_str())
    }

    async fn execute_simulated(
        &self,
        request: &ExecutionRequest,
    ) -> Result<ExecutionResponse, ExecutorError> {
        use crate::core::simulation::{SimContext, SimulationEngine};
        // sim-07/09/10: OpenAI(+compat), Anthropic(+compat incl.
        // ClaudeCompatible reuse), and Gemini non-stream AND stream.
        // A mock-only executor (openproxy-umtq) bypasses the config-format
        // check: it renders in the explicitly chosen client dialect, which the
        // dispatch short-circuit validated is simulatable.
        let supported = self.sim_override.is_some()
            || matches!(
                self.config.format.as_str(),
                "openai"
                    | "openai-compatible"
                    | "anthropic"
                    | "anthropic-compatible"
                    | "claude-compatible"
                    | "gemini"
            )
            || self.provider == "openai"
            || self.provider == "anthropic"
            || self.provider == "gemini";
        if !supported {
            return Err(ExecutorError::SimulationUnsupported {
                provider: self.provider.clone(),
                format: self.config.format.clone(),
            });
        }
        // NOTE: format resolution lives in sim_format() (single source; also
        // used by the REAL-branch fault path, sim-15).
        let format = self.sim_format();
        // sim-12: status fault short-circuits before the engine with a
        // provider-correct envelope (same render path as Validation errors).
        // Ordering (deliberate, do NOT reorder without updating the
        // precedence test): latency → status fault → engine+validation →
        // response override. Status fault pre-empts everything; override
        // applies post-validation by design.
        let fault = crate::core::simulation::FaultSpec::parse(&request.sim_headers);
        // sim-13: first-byte latency applies to ALL mock outcomes (fault error,
        // validation error, echo, SSE) — sleep before first byte, never after.
        crate::core::simulation::FaultInjector::apply_latency(&fault).await;
        if let Some((status, body, retry_after)) =
            crate::core::simulation::FaultInjector::status_fault(format, &self.provider, &fault)
        {
            return Ok(Self::sim_error_response(
                &self.provider,
                request,
                status,
                body,
                retry_after,
            ));
        }
        let engine = SimulationEngine::mvp();
        let ctx = SimContext {
            provider: &self.provider,
            model: &request.model,
            body: &request.body,
            stream: request.stream,
        };
        let envelope = match engine.execute(format, &self.provider, &ctx).await {
            Ok(env) => env,
            Err(crate::core::simulation::SimulationError::Validation {
                status,
                body,
                retry_after,
            }) => {
                return Ok(Self::sim_error_response(
                    &self.provider,
                    request,
                    status,
                    body,
                    retry_after,
                ));
            }
            Err(e) => return Err(e.into()),
        };
        // sim-14: response override (plan §5.1.1, content-level only).
        let envelope = match crate::core::simulation::FaultInjector::response_override(&fault) {
            Some(crate::core::simulation::OverrideAction::Malformed) => {
                // Malformed JSON-looking value → provider-correct 400.
                let spec400 = crate::core::simulation::FaultSpec {
                    status: Some(400),
                    ..Default::default()
                };
                let (_, body, _) = crate::core::simulation::FaultInjector::status_fault(
                    format,
                    &self.provider,
                    &spec400,
                )
                .expect("400 always allowlisted");
                return Ok(Self::sim_error_response(
                    &self.provider,
                    request,
                    400,
                    body,
                    None,
                ));
            }
            Some(crate::core::simulation::OverrideAction::Json(v)) if !request.stream => {
                // Non-stream JSON object: tool-shape objects replace the echo
                // inside the simulator envelope (spec bullet); all other
                // objects are verbatim bodies (caller owns schema, §5.1.1).
                if v.get("tool_calls").is_some() || v.get("tool_use").is_some() {
                    // Reuse the stream applier with stream=false: it mutates
                    // the envelope in place and never touches framing.
                    Self::apply_stream_override(envelope, &v, format)
                } else {
                    return Ok(Self::sim_json_response(&self.provider, request, v));
                }
            }
            Some(crate::core::simulation::OverrideAction::Json(v)) => {
                Self::apply_stream_override(envelope, &v, format)
            }
            Some(crate::core::simulation::OverrideAction::Text(t)) => {
                Self::apply_stream_text_override(envelope, &t, request.stream)
            }
            None => envelope,
        };
        if request.stream {
            let body = match format {
                crate::core::executor::ProviderFormat::Anthropic
                | crate::core::executor::ProviderFormat::AnthropicCompatible => {
                    crate::core::simulation::sse_body_anthropic(&envelope)
                }
                crate::core::executor::ProviderFormat::Gemini => {
                    crate::core::simulation::sse_body_gemini(&envelope)
                }
                _ => crate::core::simulation::sse_body_openai(&envelope),
            };
            // sim-14: disconnect truncates AFTER framing (frame boundaries kept,
            // terminals dropped) so the client sees a cut stream, never a hang.
            let body = match crate::core::simulation::FaultInjector::truncate_sse(&body, &fault) {
                Some(cut) => cut,
                None => body,
            };
            Ok(Self::sim_sse_response(&self.provider, request, body))
        } else {
            Ok(Self::sim_json_response(&self.provider, request, envelope))
        }
    }

    /// Apply a JSON-object override to a stream envelope (bead sim-14).
    /// Extracts `content` (string) and/or `tool_calls`/`tool_use` payloads;
    /// framing stays simulator-generated. Unknown shapes → content suppressed
    /// (empty chunks + terminal), never raw passthrough.
    fn apply_stream_override(
        mut envelope: serde_json::Value,
        v: &serde_json::Value,
        format: crate::core::executor::ProviderFormat,
    ) -> serde_json::Value {
        use crate::core::executor::ProviderFormat::*;
        // Plain-text content extraction (OpenAI/Gemini `content`, or raw string).
        if let Some(text) = v
            .get("content")
            .and_then(|c| c.as_str())
            .or_else(|| v.as_str())
        {
            envelope = Self::apply_stream_text_override(envelope, text, true);
        }
        // Tool payload extraction per format — applied DIRECTLY to the
        // envelope (not only sim_* hints), so every render path observes it:
        // - OpenAI non-stream reads message.tool_calls; OpenAI stream reads
        //   sim_tool_calls (sse_body); both are set here.
        // - Anthropic non-stream/stream read content[1] (tool_use block).
        // - Gemini has no tool path: warn-log instead of silent ignore.
        let tool_calls = match format {
            Anthropic | AnthropicCompatible => v.get("tool_use").cloned(),
            Gemini => {
                if v.get("tool_calls").is_some() || v.get("tool_use").is_some() {
                    tracing::warn!(
                        target: "openproxy::simulation",
                        "tool override ignored for Gemini (no tool path in MVP)"
                    );
                }
                None
            }
            _ => v
                .get("tool_calls")
                .cloned()
                .or_else(|| v.get("tool_use").cloned()),
        };
        if let Some(tc) = tool_calls {
            match format {
                Anthropic | AnthropicCompatible => {
                    // Replace content[1] with the override tool_use block;
                    // keep content[0] text, force stop_reason tool_use.
                    let block = if tc.get("type").is_some() {
                        tc.clone()
                    } else {
                        serde_json::json!({
                            "type": "tool_use",
                            "id": tc.get("id").cloned().unwrap_or(serde_json::Value::String(
                                "toolu_sim_override".to_string())),
                            "name": tc.get("name").cloned().unwrap_or(serde_json::Value::String(
                                "override".to_string())),
                            "input": tc.get("input").cloned().unwrap_or(serde_json::json!({})),
                        })
                    };
                    if let Some(content) = envelope.get_mut("content") {
                        if let Some(arr) = content.as_array_mut() {
                            if arr.len() > 1 {
                                arr[1] = block;
                            } else {
                                arr.push(block);
                            }
                        }
                    }
                    if let Some(stop) = envelope.get_mut("stop_reason") {
                        *stop = serde_json::Value::String("tool_use".to_string());
                    }
                    if let Some(obj) = envelope.as_object_mut() {
                        obj.insert("sim_tool_calls".into(), serde_json::json!([]));
                        obj.insert("sim_is_tool".into(), serde_json::Value::from(true));
                    }
                }
                _ => {
                    // OpenAI(+compat): replace message.tool_calls directly AND
                    // stash for the stream renderer.
                    if let Some(msg) = envelope
                        .get_mut("choices")
                        .and_then(|c| c.get_mut(0))
                        .and_then(|c| c.get_mut("message"))
                    {
                        if let Some(obj) = msg.as_object_mut() {
                            // Normalize bare objects to tool_calls array shape.
                            let calls = if tc.is_array() {
                                tc.clone()
                            } else {
                                serde_json::json!([{
                                    "id": tc.get("id").cloned().unwrap_or(
                                        serde_json::Value::String("call_sim_override".to_string())),
                                    "type": "function",
                                    "function": {
                                        "name": tc.get("name").cloned().unwrap_or(
                                            tc.get("function")
                                                .and_then(|f| f.get("name")).cloned()
                                                .unwrap_or(serde_json::Value::String(
                                                    "override".to_string()))),
                                        "arguments": tc.get("arguments").cloned().unwrap_or(
                                            tc.get("function")
                                                .and_then(|f| f.get("arguments")).cloned()
                                                .unwrap_or(serde_json::json!("{}"))),
                                    },
                                }])
                            };
                            obj.insert("tool_calls".into(), calls.clone());
                            obj.remove("content");
                            if let Some(obj) = envelope.as_object_mut() {
                                obj.insert("sim_tool_calls".into(), calls);
                            }
                        }
                        if let Some(ch) = envelope.get_mut("choices").and_then(|c| c.get_mut(0)) {
                            if let Some(obj) = ch.as_object_mut() {
                                obj.insert(
                                    "finish_reason".into(),
                                    serde_json::Value::String("tool_calls".to_string()),
                                );
                            }
                        }
                    }
                }
            }
        }
        envelope
    }

    /// Apply a plain-text override to an envelope (bead sim-14).
    /// Replaces message content and suppresses tool echo (plan §5.1.1).
    /// NOTE: stream text-override drops the usage chunk (`sim_include_usage`
    /// forced false) so chunk counts stay deterministic; the base echo path
    /// keeps usage. Deliberate, locked by the stream-override test.
    fn apply_stream_text_override(
        mut envelope: serde_json::Value,
        text: &str,
        stream: bool,
    ) -> serde_json::Value {
        use crate::core::executor::ProviderFormat::*;
        // Strip any tool echo: text forces a text answer.
        if let Some(msg) = envelope
            .get_mut("choices")
            .and_then(|c| c.get_mut(0))
            .and_then(|c| c.get_mut("message"))
        {
            if let Some(obj) = msg.as_object_mut() {
                obj.remove("tool_calls");
                obj.insert(
                    "content".into(),
                    serde_json::Value::String(text.to_string()),
                );
            }
            if let Some(ch) = envelope.get_mut("choices").and_then(|c| c.get_mut(0)) {
                if let Some(obj) = ch.as_object_mut() {
                    obj.insert(
                        "finish_reason".into(),
                        serde_json::Value::String("stop".to_string()),
                    );
                }
            }
        }
        if let Some(content) = envelope.get_mut("content") {
            // Anthropic non-stream shape: content[0].text.
            if let Some(first) = content.get_mut(0) {
                if let Some(obj) = first.as_object_mut() {
                    obj.retain(|k, _| k == "type");
                    obj.insert("type".into(), serde_json::Value::String("text".to_string()));
                    obj.insert("text".into(), serde_json::Value::String(text.to_string()));
                }
            }
            if let Some(stop) = envelope.get_mut("stop_reason") {
                *stop = serde_json::Value::String("end_turn".to_string());
            }
        }
        if let Some(parts) = envelope
            .get_mut("candidates")
            .and_then(|c| c.get_mut(0))
            .and_then(|c| c.get_mut("content"))
            .and_then(|c| c.get_mut("parts"))
        {
            *parts = serde_json::json!([{"text": text}]);
        }
        if stream {
            // Re-chunk the overridden text; drop tool descriptors.
            if let Some(obj) = envelope.as_object_mut() {
                obj.remove("sim_tool_calls");
                obj.remove("sim_is_tool");
                let chunks: Vec<String> = crate::core::simulation::engine::split_words(text)
                    .into_iter()
                    .collect();
                // Anthropic envelope reads sim_chunks too (same helper shape).
                obj.insert("sim_chunks".into(), serde_json::Value::from(chunks));
                obj.insert("sim_include_usage".into(), serde_json::Value::from(false));
            }
        }
        envelope
    }

    /// Build a synthetic non-stream JSON response (no network).
    /// Strips internal `sim_*` envelope hints so they never leak to clients
    /// (reviewer sim-14: the stream renderer reads them; the JSON renderer
    /// must not expose them).
    fn sim_json_response(
        provider: &str,
        request: &ExecutionRequest,
        mut body: serde_json::Value,
    ) -> ExecutionResponse {
        use reqwest::header::{HeaderMap, HeaderValue};
        if let Some(obj) = body.as_object_mut() {
            obj.retain(|k, _| !k.starts_with("sim_"));
        }
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let http_resp = http::Response::builder()
            .status(http::StatusCode::OK)
            .header("content-type", "application/json")
            .body(reqwest::Body::from(bytes))
            .unwrap();
        ExecutionResponse {
            response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
            url: format!("sim://{}/{}", provider, request.model),
            headers,
            transformed_body: body,
            transport: TransportKind::Reqwest,
        }
    }

    /// Build a synthetic provider-correct error response (no network).
    /// Renders the exact status + envelope; sets Retry-After when present.
    fn sim_error_response(
        provider: &str,
        request: &ExecutionRequest,
        status: u16,
        body: serde_json::Value,
        retry_after: Option<u64>,
    ) -> ExecutionResponse {
        use reqwest::header::{HeaderMap, HeaderValue};
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if let Some(secs) = retry_after {
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                headers.insert(reqwest::header::RETRY_AFTER, v);
            }
        }
        let code = http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::BAD_REQUEST);
        let mut builder = http::Response::builder()
            .status(code)
            .header("content-type", "application/json");
        if let Some(secs) = retry_after {
            builder = builder.header("retry-after", secs.to_string());
        }
        let http_resp = builder.body(reqwest::Body::from(bytes)).unwrap();
        ExecutionResponse {
            response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
            url: format!("sim://{}/{}", provider, request.model),
            headers,
            transformed_body: body,
            transport: TransportKind::Reqwest,
        }
    }

    /// Build a synthetic SSE response (no network).
    fn sim_sse_response(
        provider: &str,
        request: &ExecutionRequest,
        body: String,
    ) -> ExecutionResponse {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        let byte_len = body.len();
        let http_resp = http::Response::builder()
            .status(http::StatusCode::OK)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .body(reqwest::Body::from(body))
            .unwrap();
        ExecutionResponse {
            response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
            url: format!("sim://{}/{}", provider, request.model),
            headers,
            transformed_body: serde_json::json!({"sim_sse": true, "bytes": byte_len}),
            transport: TransportKind::Reqwest,
        }
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

    /// Xiaomi MiMo Preview models (9router 73cb8914 xiaomi-mimo.js): Desktop-exclusive
    /// `mimo-x-pro-preview` / `mimo-x-flash-preview` are served by the account-service
    /// route on mimo-server-cn, authorized by the Xiaomi account session cookie
    /// (NOT the sk- key). Bare ids arrive as `xiaomi/<id>` via upstreamModelId.
    pub fn is_mimo_preview_model(model: &str) -> bool {
        let bare = model.split('/').next_back().unwrap_or(model);
        matches!(bare, "mimo-x-pro-preview" | "mimo-x-flash-preview")
    }

    const MIMO_PREVIEW_URL: &'static str =
        "https://mimo-server-cn.xiaomimimo.com/api/route/chat/completions";

    const MIMO_PREVIEW_UA: &'static str =
        "miNative PC/Normal Windows_NT/10.0.19045 SDKV/1.0.0 DEVT/PC DEVS/Windows APP/miaccount_desktop APPV/0.1.0";

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

        // Xiaomi MiMo Preview models live on the account-service route, which is not
        // one of the declared transports — resolve before the runtimeTransport path
        // (9router 73cb8914; cloud models keep default handling so Claude clients
        // still reach /anthropic/v1/messages).
        if (self.provider == "xiaomi-mimo" || self.provider == "mimo")
            && Self::is_mimo_preview_model(model)
        {
            return Ok(Self::MIMO_PREVIEW_URL.to_string());
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

        if self.config.base_url.contains("{accountId}")
            || self.config.base_url.contains("{project}")
            || self.config.base_url.contains("{location}")
        {
            let mut url = self.config.base_url.clone();
            if url.contains("{accountId}") {
                let account_id = compatible_value(
                    credentials.provider_specific_data.get("accountId"),
                )
                .ok_or(ExecutorError::MissingProviderSpecificData(
                    self.provider.clone(),
                    "accountId",
                ))?;
                url = url.replace("{accountId}", account_id);
            }
            if url.contains("{project}") {
                let project = compatible_value(credentials.provider_specific_data.get("project"))
                    .ok_or(ExecutorError::MissingProviderSpecificData(
                    self.provider.clone(),
                    "project",
                ))?;
                url = url.replace("{project}", project);
            }
            if url.contains("{location}") {
                let location = compatible_value(credentials.provider_specific_data.get("location"))
                    .ok_or(ExecutorError::MissingProviderSpecificData(
                        self.provider.clone(),
                        "location",
                    ))?;
                url = url.replace("{location}", location);
            }
            return Ok(url);
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
        } else if (self.provider == "xiaomi-mimo" || self.provider == "mimo")
            && Self::is_mimo_preview_model(model)
        {
            // Preview models authenticate with the account-session cookie, not the key
            // (9router 73cb8914). Cookie is resolved in chat.rs before execute and
            // carried on provider_specific_data (`mimoAccountCookie`).
            let cookie =
                compatible_value(credentials.provider_specific_data.get("mimoAccountCookie"))
                    .ok_or_else(|| ExecutorError::MissingCredentials(self.provider.clone()))?;
            headers.insert("Cookie", HeaderValue::from_str(cookie)?);
            headers.insert(
                reqwest::header::USER_AGENT,
                HeaderValue::from_static(Self::MIMO_PREVIEW_UA),
            );
            if !stream {
                headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
            } else {
                headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
            }
            return Ok(headers);
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
        } else if provider_allows_missing_credentials(&self.provider)
            && bearer_token(credentials).is_none()
        {
            // Intentionally no Authorization header: noAuth provider, no credential.
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
                // 9router parity (open-sse/executors/default.js HEADER_HOOKS.clineHeaders
                // + open-sse/shared/clineAuth.js buildClineHeaders): the hook overlays
                // the Cline client headers. Hooks run BEFORE auth in JS, so the
                // generic Bearer Authorization above stands (verbatim token); only
                // the client-identifying headers are overlaid here.
                //
                // WorkOS JWT prefix (clineAuth.js getClineAccessToken): Cline OAuth
                // access tokens are WorkOS JWTs (base64url `eyJ…` header) and must
                // be sent as `Bearer workos:<jwt>`. ClinePass API keys (e.g.
                // `clp_…`) are NOT JWTs and go verbatim — prefixing them makes
                // the Cline API 401. Replace the generic Bearer set above.
                if let Some(raw) = credentials
                    .access_token
                    .as_deref()
                    .or(credentials.api_key.as_deref())
                {
                    let token = cline_access_token(raw);
                    if let Ok(val) = HeaderValue::from_str(&format!("Bearer {token}")) {
                        headers.insert(AUTHORIZATION, val);
                    }
                }
                headers.insert(
                    "User-Agent",
                    HeaderValue::from_str(&format!("OpenProxy/{}", env!("CARGO_PKG_VERSION")))?,
                );
                headers.insert("X-PLATFORM", HeaderValue::from_static(std::env::consts::OS));
                headers.insert("X-PLATFORM-VERSION", HeaderValue::from_static("rust"));
                headers.insert("X-CLIENT-TYPE", HeaderValue::from_static("openproxy"));
                headers.insert(
                    "X-CLIENT-VERSION",
                    HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
                );
                headers.insert(
                    "X-CORE-VERSION",
                    HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
                );
                headers.insert("X-IS-MULTIROOT", HeaderValue::from_static("false"));
            }
            // Per-model Anthropic-Beta flags (9router default.js:167-170 +
            // shared.js selectAnthropicBeta). anthropic-compatible nodes
            // serving a real Claude model sit in front of Anthropic itself,
            // so they need the same flags; the model id gates it so gateways
            // fronting other models are left untouched. Overwrites the
            // static default (which only had 2 flags).
            let is_claude_model = model.starts_with("claude-");
            if self.provider == "claude"
                || (self.provider.starts_with("anthropic-compatible") && is_claude_model)
            {
                if let Ok(val) = HeaderValue::from_str(&select_anthropic_beta(model)) {
                    headers.insert("anthropic-beta", val);
                }
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

        // Quirk: cerebras/mistral reject Anthropic's client_metadata field
        // (9router default.js dropClientMetadata — top-level delete only)
        if self.provider == "cerebras" || self.provider == "mistral" {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("client_metadata");
            }
        }

        // Strip unsupported request params for providers that don't support them
        strip_unsupported_params(&self.provider, model, &mut body);

        // Xiaomi MiMo Preview defaults (9router 73cb8914 transformRequest):
        // thinking/params get defaults only — never override the caller's values.
        if (self.provider == "xiaomi-mimo" || self.provider == "mimo")
            && Self::is_mimo_preview_model(model)
        {
            if let Some(obj) = body.as_object_mut() {
                if obj.get("thinking").is_none() {
                    obj.insert(
                        "thinking".to_string(),
                        serde_json::json!({ "type": "enabled" }),
                    );
                }
                if obj.get("temperature").is_none() {
                    obj.insert("temperature".to_string(), serde_json::json!(1.0));
                }
                if obj.get("top_p").is_none() {
                    obj.insert("top_p".to_string(), serde_json::json!(0.95));
                }
                let needs_max = obj
                    .get("max_tokens")
                    .and_then(|v| v.as_u64())
                    .is_none_or(|n| n == 0);
                if needs_max {
                    obj.insert("max_tokens".to_string(), serde_json::json!(4096));
                }
            }
        }

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
        // --- Simulation interception (bead sim-04, default-off) ---
        // Checked BEFORE any credential *use*: when mock is active, no signing,
        // no refresh, no network. (The credentials object is already resolved by
        // the caller; the branch guarantees it is never used.) Default
        // (unconfigured) is Real, so this block is unreachable in production.
        // A mock-only executor (openproxy-umtq) always simulates: it is built
        // only when the dispatch short-circuit already resolved the effective
        // mode to `mock`, so no further per-request signal is required.
        if self.sim_override.is_some() || Self::simulation_active(&request) {
            return self.execute_simulated(&request).await;
        }
        // sim-15: REAL-branch fault support (plan §2.4: injector wraps BOTH
        // branches). Parse once here. `sim_headers` NEVER reach upstream:
        // `build_headers`/`send_one` only see `headers` built from
        // credentials+config — request.sim_headers is a separate map that no
        // forward path reads. The parse below is the only REAL-branch use,
        // and sim headers are redacted from logs (never logged at info+).
        let real_fault = crate::core::simulation::FaultSpec::parse(&request.sim_headers);
        // Latency applies to REAL too (first-byte delay before send loop).
        crate::core::simulation::FaultInjector::apply_latency(&real_fault).await;
        // Build headers and transformed body once, reused across retries and
        // fallback URLs.
        let mut headers =
            self.build_headers(&request.model, &request.credentials, request.stream)?;
        let transformed_body = self.transform_request(&request.body, &request.model);

        // Try primary then fallback URLs.
        let urls = self.resolve_urls(&request.model, request.stream, &request.credentials);

        // Acquire semaphore for tokenrouter free models to limit concurrent requests to 1
        let _tokenrouter_permit = if self.provider == "tokenrouter"
            && (request.model == "qwen/qwen3.8-max-free"
                || request.model == "moonshotai/kimi-k3-free")
        {
            Some(TOKENROUTER_SEMAPHORE.acquire().await?)
        } else {
            None
        };

        for url in &urls {
            let use_hyper = self.use_hyper_transport(&request, url);

            // The retry loop for this URL.
            // One budget for the whole url: network exceptions and retryable
            // statuses share it, as in 9router.
            let max_attempts = 3u32;
            for retry in 0..max_attempts {
                // ONE budget for both failure kinds, as 9router has.
                //
                // The first version of this bead put a 3-attempt inner loop
                // around send_one while keeping the outer 0..3 status loop, so
                // a mixed case (network error, then 502, then network error...)
                // multiplied into 9 send_one calls and ~24s of sleeping — 3x
                // 9router's total, on a fix whose whole point was retrying too
                // LITTLE. 9router has a single counter: a fetch exception is
                // pushed through the 502 bucket and spends the same budget
                // (base.js:174).
                let upstream = match self
                    .send_one(url, &headers, &transformed_body, &request, use_hyper)
                    .await
                {
                    Ok(response) => response,
                    Err(err) => {
                        let (_, delay_ms) = retry_policy(http::StatusCode::BAD_GATEWAY);
                        if retry + 1 < max_attempts {
                            tracing::warn!(
                                target: "openproxy::executor",
                                provider = %self.provider,
                                attempt = retry + 1,
                                max_attempts,
                                delay_ms,
                                "network error, retrying"
                            );
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                            continue;
                        }
                        return Err(err);
                    }
                };
                let status = upstream.status();

                // Success: return immediately — unless a REAL-branch status
                // fault overrides it (sim-15: post-execution middleware proof).
                if status.is_success() {
                    if let Some((fstatus, fbody, fretry)) =
                        crate::core::simulation::FaultInjector::status_fault(
                            self.sim_format(),
                            &self.provider,
                            &real_fault,
                        )
                    {
                        return Ok(Self::sim_error_response(
                            &self.provider,
                            &request,
                            fstatus,
                            fbody,
                            fretry,
                        ));
                    }
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
                                .await?; // a single attempt: 9router re-enters its
                                         // own tryRetry for this leg, and the
                                         // status branch below still applies.
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

                // 429: tokenrouter free models get exponential backoff; other 429/404
                // follow 9router BaseExecutor.shouldRetry — retry only when another
                // fallback URL exists, otherwise surface raw response for retry-after
                // extraction and model-specific lock handling (404 modelLock_*).
                if matches!(
                    status,
                    http::StatusCode::TOO_MANY_REQUESTS | http::StatusCode::NOT_FOUND
                ) {
                    let is_tokenrouter_free = self.provider == "tokenrouter"
                        && (request.model == "qwen/qwen3.8-max-free"
                            || request.model == "moonshotai/kimi-k3-free");
                    if is_tokenrouter_free
                        && status == http::StatusCode::TOO_MANY_REQUESTS
                        && retry < 2
                    {
                        let delay_secs = 2u64.pow(retry as u32);
                        tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                        continue;
                    }
                    let has_next_url = urls.len() > 1 && url != urls.last().unwrap();
                    if !has_next_url {
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
                    break;
                }

                // 502 Bad Gateway: 3 retries x 3s, then surface the raw 502.
                if status == http::StatusCode::BAD_GATEWAY {
                    if retry + 1 < retry_policy(status).0 && url == urls.last().unwrap() {
                        tokio::time::sleep(Duration::from_millis(retry_policy(status).1)).await;
                        continue;
                    }
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

                // 503 Service Unavailable: 3 retries x 2s, then surface the raw 503.
                if status == http::StatusCode::SERVICE_UNAVAILABLE {
                    if retry + 1 < retry_policy(status).0 && url == urls.last().unwrap() {
                        tokio::time::sleep(Duration::from_millis(retry_policy(status).1)).await;
                        continue;
                    }
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

                // 504 Gateway Timeout: 2 retries x 3s
                if status == http::StatusCode::GATEWAY_TIMEOUT {
                    if retry + 1 < retry_policy(status).0 {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    }
                    // After 2 retries, fall through to next fallback URL.
                    break;
                }

                // Other non-success status: propagate the upstream error WITH
                // its body (JS base.js returns the raw response so handlers can
                // read quota text / RetryInfo / provider error codes — dropping
                // the body here blinded check_fallback_error's message matching).
                let body_text = upstream.text().await;
                let body_text = body_text.chars().take(2000).collect::<String>();
                return Err(ExecutorError::UpstreamStatus(
                    status,
                    if body_text.is_empty() {
                        format!("upstream returned {} for URL {}", status.as_u16(), url)
                    } else {
                        format!(
                            "upstream returned {} for URL {}: {}",
                            status.as_u16(),
                            url,
                            body_text
                        )
                    },
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
    /// Refresh credentials with retry, rotating the refresh_token between
    /// attempts (ported from 9router v0.5.45 fix(refresh): rotate refresh_token
    /// between retry attempts). Rotating-RT providers (xAI/grok-cli) issue a
    /// new refresh_token on every refresh; without in-place rotation the 2nd/3rd
    /// retry reuses the already-consumed RT → invalid_grant → auth_failed.
    async fn try_refresh_credentials(
        &self,
        credentials: &ProviderConnection,
    ) -> Option<ProviderConnection> {
        let mut working = credentials.clone();
        if working.refresh_token.as_deref().unwrap_or("").is_empty() {
            return None;
        }

        let provider = self.provider.clone();
        // Working copies of the rotated RT/AT, behind Arc<Mutex> so the Fn
        // closure can rotate them between retry attempts without moving fields
        // out of `working`; read back after the retry loop completes.
        let refresh_holder =
            std::sync::Arc::new(std::sync::Mutex::new(working.refresh_token.clone()));
        let access_holder =
            std::sync::Arc::new(std::sync::Mutex::new(working.access_token.clone()));
        let refresh_holder_inner = refresh_holder.clone();
        let access_holder_inner = access_holder.clone();
        let psd_for_attempt = working.provider_specific_data.clone();
        let attempt = move || {
            let provider = provider.clone();
            let refresh_holder = refresh_holder_inner.clone();
            let access_holder = access_holder_inner.clone();
            let psd = psd_for_attempt.clone();
            async move {
                let refresh_token = refresh_holder
                    .lock()
                    .map(|g| g.clone().unwrap_or_default())
                    .unwrap_or_default();
                let prior_refresh = refresh_holder.lock().map(|g| g.clone()).unwrap_or_default();
                let result = dispatch_oauth_refresh(&provider, &refresh_token, &psd).await?;
                if let Some(new_refresh) = result.refresh_token.clone() {
                    if Some(&new_refresh) != prior_refresh.as_ref() {
                        if let Ok(mut guard) = refresh_holder.lock() {
                            *guard = Some(new_refresh);
                        }
                        if let Ok(mut guard) = access_holder.lock() {
                            *guard = Some(result.access_token.clone());
                        }
                    }
                }
                Ok(result)
            }
        };

        match crate::oauth::token_refresh::refresh_with_retry(attempt).await {
            Ok(result) => {
                // The closure may have rotated credentials mid-loop; prefer the
                // freshest values from the holders.
                let rotated_access = access_holder.lock().ok().and_then(|g| g.clone());
                let rotated_refresh = refresh_holder.lock().ok().and_then(|g| g.clone());
                let mut updated = working;
                updated.access_token = rotated_access.or(Some(result.access_token));
                updated.refresh_token = result.refresh_token.clone().or(rotated_refresh);
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

fn bearer_token(credentials: &ProviderConnection) -> Option<&str> {
    non_empty_option(credentials.access_token.as_deref())
        .or_else(|| non_empty_option(credentials.api_key.as_deref()))
}

/// 9router open-sse/shared/clineAuth.js getClineAccessToken: Cline OAuth
/// access tokens are WorkOS JWTs (base64url `eyJ…` header) and must be sent
/// as `workos:<jwt>`. Anything already prefixed (case-insensitive) or not
/// a JWT (e.g. ClinePass `clp_…` keys) goes verbatim.
fn cline_access_token(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("workos:") {
        return trimmed.to_string();
    }
    let mut parts = trimmed.split('.');
    let (h, b) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let is_jwt = !h.is_empty()
        && !b.is_empty()
        && h.len() >= 3
        && h.as_bytes()[..3] == *b"eyJ"
        && h.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    if is_jwt {
        format!("workos:{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// Providers whose upstream accepts unauthenticated requests (dashboard
/// `noAuth: true`). They must reach the upstream without an Authorization
/// header instead of failing with `MissingCredentials`.
fn provider_allows_missing_credentials(provider: &str) -> bool {
    matches!(provider, "opencode-zen")
}

/// 9router open-sse/executors/opencode-go.js MESSAGES_FORMAT_MODELS — these
/// route to `${BASE}/messages` with `x-api-key` + `anthropic-version` headers.
fn opencode_go_uses_claude_format(model: &str) -> bool {
    matches!(
        model,
        "minimax-m3"
            | "minimax-m2.7"
            | "minimax-m2.5"
            | "qwen3.7-max"
            | "qwen3.7-plus"
            | "qwen3.6-plus"
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn simulation_gate_matches_env_force() {
        // Default-off contract (bead sim-04): no sim header and (in CI) no
        // OPENPROXY_DEV_MOCK env -> simulation_active is false, so execute()
        // takes the REAL path. Asserts the gate directly (no network).
        // NOTE: if the developer exports OPENPROXY_DEV_MOCK=1 locally this
        // test correctly fails — the gate IS active then.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o", "messages": []}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: HeaderMap::new(),
            force_mock: false,
        };
        let env_force = std::env::var("OPENPROXY_DEV_MOCK")
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        assert_eq!(DefaultExecutor::simulation_active(&req), env_force);
    }

    #[tokio::test]
    async fn simulated_tool_echo_e2e() {
        // sim-08: tools in body -> tool_calls + finish_reason tool_calls.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "search"}],
                "tools": [{"type": "function",
                    "function": {"name": "web_search", "parameters": {}}}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim tool execute");
        assert_eq!(
            resp.transformed_body["choices"][0]["finish_reason"],
            "tool_calls"
        );
    }

    #[tokio::test]
    async fn simulated_override_text_non_stream() {
        // sim-14 §5.1.1: plain string -> message content (suppresses echo).
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert(
                    "x-openproxy-sim-response",
                    HeaderValue::from_static("custom answer"),
                );
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim override");
        assert_eq!(
            resp.transformed_body["choices"][0]["message"]["content"],
            "custom answer"
        );
        // Envelope intact (ids, usage, finish_reason).
        assert_eq!(resp.transformed_body["choices"][0]["finish_reason"], "stop");
        assert!(
            resp.transformed_body["usage"]["total_tokens"]
                .as_u64()
                .unwrap()
                > 0
        );
    }

    #[tokio::test]
    async fn simulated_override_json_verbatim_non_stream() {
        // sim-14 §5.1.1: JSON object + non-stream → verbatim body.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert(
                    "x-openproxy-sim-response",
                    HeaderValue::from_static("{\"custom\":true}"),
                );
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim json override");
        assert_eq!(resp.transformed_body, serde_json::json!({"custom": true}));
    }

    #[tokio::test]
    async fn simulated_override_text_stream_chunks() {
        // sim-14 §5.1.1: stream value replaces content payload ONLY — framing
        // stays simulator-generated (chunks + [DONE], no raw injection).
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: true,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert(
                    "x-openproxy-sim-response",
                    HeaderValue::from_static("overridden stream"),
                );
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim stream override");
        let text = resp.response.text().await;
        assert!(text.contains("overridden"), "override payload chunked");
        assert!(!text.contains("Echo:"), "echo suppressed");
        assert!(text.ends_with("data: [DONE]\n\n"), "framing intact");
    }

    #[tokio::test]
    async fn simulated_override_malformed_400() {
        // sim-14 §5.1.1: malformed JSON-looking value → provider-correct 400.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert(
                    "x-openproxy-sim-response",
                    HeaderValue::from_static("{\"broken\": "),
                );
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim malformed renders");
        assert_eq!(resp.response.status(), http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn simulated_disconnect_truncates_stream() {
        // sim-14: disconnect-after-N keeps frame boundaries, drops [DONE],
        // client sees a cut stream (no hang — body completes, just short).
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hello world one two three"}]}),
            stream: true,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert(
                    "x-openproxy-sim-disconnect-after-chunks",
                    HeaderValue::from_static("1"),
                );
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim disconnect");
        let text = resp.response.text().await;
        assert!(!text.contains("[DONE]"), "terminal dropped");
        assert!(text.contains("data:"), "partial frames kept");
        // Full stream would be longer; truncated must be a strict prefix shape.
        assert!(
            text.matches("data:").count() < 8,
            "cut short, got {}",
            text.matches("data:").count()
        );
    }

    #[tokio::test]
    async fn simulated_override_tool_non_stream_no_leak() {
        // sim-14 fix (reviewer): tool override replaces echo AND leaves no
        // sim_* internals in the client-visible body.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [{"type": "function",
                    "function": {"name": "echo_tool", "parameters": {}}}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert(
                    "x-openproxy-sim-response",
                    HeaderValue::from_static(
                        "{\"tool_calls\":[{\"id\":\"call_9\",\"type\":\"function\",\"function\":{\"name\":\"override_tool\",\"arguments\":\"{}\"}}]}",
                    ),
                );
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim tool override");
        assert_eq!(
            resp.transformed_body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "override_tool",
            "override replaces echo"
        );
        let raw = serde_json::to_string(&resp.transformed_body).unwrap();
        assert!(!raw.contains("sim_tool_calls"), "no internal leak");
        assert!(!raw.contains("echo_tool"), "echo replaced");
    }

    #[tokio::test]
    async fn simulated_override_tool_anthropic_stream() {
        // sim-14 fix (reviewer): Anthropic stream override renders the override
        // tool name (not the echo) through named events.
        let req = ExecutionRequest {
            model: "claude-sonnet-4-6".into(),
            body: serde_json::json!({"model": "claude-sonnet-4-6", "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [{"name": "echo_tool", "description": "e",
                    "input_schema": {"type": "object"}}]}),
            stream: true,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert(
                    "x-openproxy-sim-response",
                    HeaderValue::from_static(
                        "{\"tool_use\":{\"id\":\"toolu_9\",\"name\":\"override_tool\",\"input\":{}}}",
                    ),
                );
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("anthropic", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec
            .execute(req)
            .await
            .expect("sim anthropic tool override");
        let text = resp.response.text().await;
        assert!(text.contains("override_tool"), "override name rendered");
        assert!(!text.contains("echo_tool"), "echo replaced");
    }

    #[tokio::test]
    async fn simulated_unknown_model_renders_404() {
        // sim-08: Validation renders as HTTP 404 + envelope, not Err/500.
        let req = ExecutionRequest {
            model: "gpt-999".into(),
            body: serde_json::json!({"model": "gpt-999",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim 404 renders");
        assert_eq!(resp.response.status(), http::StatusCode::NOT_FOUND);
        assert_eq!(resp.transformed_body["error"]["code"], "model_not_found");
    }

    #[tokio::test]
    async fn simulated_status_fault_429_e2e() {
        // sim-12: x-openproxy-sim-status:429 short-circuits with provider-correct
        // 429 envelope; Retry-After visible on BOTH sidecar headers and body.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert("x-openproxy-sim-status", HeaderValue::from_static("429"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim 429 renders");
        assert_eq!(resp.response.status(), http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.transformed_body["error"]["type"], "rate_limit_error");
        assert!(resp.headers.contains_key(reqwest::header::RETRY_AFTER));
        assert!(resp
            .response
            .headers()
            .contains_key(reqwest::header::RETRY_AFTER));
    }

    #[tokio::test]
    async fn simulated_status_fault_invalid_ignored() {
        // sim-12: non-allowlisted status is ignored -> normal echo path.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert("x-openproxy-sim-status", HeaderValue::from_static("418"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim ignores 418");
        assert_eq!(resp.response.status(), http::StatusCode::OK);
        assert_eq!(resp.transformed_body["object"], "chat.completion");
    }

    #[tokio::test]
    async fn simulated_fault_beats_validation_precedence() {
        // sim-12 review lock: fault status wins over validation — unknown model
        // + fault 429 renders 429 (not 404). Covers non-stream and stream.
        for stream in [false, true] {
            let req = ExecutionRequest {
                model: "gpt-999".into(),
                body: serde_json::json!({"model": "gpt-999",
                    "messages": [{"role": "user", "content": "hi"}]}),
                stream,
                credentials: ProviderConnection::default(),
                proxy: None,
                sim_headers: {
                    let mut h = HeaderMap::new();
                    h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                    h.insert("x-openproxy-sim-status", HeaderValue::from_static("429"));
                    h
                },
                force_mock: false,
            };
            let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
            let resp = exec.execute(req).await.expect("sim fault precedence");
            assert_eq!(
                resp.response.status(),
                http::StatusCode::TOO_MANY_REQUESTS,
                "stream={stream}"
            );
            assert_eq!(
                resp.transformed_body["error"]["type"], "rate_limit_error",
                "stream={stream}"
            );
        }
    }

    #[tokio::test]
    async fn simulated_latency_delays_first_byte() {
        // sim-13: latency header delays every mock outcome (echo path here).
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert(
                    "x-openproxy-sim-latency-ms",
                    HeaderValue::from_static("150"),
                );
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let t0 = std::time::Instant::now();
        let resp = exec.execute(req).await.expect("sim latency echo");
        assert!(
            t0.elapsed() >= std::time::Duration::from_millis(120),
            "first byte delayed"
        );
        assert_eq!(resp.response.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn simulated_latency_wraps_fault_path() {
        // sim-13 (reviewer sim-12 note): latency wraps the fault-error path too
        // (fault 429 + latency → delay, then 429).
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h.insert("x-openproxy-sim-status", HeaderValue::from_static("429"));
                h.insert(
                    "x-openproxy-sim-latency-ms",
                    HeaderValue::from_static("150"),
                );
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let t0 = std::time::Instant::now();
        let resp = exec.execute(req).await.expect("sim latency fault");
        assert!(
            t0.elapsed() >= std::time::Duration::from_millis(120),
            "fault delayed"
        );
        assert_eq!(resp.response.status(), http::StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn simulated_force_mock_without_header() {
        // Resolver-wiring (follow-up openproxy-1ycq): configured-mode mock
        // activates via force_mock even with no header and no credentials.
        // This is what the chat stub gate + CLI set after their DB lookup.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "wired"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: HeaderMap::new(),
            force_mock: true,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("force_mock serves");
        assert!(resp.url.starts_with("sim://"));
        assert_eq!(
            resp.transformed_body["choices"][0]["message"]["content"],
            "Echo: wired"
        );
        // And force_mock:false + no header stays real-path (would need creds;
        // assert the gate directly instead of hitting network).
        let req2 = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "wired"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: HeaderMap::new(),
            force_mock: false,
        };
        assert!(!DefaultExecutor::simulation_active(&req2));
    }

    #[tokio::test]
    async fn simulated_hostile_credentials_still_mock() {
        // sim-16 (plan §4 normative): mock branch NEVER touches credentials.
        // Expired OAuth + garbage key + garbage tokens -> 200 in all 3 formats.
        // Audit: execute_simulated + helpers contain zero credential reads;
        // build_headers/try_refresh run only on the REAL branch after the gate.
        fn hostile() -> ProviderConnection {
            let mut c = ProviderConnection::default();
            c.api_key = Some("sk-invalid-garbage".into());
            c.access_token = Some("expired-token".into());
            c.refresh_token = Some("dead-refresh".into());
            c.expires_at = Some("2000-01-01T00:00:00Z".into());
            c
        }
        fn sim_headers() -> HeaderMap {
            let mut h = HeaderMap::new();
            h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
            h
        }
        // OpenAI non-stream.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: false,
            credentials: hostile(),
            proxy: None,
            sim_headers: sim_headers(),
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("mock ignores creds");
        assert_eq!(resp.response.status(), http::StatusCode::OK);
        assert_eq!(
            resp.transformed_body["choices"][0]["message"]["content"],
            "Echo: hi"
        );
        // Anthropic stream.
        let req = ExecutionRequest {
            model: "claude-sonnet-4-6".into(),
            body: serde_json::json!({"model": "claude-sonnet-4-6", "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: true,
            credentials: hostile(),
            proxy: None,
            sim_headers: sim_headers(),
            force_mock: false,
        };
        let exec = DefaultExecutor::new("anthropic", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("mock ignores creds");
        assert_eq!(resp.response.status(), http::StatusCode::OK);
        let text = resp.response.text().await;
        assert!(text.contains("event: message_start"), "named events");
        // Gemini non-stream.
        let req = ExecutionRequest {
            model: "gemini-2.5-flash".into(),
            body: serde_json::json!({"contents": [{"parts": [{"text": "hi"}]}]}),
            stream: false,
            credentials: hostile(),
            proxy: None,
            sim_headers: sim_headers(),
            force_mock: false,
        };
        let exec = DefaultExecutor::new("gemini", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("mock ignores creds");
        assert_eq!(resp.response.status(), http::StatusCode::OK);
        assert_eq!(
            resp.transformed_body["candidates"][0]["content"]["parts"][0]["text"],
            "Echo: hi"
        );
    }

    #[tokio::test]
    async fn simulated_gemini_non_stream_e2e() {
        // sim-10: gemini provider + header -> candidates envelope, no creds.
        let req = ExecutionRequest {
            model: "gemini-2.5-flash".into(),
            body: serde_json::json!({"contents": [{"parts": [{"text": "ping"}]}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("gemini", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim gemini");
        assert!(resp.url.starts_with("sim://gemini/"));
        assert_eq!(
            resp.transformed_body["candidates"][0]["content"]["parts"][0]["text"],
            "Echo: ping"
        );
        assert_eq!(resp.response.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn simulated_gemini_stream_e2e() {
        // sim-10: Gemini-shape SSE chunks flow through the real path.
        let req = ExecutionRequest {
            model: "gemini-2.5-flash".into(),
            body: serde_json::json!({"contents": [{"parts": [{"text": "hi there"}]}]}),
            stream: true,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("gemini", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim gemini stream");
        let text = resp.response.text().await;
        assert!(text.contains("candidates"), "gemini chunks");
        assert!(text.contains("STOP"), "terminal finish");
    }

    #[tokio::test]
    async fn simulated_stream_mode_validation_rejected() {
        // sim-10 (reviewer sim-08 nit): stream + unknown model -> JSON error,
        // not SSE. Covers the stream-mode validation gap for OpenAI path.
        let req = ExecutionRequest {
            model: "gpt-999".into(),
            body: serde_json::json!({"model": "gpt-999",
                "messages": [{"role": "user", "content": "hi"}]}),
            stream: true,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim stream 404 renders");
        assert_eq!(resp.response.status(), http::StatusCode::NOT_FOUND);
        assert!(
            !resp.transformed_body.to_string().contains("data: "),
            "no SSE"
        );
    }

    #[tokio::test]
    async fn simulated_anthropic_non_stream_e2e() {
        // sim-09: anthropic provider + header -> message envelope, no creds.
        let req = ExecutionRequest {
            model: "claude-sonnet-4-6".into(),
            body: serde_json::json!({"model": "claude-sonnet-4-6", "max_tokens": 64,
            "messages": [{"role": "user", "content": "ping"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("anthropic", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim anthropic");
        assert!(resp.url.starts_with("sim://anthropic/"));
        assert_eq!(resp.transformed_body["type"], "message");
        assert_eq!(resp.transformed_body["content"][0]["text"], "Echo: ping");
        assert_eq!(resp.response.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn simulated_anthropic_stream_e2e() {
        // sim-09: named SSE events flow through the real downstream path.
        let req = ExecutionRequest {
            model: "claude-sonnet-4-6".into(),
            body: serde_json::json!({"model": "claude-sonnet-4-6", "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi there"}]}),
            stream: true,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("anthropic", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim anthropic stream");
        let text = resp.response.text().await;
        assert!(text.contains("event: message_start"), "named events");
        assert!(text.contains("event: message_stop"), "terminal event");
    }

    #[tokio::test]
    async fn simulated_execute_non_stream_e2e() {
        // sim-07: execute() with sim header takes the MOCK branch end-to-end
        // (no network, no credentials) and returns a well-formed envelope.
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "ping"}]}),
            stream: false,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim execute");
        assert!(resp.url.starts_with("sim://"));
        assert_eq!(resp.transformed_body["object"], "chat.completion");
        assert_eq!(
            resp.transformed_body["choices"][0]["message"]["content"],
            "Echo: ping"
        );
        assert_eq!(resp.response.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn simulated_execute_stream_e2e() {
        // sim-07: stream=true returns SSE body with frames + [DONE].
        let req = ExecutionRequest {
            model: "gpt-4o".into(),
            body: serde_json::json!({"model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi there"}],
                "stream_options": {"include_usage": true}}),
            stream: true,
            credentials: ProviderConnection::default(),
            proxy: None,
            sim_headers: {
                let mut h = HeaderMap::new();
                h.insert("x-openproxy-sim", HeaderValue::from_static("mock"));
                h
            },
            force_mock: false,
        };
        let exec = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let resp = exec.execute(req).await.expect("sim stream execute");
        assert!(resp.url.starts_with("sim://"));
        let text = resp.response.text().await;
        assert!(text.contains("chat.completion.chunk"), "sse frames");
        assert!(text.contains("usage"), "usage chunk");
        assert!(
            text.ends_with(
                "data: [DONE]

"
            ),
            "DONE terminal"
        );
    }

    #[test]
    fn test_opencode_go_claude_format_models() {
        // 9router opencode-go.js MESSAGES_FORMAT_MODELS — all six must use
        // the claude (/messages) format, not /chat/completions.
        for model in [
            "minimax-m3",
            "minimax-m2.7",
            "minimax-m2.5",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3.6-plus",
        ] {
            assert!(
                opencode_go_uses_claude_format(model),
                "{model} should use claude format"
            );
        }

        // Non-members keep the openai format.
        assert!(!opencode_go_uses_claude_format("qwen3.6"));
        assert!(!opencode_go_uses_claude_format("minimax-m1"));
        assert!(!opencode_go_uses_claude_format("gpt-4o"));
    }

    #[test]
    fn drops_client_metadata_for_cerebras() {
        // 9router default.js dropClientMetadata quirk (cerebras.js quirks).
        let executor = DefaultExecutor::new("cerebras", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "client_metadata": { "ideType": 9 },
            "messages": []
        });
        let transformed = executor.transform_request(&body, "llama-3.3-70b");
        assert!(
            !transformed
                .as_object()
                .unwrap()
                .contains_key("client_metadata"),
            "cerebras must drop client_metadata"
        );
        assert_eq!(transformed["messages"], serde_json::json!([]));
    }

    #[test]
    fn drops_client_metadata_for_mistral() {
        // 9router default.js dropClientMetadata quirk (mistral.js quirks).
        let executor = DefaultExecutor::new("mistral", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "client_metadata": { "ideType": 9 },
            "messages": []
        });
        let transformed = executor.transform_request(&body, "mistral-large-latest");
        assert!(
            !transformed
                .as_object()
                .unwrap()
                .contains_key("client_metadata"),
            "mistral must drop client_metadata"
        );
        assert_eq!(transformed["messages"], serde_json::json!([]));
    }

    #[test]
    fn keeps_client_metadata_for_openai() {
        // No dropClientMetadata quirk — the field must survive (JS parity).
        let executor = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "client_metadata": { "ideType": 9 },
            "messages": []
        });
        let transformed = executor.transform_request(&body, "gpt-4o");
        assert_eq!(
            transformed["client_metadata"],
            serde_json::json!({ "ideType": 9 }),
            "openai must keep client_metadata"
        );
    }

    #[test]
    fn test_default_opencode_go_base_url() {
        // 9router parity: opencode-go base URL must include the /go segment
        // (JS open-sse/executors/opencode-go.js BASE = "https://opencode.ai/zen/go/v1").
        let executor =
            DefaultExecutor::new("opencode-go", Arc::new(ClientPool::new()), None).unwrap();
        let creds = ProviderConnection::default();
        // Non-claude model → /chat/completions under the /go base.
        let url = executor.build_url("qwen3.6", false, &creds).unwrap();
        assert_eq!(
            url, "https://opencode.ai/zen/go/v1/chat/completions",
            "opencode-go default URL must include the /go segment"
        );
        // Claude-format model → /messages under the /go base.
        let url = executor.build_url("minimax-m3", false, &creds).unwrap();
        assert_eq!(
            url, "https://opencode.ai/zen/go/v1/messages",
            "claude-format opencode-go URL must include /go"
        );
    }

    fn zen_executor() -> DefaultExecutor {
        DefaultExecutor::new("opencode-zen", Arc::new(ClientPool::new()), None)
            .expect("opencode-zen must be a supported provider")
    }

    #[test]
    fn opencode_zen_posts_to_live_zen_chat_endpoint() {
        // Live-verified: POST https://opencode.ai/zen/v1/chat/completions → 200.
        let url = zen_executor()
            .build_url("gpt-5.6-sol", false, &ProviderConnection::default())
            .unwrap();
        assert_eq!(url, "https://opencode.ai/zen/v1/chat/completions");
    }

    #[test]
    fn opencode_zen_omits_authorization_without_credentials() {
        // noAuth provider: an unauthenticated POST is accepted upstream, so a
        // credential-less connection must not fail with MissingCredentials.
        let headers = zen_executor()
            .build_headers("hy3-free", &ProviderConnection::default(), false)
            .expect("noAuth provider must build headers without credentials");
        assert!(!headers.contains_key(AUTHORIZATION));
    }

    #[test]
    fn opencode_zen_sends_bearer_when_credential_present() {
        let credentials = ProviderConnection {
            api_key: Some("zen-key".to_string()),
            ..ProviderConnection::default()
        };
        let headers = zen_executor()
            .build_headers("hy3-free", &credentials, false)
            .unwrap();
        assert_eq!(headers[AUTHORIZATION], "Bearer zen-key");
    }

    #[test]
    fn openrouter_sends_attribution_headers_and_gateways_omit_them() {
        let credentials = ProviderConnection {
            api_key: Some("sk-or-test".to_string()),
            ..ProviderConnection::default()
        };
        let openrouter =
            DefaultExecutor::new("openrouter", Arc::new(ClientPool::new()), None).unwrap();
        let headers = openrouter
            .build_headers("meta/llama-3.1-8b-instruct:free", &credentials, false)
            .unwrap();
        assert_eq!(headers["HTTP-Referer"], "https://endpoint-proxy.local");
        assert_eq!(headers["X-Title"], "Endpoint Proxy");

        // OpenRouter-fronted gateways must not claim OpenRouter attribution.
        for provider in ["kilocode", "nvidia"] {
            let executor = DefaultExecutor::new(provider, Arc::new(ClientPool::new()), None);
            if let Ok(executor) = executor {
                let headers = executor
                    .build_headers("tencent/hy3:free", &credentials, false)
                    .unwrap();
                assert!(
                    !headers.contains_key("HTTP-Referer"),
                    "{provider} must omit HTTP-Referer"
                );
            }
        }
    }

    #[test]
    fn cline_access_token_prefixes_workos_jwt_only() {
        // OAuth JWT -> prefixed.
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2ln";
        assert_eq!(cline_access_token(jwt), format!("workos:{jwt}"));
        // Already prefixed (any case) -> verbatim.
        assert_eq!(
            cline_access_token("workos:eyJhYmM.def"),
            "workos:eyJhYmM.def"
        );
        assert_eq!(
            cline_access_token("WORKOS:eyJhYmM.def"),
            "WORKOS:eyJhYmM.def"
        );
        // ClinePass API key -> verbatim.
        assert_eq!(cline_access_token("clp_abc123"), "clp_abc123");
        assert_eq!(cline_access_token("sk-plain"), "sk-plain");
        assert_eq!(cline_access_token("  "), "");
    }

    #[test]
    fn kilocode_posts_to_live_openrouter_gateway_endpoint() {
        // Live-verified: POST https://api.kilo.ai/api/openrouter/chat/completions → 200.
        let executor = DefaultExecutor::new("kilocode", Arc::new(ClientPool::new()), None).unwrap();
        let credentials = ProviderConnection {
            api_key: Some("kc-test".to_string()),
            ..ProviderConnection::default()
        };
        let url = executor
            .build_url("tencent/hy3:free", false, &credentials)
            .unwrap();
        assert_eq!(url, "https://api.kilo.ai/api/openrouter/chat/completions");
    }

    #[test]
    fn xiaomi_mimo_preview_routing() {
        // 9router 73cb8914 executor parity: Preview models → account-service
        // route regardless of runtime transport; bare `xiaomi/<id>` refs match too.
        let executor =
            DefaultExecutor::new("xiaomi-mimo", Arc::new(ClientPool::new()), None).unwrap();
        let creds = ProviderConnection::default();
        let expected = "https://mimo-server-cn.xiaomimimo.com/api/route/chat/completions";
        assert_eq!(
            executor
                .build_url("mimo-x-pro-preview", true, &creds)
                .unwrap(),
            expected
        );
        assert_eq!(
            executor
                .build_url("xiaomi/mimo-x-flash-preview", true, &creds)
                .unwrap(),
            expected
        );
        assert!(DefaultExecutor::is_mimo_preview_model(
            "xiaomi/mimo-x-pro-preview"
        ));
        assert!(!DefaultExecutor::is_mimo_preview_model("mimo-v2.5-pro"));

        // Cloud models keep default handling (openai transport endpoint).
        let url = executor.build_url("mimo-v2.5-pro", true, &creds).unwrap();
        assert_eq!(url, "https://api.xiaomimimo.com/v1/chat/completions");
    }

    #[test]
    fn xiaomi_mimo_preview_headers_use_cookie() {
        // Preview calls authenticate with the account cookie, not the key.
        let executor =
            DefaultExecutor::new("xiaomi-mimo", Arc::new(ClientPool::new()), None).unwrap();
        let mut psd = std::collections::BTreeMap::new();
        psd.insert(
            "mimoAccountCookie".to_string(),
            serde_json::json!("serviceToken=abc"),
        );
        let creds = ProviderConnection {
            api_key: Some("sk-x".to_string()),
            provider_specific_data: psd,
            ..ProviderConnection::default()
        };
        let headers = executor
            .build_headers("mimo-x-pro-preview", &creds, true)
            .unwrap();
        assert_eq!(headers["Cookie"], "serviceToken=abc");
        assert!(!headers.contains_key(AUTHORIZATION));

        // Cloud calls keep the bearer key.
        let creds = ProviderConnection {
            api_key: Some("sk-x".to_string()),
            ..ProviderConnection::default()
        };
        let headers = executor
            .build_headers("mimo-v2.5-pro", &creds, true)
            .unwrap();
        assert_eq!(headers[AUTHORIZATION], "Bearer sk-x");
    }

    #[test]
    fn xiaomi_mimo_preview_defaults_do_not_override() {
        // Preview defaults fill missing fields only (9router transformRequest).
        let executor =
            DefaultExecutor::new("xiaomi-mimo", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2
        });
        let out = executor.transform_request(&body, "mimo-x-pro-preview");
        assert_eq!(out["temperature"], serde_json::json!(0.2));
        assert_eq!(out["top_p"], serde_json::json!(0.95));
        assert_eq!(out["max_tokens"], serde_json::json!(4096));
        assert_eq!(out["thinking"], serde_json::json!({"type": "enabled"}));

        let body = serde_json::json!({"messages": [{"role": "user", "content": "hi"}]});
        let out = executor.transform_request(&body, "mimo-v2.5-pro");
        assert!(out.get("thinking").is_none());
        assert!(out.get("max_tokens").is_none());
    }

    #[test]
    fn codebuddy_cn_base_url_matches_js_registry() {
        // 9router open-sse/providers/registry/codebuddy-cn.js:22 —
        // https://copilot.tencent.com/v2/chat/completions (NOT api.codebuddy.cn).
        assert_eq!(
            provider_config_base_url("codebuddy-cn"),
            Some("https://copilot.tencent.com/v2/chat/completions".to_string())
        );
    }

    #[test]
    fn alitp_intl_base_url_matches_chat_transport() {
        // src/core/chat/mod.rs:321 transport baseUrl (Alibaba Token Plan
        // Singapore-only, OpenAI-compatible) — the map entry must match it.
        assert_eq!(
            provider_config_base_url("alitp-intl"),
            Some(
                "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/chat/completions"
                    .to_string()
            )
        );
    }

    #[test]
    fn mimo_free_base_url_matches_js_registry() {
        // 9router registry/mimo-free.js transport.baseUrl (hidden:true —
        // free channel ended). The dedicated MimoFreeExecutor is source of
        // truth for dispatch (MIMO_CHAT_URL); this map entry is fallback only.
        assert_eq!(
            provider_config_base_url("mimo-free"),
            Some("https://api.xiaomimimo.com/api/free-ai/openai/chat".to_string())
        );
    }

    #[test]
    fn xiaomi_tokenplan_region_routing_matches_js_registry() {
        // 9router registry/xiaomi-tokenplan.js transport.regions — the map
        // entry must stay the sgp default (a duplicate key once shadowed it
        // with tokenplan.xiaomi.com); build_url resolves regions per-connection.
        assert_eq!(
            provider_config_base_url("xiaomi-tokenplan"),
            Some("https://token-plan-sgp.xiaomimimo.com/v1/chat/completions".to_string())
        );
        let executor =
            DefaultExecutor::new("xiaomi-tokenplan", Arc::new(ClientPool::new()), None).unwrap();
        for (region, host) in [
            ("sgp", "token-plan-sgp.xiaomimimo.com"),
            ("cn", "token-plan-cn.xiaomimimo.com"),
            ("ams", "token-plan-ams.xiaomimimo.com"),
        ] {
            let mut psd = std::collections::BTreeMap::new();
            psd.insert("region".to_string(), serde_json::json!(region));
            let creds = ProviderConnection {
                provider_specific_data: psd,
                ..ProviderConnection::default()
            };
            let url = executor.build_url("mimo-v2.5-pro", true, &creds).unwrap();
            assert!(
                url.contains(host),
                "region {region} must route to {host}, got {url}"
            );
        }
    }
}

#[cfg(test)]
mod retry_policy_tests {
    use super::retry_policy;
    // `use hyper::http;` exists at file scope; a child module reaches it via super.
    use super::http;

    /// Ported from 9router `config/runtimeConfig.js:78-83`
    /// (`DEFAULT_RETRY_CONFIG`). "attempts" counts TOTAL attempts.
    #[test]
    fn the_policy_matches_9router_default_retry_config() {
        assert_eq!(retry_policy(http::StatusCode::BAD_GATEWAY), (3, 3_000));
        assert_eq!(
            retry_policy(http::StatusCode::SERVICE_UNAVAILABLE),
            (3, 2_000)
        );
        assert_eq!(retry_policy(http::StatusCode::GATEWAY_TIMEOUT), (2, 3_000));
        // 429 is NOT in 9router's table: retrying a rate-limited request
        // immediately deepens the limit. It advances to the next URL instead
        // (base.js:84 - shouldRetry fires only when another url exists).
        assert_eq!(retry_policy(http::StatusCode::TOO_MANY_REQUESTS), (0, 0));
    }

    /// 9router maps a fetch/network exception onto the 502 bucket
    /// (base.js:174), so a refused connection or TLS error deserves the same
    /// three attempts a 502 gets. That is the defect this bead fixes: the old
    /// `?` made a single network failure fatal while a 502 was retried.
    #[test]
    fn network_errors_use_the_502_budget() {
        assert_eq!(retry_policy(http::StatusCode::BAD_GATEWAY), (3, 3_000));
    }

    /// Every other status is terminal on the same url.
    #[test]
    fn other_statuses_are_not_retried_on_the_same_url() {
        for code in [
            http::StatusCode::OK,
            http::StatusCode::UNAUTHORIZED,
            http::StatusCode::FORBIDDEN,
            http::StatusCode::NOT_FOUND,
            http::StatusCode::INTERNAL_SERVER_ERROR,
            http::StatusCode::IM_A_TEAPOT,
        ] {
            assert_eq!(
                retry_policy(code),
                (0, 0),
                "{code} must not be retried on the same url"
            );
        }
    }
}
