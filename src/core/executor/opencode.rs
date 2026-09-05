use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::core::utils::session_manager::resolve_session_identity;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const OPENCODE_BASE: &str = "https://opencode.ai";
const OPENCODE_PICKLE_PATH: &str = "/zen/v1/messages";
const OPENCODE_DEFAULT_PATH: &str = "/zen/v1/chat/completions";
const OPENCODE_RESPONSES_PATH: &str = "/zen/v1/responses";

/// Check if a model should be routed through the Responses API instead of chat.
/// Muse Spark models use `/zen/v1/responses`.
/// Mirrors `isResponsesModel` in `open-sse/executors/opencode.js:29-32`.
fn is_responses_model(model: &str) -> bool {
    let base = model.split([':', '@']).next().unwrap_or(model);
    base.contains("muse") && base.contains("spark")
}

#[derive(Clone)]
pub struct OpenCodeExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum OpenCodeExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for OpenCodeExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for OpenCodeExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for OpenCodeExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for OpenCodeExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for OpenCodeExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct OpenCodeExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
    /// Downstream request headers for passthrough (9router rawHeaders).
    pub raw_headers: std::collections::BTreeMap<String, String>,
}

pub struct OpenCodeExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl OpenCodeExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, OpenCodeExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(&self, model: &str) -> String {
        let path = if is_responses_model(model) {
            OPENCODE_RESPONSES_PATH
        } else if model == "big-pickle" {
            OPENCODE_PICKLE_PATH
        } else {
            OPENCODE_DEFAULT_PATH
        };
        format!("{}{}", OPENCODE_BASE, path)
    }

    /// Build headers for the OpenCode request.
    ///
    /// Session management (9router v0.5.55): resolve a stable per-conversation
    /// session ID and pass through downstream OpenCode-specific headers when
    /// present. Forward the downstream User-Agent if it contains "opencode".
    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
        body: &Value,
        raw_headers: &std::collections::BTreeMap<String, String>,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer public"));

        // Session ID resolution (9router resolveOpencodeSession).
        let session_id = resolve_session_identity(
            Some(&std::collections::HashMap::from_iter(
                raw_headers.iter().map(|(k, v)| (k.clone(), v.clone())),
            )),
            Some(body),
            Some(&credentials.id),
            "opencode",
        )
        .session_id;

        // Pass through downstream OpenCode-specific headers when present,
        // falling back to generated/default values.
        let downstream_ua = raw_headers
            .get("user-agent")
            .or_else(|| raw_headers.get("User-Agent"))
            .map(String::as_str)
            .unwrap_or("");
        let is_opencode_downstream = downstream_ua.to_lowercase().contains("opencode");

        let client = raw_headers
            .get("x-opencode-client")
            .map(String::as_str)
            .unwrap_or("desktop");
        let session = raw_headers
            .get("x-opencode-session")
            .map(String::as_str)
            .unwrap_or(&session_id);
        let request_id = raw_headers
            .get("x-opencode-request")
            .map(String::as_str)
            .unwrap_or("global");

        // User-Agent: forward downstream if it's an OpenCode client, else use default.
        let ua = if is_opencode_downstream {
            downstream_ua
        } else {
            "opencode"
        };
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(ua).unwrap_or_else(|_| HeaderValue::from_static("opencode")),
        );
        headers.insert(
            "x-opencode-client",
            HeaderValue::from_str(client).unwrap_or_else(|_| HeaderValue::from_static("desktop")),
        );
        headers.insert(
            "x-opencode-session",
            HeaderValue::from_str(session)
                .unwrap_or_else(|_| HeaderValue::from_static("ses_unknown")),
        );
        headers.insert(
            "x-opencode-request",
            HeaderValue::from_str(request_id)
                .unwrap_or_else(|_| HeaderValue::from_static("global")),
        );
        headers.insert(
            "x-opencode-project",
            HeaderValue::from_str(
                raw_headers
                    .get("x-opencode-project")
                    .map(String::as_str)
                    .unwrap_or("global"),
            )
            .unwrap_or_else(|_| HeaderValue::from_static("global")),
        );

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        headers
    }

    pub async fn execute_request(
        &self,
        mut request: OpenCodeExecutionRequest,
    ) -> Result<OpenCodeExecutorResponse, OpenCodeExecutorError> {
        // Normalize developer→system role (many providers reject role:developer)
        normalize_developer_role(&mut request.body);

        // Responses API models need max_tokens → max_output_tokens normalization.
        // Mirrors opencode.js:76-86 (transformRequest for isResponsesModel).
        let is_responses = is_responses_model(&request.model);
        if is_responses {
            if let Some(body_obj) = request.body.as_object_mut() {
                // Read the value first to avoid borrow conflicts
                let max_val = body_obj
                    .remove("max_tokens")
                    .or_else(|| body_obj.remove("max_completion_tokens"));
                if let Some(val) = max_val {
                    body_obj.insert("max_output_tokens".to_string(), val);
                }
            }
        }

        let url = self.build_url(&request.model);
        let headers = self.build_headers(
            &request.credentials,
            request.stream,
            &request.body,
            &request.raw_headers,
        );

        let client = self.pool.get("opencode", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&request.body)
            .send()
            .await?;

        Ok(OpenCodeExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body: request.body,
            transport: TransportKind::Reqwest,
        })
    }
}
