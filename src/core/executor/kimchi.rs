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
    ///
    /// Matches:
    /// - kimchi's own Anthropic-backed models (kimchi-sonnet-*, kimchi-haiku-*).
    ///   In 9router these are detected via cached model metadata (provider ===
    ///   "anthropic"); Rust has no metadata cache, so we keep the prefix match.
    /// - Any model id containing a claude/anthropic segment, mirroring the JS
    ///   fallback regex `/(^|[-_/])(?:claude|anthropic)(?:[-_/]|$)/i`.
    fn is_anthropic_backed_model(model: &str) -> bool {
        if model.starts_with("kimchi-sonnet") || model.starts_with("kimchi-haiku") {
            return true;
        }
        // 9router kimchi.js:92 regex fallback (case-insensitive): "claude" or
        // "anthropic" as a whole segment delimited by start/end, '-', '_', or '/'.
        model
            .split(|c| c == '-' || c == '_' || c == '/')
            .any(|seg| seg.eq_ignore_ascii_case("claude") || seg.eq_ignore_ascii_case("anthropic"))
    }

    /// Transform the request body (9router kimchi.js `transformRequest` parity):
    ///
    /// 1. Merge a top-level `system` into `messages` (prepend a system message
    ///    or join with the existing one).
    /// 2. Remove Anthropic-specific fields that leak into the OpenAI-compatible
    ///    body: `anthropic_version`, `anthropic_beta`, `client_metadata`,
    ///    `mcp_servers`, `stop_sequences`, `thinking`, `top_k`, then delete the
    ///    now-hoisted top-level `system`.
    /// 3. Suppress `reasoning_effort`, `reasoning`, and `thinking` for
    ///    Anthropic-backed models (JS deletes all three).
    /// 4. Strip `cache_control` from messages, content parts (`cache_control`
    ///    AND `signature`), and tool definitions (`stripMessageArtifacts` /
    ///    `stripToolArtifacts` parity).
    /// 5. Strip echoed `reasoning_content` from assistant REQUEST messages when
    ///    longer than the placeholder threshold (8 chars). The short placeholder
    ///    (" ") injected by the pipeline is preserved.
    fn transform_request(
        &self,
        body: &Value,
        model: &str,
        _stream: bool,
        _credentials: &ProviderConnection,
    ) -> Value {
        let mut body = body.clone();

        // 1. Merge top-level system into messages (JS mergeTopLevelSystem).
        merge_top_level_system(&mut body);

        // 2. Remove Anthropic-specific top-level fields + the hoisted system.
        if let Some(obj) = body.as_object_mut() {
            obj.remove("anthropic_version");
            obj.remove("anthropic_beta");
            obj.remove("client_metadata");
            obj.remove("mcp_servers");
            obj.remove("stop_sequences");
            obj.remove("thinking");
            obj.remove("top_k");
            obj.remove("system");
        }

        // 3. Suppress reasoning fields for Anthropic-backed models
        //    (JS deletes reasoning_effort + reasoning + thinking).
        if Self::is_anthropic_backed_model(model) {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("reasoning_effort");
                obj.remove("reasoning");
                obj.remove("thinking");
            }
        }

        // 4a. Strip message artifacts: message-level cache_control, and per
        //     content part cache_control + signature.
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for msg in messages.iter_mut() {
                strip_message_artifacts(msg);
            }
        }

        // 4b. Strip tool artifacts: tool-level cache_control.
        if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools.iter_mut() {
                if let Some(obj) = tool.as_object_mut() {
                    obj.remove("cache_control");
                }
            }
        }

        // 5. Strip echoed reasoning_content from assistant REQUEST messages
        //    (only real thinking blocks, length > 8; preserve the placeholder).
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for msg in messages.iter_mut() {
                if let Some(obj) = msg.as_object_mut() {
                    if obj.get("role").and_then(Value::as_str) == Some("assistant") {
                        if let Some(rc) = obj.get("reasoning_content").and_then(Value::as_str) {
                            if rc.chars().count() > REASONING_PLACEHOLDER_MAX_LEN {
                                obj.remove("reasoning_content");
                            }
                        }
                    }
                }
            }
        }

        body
    }
}

/// JS `systemToText` + `mergeTopLevelSystem` parity.
///
/// Flattens a top-level `system` (string or array of {text}) into a single
/// string, then either prepends `{role:"system",content:text}` to `messages`
/// or joins it with an existing system message (`text\n\n{existing}`, or
/// `{type:"text",text}` unshifted onto an array-content system message).
fn merge_top_level_system(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    if obj.get("system").is_none() {
        return;
    }
    if !obj
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|m| !m.is_empty())
    {
        return;
    }
    let text = system_to_text(obj.get("system").expect("system checked above"))
        .trim()
        .to_string();
    if text.is_empty() {
        return;
    }

    let messages = obj
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .expect("messages checked above");
    let existing = messages
        .iter_mut()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("system"));

    match existing {
        None => {
            messages.insert(0, serde_json::json!({"role": "system", "content": text}));
        }
        Some(existing_msg) => {
            if let Some(content) = existing_msg.get_mut("content") {
                match content {
                    Value::String(s) => {
                        *s = format!("{text}\n\n{s}");
                    }
                    Value::Array(arr) => {
                        arr.insert(0, serde_json::json!({"type": "text", "text": text}));
                    }
                    _ => {}
                }
            }
        }
    }
}

/// JS `systemToText` parity: a string passes through; an array flattens
/// string parts and `{text}` parts, joined with "\n".
fn system_to_text(system: &Value) -> String {
    match system {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part {
                Value::String(s) => s.clone(),
                Value::Object(m) => m
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                _ => String::new(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// JS `stripMessageArtifacts` parity: remove message-level `cache_control`,
/// and from each content part remove `cache_control` + `signature`.
fn strip_message_artifacts(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    obj.remove("cache_control");
    if let Some(content) = obj.get_mut("content").and_then(Value::as_array_mut) {
        for block in content.iter_mut() {
            if let Some(block_obj) = block.as_object_mut() {
                block_obj.remove("cache_control");
                block_obj.remove("signature");
            }
        }
    }
}

/// JS `REASONING_PLACEHOLDER_MAX_LEN` parity — only strip reasoning_content
/// longer than this (real thinking blocks); the 1-char pipeline placeholder is
/// preserved to avoid re-triggering upstream validation on the next turn.
const REASONING_PLACEHOLDER_MAX_LEN: usize = 8;

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
                if let Some(nested) = block_obj.get_mut("content").and_then(Value::as_array_mut) {
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
        format!("{}/chat/completions", KIMCHI_BASE_URL.trim_end_matches('/'))
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

        // Pass the upstream response through unchanged for both streaming and
        // non-streaming. 9router kimchi.js does NOT strip reasoning_content from
        // responses — it only strips echoed reasoning_content from REQUEST
        // assistant messages (done in transform_request above).
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
        let result =
            executor.transform_request(&body, "gpt-4", true, &ProviderConnection::default());
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
        let result =
            executor.transform_request(&body, "gpt-4", true, &ProviderConnection::default());

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
        let result =
            executor.transform_request(&body, "gpt-4", true, &ProviderConnection::default());

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
        let result =
            executor.transform_request(&body, "gpt-4", true, &ProviderConnection::default());
        assert_eq!(
            result["reasoning_effort"], "high",
            "reasoning_effort should be preserved for non-Anthropic models"
        );
    }

    #[test]
    fn test_is_anthropic_backed_model() {
        // kimchi's own Anthropic-backed models (metadata-backed in JS).
        assert!(KimchiExecutor::is_anthropic_backed_model(
            "kimchi-sonnet-4-20250514"
        ));
        assert!(KimchiExecutor::is_anthropic_backed_model(
            "kimchi-haiku-3-5-20250514"
        ));
        // claude/anthropic segments (JS regex fallback) — claude-sonnet IS
        // anthropic-backed per the JS regex, unlike the old prefix-only check.
        assert!(KimchiExecutor::is_anthropic_backed_model(
            "claude-sonnet-4-20250514"
        ));
        assert!(KimchiExecutor::is_anthropic_backed_model(
            "anthropic/claude-3-5-sonnet"
        ));
        // Non-Anthropic models.
        assert!(!KimchiExecutor::is_anthropic_backed_model("gpt-4"));
        assert!(!KimchiExecutor::is_anthropic_backed_model(
            "deepseek/deepseek-v4-pro"
        ));
    }

    #[test]
    fn test_transform_request_strips_request_reasoning_content_echo() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        // Real thinking block (> 8 chars) must be stripped.
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "reasoning_content": "aaaaaaaaa", "content": "ok"}
            ]
        });
        let result =
            executor.transform_request(&body, "gpt-4", true, &ProviderConnection::default());
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0].get("reasoning_content"), None);
        assert_eq!(msgs[0]["content"], "ok");
    }

    #[test]
    fn test_transform_request_preserves_short_reasoning_placeholder() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        // A 4-char placeholder (<= 8) must be preserved — stripping it would
        // re-trigger upstream validation on the next turn.
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "reasoning_content": "    ", "content": "ok"}
            ]
        });
        let result =
            executor.transform_request(&body, "gpt-4", true, &ProviderConnection::default());
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(
            msgs[0].get("reasoning_content").and_then(Value::as_str),
            Some("    ")
        );
    }

    #[test]
    fn test_transform_request_merges_top_level_system() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        // Top-level system string is hoisted into a prepended system message.
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "system": "You are helpful."
        });
        let result =
            executor.transform_request(&body, "gpt-4", true, &ProviderConnection::default());
        assert_eq!(
            result.get("system"),
            None,
            "top-level system must be removed"
        );
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful.");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn test_transform_request_strips_message_artifacts() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        // cache_control + signature stripped from content parts.
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Hello",
                         "cache_control": {"type": "ephemeral"},
                         "signature": "sig-123"}
                    ],
                    "cache_control": {"type": "ephemeral"}
                }
            ]
        });
        let result =
            executor.transform_request(&body, "gpt-4", true, &ProviderConnection::default());
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0].get("cache_control"), None);
        let parts = msgs[0]["content"].as_array().unwrap();
        assert_eq!(parts[0].get("cache_control"), None);
        assert_eq!(parts[0].get("signature"), None);
    }

    #[test]
    fn test_build_url() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        let url = executor.build_url("test-model", true, None, None);
        assert_eq!(url, "https://llm.kimchi.dev/openai/v1/chat/completions");
    }

    #[test]
    fn test_provider_name() {
        let executor = KimchiExecutor::new(Arc::new(ClientPool::new()), None);
        assert_eq!(executor.provider_name(), "kimchi");
    }
}
