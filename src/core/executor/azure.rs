use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const DEFAULT_AZURE_ENDPOINT: &str = "https://api.openai.com";
const DEFAULT_API_VERSION: &str = "2024-10-01-preview";
const DEFAULT_DEPLOYMENT: &str = "gpt-4";

#[derive(Clone)]
pub struct AzureExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum AzureExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for AzureExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for AzureExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for AzureExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for AzureExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for AzureExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct AzureExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct AzureExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl AzureExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, AzureExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(&self, credentials: &ProviderConnection, model: &str) -> String {
        // Prefer env vars over config values for deployment-specific overrides.
        let endpoint = std::env::var("AZURE_OPENAI_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                credentials
                    .provider_specific_data
                    .get("azureEndpoint")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| DEFAULT_AZURE_ENDPOINT.to_string());

        let api_version = std::env::var("AZURE_OPENAI_API_VERSION")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                credentials
                    .provider_specific_data
                    .get("apiVersion")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());

        let deployment = std::env::var("AZURE_OPENAI_DEPLOYMENT")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                credentials
                    .provider_specific_data
                    .get("deployment")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| {
                if model.is_empty() {
                    DEFAULT_DEPLOYMENT.to_string()
                } else {
                    model.to_string()
                }
            });

        let endpoint = endpoint.trim_end_matches('/');
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            endpoint, deployment, api_version
        )
    }

    fn build_headers(&self, credentials: &ProviderConnection, stream: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let api_key = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref());

        if let Some(key) = api_key {
            if let Ok(header_val) = HeaderValue::from_str(key) {
                headers.insert("api-key", header_val);
            }
        }

        if let Some(org) = credentials
            .provider_specific_data
            .get("organization")
            .and_then(|v| v.as_str())
        {
            if let Ok(header_val) = HeaderValue::from_str(org) {
                headers.insert("openai-organization", header_val);
            }
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        headers
    }

    pub async fn execute_request(
        &self,
        request: AzureExecutionRequest,
    ) -> Result<AzureExecutorResponse, AzureExecutorError> {
        let url = self.build_url(&request.credentials, &request.model);
        let headers = self.build_headers(&request.credentials, request.stream);
        let transformed_body = request.body.clone();

        let client = self.pool.get("azure", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(AzureExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }
}
