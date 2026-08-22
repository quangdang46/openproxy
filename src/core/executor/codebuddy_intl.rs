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

/// JS codebuddy-intl.js:24 — injected leading system prompt.
const INTL_SYSTEM_PROMPT: &str = "You are CodeBuddy Code.";

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
        let mut messages = vec![json!({"role": "system", "content": INTL_SYSTEM_PROMPT})];
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
                {"role": "system", "content": "you are claude code"},
                {"role": "developer", "content": "dev note"},
                {"role": "user", "content": "plain string prompt"},
                {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}
            ]
        });
        let out = CodeBuddyIntlExecutor::transform_request_intl(&body);
        assert_eq!(out["stream"], true);

        let messages = out["messages"].as_array().unwrap();
        // Leading injected system; system/developer dropped.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are CodeBuddy Code.");
        // Bare-string user content becomes a typed text block (11101 fix).
        assert_eq!(messages[1]["content"][0]["type"], "text");
        assert_eq!(messages[1]["content"][0]["text"], "plain string prompt");
        // Assistant array content passes through untouched.
        assert_eq!(messages[2]["role"], "assistant");
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
}
