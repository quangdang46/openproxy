mod api_key;
mod client_pool;
mod codex;
mod cursor;
mod default;
mod grok_web;
mod kiro;
mod ollama;
mod provider;
mod vertex;

pub use api_key::{
    get_api_key_provider_config, is_api_key_provider, ApiKeyExecutionRequest, ApiKeyExecutor,
    ApiKeyExecutorError, ApiKeyExecutorResponse,
};

pub use client_pool::{
    ClientPool, DirectHyperClient, CLIENT_POOL_IDLE_TIMEOUT, CLIENT_POOL_MAX_IDLE_PER_HOST,
    CLIENT_POOL_TCP_KEEPALIVE,
};
pub use codex::{
    convert_openai_sse_to_standard, CodexExecutionRequest, CodexExecutor, CodexExecutorError,
    CodexExecutorResponse,
};
pub use cursor::{
    parse_cursor_sse_events, CursorExecutionRequest, CursorExecutor, CursorExecutorError,
    CursorExecutorResponse, SseEvent,
};
pub use default::{
    DefaultExecutor, ExecutionRequest, ExecutionResponse, ExecutorError, ProviderConfig,
    TransportKind, UpstreamResponse,
};
pub use grok_web::{
    GrokWebExecutionRequest, GrokWebExecutor, GrokWebExecutorError, GrokWebExecutorResponse,
    PerplexityWebExecutionRequest, PerplexityWebExecutor, PerplexityWebExecutorError,
    PerplexityWebExecutorResponse,
};
pub use kiro::{
    AwsCredentials, EventStreamDecoder, KiroExecutionRequest, KiroExecutor, KiroExecutorError,
    KiroExecutorResponse, SseEvent as KiroSseEvent,
};
pub use ollama::{
    OllamaExecutionRequest, OllamaExecutor, OllamaExecutorError, OllamaExecutorResponse,
};
pub use provider::{
    all_providers, get_api_key_providers, get_free_providers, get_oauth_providers,
    get_provider_config, get_specialty_providers, is_supported_provider, ProviderExecutionRequest,
    ProviderExecutionResponse, ProviderExecutorConfig, ProviderExecutorError, ProviderFormat,
    UnifiedExecutor,
};
pub use vertex::{
    VertexExecutionRequest, VertexExecutor, VertexExecutorError, VertexExecutorResponse,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    Default,
}
