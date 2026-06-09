use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::config::app_constants::{
    INTERNAL_REQUEST_HEADER_NAME, INTERNAL_REQUEST_HEADER_VALUE,
};
use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const GEMINI_CLI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const GEMINI_CLI_API_CLIENT: &str = "google-genai-sdk/1.41.0 gl-node/v22.19.0";
const GEMINI_CLI_VERSION: &str = "0.31.0";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[derive(Clone)]
pub struct GeminiCliExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum GeminiCliExecutorError {
    MissingCredentials(String),
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for GeminiCliExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for GeminiCliExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for GeminiCliExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for GeminiCliExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for GeminiCliExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct GeminiCliExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct GeminiCliExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct GeminiCliTokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

impl GeminiCliExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, GeminiCliExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(&self, model: &str, stream: bool) -> String {
        let action = if stream {
            "streamGenerateContent?alt=sse"
        } else {
            "generateContent"
        };
        format!("{}/{}:{}", GEMINI_CLI_BASE_URL, model, action)
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
        model: &str,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let Some(token) = credentials.access_token.as_deref() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", token))
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }

        let ua = format!("GeminiCLI/{}/{} (linux; x64)", GEMINI_CLI_VERSION, model);
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_str(&ua).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "X-Goog-Api-Client",
            HeaderValue::from_static(GEMINI_CLI_API_CLIENT),
        );
        headers.insert(
            INTERNAL_REQUEST_HEADER_NAME,
            HeaderValue::from_static(INTERNAL_REQUEST_HEADER_VALUE),
        );

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        headers
    }

    fn transform_request(&self, body: &Value, credentials: &ProviderConnection) -> Value {
        let mut transformed = body.clone();
        if transformed.get("project").is_none() {
            if let Some(project_id) = credentials.provider_specific_data.get("projectId") {
                transformed["project"] = project_id.clone();
            }
        }
        transformed
    }

    pub async fn execute_request(
        &self,
        request: GeminiCliExecutionRequest,
    ) -> Result<GeminiCliExecutorResponse, GeminiCliExecutorError> {
        let url = self.build_url(&request.model, request.stream);
        let headers = self.build_headers(&request.credentials, request.stream, &request.model);
        let transformed_body = self.transform_request(&request.body, &request.credentials);

        let client = self.pool.get("gemini-cli", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(GeminiCliExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    pub async fn refresh_token(
        &self,
        refresh_token: &str,
        client_id: &str,
        client_secret: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Option<GeminiCliTokenResponse> {
        let client = reqwest::Client::builder().build().ok()?;

        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ];

        let response = client
            .post(GOOGLE_TOKEN_URL)
            .form(&params)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;

        if !response.status().is_success() {
            return None;
        }

        response.json::<GeminiCliTokenResponse>().await.ok()
    }
}
