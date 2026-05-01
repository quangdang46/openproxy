mod client_pool;
mod default;
mod kiro;

pub use client_pool::{
    ClientPool, DirectHyperClient, CLIENT_POOL_IDLE_TIMEOUT, CLIENT_POOL_MAX_IDLE_PER_HOST,
    CLIENT_POOL_TCP_KEEPALIVE,
};
pub use default::{
    DefaultExecutor, ExecutionRequest, ExecutionResponse, ExecutorError, ProviderConfig,
    TransportKind, UpstreamResponse,
};
pub use kiro::{
    AwsCredentials, EventStreamDecoder, KiroExecutor, KiroExecutorError, KiroExecutorResponse,
    KiroExecutionRequest, SseEvent,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    Default,
}
