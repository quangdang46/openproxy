//! CodeBuddyCN executor.
//!
//! Dedicated executor for the `codebuddy-cn` provider (api.codebuddy.cn).
//!
//! Behaviour:
//! - Always forces `stream: true` in the request body.
//! - Neutralizes system prompts that identify a coding agent (9router
//!   `AGENT_PATTERN` + `NEUTRAL_PROMPT`) so upstream does not reject them.
//! - `reasoning_effort`: deletes `"none"`/`"off"` (and does not set
//!   `reasoning_summary`); any other truthy effort sets `reasoning_summary:
//!   "auto"`.

use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;

use crate::types::{ProviderConnection, ProviderNode};

use super::provider::{
    ProviderExecutionRequest, ProviderExecutionResponse, ProviderExecutor, ProviderExecutorError,
};
use super::{ClientPool, TransportKind, UpstreamResponse};

/// Replacement system prompt when a system message reveals a coding agent.
/// 9router codebuddy-cn.js NEUTRAL_PROMPT — verbatim.
const NEUTRAL_PROMPT: &str =
    "You are a helpful AI assistant that helps with software engineering tasks.";

/// Patterns that identify a coding-agent system prompt. Ported verbatim from
/// 9router codebuddy-cn.js AGENT_PATTERN (JS `/i` flag). `(?is)` adds
/// case-insensitivity and multi-line `.` so alternates like `<agent-identity>`
/// and `cc_entrypoint` match across newlines.
static AGENT_PATTERN: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
    Regex::new(
        r"(?is)you are claude code|claude.?code.+official.+cli|anthropic.+official.+cli|anxthxropic.+official.+cli|you are (?:cursor|windsurf|cline|aider|continue|copilot|cody)|you are an? (?:ai )?(?:coding |code )?agent|cc_entrypoint\s*=\s*(?:cli|vscode|jetbrains|gui)|claude.?code.+issues|give feedback.+claude.?code|you are .{0,30}(?:powerful )?ai agent|orchestration capabilities|OhMyOpenCode|<agent-identity>|<Role>|<Behavior_Instructions>",
    )
    .expect("AGENT_PATTERN regex must compile")
});

/// Threshold (in characters) above which a system prompt is always replaced,
/// matching 9router's `text.length > 2000` check.
const SYSTEM_PROMPT_LENGTH_THRESHOLD: usize = 2000;

/// Flatten a message's `content` (string or array of `{type:"text",text}`)
/// into plain text; 9router's `flatten(message.content)`.
fn flatten_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

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

        // 2. Neutralize system prompts that identify a coding agent
        //    (9router codebuddy-cn.js: replace when >2000 chars or the
        //    AGENT_PATTERN matches, preserving the content shape).
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages.iter_mut() {
                if message.get("role").and_then(Value::as_str) != Some("system") {
                    continue;
                }
                let Some(content) = message.get("content") else {
                    continue;
                };
                let text = flatten_content(content);
                if text.is_empty() {
                    continue;
                }
                if text.len() > SYSTEM_PROMPT_LENGTH_THRESHOLD || AGENT_PATTERN.is_match(&text) {
                    match content {
                        Value::String(_) => {
                            message["content"] = Value::String(NEUTRAL_PROMPT.to_string());
                        }
                        Value::Array(_) => {
                            message["content"] = Value::Array(vec![serde_json::json!({
                                "type": "text",
                                "text": NEUTRAL_PROMPT
                            })]);
                        }
                        _ => {}
                    }
                }
            }
        }

        // 3. reasoning_effort: "none"/"off" → delete (no summary); any other
        //    truthy effort → reasoning_summary="auto"
        match body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            Some(eff) if eff == "none" || eff == "off" => {
                body.as_object_mut()
                    .map(|obj| obj.remove("reasoning_effort"));
            }
            Some(eff) if !eff.is_empty() => {
                body["reasoning_summary"] = Value::String("auto".to_string());
            }
            _ => {}
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
        let transformed_body =
            self.transform_request(&request.body, &request.model, true, &request.credentials);

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
        let result = executor.transform_request(
            &body,
            "claude-sonnet-4",
            false,
            &ProviderConnection::default(),
        );
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
        let result =
            executor.transform_request(&body, "gpt-4", false, &ProviderConnection::default());
        assert_eq!(result["stream"], true);
    }

    #[test]
    fn test_neutralize_claude_code_system_prompt() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "system", "content": "You are Claude Code, Anthropic's official CLI for AI-assisted coding. Help the user with their tasks."},
                {"role": "user", "content": "Hello"}
            ],
            "stream": true
        });
        let result = executor.transform_request(
            &body,
            "claude-sonnet-4",
            true,
            &ProviderConnection::default(),
        );
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(
            messages[0]["content"], NEUTRAL_PROMPT,
            "system prompt identifying Claude Code must be neutralized"
        );
        assert_eq!(messages[1]["content"], "Hello", "user message untouched");
    }

    #[test]
    fn test_neutralize_preserves_array_shape() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [
                {
                    "role": "system",
                    "content": [
                        {"type": "text", "text": "You are Cursor, an AI code editor."}
                    ]
                },
                {"role": "user", "content": "Hi"}
            ],
            "stream": true
        });
        let result = executor.transform_request(
            &body,
            "claude-sonnet-4",
            true,
            &ProviderConnection::default(),
        );
        let messages = result["messages"].as_array().unwrap();
        let content = &messages[0]["content"];
        assert!(content.is_array(), "array content stays an array");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], NEUTRAL_PROMPT);
    }

    #[test]
    fn test_long_system_prompt_replaced() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let long_prompt = "x".repeat(2500);
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "system", "content": long_prompt}],
            "stream": true
        });
        let result = executor.transform_request(
            &body,
            "claude-sonnet-4",
            true,
            &ProviderConnection::default(),
        );
        assert_eq!(result["messages"][0]["content"], NEUTRAL_PROMPT);
    }

    #[test]
    fn test_benign_system_prompt_untouched() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hi"}
            ],
            "stream": true
        });
        let result = executor.transform_request(
            &body,
            "claude-sonnet-4",
            true,
            &ProviderConnection::default(),
        );
        assert_eq!(
            result["messages"][0]["content"],
            "You are a helpful assistant."
        );
    }

    #[test]
    fn test_reasoning_effort_none_deleted_no_summary() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        for eff in ["none", "off"] {
            let body = json!({
                "model": "claude-sonnet-4",
                "messages": [{"role": "user", "content": "Think carefully"}],
                "reasoning_effort": eff,
                "stream": true
            });
            let result = executor.transform_request(
                &body,
                "claude-sonnet-4",
                true,
                &ProviderConnection::default(),
            );
            assert!(
                result.get("reasoning_effort").is_none(),
                "reasoning_effort {eff:?} must be deleted"
            );
            assert!(
                result.get("reasoning_summary").is_none(),
                "no reasoning_summary for effort {eff:?}"
            );
        }
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
        let result = executor.transform_request(
            &body,
            "claude-sonnet-4",
            true,
            &ProviderConnection::default(),
        );
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
        let result = executor.transform_request(
            &body,
            "claude-sonnet-4",
            true,
            &ProviderConnection::default(),
        );
        assert!(result.get("reasoning_summary").is_none());
    }

    #[test]
    fn test_build_url() {
        let executor = CodeBuddyCNExecutor::new(Arc::new(ClientPool::new()), None);
        let url = executor.build_url("claude-sonnet-4", true, None, None);
        assert_eq!(
            url, "https://copilot.tencent.com/v2/chat/completions",
            "URL should use copilot.tencent.com (9router parity)"
        );
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
            headers
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer sk-test")
        );
        assert_eq!(
            headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            headers
                .get(reqwest::header::ACCEPT)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
    }
}
