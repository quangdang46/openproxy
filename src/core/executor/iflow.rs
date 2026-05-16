use std::sync::Arc;

use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use sha2::Sha256;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

type HmacSha256 = Hmac<Sha256>;

const IFLOW_USER_AGENT: &str = "iFlow-Cli";
const IFLOW_BASE_URL: &str = "https://iflow.mintlify.cc";

#[derive(Clone)]
pub struct IFlowExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum IFlowExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for IFlowExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for IFlowExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for IFlowExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for IFlowExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for IFlowExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct IFlowExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct IFlowExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl IFlowExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, IFlowExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(&self) -> String {
        IFLOW_BASE_URL.to_string()
    }

    fn create_signature(user_agent: &str, session_id: &str, timestamp: i64, api_key: &str) -> String {
        if api_key.is_empty() {
            return String::new();
        }
        let payload = format!("{}:{}:{}", user_agent, session_id, timestamp);
        let mut mac = HmacSha256::new_from_slice(api_key.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> (HeaderMap, String) {
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let user_agent = IFLOW_USER_AGENT;
        let api_key = credentials
            .api_key
            .as_deref()
            .or_else(|| credentials.access_token.as_deref())
            .unwrap_or("");

        let signature = Self::create_signature(user_agent, &session_id, timestamp, api_key);

        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        if let Ok(val) = HeaderValue::from_str(&session_id) {
            headers.insert("session-id", val);
        }
        if let Ok(val) = HeaderValue::from_str(&timestamp.to_string()) {
            headers.insert("x-iflow-timestamp", val);
        }
        if let Ok(val) = HeaderValue::from_str(&signature) {
            headers.insert("x-iflow-signature", val);
        }

        if !credentials.api_key.as_deref().unwrap_or("").is_empty() {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", api_key)) {
                headers.insert(AUTHORIZATION, val);
            }
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        (headers, session_id)
    }

    fn transform_request(body: &Value, stream: bool) -> Value {
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
        request: IFlowExecutionRequest,
    ) -> Result<IFlowExecutorResponse, IFlowExecutorError> {
        let url = self.build_url();
        let (headers, _session_id) = self.build_headers(&request.credentials, request.stream);
        let transformed_body = Self::transform_request(&request.body, request.stream);

        let client = self.pool.get("iflow", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(IFlowExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }
}
