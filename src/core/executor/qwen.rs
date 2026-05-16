use std::sync::Arc;

use hyper::http::{self as hyper_http, uri::InvalidUri};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, DefaultExecutor, TransportKind, UpstreamResponse};

const QWEN_USER_AGENT: &str = "QwenCode/0.12.3 (linux; x64)";
const QWEN_DEFAULT_URL: &str = "portal.qwen.ai";

#[derive(Clone)]
pub struct QwenExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum QwenExecutorError {
    MissingCredentials(String),
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidUri(InvalidUri),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for QwenExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for QwenExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for QwenExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper_http::Error> for QwenExecutorError {
    fn from(_error: hyper_http::Error) -> Self {
        Self::RequestFailed("HTTP error".to_string())
    }
}

impl From<serde_json::Error> for QwenExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for QwenExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for QwenExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

pub struct QwenExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct QwenExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct QwenTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    resource_url: Option<String>,
}

impl QwenExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, QwenExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(&self, credentials: &ProviderConnection) -> String {
        let resource_url = credentials
            .provider_specific_data
            .get("resourceUrl")
            .and_then(|v| v.as_str());

        let host = resource_url
            .map(|u| {
                u.trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_string()
            })
            .unwrap_or_else(|| QWEN_DEFAULT_URL.to_string());

        format!("https://{}/v1/chat/completions", host)
    }

    fn build_headers(&self, credentials: &ProviderConnection, stream: bool) -> HeaderMap {
        let token = credentials
            .access_token
            .as_deref()
            .or_else(|| credentials.api_key.as_deref())
            .unwrap_or("");

        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token))
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(QWEN_USER_AGENT),
        );
        headers.insert("x-dashscope-authtype", HeaderValue::from_static("qwen-oauth"));
        headers.insert("x-dashscope-cachecontrol", HeaderValue::from_static("enable"));
        headers.insert("x-dashscope-useragent", HeaderValue::from_static(QWEN_USER_AGENT));
        headers.insert("x-stainless-arch", HeaderValue::from_static("x64"));
        headers.insert("x-stainless-lang", HeaderValue::from_static("js"));
        headers.insert("x-stainless-os", HeaderValue::from_static("Linux"));
        headers.insert(
            "x-stainless-package-version",
            HeaderValue::from_static("5.11.0"),
        );
        headers.insert("x-stainless-retry-count", HeaderValue::from_static("1"));
        headers.insert("x-stainless-runtime", HeaderValue::from_static("node"));
        headers.insert(
            "x-stainless-runtime-version",
            HeaderValue::from_static("v18.19.1"),
        );
        headers.insert("connection", HeaderValue::from_static("keep-alive"));
        headers.insert("accept-language", HeaderValue::from_static("*"));
        headers.insert("sec-fetch-mode", HeaderValue::from_static("cors"));

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        headers
    }

    fn is_qwen_thinking_active(body: &Value) -> bool {
        if let Some(thinking) = body.get("thinking") {
            if thinking.as_bool() == Some(true) {
                return true;
            }
            if let Some(obj) = thinking.as_object() {
                if obj.get("type").and_then(|v| v.as_str()) == Some("enabled") {
                    return true;
                }
            }
        }
        body.get("enable_thinking").and_then(|v| v.as_bool()) == Some(true)
    }

    fn transform_request(&self, body: &Value, stream: bool) -> Value {
        let mut next = body.clone();

        // Add stream_options for streaming
        if stream {
            if let Some(messages) = next.get("messages") {
                if !messages.is_null()
                    && next.get("stream_options").is_none()
                    && next.get("thinking").is_none()
                    && next.get("enable_thinking").is_none()
                    && next.get("stream").and_then(|v| v.as_bool()) != Some(false)
                {
                    next["stream_options"] = json!({ "include_usage": true });
                }
            }
        }

        // Sanitize tool_choice when thinking is active
        if Self::is_qwen_thinking_active(&next) {
            if let Some(tool_choice) = next.get("tool_choice") {
                let incompatible = tool_choice.as_str() == Some("required")
                    || (tool_choice.is_object() && !tool_choice.is_null());
                if incompatible {
                    next["tool_choice"] = json!("auto");
                }
            }
        }

        // Ensure system message
        if let Some(messages) = next.get_mut("messages") {
            if messages.is_array() {
                let system_msg = json!({
                    "role": "system",
                    "content": [{
                        "type": "text",
                        "text": "",
                        "cache_control": { "type": "ephemeral" }
                    }]
                });
                if let Some(arr) = messages.as_array_mut() {
                    arr.insert(0, system_msg);
                }
            }
        } else {
            next["messages"] = json!([{
                "role": "system",
                "content": [{
                    "type": "text",
                    "text": "",
                    "cache_control": { "type": "ephemeral" }
                }]
            }]);
        }

        next
    }

    pub async fn execute_request(
        &self,
        request: QwenExecutionRequest,
    ) -> Result<QwenExecutorResponse, QwenExecutorError> {
        let url = self.build_url(&request.credentials);
        let headers = self.build_headers(&request.credentials, request.stream);
        let transformed_body = self.transform_request(&request.body, request.stream);

        let client = self.pool.get("qwen", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(QwenExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    /// Refresh Qwen OAuth token
    pub async fn refresh_token(
        &self,
        refresh_token: &str,
        client_id: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Option<QwenTokenResponse> {
        let client = match reqwest::Client::builder().build() {
            Ok(c) => c,
            Err(_) => return None,
        };

        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ];

        let response = client
            .post("https://portal.qwen.ai/v1/oauth/token")
            .form(&params)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;

        if !response.status().is_success() {
            return None;
        }

        response.json::<QwenTokenResponse>().await.ok()
    }
}
