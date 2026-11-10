//! Dedicated executor for the `codebuddy-intl` provider (codebuddy.ai).
//!
//! Port of 9router `open-sse/executors/codebuddy-intl.js` — extends the
//! DefaultExecutor behavior with an intl-specific `transformRequest`:
//!
//! 1. Force `stream = true` (registry forceStream).
//! 2. `reasoning_effort` "none"/"off" → delete; any other effort →
//!    `reasoning_summary = "auto"`.
//! 3. CodeBuddy upstream rejects the plain OpenAI shape with code 11101:
//!    rebuild messages with a LEADING system prompt ("You are CodeBuddy
//!    Code."), drop system/developer roles, and convert bare-string user
//!    content into typed text blocks.
//!
//! Wire URL/headers come from the default.rs registry entry
//! (https://www.codebuddy.ai/v2/chat/completions + IDE UA headers).

use async_trait::async_trait;
use hyper::HeaderMap;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{
    default::{DefaultExecutor, ExecutionRequest, ExecutionResponse, ExecutorError},
    ClientPool, ProviderExecutionRequest, ProviderExecutionResponse, ProviderExecutor,
    ProviderExecutorError,
};
use crate::types::{ProviderConnection, ProviderNode};

/// JS codebuddy-intl.js:24 — injected leading system prompt (fallback when
/// the client sent no usable system/developer prompt).
const INTL_SYSTEM_PROMPT: &str = "You are CodeBuddy Code.";

/// Extract a system/developer message's text the way upstream
/// codebuddy-intl.js does: string content verbatim, array content joined
/// with "\n" on each part's `text`, anything else "".
fn intl_system_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|c| c.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub struct CodeBuddyIntlExecutor {
    inner: DefaultExecutor,
}

impl CodeBuddyIntlExecutor {
    pub fn new(pool: Arc<ClientPool>, _provider_node: Option<ProviderNode>) -> Self {
        Self {
            inner: DefaultExecutor::new("codebuddy-intl", pool, None)
                .expect("codebuddy-intl is a registered provider config"),
        }
    }
}

#[async_trait]
impl ProviderExecutor for CodeBuddyIntlExecutor {
    fn provider_name(&self) -> &str {
        "codebuddy-intl"
    }

    fn build_url(
        &self,
        _model: &str,
        _stream: bool,
        _url_index: Option<usize>,
        _credentials: Option<&ProviderConnection>,
    ) -> String {
        // 9router registry codebuddy-intl.js transport.
        "https://www.codebuddy.ai/v2/chat/completions".to_string()
    }

    fn build_headers(
        &self,
        _credentials: &ProviderConnection,
        _stream: bool,
    ) -> Result<HeaderMap, ProviderExecutorError> {
        // Registry static headers live on the DefaultExecutor config entry;
        // auth + content-type are applied there from the credentials.
        Ok(HeaderMap::new())
    }

    async fn execute(
        &self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionResponse, ProviderExecutorError> {
        let transformed = Self::transform_request_intl(&request.body);
        let exec_request = ExecutionRequest {
            model: request.model.clone(),
            body: transformed,
            stream: true, // registry forceStream
            credentials: request.credentials.clone(),
            proxy: request.proxy.clone(),
            sim_headers: HeaderMap::new(),
            // Resolver-wiring: inner request carries no dispatch context;
            // configured-mode mock for codebuddy flows via header/env only.
            force_mock: false,
        };
        let ExecutionResponse {
            response,
            url,
            headers,
            transformed_body,
            transport,
        } = self
            .inner
            .execute(exec_request)
            .await
            .map_err(|e| match e {
                ExecutorError::MissingCredentials(p) => {
                    ProviderExecutorError::MissingCredentials(p)
                }
                other => ProviderExecutorError::UnsupportedProvider(format!(
                    "codebuddy-intl: {:?}",
                    other
                )),
            })?;
        Ok(ProviderExecutionResponse {
            response,
            url,
            headers,
            transformed_body,
            transport,
        })
    }
}

impl CodeBuddyIntlExecutor {
    /// Pure intl transform (JS transformRequest, codebuddy-intl.js:19-41).
    /// Exposed for tests; `execute` applies it before dispatching through
    /// the DefaultExecutor wire path.
    pub fn transform_request_intl(body: &Value) -> Value {
        let mut body = body.clone();

        // 1. Force stream (JS sets it unconditionally).
        body["stream"] = Value::Bool(true);

        // 2. reasoning_effort handling.
        let effort = body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(String::from);
        match effort.as_deref() {
            Some("none") | Some("off") => {
                body.as_object_mut().map(|o| o.remove("reasoning_effort"));
                if let Some(obj) = body.as_object_mut() {
                    obj.remove("reasoning_summary");
                }
            }
            Some(eff) if !eff.is_empty() => {
                body["reasoning_summary"] = Value::String("auto".to_string());
            }
            _ => {}
        }

        // 3. Message reshape: leading system prompt, drop system/developer,
        // typed text blocks for bare-string user content (11101 fix).
        let source = body
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // 9router v0.5.116 3ca04639: preserve client system/developer prompts
        // instead of always injecting the default. Tencent's WAF flags
        // third-party CLI agent identity strings (Claude Code, Cursor, …) as
        // unauthorized tooling — neutralize ONLY matching prompts while user
        // prompts survive.
        let client_system_prompts: Vec<String> = source
            .iter()
            .filter(|m| {
                matches!(
                    m.get("role").and_then(Value::as_str),
                    Some("system") | Some("developer")
                )
            })
            .filter_map(|m| m.get("content"))
            .map(intl_system_text)
            .map(|text| {
                if !text.is_empty() && super::codebuddy_cn::AGENT_PATTERN.is_match(&text) {
                    super::codebuddy_cn::NEUTRAL_PROMPT.to_string()
                } else {
                    text
                }
            })
            .filter(|t| !t.is_empty())
            .collect();
        let system_prompt_text = if client_system_prompts.is_empty() {
            INTL_SYSTEM_PROMPT.to_string()
        } else {
            client_system_prompts.join("\n\n")
        };

        let mut messages = vec![json!({"role": "system", "content": system_prompt_text})];
        for message in source {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("");
            if role == "system" || role == "developer" {
                continue;
            }
            if role == "user"
                && message
                    .get("content")
                    .map(Value::is_string)
                    .unwrap_or(false)
            {
                let mut m = message.clone();
                m["content"] = json!([{
                    "type": "text",
                    "text": message["content"].as_str().unwrap_or_default(),
                }]);
                messages.push(m);
            } else {
                messages.push(message);
            }
        }
        body["messages"] = Value::Array(messages);

        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn intl_transform_leads_with_system_and_types_user_strings() {
        let body = json!({
            "model": "glm-5",
            "stream": false,
            "messages": [
                {"role": "system", "content": "You are a professional translator."},
                {"role": "developer", "content": "dev note"},
                {"role": "user", "content": "plain string prompt"},
                {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}
            ]
        });
        let out = CodeBuddyIntlExecutor::transform_request_intl(&body);
        assert_eq!(out["stream"], true);

        let messages = out["messages"].as_array().unwrap();
        // Leading system prompt now carries the CLIENT's prompts joined
        // (9router v0.5.116 3ca04639); system/developer roles are dropped
        // from the tail.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(
            messages[0]["content"],
            "You are a professional translator.\n\ndev note"
        );
        // Bare-string user content becomes a typed text block (11101 fix).
        assert_eq!(messages[1]["content"][0]["type"], "text");
        assert_eq!(messages[1]["content"][0]["text"], "plain string prompt");
        // Assistant array content passes through untouched.
        assert_eq!(messages[2]["role"], "assistant");
    }

    // --- 9router v0.5.116 3ca04639: client system prompts + WAF sanitize ---

    #[test]
    fn intl_forwards_client_system_prompt() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "You are a professional translator."},
                {"role": "user", "content": "Translate this text."}
            ]
        });
        let out = CodeBuddyIntlExecutor::transform_request_intl(&body);
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "You are a professional translator.");
    }

    #[test]
    fn intl_falls_back_to_default_prompt_when_no_system() {
        let body = json!({"messages": [{"role": "user", "content": "Hello"}]});
        let out = CodeBuddyIntlExecutor::transform_request_intl(&body);
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "You are CodeBuddy Code.");
    }

    #[test]
    fn intl_neutralizes_cli_agent_identity_for_waf() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "You are Claude Code, Anthropic's official CLI for coding."},
                {"role": "user", "content": "Fix this bug"}
            ]
        });
        let out = CodeBuddyIntlExecutor::transform_request_intl(&body);
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(
            messages[0]["content"],
            "You are a helpful AI assistant that helps with software engineering tasks."
        );
    }

    #[test]
    fn intl_system_array_content_joins_text_parts() {
        let body = json!({
            "messages": [
                {"role": "system", "content": [
                    {"type": "text", "text": "part one"},
                    {"type": "text", "text": "part two"}
                ]},
                {"role": "user", "content": "hi"}
            ]
        });
        let out = CodeBuddyIntlExecutor::transform_request_intl(&body);
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "part one\npart two");
    }

    #[test]
    fn reasoning_effort_none_deleted_other_gets_summary() {
        let none_body = json!({"messages": [], "reasoning_effort": "none"});
        let out = CodeBuddyIntlExecutor::transform_request_intl(&none_body);
        assert!(out.get("reasoning_effort").is_none());
        assert!(out.get("reasoning_summary").is_none());

        let high_body = json!({"messages": [], "reasoning_effort": "high"});
        let out2 = CodeBuddyIntlExecutor::transform_request_intl(&high_body);
        assert!(out2.get("reasoning_effort").is_some());
        assert_eq!(out2["reasoning_summary"], "auto");
    }

    // --- openproxy-4d5h.43: channel-check identity headers ---

    #[test]
    fn intl_dispatch_sends_codebuddy_channel_ids() {
        // The intl executor delegates wire headers to DefaultExecutor; the
        // channel-check ids must be present on that path (upstream
        // codebuddy-channel-headers.test.js).
        let inner = DefaultExecutor::new("codebuddy-intl", Arc::new(ClientPool::new()), None)
            .expect("codebuddy-intl registered");
        let mut creds = ProviderConnection::default();
        creds.access_token = Some("TESTTOKEN".to_string());
        let headers = inner
            .build_headers("glm-5", &creds, true, &json!({"messages": []}))
            .expect("headers build");

        let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        for name in [
            "x-request-id",
            "x-conversation-id",
            "x-conversation-request-id",
            "x-conversation-message-id",
        ] {
            assert!(
                get(name).is_some_and(|v| !v.is_empty()),
                "codebuddy-intl must send {name}"
            );
        }
        assert_eq!(get("x-request-id"), get("x-conversation-request-id"));
        assert_eq!(get("x-request-id"), get("x-conversation-message-id"));
        assert_ne!(get("x-conversation-id"), get("x-conversation-message-id"));
        assert_eq!(
            headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer TESTTOKEN")
        );
    }
}
