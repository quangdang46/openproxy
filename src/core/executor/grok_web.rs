use std::sync::Arc;

use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, COOKIE};
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

pub struct GrokWebExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug)]
pub enum GrokWebExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidUri(InvalidUri),
    InvalidRequest(hyper::http::Error),
    CookieParse(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for GrokWebExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<InvalidUri> for GrokWebExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for GrokWebExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for GrokWebExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for GrokWebExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for GrokWebExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl std::fmt::Display for GrokWebExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::InvalidCredentials(msg) => write!(f, "Invalid credentials: {}", msg),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
            Self::CookieParse(msg) => write!(f, "Cookie parse error: {}", msg),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::UnsupportedFormat(msg) => write!(f, "Unsupported format: {}", msg),
        }
    }
}

impl std::error::Error for GrokWebExecutorError {}

pub struct GrokWebExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for GrokWebExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrokWebExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

pub struct GrokWebExecutor {
    pool: Arc<ClientPool>,
}

impl GrokWebExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    pub async fn execute_request(
        &self,
        request: GrokWebExecutionRequest,
    ) -> Result<GrokWebExecutorResponse, GrokWebExecutorError> {
        let url = self.build_url();
        let headers = self.build_headers(&request.credentials)?;
        let transformed_body = self.transform_request(&request.body);

        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get("grok-web", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        Ok(GrokWebExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    fn build_url(&self) -> String {
        "https://grok.com/app-chat/conversations/new".to_string()
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, GrokWebExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        if let Some(sso) = credentials.access_token.as_ref() {
            let cookie_value = HeaderValue::from_str(&format!("sso={}", sso))
                .map_err(|_| GrokWebExecutorError::CookieParse("Invalid cookie".to_string()))?;
            headers.insert(COOKIE, cookie_value);
        }

        Ok(headers)
    }

    fn transform_request(&self, body: &Value) -> Value {
        body.clone()
    }
}

pub struct PerplexityWebExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug)]
pub enum PerplexityWebExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidUri(InvalidUri),
    InvalidRequest(hyper::http::Error),
    CookieParse(String),
    Serialize(serde_json::Error),
    SessionCache(String),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for PerplexityWebExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<InvalidUri> for PerplexityWebExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for PerplexityWebExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for PerplexityWebExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for PerplexityWebExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for PerplexityWebExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl std::fmt::Display for PerplexityWebExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::InvalidCredentials(msg) => write!(f, "Invalid credentials: {}", msg),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
            Self::CookieParse(msg) => write!(f, "Cookie parse error: {}", msg),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::SessionCache(msg) => write!(f, "Session cache error: {}", msg),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::UnsupportedFormat(msg) => write!(f, "Unsupported format: {}", msg),
        }
    }
}

impl std::error::Error for PerplexityWebExecutorError {}

pub struct PerplexityWebExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for PerplexityWebExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerplexityWebExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

pub struct PerplexityWebExecutor {
    pool: Arc<ClientPool>,
}

impl PerplexityWebExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    pub async fn execute_request(
        &self,
        request: PerplexityWebExecutionRequest,
    ) -> Result<PerplexityWebExecutorResponse, PerplexityWebExecutorError> {
        let url = self.build_url();
        let headers = self.build_headers(&request.credentials)?;
        let transformed_body = self.transform_request(&request.body, &request.credentials)?;

        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get("perplexity-web", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        Ok(PerplexityWebExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    fn build_url(&self) -> String {
        "https://perplexity.ai/rest/sse/perplexity_ask".to_string()
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, PerplexityWebExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

        if let Some(cookie) = &credentials.access_token {
            let cookie_value =
                HeaderValue::from_str(cookie).map_err(|_| {
                    PerplexityWebExecutorError::CookieParse("Invalid cookie".to_string())
                })?;
            headers.insert(COOKIE, cookie_value);
        }

        Ok(headers)
    }

    fn fnv1a_hash(&self, content: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in content.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn transform_request(
        &self,
        body: &Value,
        _credentials: &ProviderConnection,
    ) -> Result<Value, PerplexityWebExecutorError> {
        let mut transformed = serde_json::Map::new();

        if let Some(obj) = body.as_object() {
            for (k, v) in obj {
                transformed.insert(k.clone(), v.clone());
            }
        }

        if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
            let conversation_context: String = messages
                .iter()
                .filter_map(|m| {
                    let role = m.get("role")?.as_str()?;
                    let content = m.get("content")?.as_str()?;
                    Some(format!("{}: {}", role, content))
                })
                .collect::<Vec<_>>()
                .join("\n");

            let session_hash = self.fnv1a_hash(&conversation_context);
            transformed.insert(
                "session_cache_key".to_string(),
                serde_json::json!(format!("{:016x}", session_hash)),
            );
        }

        Ok(Value::Object(transformed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_hash_empty() {
        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let hash = executor.fnv1a_hash("");
        assert_eq!(hash, 0xcbf29ce484222325);
    }

    #[test]
    fn test_fnv1a_hash_deterministic() {
        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let hash1 = executor.fnv1a_hash("hello");
        let hash2 = executor.fnv1a_hash("hello");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_fnv1a_hash_different_inputs() {
        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let hash1 = executor.fnv1a_hash("hello");
        let hash2 = executor.fnv1a_hash("world");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_grok_build_url() {
        let executor = GrokWebExecutor::new(Arc::new(ClientPool::default()));
        let url = executor.build_url();
        assert!(url.contains("grok.com"));
    }

    #[test]
    fn test_perplexity_build_url() {
        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let url = executor.build_url();
        assert!(url.contains("perplexity.ai"));
    }

    #[test]
    fn test_transform_request_adds_session_cache() {
        use std::collections::BTreeMap;

        let executor = PerplexityWebExecutor::new(Arc::new(ClientPool::default()));
        let body = serde_json::json!({
            "model": "sonar",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        let credentials = ProviderConnection {
            id: "test".to_string(),
            provider: "perplexity".to_string(),
            auth_type: "cookie".to_string(),
            name: None,
            priority: None,
            is_active: Some(true),
            created_at: None,
            updated_at: None,
            display_name: None,
            email: None,
            global_priority: None,
            default_model: None,
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: None,
            test_status: None,
            last_tested: None,
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: None,
            error_code: None,
            consecutive_use_count: None,
            backoff_level: None,
            consecutive_errors: None,
            provider_specific_data: BTreeMap::new(),
            extra: BTreeMap::new(),
        };
        let result = executor.transform_request(&body, &credentials);
        assert!(result.is_ok());
        let transformed = result.unwrap();
        assert!(transformed.get("session_cache_key").is_some());
    }
}