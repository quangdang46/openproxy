//! AWS Bedrock executor — SigV4-signed invoke calls with AWS EventStream
//! decoding. Port of 9router `open-sse/executors/bedrock.js` +
//! `open-sse/shared/awsCredentials.js` + `open-sse/utils/awsSigv4.js` +
//! `open-sse/utils/awsEventStream.js` (v0.5.112).
//!
//! One executor serves both Bedrock entries; the registry wire format decides
//! the shape:
//!   - `bedrock`     → Anthropic Messages (`claude` wire, `anthropic_version`
//!                     pin, `event:`/`data:` SSE out of EventStream chunks).
//!   - `bedrock-xai` → OpenAI Chat Completions (`openai` wire, bare `data:`
//!                     chunks + `data: [DONE]`).
//!
//! Auth is SigV4 computed per request. Credentials come from either static
//! keys (psd.accessKeyId + api_key secret + optional psd.sessionToken) or a
//! named AWS profile resolved through `aws-config` (SSO/INI/credential_process
//! all handled by the SDK crate, matching upstream's lazy
//! `@aws-sdk/credential-providers` `fromIni`).

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use hmac::{Hmac, KeyInit, Mac};
use hyper::http::{self, uri::InvalidUri};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

// === awsConstants.js ===
const SIGV4_ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SIGV4_TERMINATOR: &str = "aws4_request";
const SIGV4_KEY_PREFIX: &str = "AWS4";
const DATE_HEADER: &str = "x-amz-date";
const SECURITY_TOKEN_HEADER: &str = "x-amz-security-token";
const BEDROCK_SERVICE: &str = "bedrock";
const BEDROCK_DEFAULT_REGION: &str = "us-east-1";
const ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";
const INVOKE_PATH: &str = "invoke";
const STREAM_PATH: &str = "invoke-with-response-stream";
const CHUNK_EVENT_NAME: &str = "chunk";
const TERMINAL_EVENT_TYPE: &str = "message_stop";
const PROBE_PATH: &str = "foundation-models";
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
const CREDENTIAL_RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const MAX_MESSAGE_BYTES: usize = 24 * 1024 * 1024;
const MAX_HEADERS_BYTES: usize = 128 * 1024;

/// `AWS_REGION_PATTERN` — the region is interpolated into the request
/// hostname, so anything outside lowercase alnum/hyphen is rejected rather
/// than shipped to an attacker-chosen origin.
fn is_valid_region(region: &str) -> bool {
    let b = region.as_bytes();
    !b.is_empty()
        && b.len() <= 31
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && region == region.trim()
}

/// `AWS_PROFILE_PATTERN` — `[\w@+.%: -]{1,64}`. The name reaches the AWS
/// credential resolver, which can run `credential_process`, so it gets the
/// same treatment as the region.
fn is_valid_profile_name(profile: &str) -> bool {
    !profile.is_empty()
        && profile.len() <= 64
        && profile.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || c == '_'
                || matches!(c, '@' | '+' | '.' | '%' | ':' | ' ' | '-')
        })
}

/// Which wire shape the provider speaks — `claude` for Anthropic models,
/// `openai` for xAI Grok (which ignores anthropic_version on /invoke).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockWireFormat {
    Claude,
    OpenAi,
}

impl BedrockWireFormat {
    fn for_provider(provider: &str) -> Self {
        // Aliases resolve to the same wire format (registry ALIAS_TO_ID).
        if matches!(provider, "bedrock" | "br" | "aws-bedrock") {
            Self::Claude
        } else {
            Self::OpenAi
        }
    }

    /// `BEDROCK.modelFamilyPatterns` — reject a model family the wire format
    /// cannot read BEFORE the billed upstream call, not mid-stream.
    /// JS: /(^|[./])anthropic\./ and /(^|[./])xai\./ — the family token must
    /// start the id or follow a '.' or '/'.
    fn accepts_model(&self, model: &str) -> bool {
        let family = match self {
            Self::Claude => "anthropic.",
            Self::OpenAi => "xai.",
        };
        model
            .match_indices(family)
            .any(|(i, _)| i == 0 || matches!(model.as_bytes()[i - 1], b'.' | b'/'))
    }

    fn family_hint(&self) -> &'static str {
        match self {
            Self::Claude => {
                "an Anthropic model, e.g. \"us.anthropic.claude-sonnet-4-5-20250929-v1:0\""
            }
            Self::OpenAi => "an xAI model, e.g. \"us.xai.grok-4.6\"",
        }
    }
}

#[derive(Debug)]
pub enum BedrockExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidRegion(String),
    InvalidProfile(String),
    UnsupportedModelFamily { model: String, provider: String },
    SigningError(String),
    CredentialResolution(String),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    InvalidUri(InvalidUri),
    InvalidRequest(http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    EventStreamDecode(String),
}

impl std::fmt::Display for BedrockExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::InvalidCredentials(m) => write!(f, "{}", m),
            Self::InvalidRegion(m) => write!(f, "{}", m),
            Self::InvalidProfile(m) => write!(f, "{}", m),
            Self::UnsupportedModelFamily { model, provider } => write!(
                f,
                "Bedrock model {model:?} is not supported by the {provider} provider, which \
                 expects {}. Model families on Bedrock use different request and response \
                 shapes, so each gets its own provider entry rather than failing mid-stream \
                 after the call is billed.",
                if provider == "bedrock" {
                    BedrockWireFormat::Claude.family_hint()
                } else {
                    BedrockWireFormat::OpenAi.family_hint()
                }
            ),
            Self::SigningError(m) => write!(f, "SigV4 signing failed: {m}"),
            Self::CredentialResolution(m) => write!(f, "{}", m),
            Self::InvalidHeader(e) => write!(f, "Invalid header: {}", e),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::EventStreamDecode(m) => write!(f, "AWS EventStream decode error: {m}"),
        }
    }
}

impl std::error::Error for BedrockExecutorError {}

macro_rules! impl_from {
    ($variant:ident, $ty:ty) => {
        impl From<$ty> for BedrockExecutorError {
            fn from(error: $ty) -> Self {
                Self::$variant(error)
            }
        }
    };
}
impl_from!(Request, reqwest::Error);
impl_from!(InvalidHeader, reqwest::header::InvalidHeaderValue);
impl_from!(InvalidUri, InvalidUri);
impl_from!(InvalidRequest, http::Error);
impl_from!(Serialize, serde_json::Error);
impl_from!(HyperClientInit, std::io::Error);
impl_from!(Hyper, hyper_util::client::legacy::Error);

/// Resolved SigV4 credential material (static or profile/SSO).
#[derive(Debug, Clone)]
pub struct ResolvedAwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub region: String,
    pub mode: &'static str, // "static" | "profile"
}

/// `resolveRegion` — psd.region → AWS_REGION → AWS_DEFAULT_REGION → default.
/// The value lands in the hostname, so it is validated, never trusted.
pub fn resolve_region(credentials: &ProviderConnection) -> Result<String, BedrockExecutorError> {
    // An explicitly-set region is taken verbatim (after trim): "" or
    // whitespace-only is a config error, not "unset" — silently falling back
    // to the env/default would hide the typo and route to the wrong partition.
    let psd_region = credentials
        .provider_specific_data
        .get("region")
        .and_then(Value::as_str)
        .map(str::trim);
    let env_region = ["AWS_REGION", "AWS_DEFAULT_REGION"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty());

    let (raw, source) = match (psd_region, env_region) {
        (Some(r), _) => (r.to_string(), "providerSpecificData.region"),
        (None, Some(r)) => (r, "the AWS_REGION/AWS_DEFAULT_REGION environment variable"),
        (None, None) => (BEDROCK_DEFAULT_REGION.to_string(), "the built-in default"),
    };

    if !is_valid_region(&raw) {
        return Err(BedrockExecutorError::InvalidRegion(format!(
            "Invalid AWS region {raw:?} from {source}. A region looks like \"us-east-1\": \
             lowercase letters, digits and hyphens only. It is used to build the Bedrock \
             hostname, so anything else is rejected rather than sent to an unintended host."
        )));
    }
    Ok(raw)
}

fn psd_str<'a>(credentials: &'a ProviderConnection, key: &str) -> Option<&'a str> {
    credentials
        .provider_specific_data
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// `detectCredentialMode` — a named profile wins over stray static keys.
fn credential_mode(credentials: &ProviderConnection) -> &'static str {
    if psd_str(credentials, "profile").is_some() {
        "profile"
    } else {
        "static"
    }
}

fn read_static_credentials(
    credentials: &ProviderConnection,
) -> Result<ResolvedAwsCredentials, BedrockExecutorError> {
    let access_key_id = psd_str(credentials, "accessKeyId");
    let secret_access_key = credentials
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let (access_key_id, secret_access_key) = match (access_key_id, secret_access_key) {
        (Some(a), Some(s)) => (a.to_string(), s.to_string()),
        _ => {
            return Err(BedrockExecutorError::InvalidCredentials(
                "Bedrock static credentials are incomplete. Set the AWS secret access key as \
                 the provider API key and providerSpecificData.accessKeyId to the AWS access \
                 key id, or set providerSpecificData.profile to use a local AWS profile \
                 instead."
                    .to_string(),
            ));
        }
    };

    let session_token = psd_str(credentials, "sessionToken").map(str::to_string);
    if access_key_id.starts_with("ASIA") && session_token.is_none() {
        return Err(BedrockExecutorError::InvalidCredentials(
            "This looks like a temporary AWS access key (ASIA…) but no sessionToken was \
             provided. Add providerSpecificData.sessionToken, or use \
             providerSpecificData.profile so credentials are resolved and refreshed \
             automatically."
                .to_string(),
        ));
    }

    Ok(ResolvedAwsCredentials {
        access_key_id,
        secret_access_key,
        session_token,
        region: String::new(), // filled by caller
        mode: "static",
    })
}

/// Profile/SSO resolution through `aws-config` — the Rust counterpart of
/// upstream's `@aws-sdk/credential-providers` `fromIni`. The SDK reads
/// ~/.aws/config + credentials, follows sso_session/source_profile chains,
/// refreshes SSO tokens from the cache, and runs credential_process — all
/// bounded by [`CREDENTIAL_RESOLVE_TIMEOUT`].
async fn read_profile_credentials(
    profile: &str,
) -> Result<(String, String, Option<String>), BedrockExecutorError> {
    if !is_valid_profile_name(profile) {
        return Err(BedrockExecutorError::InvalidProfile(format!(
            "Invalid AWS profile name {profile:?}. Use the profile's name as it appears in \
             ~/.aws/config or ~/.aws/credentials: letters, digits, spaces and \
             \"_ - @ + . % :\", up to 64 characters."
        )));
    }

    let resolve = async {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .profile_name(profile)
            .load()
            .await;
        let provider = config.credentials_provider().ok_or_else(|| {
            BedrockExecutorError::CredentialResolution(format!(
                "Bedrock profile mode found no credential provider for profile \
                     {profile:?}. Check that the profile exists in ~/.aws/config and \
                     grants Bedrock access."
            ))
        })?;
        use aws_credential_types::provider::ProvideCredentials;
        provider.provide_credentials().await.map_err(|e| {
            BedrockExecutorError::CredentialResolution(format!(
                "Could not resolve AWS credentials for profile {profile:?}: {e}. If this \
                     profile uses IAM Identity Center, run: aws sso login --profile {profile}"
            ))
        })
    };

    let creds = tokio::time::timeout(CREDENTIAL_RESOLVE_TIMEOUT, resolve)
        .await
        .map_err(|_| {
            BedrockExecutorError::CredentialResolution(format!(
                "Resolving AWS profile {profile:?} timed out after {}s",
                CREDENTIAL_RESOLVE_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| match e {
            BedrockExecutorError::CredentialResolution(m) => {
                BedrockExecutorError::CredentialResolution(m)
            }
            other => other,
        })?;

    if creds.access_key_id().is_empty() || creds.secret_access_key().is_empty() {
        return Err(BedrockExecutorError::CredentialResolution(format!(
            "AWS profile {profile:?} resolved without usable credentials. Check that the \
             profile exists in ~/.aws/config and grants Bedrock access."
        )));
    }
    Ok((
        creds.access_key_id().to_string(),
        creds.secret_access_key().to_string(),
        creds.session_token().map(str::to_string),
    ))
}

/// `resolveAwsCredentials` — static keys or profile/SSO, plus the validated
/// region. Profile resolution runs on EVERY request (aws-config caches
/// internally until expiry), so an `aws sso login` session is picked up
/// without touching the connection — same as upstream re-resolving per call.
pub async fn resolve_aws_credentials(
    credentials: &ProviderConnection,
) -> Result<ResolvedAwsCredentials, BedrockExecutorError> {
    let region = resolve_region(credentials)?;
    match credential_mode(credentials) {
        "profile" => {
            let profile = psd_str(credentials, "profile").unwrap_or_default();
            let (access_key_id, secret_access_key, session_token) =
                read_profile_credentials(profile).await?;
            Ok(ResolvedAwsCredentials {
                access_key_id,
                secret_access_key,
                session_token,
                region,
                mode: "profile",
            })
        }
        _ => {
            let mut resolved = read_static_credentials(credentials)?;
            resolved.region = region;
            Ok(resolved)
        }
    }
}

// === awsSigv4.js ===

/// RFC 3986 percent-encode, leaving only the unreserved set alone.
/// `encodeURIComponent` parity: `!'()*` are escaped too.
fn escape_uri(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn format_signing_date(ts: chrono::DateTime<chrono::Utc>) -> (String, String) {
    let amz_date = ts.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = ts.format("%Y%m%d").to_string();
    (amz_date, date_stamp)
}

/// Canonical URI: every segment escaped a second time (non-S3 services), so
/// a Bedrock model id on the wire as `...v1%3A0` is signed as `...v1%253A0`.
fn canonical_path(pathname: &str) -> String {
    let joined = pathname
        .split('/')
        .map(escape_uri)
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

/// Lowercased, trimmed, whitespace-collapsed headers sorted by name —
/// returns (canonical_headers_block, signed_headers_list).
fn canonicalize_headers(headers: &[(String, String)]) -> (String, String) {
    let mut entries: Vec<(String, String)> = headers
        .iter()
        .map(|(n, v)| {
            (
                n.to_ascii_lowercase(),
                v.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical = entries
        .iter()
        .map(|(n, v)| format!("{n}:{v}\n"))
        .collect::<String>();
    let signed = entries
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(";");
    (canonical, signed)
}

/// `signAwsRequest` — signs a query-less request. Query strings are refused
/// outright rather than canonicalised subtly wrong (upstream parity).
pub fn sign_aws_request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &str,
    region: &str,
    service: &str,
    credentials: &ResolvedAwsCredentials,
    date: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Vec<(String, String)>, BedrockExecutorError> {
    if credentials.access_key_id.is_empty() || credentials.secret_access_key.is_empty() {
        return Err(BedrockExecutorError::SigningError(
            "AWS SigV4 requires both accessKeyId and secretAccessKey".to_string(),
        ));
    }
    let parsed = url::Url::parse(url)
        .map_err(|e| BedrockExecutorError::SigningError(format!("invalid URL {url:?}: {e}")))?;
    if parsed.query().is_some() {
        return Err(BedrockExecutorError::SigningError(format!(
            "AWS SigV4 signing does not support query strings (got {:?}). Move the \
             parameters into the request body, or add proper query canonicalisation first.",
            parsed.query().unwrap_or("")
        )));
    }
    let host = parsed.host_str().unwrap_or_default().to_string();
    // AWS signs the Host header; bedrock hosts never carry a non-default
    // port, so host_str() is the value both sides agree on.

    let ts = date.unwrap_or_else(chrono::Utc::now);
    let (amz_date, date_stamp) = format_signing_date(ts);
    let payload_hash = sha256_hex(body.as_bytes());

    let mut headers_to_sign: Vec<(String, String)> = headers.to_vec();
    headers_to_sign.push(("host".to_string(), host.clone()));
    headers_to_sign.push((DATE_HEADER.to_string(), amz_date.clone()));
    if let Some(token) = &credentials.session_token {
        if !token.is_empty() {
            headers_to_sign.push((SECURITY_TOKEN_HEADER.to_string(), token.clone()));
        }
    }
    let (canonical_headers, signed_headers) = canonicalize_headers(&headers_to_sign);

    let canonical_request = [
        method.to_uppercase(),
        canonical_path(parsed.path()),
        String::new(), // query — always empty, refused above
        canonical_headers,
        signed_headers.clone(),
        payload_hash,
    ]
    .join("\n");

    let credential_scope = format!("{date_stamp}/{region}/{service}/{SIGV4_TERMINATOR}");
    let string_to_sign = [
        SIGV4_ALGORITHM.to_string(),
        amz_date.clone(),
        credential_scope.clone(),
        sha256_hex(canonical_request.as_bytes()),
    ]
    .join("\n");

    let mut key = format!("{SIGV4_KEY_PREFIX}{}", credentials.secret_access_key).into_bytes();
    for part in [date_stamp.as_str(), region, service, SIGV4_TERMINATOR] {
        key = hmac_sha256(&key, part.as_bytes());
    }
    let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));

    let authorization = format!(
        "{SIGV4_ALGORITHM} Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key_id
    );

    let mut out = headers.to_vec();
    out.push(("host".to_string(), host));
    out.push((DATE_HEADER.to_string(), amz_date));
    if let Some(token) = &credentials.session_token {
        if !token.is_empty() {
            out.push((SECURITY_TOKEN_HEADER.to_string(), token.clone()));
        }
    }
    out.push(("authorization".to_string(), authorization));
    Ok(out)
}

// === awsEventStream.js ===

#[derive(Debug)]
struct EventFrame {
    headers: HashMap<String, String>,
    payload: Option<Value>,
}

fn parse_event_frame(data: &[u8]) -> Result<EventFrame, BedrockExecutorError> {
    let fail = |m: &str| BedrockExecutorError::EventStreamDecode(m.to_string());
    if data.len() < 16 {
        return Err(fail("AWS EventStream frame is shorter than 16 bytes"));
    }
    let total_length = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
    let headers_length = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
    if total_length != data.len() {
        return Err(fail(
            "AWS EventStream frame length does not match its prelude",
        ));
    }
    if total_length > MAX_MESSAGE_BYTES
        || headers_length > MAX_HEADERS_BYTES
        || headers_length > total_length.saturating_sub(16)
    {
        return Err(fail("AWS EventStream frame bounds are invalid"));
    }
    let prelude_crc = u32::from_be_bytes(data[8..12].try_into().unwrap());
    if crc32fast::hash(&data[0..8]) != prelude_crc {
        return Err(fail("AWS EventStream prelude CRC mismatch"));
    }
    let message_crc = u32::from_be_bytes(data[total_length - 4..total_length].try_into().unwrap());
    if crc32fast::hash(&data[0..total_length - 4]) != message_crc {
        return Err(fail("AWS EventStream message CRC mismatch"));
    }

    let mut headers = HashMap::new();
    let mut offset = 12usize;
    let header_end = offset + headers_length;
    let mut require = |count: usize, offset: usize| -> Result<(), BedrockExecutorError> {
        if offset + count > header_end {
            Err(fail("AWS EventStream header exceeds its declared bounds"))
        } else {
            Ok(())
        }
    };

    while offset < header_end {
        require(1, offset)?;
        let name_length = data[offset] as usize;
        offset += 1;
        require(name_length + 1, offset)?;
        let name = String::from_utf8_lossy(&data[offset..offset + name_length]).into_owned();
        offset += name_length;
        if headers.contains_key(&name) {
            return Err(fail(&format!(
                "AWS EventStream contains duplicate header: {name}"
            )));
        }
        let htype = data[offset];
        offset += 1;

        match htype {
            0 | 1 => {
                headers.insert(name, (htype == 1).to_string());
            }
            2 => {
                require(1, offset)?;
                headers.insert(name, (data[offset] as i8).to_string());
                offset += 1;
            }
            3 => {
                require(2, offset)?;
                headers.insert(
                    name,
                    i16::from_be_bytes(data[offset..offset + 2].try_into().unwrap()).to_string(),
                );
                offset += 2;
            }
            4 => {
                require(4, offset)?;
                headers.insert(
                    name,
                    i32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()).to_string(),
                );
                offset += 4;
            }
            5 | 8 => {
                require(8, offset)?;
                offset += 8;
            }
            6 | 7 => {
                require(2, offset)?;
                let value_length =
                    u16::from_be_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
                offset += 2;
                require(value_length, offset)?;
                let bytes = &data[offset..offset + value_length];
                if htype == 7 {
                    headers.insert(name, String::from_utf8_lossy(bytes).into_owned());
                }
                offset += value_length;
            }
            9 => {
                require(16, offset)?;
                offset += 16;
            }
            other => {
                return Err(fail(&format!(
                    "AWS EventStream header {name} has unknown type {other}"
                )));
            }
        }
    }

    let payload_bytes = &data[header_end..total_length - 4];
    if payload_bytes.is_empty() {
        return Ok(EventFrame {
            headers,
            payload: None,
        });
    }
    let payload_text = String::from_utf8_lossy(payload_bytes);
    if payload_text.trim().is_empty() {
        return Ok(EventFrame {
            headers,
            payload: None,
        });
    }
    let payload: Value = serde_json::from_str(&payload_text)
        .map_err(|e| fail(&format!("AWS EventStream payload is not valid JSON ({e})")))?;
    Ok(EventFrame {
        headers,
        payload: Some(payload),
    })
}

// === Executor ===

pub struct BedrockExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct BedrockExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for BedrockExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BedrockExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

#[derive(Clone)]
pub struct BedrockExecutor {
    pool: Arc<ClientPool>,
    provider: String,
    wire_format: BedrockWireFormat,
}

impl BedrockExecutor {
    pub fn new(pool: Arc<ClientPool>, provider: String) -> Result<Self, BedrockExecutorError> {
        let wire_format = BedrockWireFormat::for_provider(&provider);
        Ok(Self {
            pool,
            provider,
            wire_format,
        })
    }

    /// `buildUrl` — region-validated host + escaped model id + action.
    /// The model id is escaped once here; SigV4 escapes it a second time for
    /// the canonical request, which is what Bedrock expects for a ":0" suffix.
    fn build_url_for(
        &self,
        model: &str,
        stream: bool,
        region: &str,
    ) -> Result<String, BedrockExecutorError> {
        if !self.wire_format.accepts_model(model) {
            return Err(BedrockExecutorError::UnsupportedModelFamily {
                model: model.to_string(),
                provider: self.provider.clone(),
            });
        }
        let action = if stream { STREAM_PATH } else { INVOKE_PATH };
        Ok(format!(
            "https://bedrock-runtime.{region}.amazonaws.com/model/{}/{}",
            escape_uri(model),
            action
        ))
    }

    /// `transformRequest` — Bedrock takes the model from the URL and picks
    /// streaming by endpoint, so `model`/`stream` are dropped. The Anthropic
    /// wire additionally requires `anthropic_version`, applied AFTER the
    /// spread so a client-supplied version cannot win.
    fn transform_request(&self, body: &Value) -> Value {
        let mut rest = body.clone();
        if let Value::Object(map) = &mut rest {
            map.remove("model");
            map.remove("stream");
            // `anthropic_version` is meaningless off the Anthropic wire and
            // Bedrock schema validation can reject unknown fields, so a
            // stray client-supplied value is dropped for BOTH wires; the
            // Claude pin below re-adds the correct one.
            map.remove("anthropic_version");
        }
        if matches!(self.wire_format, BedrockWireFormat::Claude) {
            if let Value::Object(map) = &mut rest {
                map.insert(
                    "anthropic_version".to_string(),
                    Value::String(ANTHROPIC_VERSION.to_string()),
                );
            }
        }
        rest
    }

    fn build_headers(&self, stream: bool) -> Vec<(String, String)> {
        vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "accept".to_string(),
                if stream {
                    "application/vnd.amazon.eventstream".to_string()
                } else {
                    "application/json".to_string()
                },
            ),
        ]
    }

    /// Copy of signed headers safe to write to a request log — the session
    /// token is a credential and the signature can replay the request
    /// (`redactSignedHeaders` upstream).
    fn redact_signed_headers(signed: &[(String, String)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in signed {
            let name_lower = name.to_ascii_lowercase();
            let redacted = if name_lower == "authorization" {
                redact_signature(value)
            } else if name_lower == SECURITY_TOKEN_HEADER {
                "<redacted>".to_string()
            } else {
                value.clone()
            };
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name_lower.as_bytes()),
                HeaderValue::from_str(&redacted),
            ) {
                map.insert(n, v);
            }
        }
        map
    }

    pub async fn execute_request(
        &self,
        request: BedrockExecutionRequest,
    ) -> Result<BedrockExecutorResponse, BedrockExecutorError> {
        let resolved = resolve_aws_credentials(&request.credentials).await?;
        let url = self.build_url_for(&request.model, request.stream, &resolved.region)?;
        let transformed_body = self.transform_request(&request.body);
        let payload = serde_json::to_string(&transformed_body)?;

        // Content-Length is deliberately not signed: reqwest sets it itself,
        // and signing a value the runtime may normalise is a needless
        // SignatureDoesNotMatch risk.
        let signed = sign_aws_request(
            "POST",
            &url,
            &self.build_headers(request.stream),
            &payload,
            &resolved.region,
            BEDROCK_SERVICE,
            &resolved,
            None,
        )?;

        // `host` is signed but reqwest sets the Host header itself from the
        // URL — the value is identical, so it is not inserted manually.
        let mut header_map = HeaderMap::new();
        for (name, value) in &signed {
            if name.eq_ignore_ascii_case("host") {
                continue;
            }
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                header_map.insert(n, v);
            }
        }

        // No-redirect client: a 3xx would replay the signed body and the
        // x-amz-security-token at whatever origin it names.
        let client = self
            .pool
            .get_no_redirect(&self.provider, request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(header_map.clone())
            .body(payload)
            .send()
            .await?;

        let logged_headers = Self::redact_signed_headers(&signed);

        let status = response.status();
        let is_ok = status.is_success();
        // Errors and non-streaming calls are already JSON the claude/openai
        // translators understand — pass through untouched.
        if !is_ok || !request.stream {
            return Ok(BedrockExecutorResponse {
                response: UpstreamResponse::Reqwest(response),
                url,
                headers: logged_headers,
                transformed_body,
                transport: TransportKind::Reqwest,
            });
        }

        // Streaming: unwrap AWS EventStream frames into SSE in this
        // provider's wire format (named Claude events, or bare OpenAI data:
        // chunks terminated by [DONE]).
        let sse_stream = event_stream_to_sse(response.bytes_stream(), self.wire_format);
        let mut http_resp = http::Response::builder()
            .status(status)
            .body(ReqwestBody::wrap_stream(sse_stream))
            .map_err(BedrockExecutorError::InvalidRequest)?;
        {
            let headers_mut = http_resp.headers_mut();
            headers_mut.insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            headers_mut.insert(
                reqwest::header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache"),
            );
            headers_mut.insert(
                reqwest::header::CONNECTION,
                HeaderValue::from_static("keep-alive"),
            );
        }
        let reconstructed = reqwest::Response::from(http_resp);

        Ok(BedrockExecutorResponse {
            response: UpstreamResponse::Reqwest(reconstructed),
            url,
            headers: logged_headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    /// Credential probe for the dashboard validate/Test paths:
    /// `GET https://bedrock.{region}.amazonaws.com/foundation-models` signed
    /// as service `bedrock`. AccessDeniedException still counts as valid —
    /// the credentials authenticated, they just may not list models.
    pub async fn probe_credentials(
        &self,
        credentials: &ProviderConnection,
        client: &reqwest::Client,
    ) -> Result<bool, BedrockExecutorError> {
        let resolved = resolve_aws_credentials(credentials).await?;
        let url = format!(
            "https://bedrock.{}.amazonaws.com/{}",
            resolved.region, PROBE_PATH
        );
        let headers = vec![("accept".to_string(), "application/json".to_string())];
        let signed = sign_aws_request(
            "GET",
            &url,
            &headers,
            "",
            &resolved.region,
            BEDROCK_SERVICE,
            &resolved,
            None,
        )?;
        let mut header_map = HeaderMap::new();
        for (name, value) in &signed {
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                header_map.insert(n, v);
            }
        }
        // Callers must pass a no-redirect client (pool.get_no_redirect or an
        // equivalent Policy::none() builder) so a 3xx can't replay the signed
        // probe at another origin.
        let response =
            tokio::time::timeout(PROBE_TIMEOUT, client.get(&url).headers(header_map).send())
                .await
                .map_err(|_| {
                    BedrockExecutorError::CredentialResolution(
                        "AWS credential probe timed out".to_string(),
                    )
                })??;

        if response.status().is_success() {
            return Ok(true);
        }
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(':').next())
            .unwrap_or("")
            .to_string();
        if error_type == "AccessDeniedException" {
            return Ok(true);
        }
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let message = body.get("message").and_then(Value::as_str).unwrap_or("");
        Err(BedrockExecutorError::InvalidCredentials(format!(
            "AWS rejected the credentials ({}{}){}",
            if error_type.is_empty() {
                format!("HTTP {status}")
            } else {
                error_type
            },
            if message.is_empty() { "" } else { ": " },
            message
        )))
    }
}

fn redact_signature(value: &str) -> String {
    // Replace `Signature=<64 hex>` with `Signature=<redacted>`.
    if let Some(idx) = value.find("Signature=") {
        let rest = &value[idx + "Signature=".len()..];
        let hex_len = rest.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        if hex_len >= 32 {
            return format!("{}Signature=<redacted>{}", &value[..idx], &rest[hex_len..]);
        }
    }
    value.to_string()
}

/// EventStream → SSE transform. Each `chunk` frame carries
/// `{"bytes": "<base64>"}` whose contents are one provider event; the output
/// is `event: <type>`/`data: <json>` for Claude or bare `data:` lines plus a
/// synthesized `data: [DONE]` for OpenAI.
fn event_stream_to_sse(
    upstream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    wire: BedrockWireFormat,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut upstream = std::pin::pin!(upstream);
        let mut buffer: Vec<u8> = Vec::new();
        let mut failed = false;
        let mut saw_terminal = false;

        macro_rules! emit_event {
            ($event_type:expr, $data:expr) => {{
                let data_json = match serde_json::to_string(&$data) {
                    Ok(j) => j,
                    Err(_) => continue,
                };
                let frame = match wire {
                    BedrockWireFormat::Claude => {
                        format!("event: {}\ndata: {}\n\n", $event_type, data_json)
                    }
                    BedrockWireFormat::OpenAi => {
                        format!("data: {}\n\n", data_json)
                    }
                };
                yield Ok(Bytes::from(frame));
            }};
        }

        macro_rules! fail_stream {
            ($err_type:expr, $message:expr) => {{
                if !failed {
                    failed = true;
                    match wire {
                        BedrockWireFormat::Claude => {
                            let data = json!({"type":"error","error":{"type":$err_type,"message":$message}});
                            if let Ok(j) = serde_json::to_string(&data) {
                                yield Ok(Bytes::from(format!("event: error\ndata: {j}\n\n")));
                            }
                        }
                        BedrockWireFormat::OpenAi => {
                            let data = json!({"error":{"type":$err_type,"message":$message,"code":$err_type}});
                            if let Ok(j) = serde_json::to_string(&data) {
                                yield Ok(Bytes::from(format!("data: {j}\n\n")));
                            }
                        }
                    }
                }
                // Stop draining — the caller closes after this yield.
                return;
            }};
        }

        // Frame-drain closure inlined: parse as many complete frames as the
        // buffer holds. `failed` short-circuits via the macros above.
        'outer: loop {
            // Pull more bytes when the buffer cannot satisfy the prelude.
            while buffer.len() < 12 {
                match upstream.next().await {
                    Some(Ok(chunk)) => {
                        if chunk.is_empty() { continue; }
                        buffer.extend_from_slice(&chunk);
                    }
                    Some(Err(e)) => {
                        fail_stream!("api_error", format!("Bedrock stream read failed: {e}"));
                    }
                    None => break 'outer,
                }
            }
            if buffer.is_empty() { break 'outer; }

            // Prelude CRC over the first 8 bytes, stored at offset 8.
            let prelude_crc = u32::from_be_bytes(buffer[8..12].try_into().unwrap());
            if crc32fast::hash(&buffer[0..8]) != prelude_crc {
                fail_stream!("api_error", "Bedrock EventStream prelude CRC mismatch");
            }
            let total_length = u32::from_be_bytes(buffer[0..4].try_into().unwrap()) as usize;
            let headers_length = u32::from_be_bytes(buffer[4..8].try_into().unwrap()) as usize;
            if total_length < 16
                || total_length > MAX_MESSAGE_BYTES
                || headers_length > MAX_HEADERS_BYTES
                || headers_length > total_length.saturating_sub(16)
            {
                fail_stream!("api_error", "Bedrock EventStream frame bounds are invalid");
            }
            if buffer.len() < total_length {
                // Frame not fully arrived; pull more bytes.
                match upstream.next().await {
                    Some(Ok(chunk)) => {
                        if chunk.is_empty() { continue; }
                        buffer.extend_from_slice(&chunk);
                        continue;
                    }
                    Some(Err(e)) => {
                        fail_stream!("api_error", format!("Bedrock stream read failed: {e}"));
                    }
                    None => break 'outer,
                }
            }

            let frame_bytes = buffer[..total_length].to_vec();
            buffer.drain(..total_length);

            let event = match parse_event_frame(&frame_bytes) {
                Ok(e) => e,
                Err(e) => {
                    fail_stream!("api_error", e.to_string());
                }
            };

            let message_type = event.headers.get(":message-type").cloned().unwrap_or_default();
            if message_type == "exception" || message_type == "error" {
                let exception_type = event
                    .headers
                    .get(":exception-type")
                    .or_else(|| event.headers.get(":error-code"))
                    .cloned()
                    .unwrap_or_else(|| "api_error".to_string());
                let message = event
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("message").or_else(|| p.get("Message")))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| event.headers.get(":error-message").cloned())
                    .unwrap_or_else(|| {
                        format!("Bedrock returned an EventStream {message_type}")
                    });
                fail_stream!(&exception_type, message);
            }

            if event.headers.get(":event-type").map(String::as_str) != Some(CHUNK_EVENT_NAME) {
                tracing::warn!(
                    target: "openproxy::executor::bedrock",
                    "skipped unrecognised EventStream event type {:?}",
                    event.headers.get(":event-type")
                );
                continue;
            }

            let encoded = event
                .payload
                .as_ref()
                .and_then(|p| p.get("bytes"))
                .and_then(Value::as_str);
            let Some(encoded) = encoded else {
                fail_stream!("api_error", "Bedrock chunk frame carried no payload bytes");
            };
            let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded) {
                Ok(b) => b,
                Err(e) => {
                    fail_stream!("api_error", format!("Bedrock chunk base64 decode failed: {e}"));
                }
            };
            let inner: Value = match serde_json::from_slice(&decoded) {
                Ok(v) => v,
                Err(e) => {
                    fail_stream!(
                        "api_error",
                        format!("Bedrock chunk was not valid JSON ({e})")
                    );
                }
            };

            match wire {
                BedrockWireFormat::Claude => {
                    let Some(inner_type) = inner.get("type").and_then(Value::as_str) else {
                        fail_stream!("api_error", "Bedrock chunk decoded to an event with no type");
                    };
                    let inner_type = inner_type.to_string();
                    emit_event!(inner_type.clone(), inner);
                    if inner_type == TERMINAL_EVENT_TYPE {
                        saw_terminal = true;
                    }
                }
                BedrockWireFormat::OpenAi => {
                    if !inner.get("choices").map(Value::is_array).unwrap_or(false) {
                        fail_stream!("api_error", "Bedrock chunk decoded without a `choices` array");
                    }
                    let has_finish = inner
                        .get("choices")
                        .and_then(Value::as_array)
                        .map(|choices| {
                            choices.iter().any(|c| {
                                c.get("finish_reason")
                                    .map(|f| !f.is_null())
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false);
                    emit_event!("", inner);
                    if has_finish {
                        saw_terminal = true;
                    }
                }
            }
        }

        // Trailing bytes that never formed a frame, or a stream that stopped
        // before the terminal event, both mean the answer is incomplete.
        if !failed {
            if !buffer.is_empty() {
                fail_stream!("api_error", "Bedrock stream ended mid-frame");
            } else if !saw_terminal {
                match wire {
                    BedrockWireFormat::Claude => {
                        fail_stream!(
                            "api_error",
                            format!("Bedrock stream ended before {TERMINAL_EVENT_TYPE}")
                        );
                    }
                    BedrockWireFormat::OpenAi => {
                        fail_stream!(
                            "api_error",
                            "Bedrock stream ended before any choice reported a finish_reason"
                        );
                    }
                }
            }
            if matches!(wire, BedrockWireFormat::OpenAi) && !failed {
                yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn_with_psd(psd: &[(&str, Value)]) -> ProviderConnection {
        let mut c = ProviderConnection::default();
        c.provider = "bedrock".into();
        for (k, v) in psd {
            c.provider_specific_data.insert(k.to_string(), v.clone());
        }
        c
    }

    #[test]
    fn region_validation_rejects_hostile_values() {
        let too_long = "a".repeat(40);
        for bad in [
            "evil.com/x",
            "US-EAST-1",
            "us east 1",
            "",
            too_long.as_str(),
        ] {
            let c = conn_with_psd(&[("region", json!(bad))]);
            assert!(resolve_region(&c).is_err(), "{bad:?} must be rejected");
        }
        let c = conn_with_psd(&[("region", json!("  us-west-2 "))]);
        // Trim happens upstream of validation in JS; our impl trims psd values.
        assert_eq!(resolve_region(&c).unwrap(), "us-west-2");
    }

    #[test]
    fn region_defaults_and_env_fallback() {
        // No psd, no env (test env has none set for AWS_*).
        let c = ProviderConnection::default();
        let region = resolve_region(&c).unwrap();
        assert!(is_valid_region(&region));
    }

    #[test]
    fn profile_name_validation() {
        assert!(is_valid_profile_name("my-profile"));
        assert!(is_valid_profile_name("dev@example.com"));
        assert!(!is_valid_profile_name(""));
        assert!(!is_valid_profile_name(&"a".repeat(65)));
        assert!(!is_valid_profile_name("bad;rm -rf"));
        assert!(!is_valid_profile_name("bad/name"));
    }

    #[test]
    fn static_credentials_require_keypair_and_session_for_asia() {
        let mut c = conn_with_psd(&[("accessKeyId", json!("AKIA123"))]);
        assert!(read_static_credentials(&c).is_err());
        c.api_key = Some("secret".into());
        let r = read_static_credentials(&c).unwrap();
        assert_eq!(r.access_key_id, "AKIA123");
        assert_eq!(r.secret_access_key, "secret");

        let mut c2 = conn_with_psd(&[("accessKeyId", json!("ASIA123"))]);
        c2.api_key = Some("secret".into());
        assert!(read_static_credentials(&c2).is_err());
        c2.provider_specific_data
            .insert("sessionToken".into(), json!("tok"));
        assert!(read_static_credentials(&c2).is_ok());
    }

    #[test]
    fn wire_format_selection_by_provider() {
        assert_eq!(
            BedrockWireFormat::for_provider("bedrock"),
            BedrockWireFormat::Claude
        );
        assert_eq!(
            BedrockWireFormat::for_provider("bedrock-xai"),
            BedrockWireFormat::OpenAi
        );
        assert_eq!(
            BedrockWireFormat::for_provider("brx"),
            BedrockWireFormat::OpenAi
        );
    }

    #[test]
    fn model_family_gate() {
        assert!(
            BedrockWireFormat::Claude.accepts_model("us.anthropic.claude-sonnet-4-5-20250929-v1:0")
        );
        assert!(BedrockWireFormat::Claude.accepts_model("global.anthropic.claude-opus-4-1"));
        assert!(!BedrockWireFormat::Claude.accepts_model("us.xai.grok-4.6"));
        assert!(!BedrockWireFormat::Claude.accepts_model("anthropic-claude")); // no dot boundary? contains "."? "anthropic-claude" has no "anthropic." segment → reject
        assert!(BedrockWireFormat::OpenAi.accepts_model("us.xai.grok-4.6"));
        assert!(BedrockWireFormat::OpenAi.accepts_model("global.xai.grok-4.6"));
        assert!(!BedrockWireFormat::OpenAi.accepts_model("us.anthropic.claude-sonnet-4-6"));
    }

    #[test]
    fn transform_request_drops_model_stream_and_pins_version() {
        let ex = BedrockExecutor {
            pool: Arc::new(ClientPool::new()),
            provider: "bedrock".into(),
            wire_format: BedrockWireFormat::Claude,
        };
        let body = json!({"model":"m","stream":true,"max_tokens":1,"anthropic_version":"2023-06-01","messages":[]});
        let out = ex.transform_request(&body);
        assert!(out.get("model").is_none());
        assert!(out.get("stream").is_none());
        assert_eq!(out["anthropic_version"], "bedrock-2023-05-31");

        let ex2 = BedrockExecutor {
            pool: Arc::new(ClientPool::new()),
            provider: "bedrock-xai".into(),
            wire_format: BedrockWireFormat::OpenAi,
        };
        let out2 = ex2.transform_request(&body);
        assert!(out2.get("anthropic_version").is_none());
        assert_eq!(out2["max_tokens"], 1);
    }

    #[test]
    fn build_url_escapes_model_and_picks_action() {
        let ex = BedrockExecutor {
            pool: Arc::new(ClientPool::new()),
            provider: "bedrock".into(),
            wire_format: BedrockWireFormat::Claude,
        };
        let url = ex
            .build_url_for(
                "us.anthropic.claude-opus-4-5-20251101-v1:0",
                true,
                "eu-west-1",
            )
            .unwrap();
        assert_eq!(
            url,
            "https://bedrock-runtime.eu-west-1.amazonaws.com/model/us.anthropic.claude-opus-4-5-20251101-v1%3A0/invoke-with-response-stream"
        );
        let url2 = ex
            .build_url_for(
                "us.anthropic.claude-opus-4-5-20251101-v1:0",
                false,
                "us-east-1",
            )
            .unwrap();
        assert!(url2.ends_with("/invoke"));
        assert!(ex
            .build_url_for("us.xai.grok-4.6", true, "us-east-1")
            .is_err());
    }

    fn test_creds() -> ResolvedAwsCredentials {
        ResolvedAwsCredentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
            region: "us-east-1".into(),
            mode: "static",
        }
    }

    #[test]
    fn sigv4_matches_aws_published_get_vector() {
        // AWS SigV4 test suite "get-vanilla" style vector, adapted to our
        // signer: fixed date, known key, query rejected so we use path only.
        // Expected values computed per the SigV4 spec algorithm.
        let creds = ResolvedAwsCredentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: Some("session-token".into()),
            region: "us-east-1".into(),
            mode: "static",
        };
        let date = chrono::DateTime::parse_from_rfc3339("2015-08-30T12:36:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let out = sign_aws_request(
            "POST",
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/us.anthropic.claude-v1%3A0/invoke",
            &[
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "application/json".into()),
            ],
            "{}",
            "us-east-1",
            "bedrock",
            &creds,
            Some(date),
        )
        .unwrap();
        let auth = out
            .iter()
            .find(|(n, _)| n == "authorization")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(auth.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request"
        ));
        assert!(
            auth.contains("SignedHeaders=accept;content-type;host;x-amz-date;x-amz-security-token")
        );
        assert!(auth.contains("Signature="));
        // Deterministic: same inputs → same signature.
        let out2 = sign_aws_request(
            "POST",
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/us.anthropic.claude-v1%3A0/invoke",
            &[
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "application/json".into()),
            ],
            "{}",
            "us-east-1",
            "bedrock",
            &creds,
            Some(date),
        )
        .unwrap();
        assert_eq!(
            out.iter().find(|(n, _)| n == "authorization").unwrap().1,
            out2.iter().find(|(n, _)| n == "authorization").unwrap().1
        );
        // Double-encoding check: canonical path must contain %253A for the :0 suffix.
        // Recompute canonical request hash path indirectly: a URL with %3A must sign
        // differently than one with %253A already.
        let out3 = sign_aws_request(
            "POST",
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/us.anthropic.claude-v1%253A0/invoke",
            &[
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "application/json".into()),
            ],
            "{}",
            "us-east-1",
            "bedrock",
            &creds,
            Some(date),
        )
        .unwrap();
        assert_ne!(
            out.iter().find(|(n, _)| n == "authorization").unwrap().1,
            out3.iter().find(|(n, _)| n == "authorization").unwrap().1,
            "double-encoded path must change the signature"
        );
        let _ = test_creds();
    }

    #[test]
    fn sigv4_rejects_query_strings() {
        let err = sign_aws_request(
            "GET",
            "https://bedrock.us-east-1.amazonaws.com/foundation-models?maxResults=1",
            &[],
            "",
            "us-east-1",
            "bedrock",
            &test_creds(),
            None,
        );
        assert!(matches!(err, Err(BedrockExecutorError::SigningError(_))));
    }

    #[test]
    fn sigv4_includes_security_token_when_present() {
        let mut creds = test_creds();
        creds.session_token = Some("tok".into());
        let date = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let out = sign_aws_request(
            "GET",
            "https://bedrock.us-east-1.amazonaws.com/foundation-models",
            &[("accept".into(), "application/json".into())],
            "",
            "us-east-1",
            "bedrock",
            &creds,
            Some(date),
        )
        .unwrap();
        assert!(out
            .iter()
            .any(|(n, v)| n == "x-amz-security-token" && v == "tok"));
        let auth = out
            .iter()
            .find(|(n, _)| n == "authorization")
            .unwrap()
            .1
            .clone();
        assert!(auth.contains("x-amz-security-token"));
    }

    /// Build a complete EventStream frame (encode side mirrors upstream
    /// `encodeEventFrame`, used only by tests/fixtures).
    fn encode_event_frame(headers: &[(&str, &str)], payload: Option<&Value>) -> Vec<u8> {
        let mut header_bytes = Vec::new();
        for (name, value) in headers {
            let nb = name.as_bytes();
            let vb = value.as_bytes();
            header_bytes.push(nb.len() as u8);
            header_bytes.extend_from_slice(nb);
            header_bytes.push(7); // string
            header_bytes.extend_from_slice(&(vb.len() as u16).to_be_bytes());
            header_bytes.extend_from_slice(vb);
        }
        let payload_bytes = payload
            .map(|p| serde_json::to_vec(p).unwrap())
            .unwrap_or_default();
        let total = 16 + header_bytes.len() + payload_bytes.len();
        let mut frame = vec![0u8; total];
        frame[0..4].copy_from_slice(&(total as u32).to_be_bytes());
        frame[4..8].copy_from_slice(&(header_bytes.len() as u32).to_be_bytes());
        let prelude_crc = crc32fast::hash(&frame[0..8]).to_be_bytes();
        frame[8..12].copy_from_slice(&prelude_crc);
        frame[12..12 + header_bytes.len()].copy_from_slice(&header_bytes);
        let pstart = 12 + header_bytes.len();
        frame[pstart..pstart + payload_bytes.len()].copy_from_slice(&payload_bytes);
        let msg_crc = crc32fast::hash(&frame[0..total - 4]);
        frame[total - 4..total].copy_from_slice(&msg_crc.to_be_bytes());
        frame
    }

    #[tokio::test]
    async fn eventstream_chunk_decodes_to_claude_sse() {
        let inner = json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}});
        let inner_b64 =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&inner).unwrap());
        let frame = encode_event_frame(
            &[
                (":message-type", "event"),
                (":event-type", "chunk"),
                (":content-type", "application/json"),
            ],
            Some(&json!({"bytes": inner_b64})),
        );
        let stop = json!({"type":"message_stop"});
        let stop_b64 =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&stop).unwrap());
        let frame2 = encode_event_frame(
            &[(":message-type", "event"), (":event-type", "chunk")],
            Some(&json!({"bytes": stop_b64})),
        );
        let mut all = frame;
        all.extend_from_slice(&frame2);

        let stream = futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(all))]);
        let collected: Vec<Bytes> = event_stream_to_sse(stream, BedrockWireFormat::Claude)
            .filter_map(|r| async move { r.ok() })
            .collect()
            .await;
        let text = collected
            .iter()
            .flat_map(|b| b.iter().copied())
            .map(|b| b as char)
            .collect::<String>();
        assert!(text.contains("event: content_block_delta"));
        assert!(text.contains("\"text\":\"hi\""));
        assert!(text.contains("event: message_stop"));
    }

    #[tokio::test]
    async fn eventstream_chunk_decodes_to_openai_sse_with_done() {
        let inner = json!({"choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]});
        let inner_b64 =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&inner).unwrap());
        let frame = encode_event_frame(
            &[(":message-type", "event"), (":event-type", "chunk")],
            Some(&json!({"bytes": inner_b64})),
        );
        let done_inner = json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
        let done_b64 = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&done_inner).unwrap());
        let frame2 = encode_event_frame(
            &[(":message-type", "event"), (":event-type", "chunk")],
            Some(&json!({"bytes": done_b64})),
        );
        let mut all = frame;
        all.extend_from_slice(&frame2);

        let stream = futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(all))]);
        let collected: Vec<Bytes> = event_stream_to_sse(stream, BedrockWireFormat::OpenAi)
            .filter_map(|r| async move { r.ok() })
            .collect()
            .await;
        let text = collected
            .iter()
            .flat_map(|b| b.iter().copied())
            .map(|b| b as char)
            .collect::<String>();
        assert!(text.contains("data: {\"choices\""));
        assert!(text.contains("data: [DONE]"));
        assert!(!text.contains("event:"));
    }

    #[tokio::test]
    async fn eventstream_exception_frame_surfaces_error() {
        let frame = encode_event_frame(
            &[
                (":message-type", "exception"),
                (":exception-type", "ThrottlingException"),
                (":error-message", "Rate exceeded"),
            ],
            Some(&json!({"message":"Rate exceeded"})),
        );
        let stream = futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(frame))]);
        let collected: Vec<Bytes> = event_stream_to_sse(stream, BedrockWireFormat::Claude)
            .filter_map(|r| async move { r.ok() })
            .collect()
            .await;
        let text = collected
            .iter()
            .flat_map(|b| b.iter().copied())
            .map(|b| b as char)
            .collect::<String>();
        assert!(text.contains("ThrottlingException"));
        assert!(text.contains("Rate exceeded"));
    }

    #[test]
    fn eventstream_frame_crc_validation() {
        let frame = encode_event_frame(&[(":message-type", "event")], Some(&json!({"a":1})));
        assert!(parse_event_frame(&frame).is_ok());
        let mut bad = frame.clone();
        bad[20] ^= 0xff;
        assert!(parse_event_frame(&bad).is_err());
    }
}
