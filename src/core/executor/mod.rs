mod antigravity;
mod api_key;
mod azure;
mod client_pool;
mod codex;
mod commandcode;
mod cursor;
mod default;
mod gemini_cli;
mod github;
mod grok_web;
mod iflow;
mod kiro;
mod ollama;
mod opencode;
mod opencode_go;
mod provider;
mod qoder;
mod qwen;
mod vertex;

pub use api_key::{
    get_api_key_provider_config, is_api_key_provider, ApiKeyExecutionRequest, ApiKeyExecutor,
    ApiKeyExecutorError, ApiKeyExecutorResponse,
};
pub use antigravity::{
    AntigravityExecutionRequest, AntigravityExecutor, AntigravityExecutorError,
    AntigravityExecutorResponse, ANTIGRAVITY_BASE_URL,
};
pub use azure::{
    AzureExecutionRequest, AzureExecutor, AzureExecutorError, AzureExecutorResponse,
};
pub use client_pool::{
    ClientPool, DirectHyperClient, CLIENT_POOL_IDLE_TIMEOUT, CLIENT_POOL_MAX_IDLE_PER_HOST,
    CLIENT_POOL_TCP_KEEPALIVE,
};
pub use codex::{
    convert_openai_sse_to_standard, CodexExecutionRequest, CodexExecutor, CodexExecutorError,
    CodexExecutorResponse,
};
pub use commandcode::{
    CommandCodeExecutionRequest, CommandCodeExecutor, CommandCodeExecutorError,
    CommandCodeExecutorResponse,
};
pub use cursor::{
    parse_cursor_sse_events, CursorExecutionRequest, CursorExecutor, CursorExecutorError,
    CursorExecutorResponse, SseEvent,
};
pub use default::{
    DefaultExecutor, ExecutionRequest, ExecutionResponse, ExecutorError, ProviderConfig,
    TransportKind, UpstreamResponse,
};
pub use gemini_cli::{
    GeminiCliExecutionRequest, GeminiCliExecutor, GeminiCliExecutorError,
    GeminiCliExecutorResponse,
};
pub use github::{
    GithubExecutionRequest, GithubExecutor, GithubExecutorError, GithubExecutorResponse,
};
pub use grok_web::{
    GrokWebExecutionRequest, GrokWebExecutor, GrokWebExecutorError, GrokWebExecutorResponse,
    PerplexityWebExecutionRequest, PerplexityWebExecutor, PerplexityWebExecutorError,
    PerplexityWebExecutorResponse,
};
pub use iflow::{
    IFlowExecutionRequest, IFlowExecutor, IFlowExecutorError, IFlowExecutorResponse,
};
pub use kiro::{
    AwsCredentials, EventStreamDecoder, KiroExecutionRequest, KiroExecutor, KiroExecutorError,
    KiroExecutorResponse, SseEvent as KiroSseEvent,
};
pub use ollama::{
    OllamaExecutionRequest, OllamaExecutor, OllamaExecutorError, OllamaExecutorResponse,
};
pub use opencode::{
    OpenCodeExecutionRequest, OpenCodeExecutor, OpenCodeExecutorError, OpenCodeExecutorResponse,
};
pub use opencode_go::{
    OpenCodeGoExecutionRequest, OpenCodeGoExecutor, OpenCodeGoExecutorError,
    OpenCodeGoExecutorResponse,
};
pub use provider::{
    all_providers, get_api_key_providers, get_free_providers, get_oauth_providers,
    get_provider_config, get_specialty_providers, is_supported_provider, ProviderExecutionRequest,
    ProviderExecutionResponse, ProviderExecutorConfig, ProviderExecutorError, ProviderFormat,
    UnifiedExecutor,
};
pub use qoder::{
    QoderExecutionRequest, QoderExecutor, QoderExecutorError, QoderExecutorResponse,
};
pub use qwen::{
    QwenExecutionRequest, QwenExecutor, QwenExecutorError, QwenExecutorResponse,
};
pub use vertex::{
    VertexExecutionRequest, VertexExecutor, VertexExecutorError, VertexExecutorResponse,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    Default,
}
