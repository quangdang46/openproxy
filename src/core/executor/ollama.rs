use std::sync::Arc;

use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

pub struct OllamaExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug)]
pub enum OllamaExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidUri(InvalidUri),
    InvalidRequest(hyper::http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    ImageExtraction(String),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for OllamaExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<InvalidUri> for OllamaExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for OllamaExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for OllamaExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for OllamaExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for OllamaExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl std::fmt::Display for OllamaExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::InvalidCredentials(msg) => write!(f, "Invalid credentials: {}", msg),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::ImageExtraction(msg) => write!(f, "Image extraction error: {}", msg),
            Self::UnsupportedFormat(msg) => write!(f, "Unsupported format: {}", msg),
        }
    }
}

impl std::error::Error for OllamaExecutorError {}

pub struct OllamaExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for OllamaExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OllamaExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

pub struct OllamaExecutor {
    pool: Arc<ClientPool>,
}

impl OllamaExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    pub async fn execute_request(
        &self,
        request: OllamaExecutionRequest,
    ) -> Result<OllamaExecutorResponse, OllamaExecutorError> {
        let url = self.build_url(&request.model, request.stream);
        let headers = self.build_headers()?;

        let transformed_body = self.transform_request(&request.body)?;

        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get("ollama", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        Ok(OllamaExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    fn build_url(&self, _model: &str, stream: bool) -> String {
        format!("http://localhost:11434/api/chat?stream={}", stream)
    }

    fn build_headers(&self) -> Result<HeaderMap, OllamaExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    fn transform_request(&self, body: &Value) -> Result<Value, OllamaExecutorError> {
        let mut transformed = body.clone();

        if let Some(messages) = transformed
            .get_mut("messages")
            .and_then(|m| m.as_array_mut())
        {
            for message in messages {
                if let Some(content) = message.get_mut("content").and_then(|c| c.as_str()) {
                    if let Some(extracted) = self.extract_images_from_content(content) {
                        if let Some(obj) = message.as_object_mut() {
                            obj.insert("images".to_string(), serde_json::json!(extracted));
                        }
                    }
                }
            }
        }

        Ok(transformed)
    }

    fn extract_images_from_content(&self, content: &str) -> Option<Vec<String>> {
        let mut images = Vec::new();
        let mut current_pos = 0;

        while let Some(start) = content[current_pos..].find("data:image/") {
            let start = current_pos + start;
            if let Some(mime_end) = content[start..].find(";base64,") {
                let data_start = start + mime_end + 8;
                let remaining = &content[data_start..];
                let end = remaining
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '+' && c != '/' && c != '=')
                    .unwrap_or(remaining.len());
                if end > 0 {
                    images.push(remaining[..end].to_string());
                    current_pos = data_start + end;
                } else {
                    current_pos = start + 1;
                }
            } else {
                current_pos = start + 1;
            }
        }

        if images.is_empty() {
            None
        } else {
            Some(images)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_images_from_content_single() {
        let executor = OllamaExecutor::new(Arc::new(ClientPool::default()));
        let content = "Here's an image: data:image/png;base64,SGVsbG8=. And more text.";
        let images = executor.extract_images_from_content(content);
        assert!(images.is_some());
        let images = images.unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0], "SGVsbG8=");
    }

    #[test]
    fn test_extract_images_from_content_multiple() {
        let executor = OllamaExecutor::new(Arc::new(ClientPool::default()));
        let content = "data:image/png;base64,abc123 data:image/jpeg;base64,xyz789";
        let images = executor.extract_images_from_content(content);
        assert!(images.is_some());
        let images = images.unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0], "abc123");
        assert_eq!(images[1], "xyz789");
    }

    #[test]
    fn test_extract_images_from_content_none() {
        let executor = OllamaExecutor::new(Arc::new(ClientPool::default()));
        let content = "No images here, just plain text.";
        let images = executor.extract_images_from_content(content);
        assert!(images.is_none());
    }

    #[test]
    fn test_transform_request_extracts_images() {
        let executor = OllamaExecutor::new(Arc::new(ClientPool::default()));
        let body = serde_json::json!({
            "model": "llama3",
            "messages": [
                {
                    "role": "user",
                    "content": "data:image/png;base64,SGVsbG8=. Describe this image."
                }
            ],
            "stream": false
        });
        let result = executor.transform_request(&body);
        assert!(result.is_ok());
        let transformed = result.unwrap();
        let messages = transformed.get("messages").unwrap().as_array().unwrap();
        let images = messages[0].get("images");
        assert!(images.is_some());
    }

    #[test]
    fn test_build_url() {
        let executor = OllamaExecutor::new(Arc::new(ClientPool::default()));
        let url = executor.build_url("llama3", true);
        assert!(url.contains("stream=true"));
        let url_no_stream = executor.build_url("llama3", false);
        assert!(url_no_stream.contains("stream=false"));
    }

    #[test]
    fn test_build_headers() {
        let executor = OllamaExecutor::new(Arc::new(ClientPool::default()));
        let headers = executor.build_headers();
        assert!(headers.is_ok());
        let headers = headers.unwrap();
        assert!(headers.contains_key(CONTENT_TYPE));
        assert!(headers.contains_key(ACCEPT));
    }
}
