use std::sync::Arc;

use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use sha2::Sha256;
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct QoderExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum QoderExecutorError {
    MissingCredentials(String),
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for QoderExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for QoderExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for QoderExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for QoderExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for QoderExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct QoderExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct QoderExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl QoderExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, QoderExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn create_signature(
        user_agent: &str,
        session_id: &str,
        timestamp: &str,
        api_key: &str,
    ) -> String {
        let payload = format!("{}:{}:{}", user_agent, session_id, timestamp);
        let mut mac =
            HmacSha256::new_from_slice(api_key.as_bytes()).expect("HMAC can take key of any size");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn build_url(&self, credentials: &ProviderConnection) -> String {
        credentials
            .provider_specific_data
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("https://api.qoder.com/v1/chat/completions")
            .to_string()
    }

    fn build_headers(&self, credentials: &ProviderConnection, stream: bool) -> HeaderMap {
        let session_id = format!("session-{}", Uuid::new_v4());
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        let user_agent = "Qoder-Cli";
        let api_key = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .unwrap_or("");

        let signature = Self::create_signature(user_agent, &session_id, &timestamp, api_key);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "session-id",
            HeaderValue::from_str(&session_id).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "x-qoder-timestamp",
            HeaderValue::from_str(&timestamp).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "x-qoder-signature",
            HeaderValue::from_str(&signature).unwrap_or_else(|_| HeaderValue::from_static("")),
        );

        if !api_key.is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", api_key))
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        headers
    }

    fn transform_request(&self, body: &Value, stream: bool) -> Value {
        let mut next = body.clone();
        if stream {
            if let Some(messages) = next.get("messages") {
                if !messages.is_null() && next.get("stream_options").is_none() {
                    next["stream_options"] = serde_json::json!({ "include_usage": true });
                }
            }
        }
        next
    }

    pub async fn execute_request(
        &self,
        request: QoderExecutionRequest,
    ) -> Result<QoderExecutorResponse, QoderExecutorError> {
        let url = self.build_url(&request.credentials);
        let headers = self.build_headers(&request.credentials, request.stream);
        let transformed_body = self.transform_request(&request.body, request.stream);

        let client = self.pool.get("qoder", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(QoderExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }
}
