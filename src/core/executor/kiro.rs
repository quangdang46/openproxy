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

/// Rewrite the AWS region segment of an amazonaws.com host, e.g.
/// `q.us-east-1.amazonaws.com` → `q.{region}.amazonaws.com`.
/// 9router getOrderedBaseUrls parity: `([a-z]+)\.[a-z0-9-]+\.amazonaws\.com`
/// → `$1.{region}.amazonaws.com`.
fn regionalize_host(host_url: &str, region: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"([a-z]+)\.[a-z0-9-]+\.amazonaws\.com").expect("static regex")
    });
    re.replace(host_url, format!("$1.{region}.amazonaws.com"))
        .into_owned()
}

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
    /// An endpoint/auth-surface failure (401/403/404) on a URL that still had
    /// fallback surfaces left — mirrors 9router shouldRetry.
    EndpointStatus {
        status: u16,
        url: String,
        message: String,
    },
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
    /// api_key / external_idp / idc → amazonaws.com hosts first, regionalized
    /// to the token's region when the account specifies one (default us-east-1).
    pub fn build_url(
        &self,
        _model: &str,
        _stream: bool,
        credentials: &ProviderConnection,
    ) -> Vec<String> {
        let auth_method = credentials
            .provider_specific_data
            .get("authMethod")
            .or_else(|| credentials.provider_specific_data.get("auth_method"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let is_cw_surface =
            auth_method == "api_key" || auth_method == "external_idp" || auth_method == "idc";

        let base_urls: Vec<String> = KIRO_BASE_URLS.iter().map(|s| (*s).to_string()).collect();
        if !is_cw_surface {
            return base_urls;
        }

        // 9router getOrderedBaseUrls regionalization: rewrite the AWS region
        // segment of every amazonaws.com host to the token's region.
        // `([a-z]+)\.[a-z0-9-]+\.amazonaws\.com` → `$1.{region}.amazonaws.com`.
        let region = credentials
            .provider_specific_data
            .get("region")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|r| !r.is_empty() && *r != "us-east-1")
            .unwrap_or("");

        let regionalize = |u: &str| -> String {
            if region.is_empty() || !u.contains("amazonaws.com") {
                return u.to_string();
            }
            regionalize_host(u, region)
        };

        let amazon: Vec<String> = base_urls
            .iter()
            .filter(|u| u.contains("amazonaws.com"))
            .map(|u| regionalize(u))
            .collect();
        let others: Vec<String> = base_urls
            .iter()
            .filter(|u| !u.contains("amazonaws.com"))
            .cloned()
            .collect();

        // API-key accounts must try the q.* surface FIRST: the legacy
        // codewhisperer.* GenerateAssistantResponse endpoint authenticates
        // the key but rejects the same valid payload with
        // REQUEST_BODY_INVALID (a terminal 400). Ported from 9router
        // v0.5.45 fix(kiro): route API keys correctly.
        if auth_method == "api_key" {
            let q: Vec<String> = amazon
                .iter()
                .filter(|u| u.contains("://q."))
                .cloned()
                .collect();
            let remaining: Vec<String> = amazon
                .iter()
                .filter(|u| !u.contains("://q."))
                .cloned()
                .collect();
            if !q.is_empty() {
                return q.into_iter().chain(remaining).chain(others).collect();
            }
        }
        if amazon.is_empty() {
            return others;
        }
        amazon.into_iter().chain(others).collect()
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

        let api_key = credentials.api_key.as_deref().or(if is_api_key {
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
                HeaderName::from_static("TokenType"),
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
                    HeaderName::from_static("TokenType"),
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
        for (url_index, url) in urls.iter().enumerate() {
            // AWS JSON credentials → SigV4 (IDC / some enterprise paths)
            let is_aws_auth = request
                .credentials
                .access_token
                .as_deref()
                .map(|t| t.trim_start().starts_with('{'))
                .unwrap_or(false);

            let headers = if is_aws_auth {
                let credentials =
                    match Self::parse_aws_credentials(
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
                    // 9router shouldRetry: endpoint/auth-surface failures
                    // (401/403/404) fall through to the next URL — the same
                    // payload can succeed on a different surface. Payload-invalid
                    // 400 is terminal (sending the same body everywhere cannot
                    // repair it). EventStream→SSE conversion runs in
                    // kiro_to_openai_streaming (ResponseTransform path).
                    let status = response.status().as_u16();
                    let is_fallback_status = status == 401 || status == 403 || status == 404;
                    let has_fallback = url_index + 1 < urls.len();
                    if is_fallback_status && has_fallback {
                        last_error = Some(KiroExecutorError::EndpointStatus {
                            status,
                            url: url.clone(),
                            message: format!(
                                "Kiro endpoint {} returned {}; trying next surface",
                                url,
                                response.status()
                            ),
                        });
                        continue;
                    }
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

    /// Decode one or more complete AWS EventStream v1 binary frames into
    /// structured events. Partial trailing frames are ignored (the caller
    /// buffers across chunks).
    pub fn decode_chunk(data: &[u8]) -> Result<Vec<KiroEvent>, KiroExecutorError> {
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

            // Decode headers (the `:event-type`, `:message-type`, `:content-type`
            // etc.) and the JSON payload. 9router parseEventFrame parity.
            let headers =
                decode_eventstream_headers(&data[offset + Self::PRELUDE_LEN..payload_start])?;
            let payload: Option<Value> = if payload_end > payload_start {
                let raw = &data[payload_start..payload_end];
                let text = std::str::from_utf8(raw).ok();
                match text.map(str::trim) {
                    Some(t) if !t.is_empty() => Some(serde_json::from_str(t).map_err(|e| {
                        KiroExecutorError::EventStreamDecode(format!(
                            "EventStream payload is not valid JSON: {}",
                            e
                        ))
                    })?),
                    _ => None,
                }
            } else {
                None
            };

            events.push(KiroEvent {
                message_type: headers.get(":message-type").cloned().unwrap_or_default(),
                event_type: headers.get(":event-type").cloned().unwrap_or_default(),
                content_type: headers.get(":content-type").cloned().unwrap_or_default(),
                payload,
            });

            offset += total_length;
        }

        Ok(events)
    }
}

/// A decoded AWS EventStream v1 event frame.
#[derive(Debug, Clone)]
pub struct KiroEvent {
    pub message_type: String,
    pub event_type: String,
    pub content_type: String,
    pub payload: Option<Value>,
}

/// Decode the AWS EventStream v1 headers section (9router parseEventFrame
/// header loop). Returns a map of header-name → string value. Binary/other
/// typed headers are stringified; UUID (type 9), blob (6), byte/int (0-4)
/// are preserved as their text/bool/integer representation where meaningful.
fn decode_eventstream_headers(
    data: &[u8],
) -> Result<std::collections::HashMap<String, String>, KiroExecutorError> {
    let mut headers = std::collections::HashMap::new();
    let mut offset = 0usize;
    let header_end = data.len();

    let require_bytes = |offset: usize, count: usize| -> Result<(), KiroExecutorError> {
        if offset + count > header_end {
            return Err(KiroExecutorError::EventStreamDecode(
                "AWS EventStream header exceeds its declared bounds".to_string(),
            ));
        }
        Ok(())
    };

    while offset < header_end {
        require_bytes(offset, 1)?;
        let name_len = data[offset] as usize;
        offset += 1;
        require_bytes(offset, name_len + 1)?;
        let name = std::str::from_utf8(&data[offset..offset + name_len])
            .map_err(|e| {
                KiroExecutorError::EventStreamDecode(format!(
                    "AWS EventStream header name is not UTF-8: {}",
                    e
                ))
            })?
            .to_string();
        offset += name_len;
        if headers.contains_key(&name) {
            return Err(KiroExecutorError::EventStreamDecode(format!(
                "AWS EventStream contains duplicate header: {}",
                name
            )));
        }
        let ty = data[offset];
        offset += 1;

        match ty {
            0 | 1 => {
                headers.insert(
                    name,
                    if ty == 0 {
                        "false".to_string()
                    } else {
                        "true".to_string()
                    },
                );
            }
            2 => {
                require_bytes(offset, 1)?;
                headers.insert(name, data[offset].to_string());
                offset += 1;
            }
            3 => {
                require_bytes(offset, 2)?;
                let v = u16::from_be_bytes([data[offset], data[offset + 1]]) as i16;
                headers.insert(name, v.to_string());
                offset += 2;
            }
            4 => {
                require_bytes(offset, 4)?;
                let v = i32::from_be_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                headers.insert(name, v.to_string());
                offset += 4;
            }
            5 | 8 => {
                // Byte array (5) / long (8) — skip, not semantically needed.
                require_bytes(offset, 8)?;
                offset += 8;
            }
            6 | 7 => {
                // Blob (6) / string (7).
                require_bytes(offset, 2)?;
                let value_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
                offset += 2;
                require_bytes(offset, value_len)?;
                let raw = &data[offset..offset + value_len];
                let value = if ty == 7 {
                    String::from_utf8_lossy(raw).to_string()
                } else {
                    format!("{} bytes", raw.len())
                };
                headers.insert(name, value);
                offset += value_len;
            }
            9 => {
                // UUID.
                require_bytes(offset, 16)?;
                offset += 16;
            }
            other => {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "AWS EventStream header {} has unknown type {}",
                    name, other
                )));
            }
        }
    }

    Ok(headers)
}

/// Number of complete EventStream bytes consumed from a buffer, stopping at
/// the first incomplete frame (so the caller can keep the tail for the next
/// chunk). Mirrors `decode_chunk`'s framing logic without decoding payloads.
pub fn consumed_eventstream_bytes(data: &[u8]) -> usize {
    let mut offset = 0usize;
    while offset + 12 <= data.len() {
        let total_length = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        if !(16..=MAX_EVENTSTREAM_MESSAGE_LENGTH).contains(&total_length) {
            return offset;
        }
        if offset + total_length > data.len() {
            return offset;
        }
        offset += total_length;
    }
    offset
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
    fn test_event_stream_decoder_parses_headers_and_payload() {
        // Build a minimal AWS EventStream frame:
        //   prelude: total_length=12+headers+4, headers_length, crc(8 bytes)
        //   headers: nameLen ":event-type" ty=7 len valueLen "assistantResponseEvent"
        //   payload: JSON {"content":"hi"}
        let header_bytes = {
            let name = b":event-type";
            let value = b"assistantResponseEvent";
            let mut v = Vec::new();
            v.push(name.len() as u8);
            v.extend_from_slice(name);
            v.push(7u8); // string
            v.extend_from_slice(&(value.len() as u16).to_be_bytes());
            v.extend_from_slice(value);
            v
        };
        let payload = br#"{"content":"hi"}"#;
        let total = 12 + header_bytes.len() + payload.len() + 4;
        let mut frame = Vec::new();
        frame.extend_from_slice(&(total as u32).to_be_bytes());
        frame.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
        let prelude_crc = crc32fast::hash(&frame[..8]);
        frame.extend_from_slice(&prelude_crc.to_be_bytes());
        frame.extend_from_slice(&header_bytes);
        frame.extend_from_slice(payload);
        let msg_crc = crc32fast::hash(&frame);
        frame.extend_from_slice(&msg_crc.to_be_bytes());

        let events = EventStreamDecoder::decode_chunk(&frame).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "assistantResponseEvent");
        assert_eq!(events[0].payload.as_ref().unwrap()["content"], "hi");
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
    fn test_regionalize_host_eu_west_1() {
        assert_eq!(
            regionalize_host(
                "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
                "eu-west-1"
            ),
            "https://q.eu-west-1.amazonaws.com/generateAssistantResponse"
        );
        assert_eq!(
            regionalize_host(
                "https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse",
                "eu-west-1"
            ),
            "https://codewhisperer.eu-west-1.amazonaws.com/generateAssistantResponse"
        );
    }

    #[test]
    fn test_regionalize_host_noop_for_us_east_1_or_non_aws() {
        // Default region (us-east-1) leaves the host unchanged.
        assert_eq!(
            regionalize_host(
                "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
                "us-east-1"
            ),
            "https://q.us-east-1.amazonaws.com/generateAssistantResponse"
        );
        // Non-amazonaws host is untouched.
        assert_eq!(
            regionalize_host(
                "https://runtime.us-east-1.kiro.dev/generateAssistantResponse",
                "eu-west-1"
            ),
            "https://runtime.us-east-1.kiro.dev/generateAssistantResponse"
        );
    }

    #[test]
    fn test_build_url_regionalizes_q_host() {
        // 9router getOrderedBaseUrls: api_key surface regionalizes the AWS
        // host to the account's region and orders the q.* surface first.
        let executor = KiroExecutor::new(Arc::new(ClientPool::default()), None).unwrap();
        let mut psd = std::collections::BTreeMap::new();
        psd.insert("authMethod".to_string(), serde_json::json!("api_key"));
        psd.insert("region".to_string(), serde_json::json!("eu-west-1"));
        let credentials = ProviderConnection {
            provider_specific_data: psd,
            api_key: Some("key".to_string()),
            access_token: None,
            ..Default::default()
        };
        let urls = executor.build_url("amazon-nova-pro-v1.0", false, &credentials);
        assert_eq!(
            urls[0],
            "https://q.eu-west-1.amazonaws.com/generateAssistantResponse"
        );
        assert!(urls[0].contains("q.eu-west-1.amazonaws.com"));
        assert!(urls
            .iter()
            .any(|u| u.contains("codewhisperer.eu-west-1.amazonaws.com")));
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
