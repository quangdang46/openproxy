use std::sync::Arc;

use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Maximum total AWS EventStream message length (1 MiB).
const MAX_EVENTSTREAM_MESSAGE_LENGTH: usize = 1024 * 1024;

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

/// 9router registry baseUrls (generateAssistantResponse surfaces).
const KIRO_BASE_URLS: &[&str] = &[
    "https://runtime.us-east-1.kiro.dev/generateAssistantResponse",
    "https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse",
    "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
];
const KIRO_REGION: &str = "us-east-1";
const KIRO_SERVICE: &str = "codewhisperer";

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

    /// Auth-aware URL order (9router getOrderedBaseUrls).
    /// api_key / external_idp / idc → amazonaws.com hosts first.
    pub fn build_url(&self, _model: &str, _stream: bool, credentials: &ProviderConnection) -> Vec<String> {
        let auth_method = credentials
            .provider_specific_data
            .get("authMethod")
            .or_else(|| credentials.provider_specific_data.get("auth_method"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let is_cw_surface =
            auth_method == "api_key" || auth_method == "external_idp" || auth_method == "idc";

        let mut urls: Vec<String> = KIRO_BASE_URLS.iter().map(|s| (*s).to_string()).collect();
        if is_cw_surface {
            let amazon: Vec<String> = urls
                .iter()
                .filter(|u| u.contains("amazonaws.com"))
                .cloned()
                .collect();
            let others: Vec<String> = urls
                .iter()
                .filter(|u| !u.contains("amazonaws.com"))
                .cloned()
                .collect();
            if !amazon.is_empty() {
                urls = amazon.into_iter().chain(others).collect();
            }
        }
        urls
    }

    fn build_bearer_headers(
        &self,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, KiroExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.amazon.eventstream"),
        );
        headers.insert(
            HeaderName::from_static("amz-sdk-request"),
            HeaderValue::from_static("attempt=1; max=3"),
        );
        let inv_id = uuid::Uuid::new_v4().to_string();
        headers.insert(
            HeaderName::from_static("amz-sdk-invocation-id"),
            HeaderValue::from_str(&inv_id).map_err(KiroExecutorError::InvalidHeader)?,
        );

        let auth_method = credentials
            .provider_specific_data
            .get("authMethod")
            .or_else(|| credentials.provider_specific_data.get("auth_method"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_api_key = auth_method == "api_key";
        let is_external_idp = auth_method == "external_idp";

        let api_key = credentials
            .api_key
            .as_deref()
            .or(if is_api_key {
                credentials.access_token.as_deref()
            } else {
                None
            });

        if is_api_key {
            let key = api_key
                .or(credentials.access_token.as_deref())
                .ok_or_else(|| KiroExecutorError::MissingCredentials("kiro".into()))?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(KiroExecutorError::InvalidHeader)?,
            );
            headers.insert(
                HeaderName::from_static("tokentype"),
                HeaderValue::from_static("API_KEY"),
            );
        } else {
            let token = credentials
                .access_token
                .as_deref()
                .ok_or_else(|| KiroExecutorError::MissingCredentials("kiro".into()))?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(KiroExecutorError::InvalidHeader)?,
            );
            if is_external_idp {
                headers.insert(
                    HeaderName::from_static("tokentype"),
                    HeaderValue::from_static("EXTERNAL_IDP"),
                );
            }
        }
        Ok(headers)
    }

    pub async fn execute_request(
        &self,
        request: KiroExecutionRequest,
    ) -> Result<KiroExecutorResponse, KiroExecutorError> {
        let urls = self.build_url(&request.model, request.stream, &request.credentials);
        let body_bytes = serde_json::to_vec(&request.body)?;
        let content_hash = sha256_hex(&body_bytes);

        // Try each URL with failover
        let mut last_error = None;
        for url in &urls {
            // AWS JSON credentials → SigV4 (IDC / some enterprise paths)
            let is_aws_auth = request
                .credentials
                .access_token
                .as_deref()
                .map(|t| t.trim_start().starts_with('{'))
                .unwrap_or(false);

            let headers = if is_aws_auth {
                let credentials = match Self::parse_aws_credentials(
                    request.credentials.access_token.as_deref().ok_or_else(|| {
                        KiroExecutorError::MissingCredentials("kiro".to_string())
                    })?,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        last_error = Some(e);
                        continue;
                    }
                };
                match self
                    .sign_request(url, &credentials, &content_hash, request.stream)
                    .await
                {
                    Ok(h) => h,
                    Err(e) => {
                        last_error = Some(e);
                        continue;
                    }
                }
            } else {
                // 9router: Bearer accessToken (+ tokentype API_KEY / EXTERNAL_IDP)
                match self.build_bearer_headers(&request.credentials) {
                    Ok(h) => h,
                    Err(e) => {
                        last_error = Some(e);
                        continue;
                    }
                }
            };

            let client = self.pool.get("kiro", request.proxy.as_ref())?;
            match client
                .post(url)
                .headers(headers.clone())
                .body(body_bytes.clone())
                .send()
                .await
            {
                Ok(response) => {
                    // EventStream→SSE conversion runs in kiro_to_openai_streaming
                    // (ResponseTransform path). URLs/auth now match 9router.
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
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.amazon.eventstream"),
        );

        let timestamp = chrono::Utc::now();
        let date_time = timestamp.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = timestamp.format("%Y%m%d").to_string();

        // Extract host from the actual URL for SigV4 signing
        let parsed_url =
            url::Url::parse(url).map_err(|e| KiroExecutorError::SigningError(e.to_string()))?;
        let host = parsed_url
            .host_str()
            .unwrap_or("runtime.us-east-1.kiro.dev");
        let region = KIRO_REGION;
        let service = KIRO_SERVICE;

        let x_amz_date = HeaderName::from_bytes(b"x-amz-date").unwrap();
        headers.insert(
            x_amz_date,
            HeaderValue::from_str(&date_time).map_err(KiroExecutorError::InvalidHeader)?,
        );

        let x_amz_content_sha256 = HeaderName::from_bytes(b"x-amz-content-sha256").unwrap();
        headers.insert(
            x_amz_content_sha256,
            HeaderValue::from_str(content_hash).map_err(KiroExecutorError::InvalidHeader)?,
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
            "accept:application/vnd.amazon.eventstream\ncontent-type:application/json\nhost:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\nx-amz-nonce:{}{}",
            host,
            content_hash,
            date_time,
            nonce,
            if let Some(token) = &credentials.session_token {
                format!("\nx-amz-security-token:{token}")
            } else {
                String::new()
            }
        );

        let signed_headers_str = if credentials.session_token.is_some() {
            "accept;content-type;host;x-amz-content-sha256;x-amz-date;x-amz-nonce;x-amz-security-token"
        } else {
            "accept;content-type;host;x-amz-content-sha256;x-amz-date;x-amz-nonce"
        };
        let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, region, service);

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, path, query, canonical_headers, signed_headers_str, content_hash
        );

        let canonical_request_hash = sha256_hex(canonical_request.as_bytes());

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            date_time, credential_scope, canonical_request_hash
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
    /// AWS EventStream v1 binary message prelude: 12 bytes.
    const PRELUDE_LEN: usize = 12;
    /// Trailing message CRC: 4 bytes.
    const TRAILING_CRC_LEN: usize = 4;

    pub fn decode_chunk(data: &[u8]) -> Result<Vec<SseEvent>, KiroExecutorError> {
        if data.is_empty() {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        let mut offset = 0;

        while offset + Self::PRELUDE_LEN <= data.len() {
            // Parse the 12-byte prelude
            let prelude = &data[offset..offset + Self::PRELUDE_LEN];
            let total_length =
                u32::from_be_bytes([prelude[0], prelude[1], prelude[2], prelude[3]]) as usize;
            let headers_length =
                u32::from_be_bytes([prelude[4], prelude[5], prelude[6], prelude[7]]) as usize;
            let prelude_crc =
                u32::from_be_bytes([prelude[8], prelude[9], prelude[10], prelude[11]]);

            // Validate total length
            if !(Self::PRELUDE_LEN + Self::TRAILING_CRC_LEN..=MAX_EVENTSTREAM_MESSAGE_LENGTH)
                .contains(&total_length)
            {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "invalid message total_length={}",
                    total_length
                )));
            }

            // Validate headers length
            if headers_length > total_length - Self::PRELUDE_LEN - Self::TRAILING_CRC_LEN {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "invalid headers_length={} for total_length={}",
                    headers_length, total_length
                )));
            }

            // Verify prelude CRC (CRC32 of first 8 bytes)
            let expected_crc = crc32fast::hash(&prelude[..8]);
            if prelude_crc != expected_crc {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "prelude CRC mismatch: got {:#010x}, expected {:#010x}",
                    prelude_crc, expected_crc
                )));
            }

            // Check we have enough data for the full message
            if offset + total_length > data.len() {
                break;
            }

            let payload_start = offset + Self::PRELUDE_LEN + headers_length;
            let payload_end = offset + total_length - Self::TRAILING_CRC_LEN;
            let crc_start = offset + total_length - Self::TRAILING_CRC_LEN;

            // Verify message CRC (CRC32 of everything except the trailing 4 bytes)
            let message_crc = u32::from_be_bytes([
                data[crc_start],
                data[crc_start + 1],
                data[crc_start + 2],
                data[crc_start + 3],
            ]);
            let expected_message_crc = crc32fast::hash(&data[offset..crc_start]);
            if message_crc != expected_message_crc {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "message CRC mismatch: got {:#010x}, expected {:#010x}",
                    message_crc, expected_message_crc
                )));
            }

            // Extract payload and parse SSE lines
            if payload_end > payload_start {
                let payload = &data[payload_start..payload_end];
                if let Ok(text) = std::str::from_utf8(payload) {
                    for line in text.lines() {
                        if line.starts_with("data: ") {
                            let data_content = line.trim_start_matches("data: ");
                            if !data_content.is_empty() && data_content != "[DONE]" {
                                let cleaned = strip_thinking_tags(data_content);
                                events.push(SseEvent { data: cleaned });
                            }
                        }
                    }
                }
            }

            offset += total_length;
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
