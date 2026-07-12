//! CodeBuddyCN executor.
//!
//! Dedicated executor for the `codebuddy-cn` provider (api.codebuddy.cn).
//!
//! Behaviour:
//! - Always forces `stream: true` in the request body.
//! - If `reasoning_effort` is present, sets `reasoning_summary: "auto"`.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;

use crate::types::{ProviderConnection, ProviderNode};

use super::provider::{
    ProviderExecutionRequest, ProviderExecutionResponse, ProviderExecutor, ProviderExecutorError,
};
use super::{ClientPool, TransportKind, UpstreamResponse};

/// Dedicated executor for the `codebuddy-cn` provider.
#[derive(Clone)]
pub struct CodeBuddyCNExecutor {
    pool: Arc<ClientPool>,
    #[allow(dead_code)]
    provider_node: Option<ProviderNode>,
}

impl CodeBuddyCNExecutor {
    pub fn new(pool: Arc<ClientPool>, provider_node: Option<ProviderNode>) -> Self {
        Self {
            pool,
            provider_node,
        }
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }
}

#[async_trait]
impl ProviderExecutor for CodeBuddyCNExecutor {
    fn provider_name(&self) -> &str {
        "codebuddy-cn"
    }

    fn build_url(
        &self,
        _model: &str,
        _stream: bool,
        _url_index: Option<usize>,
        _credentials: Option<&ProviderConnection>,
    ) -> String {
        // 9router registry: copilot.tencent.com/v2/chat/completions
        "https://copilot.tencent.com/v2/chat/completions".to_string()
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        _stream: bool,
    ) -> Result<HeaderMap, ProviderExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        let token = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .ok_or_else(|| {
                ProviderExecutorError::MissingCredentials(self.provider_name().to_string())
            })?;

        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );

        // Force stream means we always want SSE responses
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );

        Ok(headers)
    }

    fn transform_request(
        &self,
        body: &Value,
        _model: &str,
        _stream: bool,
        _credentials: &ProviderConnection,
    ) -> Value {
        let mut body = body.clone();

        // 1. Force stream=true always
        body["stream"] = Value::Bool(true);

        // 2. If reasoning_effort is present, set reasoning_summary=auto
        if body.get("reasoning_effort").is_some() {
            body["reasoning_summary"] = Value::String("auto".to_string());
        }

        body
    }

    async fn execute(
        &self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionResponse, ProviderExecutorError> {
        let url = self.build_url(
            &request.model,
            true,
            request.proxy_options.as_ref().and_then(|o| o.url_index),
            Some(&request.credentials),
        );
        let headers = self.build_headers(&request.credentials, true)?;
        let transformed_body = self.transform_request(
            &request.body,
            &request.model,
            true,
            &request.credentials,
        );

        let body_bytes = serde_json::to_vec(&transformed_body)?;
        let client = self.pool.get("codebuddy-cn", request.proxy.as_ref())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::executor::ClientPool;
    use serde_json::json;

    #[test]
    fn test_transform_request_forces_stream_true() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1024
        });
        let result = executor.transform_request(&body, "claude-sonnet-4", false, &ProviderConnection::default());
        assert_eq!(result["stream"], true);
        assert_eq!(result["model"], "claude-sonnet-4");
        assert_eq!(result["max_tokens"], 1024);
    }

    #[test]
    fn test_transform_request_overwrites_false_stream() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "stream": false
        });
        let result = executor.transform_request(&body, "gpt-4", false, &ProviderConnection::default());
        assert_eq!(result["stream"], true);
    }

    #[test]
    fn test_transform_request_sets_reasoning_summary_when_reasoning_effort_present() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "user", "content": "Think carefully"}],
            "reasoning_effort": "high",
            "stream": true
        });
        let result = executor.transform_request(&body, "claude-sonnet-4", true, &ProviderConnection::default());
        assert_eq!(result["reasoning_summary"], "auto");
        assert_eq!(result["reasoning_effort"], "high");
    }

    #[test]
    fn test_transform_request_no_reasoning_summary_without_reasoning_effort() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        });
        let result = executor.transform_request(&body, "claude-sonnet-4", true, &ProviderConnection::default());
        assert!(result.get("reasoning_summary").is_none());
    }

    #[test]
    fn test_build_url() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let url = executor.build_url("claude-sonnet-4", true, None, None);
        assert_eq!(url, "https://api.codebuddy.cn/v1/chat/completions");
    }

    #[test]
    fn test_build_headers_missing_credentials() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let creds = ProviderConnection::default();
        let result = executor.build_headers(&creds, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_headers_with_api_key() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let mut creds = ProviderConnection::default();
        creds.api_key = Some("sk-test".to_string());
        let headers = executor.build_headers(&creds, true).unwrap();
        assert_eq!(
            headers.get(reqwest::header::AUTHORIZATION).and_then(|v| v.to_str().ok()),
            Some("Bearer sk-test")
        );
        assert_eq!(
            headers.get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            headers.get(reqwest::header::ACCEPT).and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
    }
}
