mod client_pool;
mod codex;
mod default;
mod kiro;
mod vertex;

pub use client_pool::{
    ClientPool, DirectHyperClient, CLIENT_POOL_IDLE_TIMEOUT, CLIENT_POOL_MAX_IDLE_PER_HOST,
    CLIENT_POOL_TCP_KEEPALIVE,
};
pub use codex::{
    CodexExecutor, CodexExecutorError, CodexExecutionRequest, CodexExecutorResponse,
    convert_openai_sse_to_standard,
};
pub use default::{
    DefaultExecutor, ExecutionRequest, ExecutionResponse, ExecutorError, ProviderConfig,
    TransportKind, UpstreamResponse,
};
pub use kiro::{
    AwsCredentials, EventStreamDecoder, KiroExecutor, KiroExecutorError, KiroExecutorResponse,
    KiroExecutionRequest, SseEvent,
};
pub use vertex::{
    VertexExecutor, VertexExecutorError, VertexExecutionRequest, VertexExecutorResponse,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    Default,
}
