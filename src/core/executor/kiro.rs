use std::sync::Arc;

use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

pub struct KiroExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

/// Primary and fallback Kiro runtime endpoints.
/// The runtime endpoint uses the Kiro Gateway (not the old Bedrock API).
const KIRO_PRIMARY_ENDPOINT: &str = "https://runtime.us-east-1.kiro.dev/v1";
const KIRO_FALLBACK_ENDPOINTS: &[&str] = &[
    "https://runtime.us-east-2.kiro.dev/v1",
    "https://runtime.eu-west-1.kiro.dev/v1",
];
const KIRO_REGION: &str = "us-east-1";
const KIRO_SERVICE: &str = "kiro";

fn normalize_kiro_model(model: &str) -> String {
    if let Some(stripped) = model.strip_suffix("-thinking-agentic") {
        return stripped.to_string();
    }
    if let Some(stripped) = model.strip_suffix("-thinking") {
        return stripped.to_string();
    }
    if let Some(stripped) = model.strip_suffix("-agentic") {
        return stripped.to_string();
    }
    model.to_string()
}

pub struct KiroExecutorResponse {
    pub response: super::UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: super::TransportKind,
}

impl std::fmt::Debug for KiroExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiroExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

#[derive(Debug)]
pub enum KiroExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    SigningError(String),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    InvalidUri(InvalidUri),
    InvalidRequest(http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    EventStreamDecode(String),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for KiroExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for KiroExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for KiroExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<http::Error> for KiroExecutorError {
    fn from(error: http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for KiroExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for KiroExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for KiroExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct KiroExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

impl KiroExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, KiroExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn parse_aws_credentials(access_token: &str) -> Result<AwsCredentials, KiroExecutorError> {
        let credentials: AwsCredentials = serde_json::from_str(access_token).map_err(|e| {
            KiroExecutorError::InvalidCredentials(format!("JSON parse error: {}", e))
        })?;

        if credentials.access_key.is_empty() || credentials.secret_key.is_empty() {
            return Err(KiroExecutorError::InvalidCredentials(
                "AWS credentials missing access_key or secret_key".to_string(),
            ));
        }

        Ok(credentials)
    }

    pub fn build_url(&self, model: &str, stream: bool) -> Vec<String> {
        let action = if stream { "stream" } else { "invoke" };
        let model = normalize_kiro_model(model);
        let path = format!("/v1/{model}/{action}",);
        // Build list of candidate URLs with multi-host failover parity
        let mut urls = Vec::new();
        urls.push(format!("{}{}", KIRO_PRIMARY_ENDPOINT.trim_end_matches('/'), path));
        for fallback in KIRO_FALLBACK_ENDPOINTS {
            urls.push(format!("{}{}", fallback.trim_end_matches('/'), path));
        }
        urls
    }

    pub async fn execute_request(
        &self,
        request: KiroExecutionRequest,
    ) -> Result<KiroExecutorResponse, KiroExecutorError> {
        let urls = self.build_url(&request.model, request.stream);
        let body_bytes = serde_json::to_vec(&request.body)?;
        let content_hash = sha256_hex(&body_bytes);

        // Try each URL with failover
        let mut last_error = None;
        for url in &urls {
            // Determine auth path based on credentials
            // If access_token is AWS-style JSON -> SigV4; if plain string -> api-key header
            let is_aws_auth = request
                .credentials
                .access_token
                .as_deref()
                .map(|t| t.trim_start().starts_with('{'))
                .unwrap_or(false);

            if is_aws_auth {
                let credentials = match Self::parse_aws_credentials(
                    request
                        .credentials
                        .access_token
                        .as_deref()
                        .ok_or_else(|| KiroExecutorError::MissingCredentials("kiro".to_string()))?,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        last_error = Some(e);
                        continue;
                    }
                };

                let signed_headers = self
                    .sign_request(url, &credentials, &content_hash, request.stream)
                    .await?;

                let client = self.pool.get("kiro", request.proxy.as_ref())?;
                match client
                    .post(url)
                    .headers(signed_headers.clone())
                    .body(body_bytes.clone())
                    .send()
                    .await
                {
                    Ok(response) => {
                        return Ok(KiroExecutorResponse {
                            response: UpstreamResponse::Reqwest(response),
                            url: url.clone(),
                            headers: signed_headers,
                            transformed_body: request.body.clone(),
                            transport: TransportKind::Reqwest,
                        });
                    }
                    Err(e) => {
                        last_error = Some(KiroExecutorError::Request(e));
                        continue;
                    }
                }
            } else {
                // API key / external IDP auth path: x-api-key header
                let api_key = request
                    .credentials
                    .api_key
                    .as_deref()
                    .or_else(|| request.credentials.access_token.as_deref())
                    .ok_or_else(|| KiroExecutorError::MissingCredentials("kiro".to_string()))?;

                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
                headers.insert(
                    HeaderName::from_bytes(b"x-api-key").unwrap(),
                    HeaderValue::from_str(api_key).map_err(KiroExecutorError::InvalidHeader)?,
                );

                let client = self.pool.get("kiro", request.proxy.as_ref())?;
                match client
                    .post(url)
                    .headers(headers.clone())
                    .body(body_bytes.clone())
                    .send()
                    .await
                {
                    Ok(response) => {
                        return Ok(KiroExecutorResponse {
                            response: UpstreamResponse::Reqwest(response),
                            url: url.clone(),
                            headers,
                            transformed_body: request.body.clone(),
                            transport: TransportKind::Reqwest,
                        });
                    }
                    Err(e) => {
                        last_error = Some(KiroExecutorError::Request(e));
                        continue;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            KiroExecutorError::SigningError("All Kiro endpoints failed".to_string())
        }))
    }

    async fn sign_request(
        &self,
        url: &str,
        credentials: &AwsCredentials,
        content_hash: &str,
        _stream: bool,
    ) -> Result<HeaderMap, KiroExecutorError> {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let timestamp = chrono::Utc::now();
        let date_time = timestamp.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = timestamp.format("%Y%m%d").to_string();

        // Extract host from the actual URL for SigV4 signing
        let parsed_url = url::Url::parse(url)
            .map_err(|e| KiroExecutorError::SigningError(e.to_string()))?;
        let host = parsed_url.host_str().unwrap_or("runtime.us-east-1.kiro.dev");
        let region = KIRO_REGION;
        let service = KIRO_SERVICE;

        let x_amz_date = HeaderName::from_bytes(b"x-amz-date").unwrap();
        headers.insert(
            x_amz_date,
            HeaderValue::from_str(&date_time).map_err(KiroExecutorError::InvalidHeader)?,
        );

        let nonce = generate_nonce();
        let x_amz_nonce = HeaderName::from_bytes(b"x-amz-nonce").unwrap();
        headers.insert(
            x_amz_nonce,
            HeaderValue::from_str(&nonce).map_err(KiroExecutorError::InvalidHeader)?,
        );

        if let Some(ref session_token) = credentials.session_token {
            let x_amz_security_token = HeaderName::from_bytes(b"x-amz-security-token").unwrap();
            headers.insert(
                x_amz_security_token,
                HeaderValue::from_str(session_token).map_err(KiroExecutorError::InvalidHeader)?,
            );
        }

        let method = "POST";
        let parsed_url =
            url::Url::parse(url).map_err(|e| KiroExecutorError::SigningError(e.to_string()))?;
        let path = parsed_url.path();
        let query = parsed_url.query().unwrap_or("");

        let canonical_headers = format!(
            "accept:application/json\ncontent-type:application/json\nhost:{}\nx-amz-date:{}\nx-amz-nonce:{}{}",
            host,
            date_time,
            nonce,
            if let Some(token) = &credentials.session_token {
                format!("\nx-amz-security-token:{token}")
            } else {
                String::new()
            }
        );

        let signed_headers_str = "accept;content-type;host;x-amz-date;x-amz-nonce";
        let credential_scope = format!(
            "{}/{}/{}/{}/aws4_request",
            date_stamp, region, service, "aws4_request"
        );

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, path, query, canonical_headers, signed_headers_str, content_hash
        );

        let canonical_request_hash = sha256_hex(canonical_request.as_bytes());

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}\n{}",
            date_time, credential_scope, canonical_request_hash, canonical_request_hash
        );

        let mut k_date =
            HmacSha256::new_from_slice(format!("AWS4{}", credentials.secret_key).as_bytes())
                .expect("HMAC key length is valid");
        k_date.update(date_stamp.as_bytes());
        let k_date = k_date.finalize().into_bytes();

        let mut k_region = HmacSha256::new_from_slice(&k_date).expect("HMAC key length is valid");
        k_region.update(region.as_bytes());
        let k_region = k_region.finalize().into_bytes();

        let mut k_service =
            HmacSha256::new_from_slice(&k_region).expect("HMAC key length is valid");
        k_service.update(service.as_bytes());
        let k_service = k_service.finalize().into_bytes();

        let mut k_signing =
            HmacSha256::new_from_slice(&k_service).expect("HMAC key length is valid");
        k_signing.update(b"aws4_request");
        let k_signing = k_signing.finalize().into_bytes();

        let mut signature =
            HmacSha256::new_from_slice(&k_signing).expect("HMAC key length is valid");
        signature.update(string_to_sign.as_bytes());
        let signature = hex::encode(signature.finalize().into_bytes());

        let auth_header = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            credentials.access_key, credential_scope, signed_headers_str, signature
        );

        let authorization = HeaderName::from_bytes(b"authorization").unwrap();
        headers.insert(
            authorization,
            HeaderValue::from_str(&auth_header).map_err(KiroExecutorError::InvalidHeader)?,
        );

        Ok(headers)
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsCredentials {
    pub access_key: String,
    pub secret_key: String,
    #[serde(default)]
    pub session_token: Option<String>,
    #[serde(default)]
    pub expiration: Option<String>,
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn generate_nonce() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.gen();
    hex::encode(bytes)
}

/// Strip <thinking>...</thinking> blocks from Kiro streamed SSE content.
/// 9router open-sse/executors/kiro.js:~L165-180 parity.
fn strip_thinking_tags(data: &str) -> String {
    // Fast path: no thinking tags
    if !data.contains("<thinking") {
        return data.to_string();
    }
    let mut result = String::with_capacity(data.len());
    let mut remaining = data;
    while let Some(start) = remaining.find("<thinking") {
        // Append everything before <thinking
        result.push_str(&remaining[..start]);
        // Find the closing tag
        if let Some(end) = remaining[start..].find("</thinking>") {
            let close = start + end + "</thinking>".len();
            remaining = &remaining[close..];
        } else {
            // Unclosed tag — remove from <thinking to end
            break;
        }
    }
    result
}

pub struct EventStreamDecoder;

impl EventStreamDecoder {
    pub fn decode_chunk(data: &[u8]) -> Result<Vec<SseEvent>, KiroExecutorError> {
        if data.is_empty() {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            if data[offset] == 0xFF {
                if offset + 4 > data.len() {
                    break;
                }
                let length = u32::from_be_bytes([
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                    data[offset + 4],
                ]) as usize;

                if offset + 8 + length > data.len() {
                    break;
                }

                let payload = &data[offset + 8..offset + 8 + length];
                offset += 8 + length;

                if let Ok(text) = std::str::from_utf8(payload) {
                    for line in text.lines() {
                        if line.starts_with("data: ") {
                            let data_content = line.trim_start_matches("data: ");
                            if !data_content.is_empty() && data_content != "[DONE]" {
                                // Strip <thinking>...</thinking> blocks from Kiro streamed content
                                // 9router open-sse/executors/kiro.js:~L165-180 parity
                                let cleaned = strip_thinking_tags(data_content);
                                events.push(SseEvent { data: cleaned });
                            }
                        }
                    }
                }
            } else {
                offset += 1;
            }
        }

        Ok(events)
    }
}

#[derive(Debug, Clone)]
pub struct SseEvent {
    pub data: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_aws_credentials() {
        let json = r#"{"access_key":"AKIAIOSFODNN7EXAMPLE","secret_key":"secret123","session_token":"token"}"#;
        let creds = KiroExecutor::parse_aws_credentials(json).unwrap();
        assert_eq!(creds.access_key, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(creds.secret_key, "secret123");
        assert_eq!(creds.session_token, Some("token".to_string()));
    }

    #[test]
    fn test_event_stream_decoder_empty() {
        let events = EventStreamDecoder::decode_chunk(&[]).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex(b"hello");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_generate_nonce() {
        let nonce = generate_nonce();
        assert_eq!(nonce.len(), 32);
    }

    #[test]
    fn test_normalize_kiro_model() {
        assert_eq!(
            normalize_kiro_model("amazon-nova-pro-v1.0-thinking-agentic"),
            "amazon-nova-pro-v1.0"
        );
        assert_eq!(
            normalize_kiro_model("amazon-nova-pro-v1.0-thinking"),
            "amazon-nova-pro-v1.0"
        );
        assert_eq!(
            normalize_kiro_model("amazon-nova-pro-v1.0-agentic"),
            "amazon-nova-pro-v1.0"
        );
        assert_eq!(
            normalize_kiro_model("amazon-nova-pro-v1.0"),
            "amazon-nova-pro-v1.0"
        );
    }
}
