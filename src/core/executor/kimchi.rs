//! Kimchi executor.
//!
//! Dedicated executor for the `kimchi` provider (llm.kimchi.dev).
//!
//! Behaviour:
//! - Strips Anthropic-specific fields from request body
//! - Removes `cache_control` from messages, content blocks, and tool definitions
//! - Suppresses `reasoning_effort` for Anthropic-backed models (kimchi-sonnet, kimchi-haiku)
//! - Strips `reasoning_content` echo from assistant content blocks in non-streaming responses

use std::sync::Arc;

use async_trait::async_trait;
use hyper::http;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Body as ReqwestBody;
use serde_json::Value;

use crate::types::{ProviderConnection, ProviderNode};

use super::provider::{
    ProviderExecutionRequest, ProviderExecutionResponse, ProviderExecutor, ProviderExecutorError,
};
use super::{ClientPool, TransportKind, UpstreamResponse};

const KIMCHI_BASE_URL: &str = "https://llm.kimchi.dev/openai/v1";

/// Dedicated executor for the `kimchi` provider.
#[derive(Clone)]
pub struct KimchiExecutor {
    pool: Arc<ClientPool>,
    #[allow(dead_code)]
    provider_node: Option<ProviderNode>,
}

impl KimchiExecutor {
    pub fn new(pool: Arc<ClientPool>, provider_node: Option<ProviderNode>) -> Self {
        Self {
            pool,
            provider_node,
        }
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// Check if a model name identifies an Anthropic-backed model.
    /// These models don't support `reasoning_effort` via the OpenAI-compatible API.
    fn is_anthropic_backed_model(model: &str) -> bool {
        model.starts_with("kimchi-sonnet") || model.starts_with("kimchi-haiku")
    }

    /// Transform the request body:
    ///
    /// 1. Remove Anthropic-specific fields that leak into the OpenAI-compatible body:
    ///    `anthropic_version`, `anthropic_beta`, `client_metadata`, `mcp_servers`,
    ///    `stop_sequences`, `thinking`, `top_k`
    /// 2. Remove `cache_control` from messages, content blocks, and tool definitions
    /// 3. Suppress `reasoning_effort` for Anthropic-backed models (kimchi-sonnet, kimchi-haiku)
    fn transform_request(
        &self,
        body: &Value,
        model: &str,
        _stream: bool,
        _credentials: &ProviderConnection,
    ) -> Value {
        let mut body = body.clone();

        // 1. Remove Anthropic-specific top-level fields
        if let Some(obj) = body.as_object_mut() {
            obj.remove("anthropic_version");
            obj.remove("anthropic_beta");
            obj.remove("client_metadata");
            obj.remove("mcp_servers");
            obj.remove("stop_sequences");
            obj.remove("thinking");
            obj.remove("top_k");
        }

        // 2. Remove cache_control from messages and their content blocks
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for msg in messages.iter_mut() {
                remove_cache_control(msg);
            }
        }

        // 3. Remove cache_control from tool definitions
        if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools.iter_mut() {
                if let Some(obj) = tool.as_object_mut() {
                    obj.remove("cache_control");
                }
            }
        }

        // 4. Suppress reasoning_effort for Anthropic-backed models
        if Self::is_anthropic_backed_model(model) {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("reasoning_effort");
            }
        }

        body
    }
}

/// Recursively remove `cache_control` from a message value.
///
/// Handles:
/// - Message-level `cache_control` field
/// - Content block-level `cache_control` field inside `content[]` arrays
/// - Nested content inside content blocks
fn remove_cache_control(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    obj.remove("cache_control");

    // Handle content blocks (array of parts)
    if let Some(content) = obj.get_mut("content").and_then(Value::as_array_mut) {
        for block in content.iter_mut() {
            if let Some(block_obj) = block.as_object_mut() {
                block_obj.remove("cache_control");
                // Handle nested content within blocks
                if let Some(nested) = block_obj
                    .get_mut("content")
                    .and_then(Value::as_array_mut)
                {
                    for nested_block in nested.iter_mut() {
                        if let Some(nb_obj) = nested_block.as_object_mut() {
                            nb_obj.remove("cache_control");
                        }
                    }
                }
            }
        }
    }
}

/// Strip `reasoning_content` from assistant message content blocks
/// in an OpenAI Chat Completions response body.
///
/// Handles both:
/// - Non-streaming: `choices[].message.reasoning_content`
/// - Streaming chunk: `choices[].delta.reasoning_content`
fn remove_reasoning_content(body: &mut Value) {
    let Some(choices) = body.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };
    for choice in choices.iter_mut() {
        let Some(choice_obj) = choice.as_object_mut() else {
            continue;
        };
        // Non-streaming: choices[].message.reasoning_content
        if let Some(msg) = choice_obj.get_mut("message").and_then(Value::as_object_mut) {
            msg.remove("reasoning_content");
        }
        // Streaming: choices[].delta.reasoning_content
        if let Some(delta) = choice_obj.get_mut("delta").and_then(Value::as_object_mut) {
            delta.remove("reasoning_content");
        }
    }
}

#[async_trait]
impl ProviderExecutor for KimchiExecutor {
    fn provider_name(&self) -> &str {
        "kimchi"
    }

    fn build_url(
        &self,
        _model: &str,
        _stream: bool,
        _url_index: Option<usize>,
        _credentials: Option<&ProviderConnection>,
    ) -> String {
        format!(
            "{}/chat/completions",
            KIMCHI_BASE_URL.trim_end_matches('/')
        )
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
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

        if stream {
            headers.insert(
                reqwest::header::ACCEPT,
                HeaderValue::from_static("text/event-stream"),
            );
        }

        Ok(headers)
    }

    fn transform_request(
        &self,
        body: &Value,
        model: &str,
        stream: bool,
        credentials: &ProviderConnection,
    ) -> Value {
        self.transform_request(body, model, stream, credentials)
    }

    async fn execute(
        &self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionResponse, ProviderExecutorError> {
        let url = self.build_url(
            &request.model,
            request.stream,
            request.proxy_options.as_ref().and_then(|o| o.url_index),
            Some(&request.credentials),
        );
        let headers = self.build_headers(&request.credentials, request.stream)?;
        let transformed_body = self.transform_request(
            &request.body,
            &request.model,
            request.stream,
            &request.credentials,
        );

        let body_bytes = serde_json::to_vec(&transformed_body)?;
        let client = self.pool.get("kimchi", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        // For non-streaming responses, strip reasoning_content from the body.
        // Streaming reasoning_content filtering is handled at the translator /
        // response_transform layer (SSE chunk transformation).
        if !request.stream {
            let status = response.status();
            let resp_headers = response.headers().clone();
            let body_bytes = response.bytes().await?;

            // Parse, strip reasoning_content, and reconstruct
            let mut body_value: Value = serde_json::from_slice(&body_bytes)?;
            remove_reasoning_content(&mut body_value);
            let modified_bytes = serde_json::to_vec(&body_value)?;

            let mut http_resp = http::Response::new(ReqwestBody::from(modified_bytes));
            *http_resp.status_mut() = status;
            *http_resp.headers_mut() = resp_headers;
            let reconstructed = reqwest::Response::from(http_resp);

            Ok(ProviderExecutionResponse {
                response: UpstreamResponse::Reqwest(reconstructed),
                url,
                headers,
                transformed_body,
                transport: TransportKind::Reqwest,
            })
        } else {
            Ok(ProviderExecutionResponse {
                response: UpstreamResponse::Reqwest(response),
                url,
                headers,
                transformed_body,
                transport: TransportKind::Reqwest,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::executor::ClientPool;
    use serde_json::json;

    #[test]
    fn test_transform_request_strips_anthropic_fields() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "anthropic_version": "2023-06-01",
            "anthropic_beta": ["some-beta"],
            "client_metadata": {"key": "value"},
            "mcp_servers": [],
            "stop_sequences": ["\n\n"],
            "thinking": {"type": "enabled"},
            "top_k": 40
        });
        let result = executor.transform_request(
            &body,
            "gpt-4",
            true,
            &ProviderConnection::default(),
        );
        assert_eq!(result.get("anthropic_version"), None);
        assert_eq!(result.get("anthropic_beta"), None);
        assert_eq!(result.get("client_metadata"), None);
        assert_eq!(result.get("mcp_servers"), None);
        assert_eq!(result.get("stop_sequences"), None);
        assert_eq!(result.get("thinking"), None);
        assert_eq!(result.get("top_k"), None);
        assert_eq!(result["model"], "gpt-4");
    }

    #[test]
    fn test_transform_request_strips_cache_control_from_messages() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Hello", "cache_control": {"type": "ephemeral"}},
                        {"type": "text", "text": "World"}
                    ],
                    "cache_control": {"type": "ephemeral"}
                },
                {
                    "role": "assistant",
                    "content": "Hi there!",
                    "cache_control": {"type": "ephemeral"}
                }
            ]
        });
        let result = executor.transform_request(
            &body,
            "gpt-4",
            true,
            &ProviderConnection::default(),
        );

        // Message-level cache_control removed
        for msg in result["messages"].as_array().unwrap() {
            assert_eq!(
                msg.get("cache_control"),
                None,
                "cache_control should be removed from messages"
            );
        }
        // Content block cache_control removed
        let content = &result["messages"][0]["content"];
        for block in content.as_array().unwrap() {
            assert_eq!(
                block.get("cache_control"),
                None,
                "cache_control should be removed from content blocks"
            );
        }
    }

    #[test]
    fn test_transform_request_strips_cache_control_from_tools() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "tools": [
                {
                    "name": "test_tool",
                    "description": "A test tool",
                    "input_schema": {"type": "object"},
                    "cache_control": {"type": "ephemeral"}
                }
            ]
        });
        let result = executor.transform_request(
            &body,
            "gpt-4",
            true,
            &ProviderConnection::default(),
        );

        for tool in result["tools"].as_array().unwrap() {
            assert_eq!(
                tool.get("cache_control"),
                None,
                "cache_control should be removed from tools"
            );
        }
    }

    #[test]
    fn test_transform_request_suppresses_reasoning_effort_for_anthropic_models() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "kimchi-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Hello"}],
            "reasoning_effort": "high"
        });
        let result = executor.transform_request(
            &body,
            "kimchi-sonnet-4-20250514",
            true,
            &ProviderConnection::default(),
        );
        assert_eq!(
            result.get("reasoning_effort"),
            None,
            "reasoning_effort should be removed for Anthropic-backed models"
        );
    }

    #[test]
    fn test_transform_request_preserves_reasoning_effort_for_non_anthropic_models() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "reasoning_effort": "high"
        });
        let result = executor.transform_request(
            &body,
            "gpt-4",
            true,
            &ProviderConnection::default(),
        );
        assert_eq!(
            result["reasoning_effort"], "high",
            "reasoning_effort should be preserved for non-Anthropic models"
        );
    }

    #[test]
    fn test_is_anthropic_backed_model() {
        assert!(KimchiExecutor::is_anthropic_backed_model(
            "kimchi-sonnet-4-20250514"
        ));
        assert!(KimchiExecutor::is_anthropic_backed_model(
            "kimchi-haiku-3-5-20250514"
        ));
        assert!(!KimchiExecutor::is_anthropic_backed_model("gpt-4"));
        assert!(!KimchiExecutor::is_anthropic_backed_model(
            "claude-sonnet-4-20250514"
        ));
    }

    #[test]
    fn test_remove_reasoning_content_non_streaming() {
        let mut body = json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "Hello!",
                        "reasoning_content": "Let me think..."
                    }
                }
            ]
        });
        remove_reasoning_content(&mut body);
        assert_eq!(
            body["choices"][0]["message"].get("reasoning_content"),
            None
        );
        assert_eq!(body["choices"][0]["message"]["content"], "Hello!");
    }

    #[test]
    fn test_remove_reasoning_content_streaming() {
        let mut body = json!({
            "choices": [
                {
                    "delta": {
                        "content": "Hello",
                        "reasoning_content": "Thinking..."
                    }
                }
            ]
        });
        remove_reasoning_content(&mut body);
        assert_eq!(body["choices"][0]["delta"].get("reasoning_content"), None);
        assert_eq!(body["choices"][0]["delta"]["content"], "Hello");
    }

    #[test]
    fn test_remove_reasoning_content_no_choices() {
        let mut body = json!({"error": "test"});
        remove_reasoning_content(&mut body);
        assert_eq!(body["error"], "test");
    }

    #[test]
    fn test_remove_reasoning_content_mixed_streaming_and_non_streaming() {
        let mut body = json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "Final answer",
                        "reasoning_content": "Step by step..."
                    },
                    "delta": {
                        "content": "",
                        "reasoning_content": "Thinking..."
                    }
                }
            ]
        });
        remove_reasoning_content(&mut body);
        assert_eq!(
            body["choices"][0]["message"].get("reasoning_content"),
            None
        );
        assert_eq!(body["choices"][0]["delta"].get("reasoning_content"), None);
        assert_eq!(body["choices"][0]["message"]["content"], "Final answer");
    }

    #[test]
    fn test_remove_reasoning_content_empty_choices() {
        let mut body = json!({"choices": []});
        remove_reasoning_content(&mut body);
        let choices = body["choices"].as_array().unwrap();
        assert!(choices.is_empty());
    }

    #[test]
    fn test_build_url() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        let url = executor.build_url("test-model", true, None, None);
        assert_eq!(
            url,
            "https://llm.kimchi.dev/openai/v1/chat/completions"
        );
    }

    #[test]
    fn test_provider_name() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        assert_eq!(executor.provider_name(), "kimchi");
    }
}
