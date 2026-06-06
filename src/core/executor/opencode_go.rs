use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

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
        request: OpenCodeGoExecutionRequest,
    ) -> Result<OpenCodeGoExecutorResponse, OpenCodeGoExecutorError> {
        let url = self.build_url(&request.model);
        let headers = self.build_headers(&request.credentials, request.stream, &request.model);

        // Normalize developer→system role for providers that reject role:developer (DeepSeek, etc.)
        normalize_developer_role(&mut request.body);

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
