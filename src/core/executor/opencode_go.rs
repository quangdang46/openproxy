use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

// Fields that Fireworks AI / OCg upstream reject as "Extra inputs not permitted"
const FORBIDDEN_FIELDS: &[&str] = &[
    "client_metadata",
    "client_meta_data",
    "include",   // Responses API field
    "reasoning", // Responses API field
];

// Tool types that Fireworks AI / OCg upstream accepts (only "function")
const ALLOWED_TOOL_TYPES: &[&str] = &["function"];

// Tool-level fields that Fireworks AI / OCg upstream rejects
const TOOL_FORBIDDEN_FIELDS: &[&str] = &["strict"];

const OPENCODE_GO_BASE: &str = "https://opencode.ai/zen/go/v1";
const OPENCODE_GO_CLAUDE_PATH: &str = "/messages";
const OPENCODE_GO_DEFAULT_PATH: &str = "/chat/completions";
const CLAUDE_FORMAT_MODELS: [&str; 2] = ["minimax-m2.5", "minimax-m2.7"];

#[derive(Clone)]
pub struct OpenCodeGoExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum OpenCodeGoExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for OpenCodeGoExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for OpenCodeGoExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for OpenCodeGoExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for OpenCodeGoExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for OpenCodeGoExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct OpenCodeGoExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct OpenCodeGoExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl OpenCodeGoExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, OpenCodeGoExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn is_claude_format(model: &str) -> bool {
        CLAUDE_FORMAT_MODELS.contains(&model)
    }

    fn build_url(&self, model: &str) -> String {
        let path = if Self::is_claude_format(model) {
            OPENCODE_GO_CLAUDE_PATH
        } else {
            OPENCODE_GO_DEFAULT_PATH
        };
        format!("{}{}", OPENCODE_GO_BASE, path)
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
        model: &str,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let key = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .unwrap_or("");

        if Self::is_claude_format(model) {
            headers.insert(
                "x-api-key",
                HeaderValue::from_str(key).unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        } else {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", key))
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        headers
    }

    pub async fn execute_request(
        &self,
        mut request: OpenCodeGoExecutionRequest,
    ) -> Result<OpenCodeGoExecutorResponse, OpenCodeGoExecutorError> {
        let url = self.build_url(&request.model);
        let headers = self.build_headers(&request.credentials, request.stream, &request.model);

        // Normalize developer→system role for providers that reject role:developer (DeepSeek, etc.)
        normalize_developer_role(&mut request.body);

        // Strip forbidden fields that Cloudflare Workers AI / Fireworks reject
        let mut needs_chat_format = false;
        if let Some(obj) = request.body.as_object_mut() {
            for field in FORBIDDEN_FIELDS {
                obj.remove(*field);
            }
            // Also strip from nested assistant tool_calls and messages
            if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
                for msg in messages.iter_mut() {
                    if let Some(msg_obj) = msg.as_object_mut() {
                        for field in FORBIDDEN_FIELDS {
                            msg_obj.remove(*field);
                        }
                    }
                }
            }

            // Convert Responses API format (input) to chat format (messages)
            // Codex CLI sends { model, input, client_metadata, ... }
            // OpenCode Go /zen/go/v1/chat/completions expects { model, messages, ... }
            if obj.contains_key("input") && !obj.contains_key("messages") {
                needs_chat_format = true;
                if let Some(input) = obj.remove("input") {
                    let messages = responses_input_to_messages(input);
                    obj.insert("messages".to_string(), messages);
                }
                // Responses API uses `previous_response_id` for multi-turn
                obj.remove("previous_response_id");
                // Responses API uses `instructions` — map to system message if present
                if let Some(instructions) = obj.remove("instructions") {
                    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
                        let sys_msg = serde_json::json!({
                            "role": "system",
                            "content": instructions
                        });
                        messages.insert(0, sys_msg);
                    }
                }
            }

            // Remove Responses API fields that aren't needed for chat
            obj.remove("tool_choice"); // OCg chat endpoint doesn't use Responses' tool_choice format
        }

        // Re-normalize developer→system after potential input→messages conversion
        if needs_chat_format {
            normalize_developer_role(&mut request.body);
        }

        // --- Fix: strip unsupported tools for Fireworks AI ---
        if let Some(obj) = request.body.as_object_mut() {
            // Fireworks rejects: tools with type != "function", and any top-level
            // tool fields besides "type" and "function" (no name/description/parameters at tool level).
            // Codex may send tools in flat format (name/desc/params at tool level) or
            // nested format (inside function:{}). Both need to be normalized.
            if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
                tools.retain(|tool| {
                    let t = tool.get("type").and_then(Value::as_str).unwrap_or("");
                    ALLOWED_TOOL_TYPES.contains(&t)
                });
                for tool in tools.iter_mut() {
                    let mut cleaned = serde_json::Map::new();
                    // Keep type
                    if let Some(t) = tool.get("type").and_then(Value::as_str) {
                        cleaned.insert("type".into(), Value::String(t.into()));
                    }
                    // Extract function name from either nested function:{} or top-level name
                    let fname = tool
                        .get("function")
                        .and_then(|f| f.get("name").and_then(Value::as_str))
                        .or_else(|| tool.get("name").and_then(Value::as_str))
                        .unwrap_or("")
                        .to_string();
                    if fname.is_empty() {
                        continue;
                    }
                    cleaned.insert(
                        "function".into(),
                        serde_json::json!({
                            "name": fname
                        }),
                    );
                    *tool = Value::Object(cleaned);
                }
            }
        }

        // Log if we had to convert format, so debugging is easier
        if needs_chat_format {
            tracing::debug!("Converted Responses API format to chat format for opencode-go");
        }

        let client = self.pool.get("opencode-go", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&request.body)
            .send()
            .await?;

        Ok(OpenCodeGoExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body: request.body,
            transport: TransportKind::Reqwest,
        })
    }
}

/// Convert Responses API `input` (array or string) to chat `messages` array.
///
/// OpenAI Responses API format:
/// ```json
/// {"input": [{"role": "user", "content": "..."}, {"role": "assistant", "content": "..."}]}
/// ```
/// or a plain string `"input": "hello"`.
///
/// Chat format expects:
/// ```json
/// {"messages": [{"role": "user", "content": "..."}]}
/// ```
fn responses_input_to_messages(input: Value) -> Value {
    match input {
        Value::Array(items) => {
            let messages: Vec<Value> = items
                .into_iter()
                .filter_map(|item| {
                    let item_obj = match item {
                        Value::Object(m) => m,
                        _ => return None,
                    };
                    // Only keep items that have a "role" field
                    if item_obj.contains_key("role") {
                        Some(Value::Object(item_obj))
                    } else {
                        None
                    }
                })
                .collect();
            Value::Array(messages)
        }
        Value::String(text) => {
            json!([{"role": "user", "content": text}])
        }
        _ => {
            json!([{"role": "user", "content": input.to_string()}])
        }
    }
}
