use std::sync::Arc;

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hyper::http;
use md5::{Digest, Md5};
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use rsa::{pkcs8::DecodePublicKey, Pkcs1v15Encrypt, RsaPublicKey};
use serde_json::Value;
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

// ---------------------------------------------------------------------------
// Constants (ported from upstream src/lib/qoder/constants.js)
// ---------------------------------------------------------------------------

const QODER_CHAT_URL_ENCODED: &str = "https://api3.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";

/// jt- tokens route to api2 (9router QODER_CHAT_BASE_ALT + QODER_CHAT_SIG_PATH).
const QODER_CHAT_URL_ALT: &str = "https://api2.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";

/// Live model catalog (9router getQoderModelConfig).
const QODER_MODEL_LIST_URL: &str = "https://api3.qoder.sh/algo/api/v2/model/list";

// ─── PAT → job-token exchange (ported from 9router v0.5.45 qoder.js) ────────
// PATs (pt-...) cannot sign COSY requests directly. Exchange them for a
// short-lived job token (jt-...) via /api/v1/jobToken/exchange (plain JSON,
// not COSY-signed), then resolve the userId from userinfo. Cached per-PAT
// until near-expiry.
const QODER_JOB_TOKEN_EXCHANGE_URL: &str = "https://openapi.qoder.sh/api/v1/jobToken/exchange";
const QODER_USERINFO_URL: &str = "https://openapi.qoder.sh/api/v1/userinfo";
const PAT_REFRESH_BUFFER_SECS: u64 = 5 * 60;

static PAT_JOB_CACHE: Lazy<Mutex<HashMap<String, (String, String, u64)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Resolve a PAT to `(job_token, user_id)`, using the per-PAT cache when fresh.
async fn resolve_pat_credential(pat: &str) -> Result<(String, String), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some((job_token, user_id, expires_at)) = PAT_JOB_CACHE
        .lock()
        .map(|g| g.get(pat).cloned())
        .unwrap_or(None)
    {
        if expires_at.saturating_sub(now) > PAT_REFRESH_BUFFER_SECS {
            return Ok((job_token, user_id));
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(QODER_JOB_TOKEN_EXCHANGE_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "qodercli/1.0.0")
        .header("Cosy-Version", QODER_IDE_VERSION)
        .header("Cosy-ClientType", QODER_CLIENT_TYPE)
        .json(&serde_json::json!({ "personal_token": pat }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "qoder PAT exchange failed: {} {}",
            response.status(),
            response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }
    let data: Value = response.json().await.map_err(|e| e.to_string())?;
    let job_token = data
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "qoder PAT exchange returned no job token".to_string())?
        .to_string();

    let mut expires_at = now + 24 * 60 * 60;
    if let Some(exp) = data.get("expires_at").and_then(|v| v.as_str()) {
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(exp) {
            expires_at = parsed.timestamp() as u64;
        }
    } else if let Some(exp_in) = data.get("expires_in").and_then(|v| v.as_u64()) {
        if exp_in > 0 {
            expires_at = now + exp_in;
        }
    }

    // Resolve userId from userinfo (best-effort).
    let user_id = match client
        .get(QODER_USERINFO_URL)
        .header("Authorization", format!("Bearer {job_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "qodercli/1.0.0")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r
            .json::<Value>()
            .await
            .ok()
            .and_then(|info| {
                info.get("id")
                    .or_else(|| info.get("userId"))
                    .or_else(|| info.get("user_id"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_default(),
        _ => String::new(),
    };

    if let Ok(mut cache) = PAT_JOB_CACHE.lock() {
        cache.insert(
            pat.to_string(),
            (job_token.clone(), user_id.clone(), expires_at),
        );
    }
    Ok((job_token, user_id))
}

const QODER_IDE_VERSION: &str = "1.0.0";
const QODER_CLIENT_TYPE: &str = "5";
const QODER_DATA_POLICY: &str = "disagree";
const QODER_LOGIN_VERSION: &str = "v2";
const QODER_MACHINE_OS: &str = "x86_64_windows";
const QODER_MACHINE_TYPE: &str = "5";

// RSA public key for COSY encryption (extracted from Qoder IDE v0.9).
// Matches the CLIProxyAPIPlus branch and live qodercli traffic.
// SPKI ("BEGIN PUBLIC KEY") + RSA_PKCS1_PADDING, matching JS
// crypto.publicEncrypt({ key: QODER_RSA_PUBLIC_KEY, padding: RSA_PKCS1_PADDING })
// in cosy.js (verified against tests/unit/qoder.test.js cosy vectors).
const QODER_RSA_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
XcW+ML9FoCI6AOvOzwIDAQAB
-----END PUBLIC KEY-----";

// Qoder WAF-bypass encoding alphabets
const QODER_STD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const QODER_CUSTOM_ALPHABET: &[u8; 64] =
    b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";

// ---------------------------------------------------------------------------
// AES-128-CBC type aliases
// ---------------------------------------------------------------------------

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum QoderExecutorError {
    MissingCredentials(String),
    RequestFailed(String),
    CryptoError(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl std::fmt::Display for QoderExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(msg) => write!(f, "Missing credentials: {msg}"),
            Self::RequestFailed(msg) => write!(f, "Request failed: {msg}"),
            Self::CryptoError(msg) => write!(f, "Crypto error: {msg}"),
            Self::Serialize(e) => write!(f, "Serialize error: {e}"),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {e}"),
            Self::Hyper(e) => write!(f, "Hyper error: {e}"),
            Self::Request(e) => write!(f, "Request error: {e}"),
            Self::InvalidHeader(e) => write!(f, "Invalid header: {e}"),
        }
    }
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

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

pub struct QoderExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

// ---------------------------------------------------------------------------
// Billing block detection (ported from 9router v0.5.55 qoder.js)
// ---------------------------------------------------------------------------

/// Fallback `model_config` when the live catalog cannot be reached (network
/// error / non-2xx). Chat still proceeds; a missing entry after a successful
/// fetch is a hard error instead.
fn stub_model_config(qoder_key: &str) -> Value {
    serde_json::json!({
        "key": qoder_key,
        "is_reasoning": false,
        "max_output_tokens": 32768,
        "source": "system",
    })
}

/// Billing/quota error codes that should trigger combo fallback.
/// 9router `isBillingBlock` (qoder.js) matches string codes only via
/// `/"code"\s*:\s*"(112|10605)"/` (whitespace-tolerant, strings only).
/// Rust additionally accepts numeric `{"code":112}` — a benign superset for
/// live traffic that sends numbers. Both shapes are gated behind the
/// `statusCodeValue != 200` check in `detect_qoder_billing_block` (9router
/// qoder.js:376 `statusVal !== 200 && isBillingBlock(inner)`), so a normal
/// 200 chunk merely mentioning `"code":"112"` never fires.
const QODER_BILLING_CODES: &[&str] = &["112", "10605"];

/// Extract the `code` field of a Qoder inner-body JSON object as a string,
/// accepting both `"112"` and `112` shapes. 9router `isBillingBlock` matches
/// strings only; the numeric shape is a benign Rust-side superset (see
/// `QODER_BILLING_CODES`), gated behind the status != 200 check.
pub fn qoder_billing_code(obj: &Value) -> Option<String> {
    match obj.get("code") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Detect a billing block in a Qoder SSE envelope body.
///
/// Returns `Some(error_message)` if the body is a billing/quota error that
/// should be surfaced as a synthetic 403 to trigger combo/account fallback.
/// Returns `None` if the body is normal (should be piped to the client).
///
/// Mirrors 9router qoder.js:376 — only a frame whose envelope
/// `statusCodeValue !== 200` AND whose inner body is a billing block counts.
/// A normal 200 chunk that merely mentions `"code":"112"` (e.g. model output
/// text) must NOT fire. A missing/non-numeric `statusCodeValue` defaults to
/// 200, exactly like the JS (`typeof ... === "number" ? ... : 200`).
pub fn detect_qoder_billing_block(body: &str) -> Option<String> {
    let envelope: Value = serde_json::from_str(body).ok()?;
    let inner = envelope.get("body").and_then(Value::as_str)?;
    let status_val = envelope
        .get("statusCodeValue")
        .and_then(Value::as_u64)
        .unwrap_or(200);
    if status_val == 200 {
        return None;
    }

    // Parse the inner body as JSON (it may be a stringified JSON).
    let inner_json: Option<Value> = serde_json::from_str(inner).ok();

    // Check for billing error codes in the inner JSON (string or number).
    if let Some(ref obj) = inner_json {
        if let Some(code) = qoder_billing_code(obj) {
            if QODER_BILLING_CODES.contains(&code.as_str()) {
                let msg = obj
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("billing/quota error");
                return Some(format!("qoder billing block (code {code}): {msg}"));
            }
        }
        // Check for pricingUrl field (indicates billing required). 9router
        // lowercases the inner body first (`lowerMsg.includes("pricingurl")`),
        // so match case-insensitively here too.
        if obj.get("pricingUrl").is_some() {
            return Some("qoder billing block: pricingUrl present".to_string());
        }
    }

    // Also check the raw string for billing indicators (both `"112"` and
    // `112` shapes, optional whitespace — the JS regex tolerates whitespace
    // but matches strings only; the numeric shapes are the benign Rust-side
    // superset documented on `QODER_BILLING_CODES`).
    // 9router checks `lowerMsg.includes("pricingurl")` (case-insensitive).
    let lower = inner.to_lowercase();
    if lower.contains("pricingurl") {
        return Some("qoder billing block detected in raw body".to_string());
    }
    let compact: String = inner.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("\"code\":\"112\"")
        || compact.contains("\"code\":112")
        || compact.contains("\"code\":\"10605\"")
        || compact.contains("\"code\":10605")
    {
        return Some("qoder billing block detected in raw body".to_string());
    }

    None
}

/// Check if a Qoder SSE line contains a billing block.
/// Returns `Some(error_frame)` if billing detected, `None` if normal.
pub fn check_billing_in_sse_line(line: &str) -> Option<String> {
    let line = line.trim_end();
    if !line.starts_with("data:") {
        return None;
    }
    let payload = line.trim_start_matches("data:").trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    detect_qoder_billing_block(payload).map(|err_msg| {
        // Return a synthetic 403 error frame that the chat handler can detect.
        format!("{{\"error\":true,\"status\":403,\"message\":\"{err_msg}\"}}")
    })
}

// ---------------------------------------------------------------------------
// Image upload + attachment rewrite (ported from attachments.js)
// ---------------------------------------------------------------------------

/// COSY-signed multipart upload endpoint (sig path; full URL adds `/algo`).
const QODER_IMAGE_UPLOAD_SIG_PATH: &str = "/api/v2/image/upload";
/// Images larger than this are never uploaded — they become stubs.
const QODER_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// When the upload fails, data-URIs at or under this size stay inline.
const QODER_INLINE_FALLBACK_MAX_BYTES: usize = 512 * 1024;
/// After rewriting, remaining data-URIs are stripped above this payload size.
const QODER_MAX_PAYLOAD_BYTES: usize = 6 * 1024 * 1024;

/// Parse a base64 data URI into `(mime, base64)`. Returns `None` when the
/// input is not a data URI (9router `parseDataUri`).
pub fn parse_qoder_data_uri(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (mime, b64) = rest.split_once(";base64,")?;
    if mime.is_empty() || b64.is_empty() {
        return None;
    }
    Some((mime.to_string(), b64.to_string()))
}

/// Estimated decoded byte length of a base64 payload (whitespace-tolerant).
pub fn qoder_decoded_bytes(b64: &str) -> usize {
    let compact_len = b64.chars().filter(|c| !c.is_whitespace()).count();
    compact_len * 3 / 4
}

fn qoder_mime_ext(mime: &str) -> &'static str {
    let m = mime.to_lowercase();
    if m.contains("png") {
        "png"
    } else if m.contains("jpeg") || m.contains("jpg") {
        "jpg"
    } else if m.contains("gif") {
        "gif"
    } else if m.contains("webp") {
        "webp"
    } else if m.contains("bmp") {
        "bmp"
    } else if m.contains("pdf") {
        "pdf"
    } else {
        "bin"
    }
}

fn qoder_stub_text(name: &str, mime: &str, bytes: usize, reason: &str) -> String {
    let label = if !name.is_empty() {
        name.to_string()
    } else if !mime.is_empty() {
        mime.to_string()
    } else {
        "attachment".to_string()
    };
    let size = if bytes > 0 {
        format!(", {bytes} bytes")
    } else {
        String::new()
    };
    format!("[file omitted: {label}{size} — {reason}]")
}

/// Build a multipart/form-data body for one file field (9router
/// `buildMultipartFile`). Returns `(boundary, body)`.
pub fn build_qoder_multipart_file(
    buffer: &[u8],
    file_name: &str,
    media_type: &str,
) -> (String, Vec<u8>) {
    let boundary = format!(
        "----9routerQoder{:x}{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        rand::thread_rng().gen::<u32>()
    );
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: {media_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(buffer);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (boundary, body)
}

/// Pull the uploaded file URL out of the upload response JSON, accepting the
/// same key variants as JS `extractUrlFromUploadResponse`.
pub fn extract_qoder_upload_url(json: &Value) -> Option<String> {
    if !json.is_object() {
        return None;
    }
    let result = match json.get("result") {
        Some(r) if r.is_object() => r,
        _ => json,
    };
    for key in ["imageUrls", "image_urls"] {
        for holder in [result, json] {
            if let Some(arr) = holder.get(key).and_then(Value::as_array) {
                if let Some(first) = arr.first().and_then(Value::as_str) {
                    if !first.is_empty() {
                        return Some(first.to_string());
                    }
                }
            }
        }
    }
    for key in [
        "imageUrl",
        "image_url",
        "url",
        "ossUrl",
        "oss_url",
        "originalUrl",
        "originUrl",
        "link",
        "image",
    ] {
        for holder in [result, json] {
            if let Some(v) = holder.get(key).and_then(Value::as_str) {
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    if let Some(body) = json.get("body").and_then(Value::as_str) {
        if let Ok(nested) = serde_json::from_str::<Value>(body) {
            return extract_qoder_upload_url(&nested);
        }
    }
    None
}

/// PUT one image buffer to the Qoder file API (COSY-signed multipart, field
/// "file"). Returns the OSS URL. `inference_base` is api3 (or api2 for jt-).
async fn upload_qoder_image(
    client: &reqwest::Client,
    inference_base: &str,
    buffer: &[u8],
    media_type: &str,
    creds: &QoderCreds,
) -> Result<String, String> {
    let request_id = Uuid::new_v4().to_string();
    let ext = qoder_mime_ext(media_type);
    let url = format!("{inference_base}/algo{QODER_IMAGE_UPLOAD_SIG_PATH}?request_id={request_id}");
    let (boundary, body) = build_qoder_multipart_file(buffer, &format!("image.{ext}"), media_type);
    let cosy = QoderExecutor::build_cosy_headers(body.as_slice(), &url, creds)
        .map_err(|e| e.to_string())?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    let mut headers = HeaderMap::new();
    headers.insert("Accept", HeaderValue::from_static("application/json"));
    headers.insert(
        "Content-Type",
        HeaderValue::from_str(&format!("multipart/form-data; boundary={boundary}"))
            .map_err(|e| e.to_string())?,
    );
    headers.insert(
        "Content-Length",
        HeaderValue::from_str(&body.len().to_string()).map_err(|e| e.to_string())?,
    );
    headers.insert(
        "AI-CLIENT-TIMESTAMP",
        HeaderValue::from_str(&timestamp).map_err(|e| e.to_string())?,
    );
    headers.insert("Accept-Encoding", HeaderValue::from_static("identity"));
    for (name, value) in [
        ("Authorization", &cosy.authorization),
        ("Cosy-Key", &cosy.cosy_key),
        ("Cosy-User", &cosy.cosy_user),
        ("Cosy-Date", &cosy.cosy_date),
        ("Cosy-Version", &cosy.cosy_version),
        ("Cosy-Machineid", &cosy.cosy_machineid),
        ("Cosy-Machinetoken", &cosy.cosy_machinetoken),
        ("Cosy-Machinetype", &cosy.cosy_machinetype),
        ("Cosy-Machineos", &cosy.cosy_machineos),
        ("Cosy-Clienttype", &cosy.cosy_clienttype),
        ("Cosy-Clientip", &cosy.cosy_clientip),
        ("Cosy-Bodyhash", &cosy.cosy_bodyhash),
        ("Cosy-Bodylength", &cosy.cosy_bodylength),
        ("Cosy-Sigpath", &cosy.cosy_sigpath),
        ("Cosy-Data-Policy", &cosy.cosy_data_policy),
        ("Cosy-Organization-Id", &cosy.cosy_organization_id),
        ("Cosy-Organization-Tags", &cosy.cosy_organization_tags),
        ("Login-Version", &cosy.login_version),
        ("X-Request-Id", &cosy.x_request_id),
    ] {
        headers.insert(
            name,
            HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
    }
    let res = client
        .put(&url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {status} {}",
            text.chars().take(180).collect::<String>()
        ));
    }
    let json: Value = res.json().await.map_err(|e| e.to_string())?;
    extract_qoder_upload_url(&json).ok_or_else(|| "upload response missing url".to_string())
}

fn is_qoder_image_mime(mime: &str) -> bool {
    mime.to_lowercase().starts_with("image/")
}

/// Outcome of resolving one image payload: upload it (cached by sha256),
/// keep it inline when tiny, or stub it.
async fn resolve_qoder_image(
    client: &reqwest::Client,
    inference_base: &str,
    base64_data: &str,
    media_type: &str,
    creds: &QoderCreds,
    cache: &mut HashMap<String, String>,
) -> QoderImageOutcome {
    let compact: String = base64_data.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return QoderImageOutcome::Stub {
            bytes: 0,
            mime: media_type.to_string(),
        };
    }
    let bytes = qoder_decoded_bytes(&compact);
    if bytes > QODER_MAX_IMAGE_BYTES {
        tracing::warn!(target: "openproxy::executor", "qoder image {bytes} bytes exceeds upload cap, stubbing");
        return QoderImageOutcome::Stub {
            bytes,
            mime: media_type.to_string(),
        };
    }
    let buffer = match B64.decode(&compact) {
        Ok(b) => b,
        Err(_) => {
            return QoderImageOutcome::Stub {
                bytes,
                mime: media_type.to_string(),
            }
        }
    };
    let digest = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&buffer))
    };
    if let Some(url) = cache.get(&digest) {
        return QoderImageOutcome::Url {
            url: url.clone(),
            bytes,
        };
    }
    match upload_qoder_image(client, inference_base, &buffer, media_type, creds).await {
        Ok(url) => {
            cache.insert(digest, url.clone());
            QoderImageOutcome::Url { url, bytes }
        }
        Err(e) => {
            tracing::warn!(target: "openproxy::executor", "qoder image upload failed ({e})");
            if bytes <= QODER_INLINE_FALLBACK_MAX_BYTES {
                QoderImageOutcome::Keep { bytes }
            } else {
                QoderImageOutcome::Stub {
                    bytes,
                    mime: media_type.to_string(),
                }
            }
        }
    }
}

enum QoderImageOutcome {
    Url { url: String, bytes: usize },
    Keep { bytes: usize },
    Stub { bytes: usize, mime: String },
}

fn qoder_image_url_block(url: &str) -> Value {
    serde_json::json!({ "type": "image_url", "image_url": { "url": url } })
}

/// Rewrite one content block: upload inlined images, stub huge files (9router
/// `rewriteBlock`). Returns `None` to drop the block.
async fn rewrite_qoder_block(block: &Value, ctx: &mut QoderRewriteCtx<'_>) -> Option<Value> {
    let obj = block.as_object()?;
    let block_type = obj.get("type").and_then(Value::as_str).unwrap_or("");

    if block_type == "image_url" {
        let raw = match obj.get("image_url") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            None => String::new(),
        };
        if raw.is_empty() {
            return None;
        }
        if raw.starts_with("http://") || raw.starts_with("https://") {
            return Some(qoder_image_url_block(&raw));
        }
        let (mime, b64) = match parse_qoder_data_uri(&raw) {
            Some(p) => p,
            None => {
                return Some(
                    serde_json::json!({ "type": "text", "text": qoder_stub_text("attachment", "", 0, "unreadable data URI") }),
                );
            }
        };
        if !is_qoder_image_mime(&mime) {
            return Some(
                serde_json::json!({ "type": "text", "text": qoder_stub_text("file", &mime, qoder_decoded_bytes(&b64), "non-image bytes are not inlined into Qoder context") }),
            );
        }
        match resolve_qoder_image(
            ctx.client,
            ctx.inference_base,
            &b64,
            &mime,
            ctx.creds,
            ctx.cache,
        )
        .await
        {
            QoderImageOutcome::Url { url, .. } => Some(qoder_image_url_block(&url)),
            QoderImageOutcome::Keep { .. } => Some(qoder_image_url_block(&raw)),
            QoderImageOutcome::Stub { bytes, mime } => Some(
                serde_json::json!({ "type": "text", "text": qoder_stub_text("image", &mime, bytes, "upload failed; not inlined") }),
            ),
        }
    } else if block_type == "image" {
        // Claude-style {type:"image", source:{...}} → image_url.
        let src = obj.get("source")?;
        if src.get("type").and_then(Value::as_str) == Some("url") {
            let url = src.get("url").and_then(Value::as_str).unwrap_or("");
            return Some(qoder_image_url_block(url));
        }
        if src.get("type").and_then(Value::as_str) == Some("base64") {
            let data = src.get("data").and_then(Value::as_str).unwrap_or("");
            if data.is_empty() {
                return None;
            }
            let mime = src
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            match resolve_qoder_image(
                ctx.client,
                ctx.inference_base,
                data,
                mime,
                ctx.creds,
                ctx.cache,
            )
            .await
            {
                QoderImageOutcome::Url { url, .. } => Some(qoder_image_url_block(&url)),
                QoderImageOutcome::Keep { .. } => {
                    Some(qoder_image_url_block(&format!("data:{mime};base64,{data}")))
                }
                QoderImageOutcome::Stub { bytes, mime } => Some(
                    serde_json::json!({ "type": "text", "text": qoder_stub_text("image", &mime, bytes, "upload failed; not inlined") }),
                ),
            }
        } else {
            None
        }
    } else if block_type == "file" {
        let file = obj.get("file")?;
        let name = file
            .get("filename")
            .or_else(|| file.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("file");
        let file_data = file.get("file_data").and_then(Value::as_str).unwrap_or("");
        let (b64, mime) = match parse_qoder_data_uri(file_data) {
            Some((m, b)) => (b, m),
            None => (
                if file_data.starts_with("data:") {
                    String::new()
                } else {
                    file_data.to_string()
                },
                file.get("format")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_string(),
            ),
        };
        if !b64.is_empty() && is_qoder_image_mime(&mime) {
            if let QoderImageOutcome::Url { url, .. } = resolve_qoder_image(
                ctx.client,
                ctx.inference_base,
                &b64,
                &mime,
                ctx.creds,
                ctx.cache,
            )
            .await
            {
                return Some(qoder_image_url_block(&url));
            }
        }
        Some(
            serde_json::json!({ "type": "text", "text": qoder_stub_text(name, &mime, qoder_decoded_bytes(&b64), "Qoder reads documents via its file API, not inlined bytes") }),
        )
    } else if block_type == "document" {
        // Claude document block.
        let name = obj
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("document");
        let src = obj.get("source")?;
        if src.get("type").and_then(Value::as_str) == Some("base64") {
            let data = src.get("data").and_then(Value::as_str).unwrap_or("");
            let mime = src
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("application/pdf");
            if is_qoder_image_mime(mime) {
                if let QoderImageOutcome::Url { url, .. } = resolve_qoder_image(
                    ctx.client,
                    ctx.inference_base,
                    data,
                    mime,
                    ctx.creds,
                    ctx.cache,
                )
                .await
                {
                    return Some(qoder_image_url_block(url.as_str()));
                }
            }
            return Some(
                serde_json::json!({ "type": "text", "text": qoder_stub_text(name, mime, qoder_decoded_bytes(data), "Qoder reads documents via its file API, not inlined bytes") }),
            );
        }
        Some(block.clone())
    } else if let Some(text) = obj.get("text").and_then(Value::as_str) {
        if text.contains("data:") && text.len() > 8192 {
            return Some(
                serde_json::json!({ "type": obj.get("type").cloned().unwrap_or(Value::String("text".to_string())), "text": strip_qoder_data_uris(text) }),
            );
        }
        Some(block.clone())
    } else {
        Some(block.clone())
    }
}

/// Scan `text` for inlined `data:` URIs, handing each match to `replace`.
/// Shared by the two stripping passes below, which differ only in whether a
/// small URI is kept.
fn map_qoder_data_uris(text: &str, mut replace: impl FnMut(&str) -> String) -> String {
    // Scan for data: URIs terminated by whitespace/quote.
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("data:") {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')')
            .unwrap_or(tail.len());
        let candidate = &tail[..end];
        out.push_str(&replace(candidate));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Replace oversized inlined data-URIs in free text with stubs (9router
/// `rewriteContent` string path). Small ones stay inline.
fn strip_qoder_data_uris(text: &str) -> String {
    map_qoder_data_uris(text, |candidate| match parse_qoder_data_uri(candidate) {
        Some((mime, b64)) if qoder_decoded_bytes(&b64) > QODER_INLINE_FALLBACK_MAX_BYTES => {
            qoder_stub_text(
                "",
                &mime,
                qoder_decoded_bytes(&b64),
                "inlined data URI stripped from Qoder context",
            )
        }
        _ => candidate.to_string(),
    })
}

/// Final over-budget safety net: stub EVERY inlined data-URI, however small
/// (9router `stripRemainingDataUris`, attachments.js:258-278). Unlike the main
/// rewrite pass this applies no size threshold — the payload is already over
/// budget, so even a "small" URI is what pushed it there. The stub reports the
/// URI's own length, not its decoded size, as 9router does.
fn strip_all_qoder_data_uris(text: &str) -> String {
    map_qoder_data_uris(text, |candidate| {
        qoder_stub_text("", "", candidate.len(), "payload over Qoder size budget")
    })
}

struct QoderRewriteCtx<'a> {
    client: &'a reqwest::Client,
    inference_base: &'a str,
    creds: &'a QoderCreds,
    cache: &'a mut HashMap<String, String>,
}

/// Rewrite OpenAI-shaped messages in place: upload images, stub huge files.
/// Returns `(uploaded_urls, stubbed_count)` (9router
/// `rewriteQoderMessageAttachments`).
pub async fn rewrite_qoder_message_attachments(
    messages: &mut [Value],
    client: &reqwest::Client,
    inference_base: &str,
    creds: &QoderCreds,
) -> (Vec<String>, usize) {
    let mut cache: HashMap<String, String> = HashMap::new();
    let mut ctx = QoderRewriteCtx {
        client,
        inference_base,
        creds,
        cache: &mut cache,
    };
    for msg in messages.iter_mut() {
        let obj = match msg.as_object_mut() {
            Some(o) => o,
            None => continue,
        };
        // Ollama-style sidecar images: fold into content.
        if let Some(images) = obj.remove("images") {
            if let Some(arr) = images.as_array() {
                let extras: Vec<Value> = arr
                    .iter()
                    .filter_map(|u| u.as_str())
                    .map(|u| qoder_image_url_block(u))
                    .collect();
                if !extras.is_empty() {
                    let content = obj
                        .remove("content")
                        .unwrap_or(Value::String(String::new()));
                    let mut blocks = match content {
                        Value::Array(a) => a,
                        Value::String(s) => vec![serde_json::json!({ "type": "text", "text": s })],
                        _ => vec![serde_json::json!({ "type": "text", "text": "" })],
                    };
                    blocks.extend(extras);
                    obj.insert("content".to_string(), Value::Array(blocks));
                }
            }
        }
        let content = obj.remove("content").unwrap_or(Value::Null);
        let rewritten = match content {
            Value::String(s) => {
                if s.contains("data:") && s.len() > 8192 {
                    Value::String(strip_qoder_data_uris(&s))
                } else {
                    Value::String(s)
                }
            }
            Value::Array(blocks) => {
                let mut out = Vec::new();
                for block in &blocks {
                    if let Some(next) = rewrite_qoder_block(block, &mut ctx).await {
                        out.push(next);
                    }
                }
                if out.is_empty() {
                    Value::String(String::new())
                } else {
                    Value::Array(out)
                }
            }
            other => other,
        };
        obj.insert("content".to_string(), rewritten);
    }
    let mut image_urls = Vec::new();
    let mut stubbed = 0usize;
    for msg in messages.iter() {
        let Some(blocks) = msg.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            let url = block
                .get("type")
                .and_then(Value::as_str)
                .filter(|t| *t == "image_url")
                .and_then(|_| block.get("image_url"))
                .and_then(|v| match v {
                    Value::String(s) => Some(s.as_str()),
                    _ => v.get("url").and_then(Value::as_str),
                });
            if let Some(u) = url {
                if u.starts_with("http://") || u.starts_with("https://") {
                    image_urls.push(u.to_string());
                } else if u.starts_with("data:") {
                    image_urls.push(u.to_string());
                }
            }
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    if t.starts_with("[file omitted:") {
                        stubbed += 1;
                    }
                }
            }
        }
    }
    // Drop remaining inlined binaries when the payload still exceeds budget.
    let payload_bytes = serde_json::to_vec(&messages).map(|v| v.len()).unwrap_or(0);
    if payload_bytes > QODER_MAX_PAYLOAD_BYTES {
        tracing::warn!(target: "openproxy::executor", "qoder request still {payload_bytes} bytes after rewrite; stripping leftover data URIs");
        for msg in messages.iter_mut() {
            strip_qoder_message_data_uris(msg);
        }
    }
    (image_urls, stubbed)
}

// ---------------------------------------------------------------------------
// Context-window tiers (ported from contextTier.js, pure functions, no I/O)
// ---------------------------------------------------------------------------

/// Context-window tier modes (`QODER_CONTEXT_TIER` env: auto|max|default|<name>).
const QODER_CONTEXT_TIER_HEADROOM: f64 = 0.15;

/// Tier the headroom estimate, in tokens, of a char class.
fn is_qoder_cjk(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{11ff}'
        | '\u{2e80}'..='\u{9fff}'
        | '\u{ac00}'..='\u{d7af}'
        | '\u{f900}'..='\u{faff}'
        | '\u{ff00}'..='\u{ffef}')
}

/// "200K" | "1M" | "204800" | 204800 → token count, 0 when unparseable.
pub fn parse_qoder_tier_tokens(value: &Value) -> u64 {
    match value {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => {
            let t = s.trim().to_uppercase();
            let (num_part, mult) = if let Some(stripped) = t.strip_suffix('K') {
                (stripped, 1_000u64)
            } else if let Some(stripped) = t.strip_suffix('M') {
                (stripped, 1_000_000u64)
            } else {
                (t.as_str(), 1u64)
            };
            match num_part.trim().parse::<f64>() {
                Ok(n) if n.is_finite() && n > 0.0 => (n * mult as f64).floor() as u64,
                _ => 0,
            }
        }
        _ => 0,
    }
}

struct QoderTier {
    name: String,
    token_count: u64,
    is_default: bool,
}

/// Normalize a model_config into sorted tiers (9router `getQoderContextTiers`).
pub fn qoder_context_tiers(model_config: &Value) -> Vec<(String, u64, bool)> {
    let list = model_config
        .get("context_config")
        .or_else(|| model_config.get("contextConfig"))
        .and_then(Value::as_array);
    let Some(list) = list else {
        return Vec::new();
    };
    let mut by_count: HashMap<u64, QoderTier> = HashMap::new();
    for entry in list {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let count = [
            "tokenCount",
            "token_count",
            "max_input_tokens",
            "maxInputTokens",
            "contextLength",
            "context_length",
        ]
        .iter()
        .filter_map(|k| obj.get(*k))
        .map(parse_qoder_tier_tokens)
        .find(|n| *n > 0)
        .unwrap_or(0);
        if count == 0 {
            continue;
        }
        let name = ["name", "label", "display_name", "displayName", "key", "id"]
            .iter()
            .filter_map(|k| obj.get(*k).and_then(Value::as_str))
            .map(str::trim)
            .find(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                if count >= 1_000_000 && count % 1_000_000 == 0 {
                    format!("{}M", count / 1_000_000)
                } else if count >= 1_000 && count % 1_000 == 0 {
                    format!("{}K", count / 1_000)
                } else {
                    count.to_string()
                }
            });
        let is_default = ["isDefault", "is_default", "default"]
            .iter()
            .any(|k| obj.get(*k).and_then(Value::as_bool) == Some(true));
        by_count
            .entry(count)
            .and_modify(|t| t.is_default = t.is_default || is_default)
            .or_insert(QoderTier {
                name,
                token_count: count,
                is_default,
            });
    }
    let mut tiers: Vec<QoderTier> = by_count.into_values().collect();
    tiers.sort_by_key(|t| t.token_count);
    tiers
        .into_iter()
        .map(|t| (t.name, t.token_count, t.is_default))
        .collect()
}

/// Rough prompt-size estimate in tokens: CJK chars ~1 token each, everything
/// else ~4 chars/token (9router `estimateQoderPromptTokens`).
pub fn estimate_qoder_prompt_tokens(system: &str, messages: &Value, tools: Option<&Value>) -> u64 {
    let text = serde_json::json!({
        "system": system,
        "messages": messages,
        "tools": tools.cloned().unwrap_or(Value::Array(vec![])),
    })
    .to_string();
    let cjk = text.chars().filter(|c| is_qoder_cjk(*c)).count() as u64;
    // `text.length` in JS is the UTF-16 code-unit count, so an astral-plane
    // character (emoji, rare CJK) contributes 2, not 1.
    let rest = text.encode_utf16().count() as u64 - cjk;
    (cjk * 4 + rest).div_ceil(4)
}

/// Decide which tier a request should run under (9router
/// `resolveQoderContextTier`). Returns `(name, token_count, reason)` or `None`
/// to leave the payload untouched. Reads `QODER_CONTEXT_TIER` env.
pub fn resolve_qoder_context_tier(
    model_config: &Value,
    system: &str,
    messages: &Value,
    tools: Option<&Value>,
) -> Option<(String, u64, String)> {
    let tiers = qoder_context_tiers(model_config);
    if tiers.is_empty() {
        return None;
    }
    let mode = std::env::var("QODER_CONTEXT_TIER")
        .unwrap_or_else(|_| "auto".to_string())
        .trim()
        .to_string();
    let mode_lower = mode.to_lowercase();
    let largest = tiers.last().unwrap();
    let default_tier = tiers.iter().find(|t| t.2).unwrap_or(&tiers[0]);
    let estimated = estimate_qoder_prompt_tokens(system, messages, tools);
    let need = (estimated as f64 * (1.0 + QODER_CONTEXT_TIER_HEADROOM)).ceil() as u64;

    if mode_lower == "max" {
        return Some((largest.0.clone(), largest.1, "forced:max".to_string()));
    }
    if mode_lower == "default" {
        return Some((
            default_tier.0.clone(),
            default_tier.1,
            "forced:default".to_string(),
        ));
    }
    if mode_lower != "auto" {
        let wanted = mode.replace(char::is_whitespace, "").to_uppercase();
        let as_count = parse_qoder_tier_tokens(&Value::String(wanted.clone()));
        if let Some(found) = tiers.iter().find(|t| {
            t.0.replace(char::is_whitespace, "").to_uppercase() == wanted
                || (as_count > 0 && t.1 == as_count)
        }) {
            return Some((found.0.clone(), found.1, format!("forced:{}", found.0)));
        }
        // Unknown tier name → fall through to auto.
    }

    let current_max = model_config
        .get("max_input_tokens")
        .or_else(|| model_config.get("maxInputTokens"))
        .map(parse_qoder_tier_tokens)
        .unwrap_or(0);
    let current_limit = if current_max > 0 {
        current_max
    } else {
        default_tier.1
    };
    if need <= current_limit {
        return None;
    }
    let fits = tiers.iter().find(|t| t.1 >= need && t.1 > current_limit);
    let tier = fits.unwrap_or(largest);
    if tier.1 <= current_limit {
        return None;
    }
    Some((
        tier.0.clone(),
        tier.1,
        if fits.is_some() {
            "auto:fits".to_string()
        } else {
            "auto:largest".to_string()
        },
    ))
}

/// Write the chosen tier into a Qoder chat payload (9router
/// `applyQoderContextTier` in contextTier.js): parameters.context_length,
/// chat_context.extra.ideModelConfigOverride, model_config.
///
/// Mirrors the JS spread-defaults: `parameters` and `chat_context`/`extra`
/// are created when absent. `model_config` is only touched when already
/// present and an object — the JS guards with `typeof ... === "object"`.
pub fn apply_qoder_context_tier(payload: &mut Value, tier: &(String, u64, String)) {
    let count = tier.1;
    if !payload.is_object() {
        return;
    }
    let params = payload
        .as_object_mut()
        .expect("checked is_object")
        .entry("parameters")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Some(obj) = params.as_object_mut() {
        obj.insert("context_length".to_string(), Value::from(count));
    }
    let chat_ctx = payload
        .as_object_mut()
        .expect("checked is_object")
        .entry("chat_context")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Some(ctx_obj) = chat_ctx.as_object_mut() {
        let extra = ctx_obj
            .entry("extra")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(extra_obj) = extra.as_object_mut() {
            let mut over = extra_obj
                .get("ideModelConfigOverride")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            over.insert("max_input_tokens".to_string(), Value::from(count));
            extra_obj.insert("ideModelConfigOverride".to_string(), Value::Object(over));
        }
    }
    if let Some(mc) = payload.get_mut("model_config") {
        if let Some(obj) = mc.as_object_mut() {
            obj.insert("max_input_tokens".to_string(), Value::from(count));
        }
    }
}

/// Final budget pass: replace leftover data-URIs with stubs (9router
/// `stripRemainingDataUris`).
fn strip_qoder_message_data_uris(msg: &mut Value) {
    let Some(obj) = msg.as_object_mut() else {
        return;
    };
    let Some(content) = obj.get_mut("content") else {
        return;
    };
    if let Some(s) = content.as_str() {
        if s.contains("data:") {
            *content = Value::String(strip_all_qoder_data_uris(s));
        }
        return;
    }
    if let Some(blocks) = content.as_array_mut() {
        for block in blocks.iter_mut() {
            let is_image_url = block.get("type").and_then(Value::as_str) == Some("image_url");
            if is_image_url {
                let raw = match block.get("image_url") {
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(v) => v.get("url").and_then(Value::as_str).map(String::from),
                    None => None,
                };
                if let Some(u) = raw {
                    if u.starts_with("data:") {
                        *block = serde_json::json!({ "type": "text", "text": qoder_stub_text("image", "", 0, "payload over Qoder size budget") });
                        continue;
                    }
                }
            }
            if let Some(t) = block.get("text").and_then(Value::as_str).map(String::from) {
                if t.contains("data:") {
                    if let Some(o) = block.as_object_mut() {
                        o.insert(
                            "text".to_string(),
                            Value::String(strip_all_qoder_data_uris(&t)),
                        );
                    }
                }
            }
        }
    }
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

    // -----------------------------------------------------------------------
    // COSY crypto helpers
    // -----------------------------------------------------------------------

    /// Generate a random 16-byte AES key from the first 16 chars of a UUID
    /// (matches qodercli/Veria convention).
    fn generate_aes_key() -> String {
        let uuid = Uuid::new_v4().to_string();
        uuid[..16].to_string()
    }

    /// AES-128-CBC encrypt with PKCS7 padding, IV = key bytes, returns base64.
    fn aes_cbc_encrypt_base64(
        plaintext: &[u8],
        key_str: &str,
    ) -> Result<String, QoderExecutorError> {
        let key_bytes = key_str.as_bytes();
        if key_bytes.len() != 16 {
            return Err(QoderExecutorError::CryptoError(format!(
                "AES key must be 16 bytes, got {}",
                key_bytes.len()
            )));
        }

        // IV is the key itself (matches upstream: iv = keyBytes.subarray(0, 16))
        let iv = key_bytes;

        // PKCS7 pad manually so we can use no-padding mode on the cipher
        let block_size = 16usize;
        let padding_len = block_size - (plaintext.len() % block_size);
        let padded_len = plaintext.len() + padding_len;
        let mut padded = vec![0u8; padded_len + block_size]; // extra block for potential padding expansion
        padded[..plaintext.len()].copy_from_slice(plaintext);
        padded[plaintext.len()..padded_len].fill(padding_len as u8);

        let encryptor = Aes128CbcEnc::new(key_bytes.into(), iv.into());
        let encrypted = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut padded, padded_len)
            .map_err(|e| QoderExecutorError::CryptoError(format!("AES encrypt error: {:?}", e)))?;

        Ok(B64.encode(encrypted))
    }

    /// RSA PKCS#1 v1.5 encrypt the AES key with the hardcoded SPKI public
    /// key, returns base64. Matches JS `crypto.publicEncrypt({ padding:
    /// RSA_PKCS1_PADDING })` in cosy.js.
    fn rsa_encrypt_base64(data: &str) -> Result<String, QoderExecutorError> {
        let public_key = RsaPublicKey::from_public_key_pem(QODER_RSA_PUBLIC_KEY_PEM)
            .map_err(|e| QoderExecutorError::CryptoError(format!("RSA key parse error: {e}")))?;

        let mut rng = rand::thread_rng();
        let encrypted = public_key
            .encrypt(&mut rng, Pkcs1v15Encrypt, data.as_bytes())
            .map_err(|e| QoderExecutorError::CryptoError(format!("RSA encrypt error: {e}")))?;

        Ok(B64.encode(&encrypted))
    }

    /// Encrypt user info: generate AES key, encrypt user JSON, wrap AES key
    /// with RSA. Returns (cosy_key_b64, info_b64).
    fn encrypt_user_info(user_info: &Value) -> Result<(String, String), QoderExecutorError> {
        let aes_key = Self::generate_aes_key();
        let plaintext = serde_json::to_string(user_info)?;
        let info_b64 = Self::aes_cbc_encrypt_base64(plaintext.as_bytes(), &aes_key)?;
        let cosy_key_b64 = Self::rsa_encrypt_base64(&aes_key)?;
        Ok((cosy_key_b64, info_b64))
    }

    /// Compute MD5 hex digest.
    fn md5_hex(input: &[u8]) -> String {
        let mut hasher = Md5::new();
        hasher.update(input);
        hex::encode(hasher.finalize())
    }

    /// Strip the leading "/algo" prefix from the request path (matches qodercli
    /// convention).
    fn compute_sig_path(request_url: &str) -> String {
        // Extract pathname from full URL. Find "://", then find the next '/'
        // after the host portion.
        let pathname = if let Some(scheme_end) = request_url.find("://") {
            let after_scheme = &request_url[scheme_end + 3..];
            if let Some(path_idx) = after_scheme.find('/') {
                let full_path = &after_scheme[path_idx..];
                full_path.split('?').next().unwrap_or("")
            } else {
                "/"
            }
        } else {
            // Not a full URL, treat as path
            request_url.split('?').next().unwrap_or("")
        };

        if let Some(stripped) = pathname.strip_prefix("/algo") {
            stripped.to_string()
        } else {
            pathname.to_string()
        }
    }

    /// Qoder WAF-bypass body encoding.
    ///
    /// Algorithm (ported from encoding.js):
    ///   1. base64-encode the plaintext bytes (standard alphabet).
    ///   2. Rearrange: split into thirds, reorder as [tail][mid][head].
    ///   3. Substitute each character via a custom alphabet mapping.
    fn qoder_encode_body(plaintext: &[u8]) -> String {
        let std_b64 = B64.encode(plaintext);
        let std_bytes = std_b64.as_bytes();
        let n = std_bytes.len();
        if n == 0 {
            return String::new();
        }
        let a = n / 3;

        // Build substitution table: standard -> custom
        let mut s2c = [0u8; 128];
        for i in 0..64 {
            let std_char = QODER_STD_ALPHABET[i] as usize;
            s2c[std_char] = QODER_CUSTOM_ALPHABET[i];
        }
        s2c[b'=' as usize] = b'$';

        // Rearrange: [tail][mid][head]
        let tail = &std_bytes[n - a..];
        let mid = &std_bytes[a..n - a];
        let head = &std_bytes[..a];

        let mut rearranged = Vec::with_capacity(n);
        rearranged.extend_from_slice(tail);
        rearranged.extend_from_slice(mid);
        rearranged.extend_from_slice(head);

        // Substitute
        let mut out = Vec::with_capacity(n);
        for &c in &rearranged {
            if (c as usize) < 128 && s2c[c as usize] != 0 {
                out.push(s2c[c as usize]);
            } else {
                out.push(c);
            }
        }

        // All bytes are valid ASCII/latin1
        String::from_utf8_lossy(&out).to_string()
    }

    // -----------------------------------------------------------------------
    // COSY header builder
    // -----------------------------------------------------------------------

    /// Build the full Cosy-* header set for a single Qoder request.
    /// This is the Rust port of `buildCosyHeaders` from cosy.js.
    fn build_cosy_headers(
        body: &[u8],
        request_url: &str,
        creds: &QoderCreds,
    ) -> Result<CosyHeaders, QoderExecutorError> {
        if creds.user_id.is_empty() {
            return Err(QoderExecutorError::MissingCredentials(
                "cosy: user id is empty".into(),
            ));
        }
        if creds.auth_token.is_empty() {
            return Err(QoderExecutorError::MissingCredentials(
                "cosy: auth token is empty".into(),
            ));
        }

        let user_info = serde_json::json!({
            "uid": creds.user_id,
            "security_oauth_token": creds.auth_token,
            "name": creds.name,
            "aid": "",
            "email": creds.email,
        });

        let (cosy_key, info) = Self::encrypt_user_info(&user_info)?;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string();

        let request_id = Uuid::new_v4().to_string();

        let payload_json = serde_json::json!({
            "version": "v1",
            "requestId": request_id,
            "info": info,
            "cosyVersion": QODER_IDE_VERSION,
            "ideVersion": "",
        });
        let payload_json_str = serde_json::to_string(&payload_json)?;
        let payload_b64 = B64.encode(payload_json_str.as_bytes());

        let sig_path = Self::compute_sig_path(request_url);

        // sigInput = payloadB64 + "\n" + cosyKey + "\n" + timestamp + "\n" + body + "\n" + sigPath.
        // Built as raw bytes: JS hashes `Buffer.from(sigInput, "latin1")`, and
        // latin1 round-trips 0x00-0xFF, so a multipart body carrying image bytes
        // must be hashed verbatim. A lossy UTF-8 rendering would hash U+FFFD
        // placeholders instead of the bytes the server sees.
        let mut sig_input: Vec<u8> = Vec::with_capacity(
            payload_b64.len() + cosy_key.len() + timestamp.len() + body.len() + sig_path.len() + 4,
        );
        sig_input.extend_from_slice(payload_b64.as_bytes());
        sig_input.push(b'\n');
        sig_input.extend_from_slice(cosy_key.as_bytes());
        sig_input.push(b'\n');
        sig_input.extend_from_slice(timestamp.as_bytes());
        sig_input.push(b'\n');
        sig_input.extend_from_slice(body);
        sig_input.push(b'\n');
        sig_input.extend_from_slice(sig_path.as_bytes());
        let sig = Self::md5_hex(&sig_input);

        let machine_id = if creds.machine_id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            creds.machine_id.clone()
        };
        let body_hash = Self::md5_hex(body);
        let body_length = body.len().to_string();

        Ok(CosyHeaders {
            authorization: format!("Bearer COSY.{}.{}", payload_b64, sig),
            cosy_key,
            cosy_user: creds.user_id.clone(),
            cosy_date: timestamp,
            cosy_version: QODER_IDE_VERSION.to_string(),
            cosy_machineid: machine_id.clone(),
            cosy_machinetoken: machine_id,
            cosy_machinetype: QODER_MACHINE_TYPE.to_string(),
            cosy_machineos: QODER_MACHINE_OS.to_string(),
            cosy_clienttype: QODER_CLIENT_TYPE.to_string(),
            cosy_clientip: "127.0.0.1".to_string(),
            cosy_bodyhash: body_hash,
            cosy_bodylength: body_length,
            cosy_sigpath: sig_path,
            cosy_data_policy: QODER_DATA_POLICY.to_string(),
            cosy_organization_id: String::new(),
            cosy_organization_tags: String::new(),
            login_version: QODER_LOGIN_VERSION.to_string(),
            x_request_id: Uuid::new_v4().to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // URL & headers
    // -----------------------------------------------------------------------

    /// 9router qoder.js buildUrl: `jt-` tokens (that are not `pt-`) route to
    /// api2.qoder.sh; everything else uses api3.
    fn build_url(&self, credentials: &ProviderConnection) -> String {
        let raw = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .unwrap_or("");
        if !raw.starts_with("pt-") && raw.starts_with("jt-") {
            QODER_CHAT_URL_ALT.to_string()
        } else {
            QODER_CHAT_URL_ENCODED.to_string()
        }
    }

    fn build_headers(
        &self,
        encoded_body: &[u8],
        request_url: &str,
        creds: &QoderCreds,
        qoder_key: &str,
        model_source: &str,
    ) -> Result<HeaderMap, QoderExecutorError> {
        let cosy = Self::build_cosy_headers(encoded_body, request_url, creds)?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("Accept", HeaderValue::from_static("text/event-stream"));
        headers.insert("Cache-Control", HeaderValue::from_static("no-cache"));
        // gzip triggers signature validation on Qoder's CDN; force identity.
        headers.insert("Accept-Encoding", HeaderValue::from_static("identity"));

        // 9router: X-Model-Key / X-Model-Source (modelSource from
        // payload.model_config.source || "system").
        headers.insert(
            "X-Model-Key",
            HeaderValue::from_str(qoder_key).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "X-Model-Source",
            HeaderValue::from_str(model_source).unwrap_or_else(|_| HeaderValue::from_static("")),
        );

        headers.insert(
            "Authorization",
            HeaderValue::from_str(&cosy.authorization)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Key",
            HeaderValue::from_str(&cosy.cosy_key).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-User",
            HeaderValue::from_str(&cosy.cosy_user).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Date",
            HeaderValue::from_str(&cosy.cosy_date).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Version",
            HeaderValue::from_str(&cosy.cosy_version)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Machineid",
            HeaderValue::from_str(&cosy.cosy_machineid)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Machinetoken",
            HeaderValue::from_str(&cosy.cosy_machinetoken)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Machinetype",
            HeaderValue::from_str(&cosy.cosy_machinetype)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Machineos",
            HeaderValue::from_str(&cosy.cosy_machineos)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Clienttype",
            HeaderValue::from_str(&cosy.cosy_clienttype)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Clientip",
            HeaderValue::from_str(&cosy.cosy_clientip)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Bodyhash",
            HeaderValue::from_str(&cosy.cosy_bodyhash)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Bodylength",
            HeaderValue::from_str(&cosy.cosy_bodylength)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Sigpath",
            HeaderValue::from_str(&cosy.cosy_sigpath)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Data-Policy",
            HeaderValue::from_str(&cosy.cosy_data_policy)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Organization-Id",
            HeaderValue::from_str(&cosy.cosy_organization_id)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Cosy-Organization-Tags",
            HeaderValue::from_str(&cosy.cosy_organization_tags)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "Login-Version",
            HeaderValue::from_str(&cosy.login_version)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "X-Request-Id",
            HeaderValue::from_str(&cosy.x_request_id)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );

        Ok(headers)
    }

    // -----------------------------------------------------------------------
    // Request body transformation
    // -----------------------------------------------------------------------

    /// Extract text from a message content field (string or array of parts).
    fn extract_text(content: &Value) -> String {
        if let Some(s) = content.as_str() {
            return s.to_string();
        }
        if content.is_null() {
            return String::new();
        }
        if let Some(arr) = content.as_array() {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|item| {
                    if let Some(obj) = item.as_object() {
                        if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                            return Some(text.to_string());
                        }
                    }
                    None
                })
                .collect();
            return parts.join("\n");
        }
        content.to_string()
    }

    /// Hoist role:"system" messages out of the messages array (Qoder rejects
    /// system in messages). Text-only content flattens to a plain string;
    /// when images are present the content stays an array and `image_url`
    /// blocks are preserved (9router `normalizeContent`). Claude
    /// `{type:"image", source:{...}}` blocks convert to `image_url`;
    /// surviving file/document blocks become short stubs.
    fn normalize_messages(messages: &[Value]) -> (Vec<Value>, String) {
        let mut system_parts = Vec::new();
        let mut out = Vec::new();

        for msg in messages {
            let obj = match msg.as_object() {
                Some(o) => o,
                None => continue,
            };
            let role = obj.get("role").and_then(|v| v.as_str()).unwrap_or("");

            if role == "system" || role == "developer" {
                let text = Self::extract_text(msg.get("content").unwrap_or(&Value::Null));
                if !text.is_empty() {
                    system_parts.push(text);
                }
                continue;
            }

            let mut cloned = msg.clone();
            if let Some(obj) = cloned.as_object_mut() {
                obj.insert(
                    "content".to_string(),
                    Self::normalize_content(msg.get("content").unwrap_or(&Value::Null)),
                );
            }
            out.push(cloned);
        }

        (out, system_parts.join("\n\n"))
    }

    /// Normalize one message's content (9router `normalizeContent`).
    fn normalize_content(content: &Value) -> Value {
        if let Some(s) = content.as_str() {
            return Value::String(s.to_string());
        }
        if content.is_null() {
            return Value::String(String::new());
        }
        let Some(blocks) = content.as_array() else {
            return Value::String(content.to_string());
        };

        let mut out_blocks: Vec<Value> = Vec::new();
        let mut text_parts: Vec<String> = Vec::new();
        let mut has_image = false;
        // Inline helper instead of a closure: the loop also pushes to
        // out_blocks / sets has_image, which a capturing closure forbids.
        macro_rules! push_text {
            ($text:expr) => {
                if !$text.is_empty() {
                    if has_image || !out_blocks.is_empty() {
                        out_blocks.push(serde_json::json!({ "type": "text", "text": $text }));
                    } else {
                        text_parts.push($text.to_string());
                    }
                }
            };
        }

        for item in blocks {
            let Some(obj) = item.as_object() else {
                continue;
            };
            let item_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
            if item_type == "image_url" {
                let url = match obj.get("image_url") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    None => String::new(),
                };
                if !url.is_empty() {
                    out_blocks.push(qoder_image_url_block(&url));
                    has_image = true;
                    continue;
                }
            } else if item_type == "image" {
                // Claude base64/url image → OpenAI image_url equivalent.
                let url = obj.get("source").and_then(|src| {
                    if src.get("type").and_then(Value::as_str) == Some("base64") {
                        src.get("data").and_then(Value::as_str).map(|data| {
                            let mime = src
                                .get("media_type")
                                .and_then(Value::as_str)
                                .unwrap_or("image/png");
                            format!("data:{mime};base64,{data}")
                        })
                    } else {
                        src.get("url").and_then(Value::as_str).map(String::from)
                    }
                });
                if let Some(u) = url {
                    if !u.is_empty() {
                        out_blocks.push(qoder_image_url_block(&u));
                        has_image = true;
                        continue;
                    }
                }
            } else if item_type == "file" {
                let name = obj
                    .get("file")
                    .and_then(|f| f.get("filename").or_else(|| f.get("name")))
                    .and_then(Value::as_str)
                    .unwrap_or("file");
                let stub = qoder_stub_text(
                    name,
                    "",
                    0,
                    "Qoder reads documents via its file API, not inlined bytes",
                );
                push_text!(stub.as_str());
                continue;
            } else if item_type == "document" {
                let name = obj
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("document");
                let stub = qoder_stub_text(
                    name,
                    "",
                    0,
                    "Qoder reads documents via its file API, not inlined bytes",
                );
                push_text!(stub.as_str());
                continue;
            }
            if let Some(text) = obj.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    push_text!(text);
                }
            }
        }

        if !has_image {
            return Value::String(text_parts.join("\n"));
        }
        if !text_parts.is_empty() {
            out_blocks.insert(
                0,
                serde_json::json!({ "type": "text", "text": text_parts.join("\n") }),
            );
        }
        Value::Array(out_blocks)
    }

    /// Get the last user message text (for chat_context).
    fn last_user_text(messages: &[Value]) -> String {
        for msg in messages.iter().rev() {
            if let Some(obj) = msg.as_object() {
                if obj.get("role").and_then(|v| v.as_str()) == Some("user") {
                    if let Some(content) = obj.get("content") {
                        if let Some(s) = content.as_str() {
                            return s.to_string();
                        }
                    }
                }
            }
        }
        String::new()
    }

    /// Truncate a string to n characters with "..." suffix.
    fn truncate(s: &str, n: usize) -> String {
        if s.len() > n {
            format!("{}...", &s[..n])
        } else {
            s.to_string()
        }
    }

    /// Compute a stable hash (first 16 hex chars of SHA-256) over the given
    /// parts separated by null bytes. Used for session_id and chat_record_id.
    fn stable_hash(prefix: &[u8], parts: &[&str]) -> String {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(prefix);
        for p in parts {
            hasher.update(b"\0");
            hasher.update(p.as_bytes());
        }
        hex::encode(hasher.finalize())[..16].to_string()
    }

    /// Fetch the live model catalog and resolve the full entry for `qoder_key`
    /// (9router getQoderModelConfig / fetchQoderCatalogRaw). The API returns
    /// `body.chat` (array of full model_config blocks); the `data`/`models`
    /// keys are a benign Rust-side extension beyond the JS (which requires
    /// `Array.isArray(body.chat)`). Returns the full catalog entry (cloned) so the
    /// chat payload can send the complete server-published `model_config`
    /// instead of a 3-field stub — sending the wrong block silently downgrades
    /// to a different model upstream. Hard error when the model is unknown
    /// after a forced refresh. A network/HTTP failure on the catalog (no
    /// catalog access) falls back to a minimal stub so the chat path does not
    /// break.
    /// Parse a catalog response into `(models summary, raw configs)`.
    /// 9router `fetchQoderCatalogRaw` requires `Array.isArray(body.chat)` and
    /// returns null otherwise; the `data` / `models` keys checked here are a
    /// benign Rust-side extension beyond the JS (harmless: upstream sends
    /// `chat`, and the fallbacks only matter for nonstandard shapes).
    /// Hidden entries (`enable: false`)
    /// are still cached — upstream accepts chat for these keys.
    pub fn parse_qoder_catalog(catalog: &Value) -> (Vec<Value>, HashMap<String, Value>) {
        let arr = catalog
            .get("chat")
            .or_else(|| catalog.get("data"))
            .or_else(|| catalog.get("models"))
            .and_then(Value::as_array);
        let mut models = Vec::new();
        let mut raw: HashMap<String, Value> = HashMap::new();
        for entry in arr.cloned().unwrap_or_default() {
            let key = entry
                .get("key")
                .and_then(Value::as_str)
                .or_else(|| entry.get("model").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            if key.is_empty() {
                continue;
            }
            raw.insert(key.clone(), entry.clone());
            if entry.get("enable").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let display = entry
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or(&key);
            let ctx = entry
                .get("max_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(131_072);
            models.push(serde_json::json!({
                "id": key,
                "name": display,
                "contextLength": ctx,
                "isVL": entry.get("is_vl").and_then(Value::as_bool).unwrap_or(false),
                "isReasoning": entry.get("is_reasoning").and_then(Value::as_bool).unwrap_or(false),
                "maxOutputTokens": entry.get("max_output_tokens").and_then(Value::as_u64).unwrap_or(0),
                "description": entry.get("description").and_then(Value::as_str).unwrap_or(""),
            }));
        }
        (models, raw)
    }

    async fn fetch_model_config(
        &self,
        qoder_key: &str,
        creds: &QoderCreds,
    ) -> Result<Value, QoderExecutorError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| QoderExecutorError::MissingCredentials(e.to_string()))?;
        // The model-list GET is signed with an empty body against the
        // model-list URL (9router signs the catalog request the same way).
        let cosy = Self::build_cosy_headers(b"", QODER_MODEL_LIST_URL, creds)?;
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("Authorization", &cosy.authorization),
            ("Cosy-Key", &cosy.cosy_key),
            ("Cosy-User", &cosy.cosy_user),
            ("Cosy-Date", &cosy.cosy_date),
            ("Cosy-Version", &cosy.cosy_version),
            ("Cosy-Machineid", &cosy.cosy_machineid),
            ("Cosy-Machinetoken", &cosy.cosy_machinetoken),
            ("Cosy-Machinetype", &cosy.cosy_machinetype),
            ("Cosy-Machineos", &cosy.cosy_machineos),
            ("Cosy-Clienttype", &cosy.cosy_clienttype),
            ("Cosy-Clientip", &cosy.cosy_clientip),
            ("Cosy-Bodyhash", &cosy.cosy_bodyhash),
            ("Cosy-Bodylength", &cosy.cosy_bodylength),
            ("Cosy-Sigpath", &cosy.cosy_sigpath),
            ("Cosy-Data-Policy", &cosy.cosy_data_policy),
            ("Cosy-Organization-Id", &cosy.cosy_organization_id),
            ("Cosy-Organization-Tags", &cosy.cosy_organization_tags),
            ("Login-Version", &cosy.login_version),
            ("X-Request-Id", &cosy.x_request_id),
        ] {
            headers.insert(
                name,
                HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }

        let response = client
            .get(QODER_MODEL_LIST_URL)
            .headers(headers)
            .send()
            .await;
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(target: "openproxy::executor", "qoder model/list fetch failed: {e}");
                return Ok(stub_model_config(qoder_key));
            }
        };
        if !response.status().is_success() {
            tracing::warn!(
                target: "openproxy::executor",
                "qoder model/list returned HTTP {}",
                response.status().as_u16()
            );
            return Ok(stub_model_config(qoder_key));
        }
        let catalog: Value = response.json().await.map_err(|e| {
            QoderExecutorError::MissingCredentials(format!("qoder model/list JSON error: {e}"))
        })?;
        let (_, raw) = Self::parse_qoder_catalog(&catalog);
        match raw.get(qoder_key) {
            Some(entry) => Ok(entry.clone()),
            None => Err(QoderExecutorError::MissingCredentials(format!(
                "qoder: model_config for \"{qoder_key}\" not yet known"
            ))),
        }
    }

    /// Map the OpenAI-style request body into the exact shape Qoder expects.
    /// `model_entry` is the FULL live catalog entry (9router sends the whole
    /// `model_config` block — a 3-field stub silently downgrades the upstream
    /// model). `max_tokens` prefers the caller's cap like JS
    /// (`Math.min(body.max_tokens, modelConfig.max_output_tokens)`).
    fn transform_request(
        &self,
        body: &Value,
        model: &str,
        credentials: &ProviderConnection,
        model_entry: &Value,
    ) -> Result<Value, QoderExecutorError> {
        // Strip "qoder/" prefix if present
        let qoder_key = model.strip_prefix("qoder/").unwrap_or(model);

        let messages = body
            .get("messages")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let (normalized_msgs, system_text) = Self::normalize_messages(&messages);
        let last_user = Self::last_user_text(&messages);

        let psd = &credentials.provider_specific_data;
        let user_id = psd.get("userId").and_then(|v| v.as_str()).unwrap_or("");

        // Stable session ID from user + model
        let session_id = Self::stable_hash(b"qoder-session", &[user_id, qoder_key]);

        // Stable chat record ID
        let record_id = {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(b"qoder-record\0");
            hasher.update(qoder_key.as_bytes());
            for m in &normalized_msgs {
                if let Some(obj) = m.as_object() {
                    if let Some(role) = obj.get("role").and_then(|v| v.as_str()) {
                        hasher.update(b"\0");
                        hasher.update(role.as_bytes());
                    }
                    if let Some(content) = obj.get("content").and_then(|v| v.as_str()) {
                        if !content.is_empty() {
                            hasher.update(b"\0");
                            hasher.update(content.as_bytes());
                        }
                    }
                }
            }
            let max_tokens = body
                .get("max_tokens")
                .or_else(|| body.get("max_completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(32768);
            hasher.update(format!("\0mt={}", max_tokens).as_bytes());
            hex::encode(hasher.finalize())[..16].to_string()
        };

        // Max output tokens: model default, capped by the caller's request.
        let max_output = model_entry
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .unwrap_or(32_768);
        let mut max_tokens = max_output;
        for key in ["max_tokens", "max_completion_tokens"] {
            if let Some(want) = body.get(key).and_then(Value::as_u64) {
                if want > 0 && want < max_tokens {
                    max_tokens = want;
                }
            }
        }

        let tools = body.get("tools").cloned().unwrap_or(Value::Array(vec![]));

        // 9router: send the FULL catalog entry as model_config (+ key aligned
        // to the requested alias), not a 3-field stub.
        let mut model_config = model_entry.clone();
        if let Some(obj) = model_config.as_object_mut() {
            obj.insert("key".to_string(), Value::String(qoder_key.to_string()));
        }
        let is_reasoning = model_entry
            .get("is_reasoning")
            .or_else(|| model_entry.get("reasoning"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let model_source = model_entry
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("system")
            .to_string();

        let mut payload = serde_json::json!({
            "request_id": Uuid::new_v4().to_string(),
            "request_set_id": record_id,
            "chat_record_id": record_id,
            "session_id": session_id,
            "stream": true,
            "chat_task": "FREE_INPUT",
            "is_reply": true,
            "is_retry": false,
            "source": 1,
            "version": "3",
            "session_type": "qodercli",
            "agent_id": "agent_common",
            "task_id": "common",
            "code_language": "",
            "chat_prompt": "",
            "image_urls": null,
            "aliyun_user_type": "",
            "system": system_text,
            "messages": normalized_msgs,
            "tools": tools,
            "parameters": {
                "max_tokens": max_tokens
            },
            "model_config": model_config,
            "chat_context": {
                "chatPrompt": "",
                "imageUrls": null,
                "extra": {
                    "context": [],
                    "modelConfig": {
                        "key": qoder_key,
                        "is_reasoning": is_reasoning
                    },
                    "originalContent": last_user
                },
                "features": [],
                "text": last_user
            },
            "business": {
                "product": "cli",
                "version": "1.0.0",
                "type": "agent",
                "stage": "start",
                "id": Uuid::new_v4().to_string(),
                "name": Self::truncate(&last_user, 30),
                "begin_at": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            }
        });
        // Context-window tier escalation (9router contextTier.js): applied
        // between payload build and encode. Pure + in-process (no I/O).
        if let Some(tier) = resolve_qoder_context_tier(
            &model_config,
            &system_text,
            &payload["messages"],
            payload.get("tools"),
        ) {
            apply_qoder_context_tier(&mut payload, &tier);
        }
        Ok(payload)
    }

    // -----------------------------------------------------------------------
    // Execute
    // -----------------------------------------------------------------------

    pub async fn execute_request(
        &self,
        mut request: QoderExecutionRequest,
    ) -> Result<QoderExecutorResponse, QoderExecutorError> {
        // PAT (pt-...) → exchange for a short-lived job token + resolve userId
        // so downstream COSY signing + catalog fetch work. Device tokens
        // (dt-...) and job tokens (jt-...) skip this and are used directly.
        // Ported from 9router v0.5.45 fix(qoder): support PAT auth.
        let raw_token = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("")
            .to_string();
        if raw_token.starts_with("pt-") {
            match resolve_pat_credential(&raw_token).await {
                Ok((job_token, user_id)) => {
                    request.credentials.access_token = Some(job_token);
                    request.credentials.api_key = None;
                    request
                        .credentials
                        .provider_specific_data
                        .insert("userId".to_string(), Value::String(user_id));
                    request
                        .credentials
                        .provider_specific_data
                        .insert("authMethod".to_string(), Value::String("pat".to_string()));
                }
                Err(e) => {
                    return Err(QoderExecutorError::MissingCredentials(format!(
                        "qoder PAT exchange failed: {e}"
                    )));
                }
            }
        }

        let psd = &request.credentials.provider_specific_data;
        let user_id = psd
            .get("userId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let machine_id = psd
            .get("machineId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if user_id.is_empty() {
            return Err(QoderExecutorError::MissingCredentials(
                "qoder credential is missing userId; reconnect the account".into(),
            ));
        }

        let access_token = request
            .credentials
            .access_token
            .as_deref()
            .unwrap_or("")
            .to_string();
        if access_token.is_empty() {
            return Err(QoderExecutorError::MissingCredentials(
                "qoder credential is missing accessToken; reconnect the account".into(),
            ));
        }

        let creds = QoderCreds {
            user_id,
            auth_token: access_token,
            name: request.credentials.display_name.clone().unwrap_or_default(),
            email: request.credentials.email.clone().unwrap_or_default(),
            machine_id,
        };

        let url = self.build_url(&request.credentials);

        // 9router buildUrl: jt- tokens route to api2.
        let qoder_key = request
            .model
            .strip_prefix("qoder/")
            .unwrap_or(&request.model)
            .to_string();

        // Live model config → FULL catalog entry (9router getQoderModelConfig).
        let model_entry = match self.fetch_model_config(&qoder_key, &creds).await {
            Ok(cfg) => cfg,
            Err(e) => {
                // Unknown model after refresh is a hard error (JS throws).
                return Err(e);
            }
        };
        let model_source = model_entry
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("system")
            .to_string();

        let client = self.pool.get("qoder", request.proxy.as_ref())?;
        let inference_base = Self::inference_base(&request.credentials);

        // Attachment rewrite BEFORE payload build (9router
        // rewriteQoderMessageAttachments in buildQoderRequestBody): upload
        // images, stub huge files. Best-effort — failures keep going.
        let mut openai_body = request.body.clone();
        if let Some(msgs) = openai_body
            .get_mut("messages")
            .and_then(|v| v.as_array_mut())
        {
            let _ = rewrite_qoder_message_attachments(msgs, &client, &inference_base, &creds).await;
        }

        // Transform the OpenAI-compatible body into Qoder's format
        let transformed_body = self.transform_request(
            &openai_body,
            &request.model,
            &request.credentials,
            &model_entry,
        )?;

        // Encode body with Qoder's WAF-bypass scheme
        let plain_body = serde_json::to_vec(&transformed_body)?;
        let encoded_body_str = Self::qoder_encode_body(&plain_body);
        let encoded_body = encoded_body_str.as_bytes();

        // Build COSY-signed headers from the *encoded* body
        let headers = self.build_headers(encoded_body, &url, &creds, &qoder_key, &model_source)?;

        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(encoded_body.to_vec())
            .send()
            .await?;

        // Peek the first SSE frame for billing blocks (9router
        // peekFirstQoderFrame): return a real 403 before streaming so the
        // combo dispatcher marks the connection unavailable and falls over.
        // Normal frames are re-attached so nothing is dropped.
        if response.status().is_success() {
            match Self::peek_first_frame(response, &url).await {
                QoderPeek::Billing { response } => {
                    return Ok(QoderExecutorResponse {
                        response: UpstreamResponse::Reqwest(response),
                        url,
                        headers,
                        transformed_body,
                        transport: TransportKind::Reqwest,
                    });
                }
                QoderPeek::Passthrough { response } => {
                    return Ok(QoderExecutorResponse {
                        response: UpstreamResponse::Reqwest(response),
                        url,
                        headers,
                        transformed_body,
                        transport: TransportKind::Reqwest,
                    });
                }
            }
        }

        Ok(QoderExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    /// Inference host for this credential (api2 for jt-, api3 otherwise).
    /// Public so the connection-test probe uses the same host the executor
    /// dials. Qoder serves two hosts and the choice depends on the token
    /// prefix, so re-deriving it here would drift.
    pub fn inference_base(credentials: &ProviderConnection) -> String {
        let raw = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .unwrap_or("");
        if !raw.starts_with("pt-")
            && (raw.starts_with("jt-")
                || credentials
                    .access_token
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("jt-"))
        {
            "https://api2.qoder.sh".to_string()
        } else {
            "https://api3.qoder.sh".to_string()
        }
    }

    /// Peek the first SSE `data:` line of a successful chat response.
    /// Billing block → synthetic 403 JSON response (combo fallback).
    /// Anything else → the original response rebuilt with the consumed bytes
    /// prepended so the stream loses nothing (9router `consumed` re-process).
    async fn peek_first_frame(response: reqwest::Response, url: &str) -> QoderPeek {
        use futures_util::StreamExt;
        let status = response.status();
        let resp_headers = response.headers().clone();
        let _ = url;
        let mut stream = response.bytes_stream();
        let mut buffered: Vec<u8> = Vec::new();
        // Read until the first full line (or EOF / timeout).
        let first_line: Option<String> = loop {
            if let Some(nl) = buffered.iter().position(|&b| b == b'\n') {
                let line = String::from_utf8_lossy(&buffered[..nl]).to_string();
                break Some(line);
            }
            let next =
                tokio::time::timeout(std::time::Duration::from_secs(30), stream.next()).await;
            match next {
                Ok(Some(Ok(chunk))) => buffered.extend_from_slice(&chunk),
                _ => break None,
            }
        };
        // Rebuild the stream: buffered bytes first, then the remainder.
        let rebuild = |prefix: Vec<u8>,
                       rest: futures_util::stream::BoxStream<
            'static,
            Result<bytes::Bytes, reqwest::Error>,
        >| {
            let prefix_stream = futures_util::stream::once(async move {
                Ok::<bytes::Bytes, reqwest::Error>(bytes::Bytes::from(prefix))
            });
            let combined = prefix_stream.chain(rest);
            let body = reqwest::Body::wrap_stream(combined);
            let mut builder = http::Response::builder().status(status);
            for (k, v) in resp_headers.iter() {
                builder = builder.header(k, v);
            }
            let http_resp = builder.body(body).unwrap();
            reqwest::Response::from(http_resp)
        };
        if let Some(line) = first_line {
            let trimmed = line.trim_end_matches('\r').trim().to_string();
            if trimmed.starts_with("data:") {
                if let Some(err_msg) = check_billing_in_sse_line(&trimmed)
                    .and_then(|f| serde_json::from_str::<Value>(&f["data: ".len()..]).ok())
                    .and_then(|v| v.get("message").and_then(Value::as_str).map(String::from))
                {
                    let body = serde_json::json!({ "error": { "message": err_msg, "code": 403 } })
                        .to_string();
                    let http_resp = http::Response::builder()
                        .status(403)
                        .header("Content-Type", "application/json")
                        .body(reqwest::Body::from(body))
                        .unwrap();
                    return QoderPeek::Billing {
                        response: reqwest::Response::from(http_resp),
                    };
                }
                // Also detect via the raw envelope when the helper shape differs.
                let payload = trimmed["data:".len()..].trim();
                if payload != "[DONE]" {
                    if let Ok(envelope) = serde_json::from_str::<Value>(payload) {
                        let status_val = envelope
                            .get("statusCodeValue")
                            .and_then(Value::as_u64)
                            .unwrap_or(200);
                        let inner = envelope.get("body").and_then(Value::as_str).unwrap_or("");
                        if status_val != 200 && detect_qoder_billing_block(payload).is_some() {
                            let body = serde_json::json!({ "error": { "message": inner, "code": status_val } }).to_string();
                            let http_resp = http::Response::builder()
                                .status(403)
                                .header("Content-Type", "application/json")
                                .body(reqwest::Body::from(body))
                                .unwrap();
                            return QoderPeek::Billing {
                                response: reqwest::Response::from(http_resp),
                            };
                        }
                    }
                }
            }
            let rest = stream.boxed();
            return QoderPeek::Passthrough {
                response: rebuild(buffered, rest),
            };
        }
        let rest = stream.boxed();
        QoderPeek::Passthrough {
            response: rebuild(buffered, rest),
        }
    }

    /// Normalize Qoder/OpenAI usage into the shape stream consumers
    /// understand (9router `canonicalizeQoderUsage` in sse.js).
    pub fn canonicalize_qoder_usage(usage: &Value) -> Option<Value> {
        let obj = usage.as_object()?;
        let num = |v: Option<&Value>| v.and_then(Value::as_f64).filter(|n| n.is_finite());
        let prompt = num(obj.get("prompt_tokens")).or_else(|| num(obj.get("input_tokens")));
        let completion =
            num(obj.get("completion_tokens")).or_else(|| num(obj.get("output_tokens")));
        if prompt.is_none() && completion.is_none() {
            return None;
        }
        let mut details = obj
            .get("prompt_tokens_details")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let cached = num(details.get("cached_tokens"))
            .or_else(|| num(obj.get("cached_tokens")))
            .or_else(|| num(obj.get("prompt_cache_hit_tokens")))
            .or_else(|| num(obj.get("cache_read_input_tokens")));
        let cache_creation = num(details.get("cache_creation_tokens"))
            .or_else(|| num(obj.get("cache_creation_input_tokens")));
        let prompt_tokens = prompt.unwrap_or(0.0) as u64;
        let completion_tokens = completion.unwrap_or(0.0) as u64;
        let total = num(obj.get("total_tokens"))
            .map(|n| n as u64)
            .unwrap_or(prompt_tokens + completion_tokens);
        let mut out = serde_json::json!({
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total,
        });
        if let Some(c) = cached {
            out["cached_tokens"] = Value::from(c as u64);
            details.insert("cached_tokens".to_string(), Value::from(c as u64));
        }
        if let Some(cc) = cache_creation {
            details.insert("cache_creation_tokens".to_string(), Value::from(cc as u64));
        }
        if !details.is_empty() {
            out["prompt_tokens_details"] = Value::Object(details);
        }
        if let Some(ctd) = obj.get("completion_tokens_details") {
            if ctd.is_object() {
                out["completion_tokens_details"] = ctd.clone();
            }
        }
        let reasoning = num(obj.get("reasoning_tokens")).or_else(|| {
            obj.get("completion_tokens_details")
                .and_then(|d| num(d.get("reasoning_tokens")))
        });
        if let Some(r) = reasoning {
            out["reasoning_tokens"] = Value::from(r as u64);
        }
        Some(out)
    }

    /// Unwrap one SSE line's `{statusCodeValue, body}` envelope into the inner
    /// body string (9router wrapQoderSSE line handling, without the coalescer).
    /// Returns `None` for non-`data:` lines; `Some("[DONE]")` for terminal
    /// frames; `Some(inner)` otherwise (embedded newlines stripped). Non-200
    /// statuses become the `\n[qoder error {status}: ...]` marker text.
    pub fn unwrap_qoder_envelope(line: &str) -> Option<String> {
        let line = line.trim_end();
        if !line.starts_with("data:") {
            return None;
        }
        let payload = line.trim_start_matches("data:").trim();
        if payload.is_empty() || payload == "[DONE]" {
            return Some(payload.to_string());
        }
        let envelope: Value = serde_json::from_str(payload).ok()?;
        let status = envelope.get("statusCodeValue").and_then(Value::as_u64);
        let inner = envelope
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match status {
            Some(s) if s != 200 => {
                let msg: String = inner
                    .chars()
                    .take(200)
                    .collect::<String>()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                Some(format!("\n[qoder error {s}: {msg}]"))
            }
            _ => {
                if inner == "[DONE]" {
                    return Some("[DONE]".to_string());
                }
                Some(inner.replace(['\n', '\r'], ""))
            }
        }
    }

    /// Unwrap Qoder's `{statusCodeValue, body}` SSE envelope into OpenAI-style
    /// chunks (9router wrapQoderSSE). Pure per-line function — returns
    /// `Some(frame)` to emit or `None` for lines that should be dropped
    /// (keepalives / terminal frames).
    ///
    /// - non-200 status → error chunk `\n[qoder error {status}: {truncated}]\n\n`
    ///   (truncated to 200 chars) followed by `data: [DONE]`
    /// - inner `[DONE]` → `data: [DONE]`
    /// - else → `data: {inner}\n\n` with embedded newlines stripped so the
    ///   SSE frame stays one event.
    pub fn wrap_qoder_sse_line(line: &str) -> Option<String> {
        let line = line.trim_end();
        if !line.starts_with("data:") {
            return None;
        }
        let payload = line.trim_start_matches("data:").trim();
        if payload.is_empty() || payload == "[DONE]" {
            return Some(format!("data: {payload}\n\n"));
        }
        let envelope: Value = serde_json::from_str(payload).ok()?;
        let status = envelope.get("statusCodeValue").and_then(Value::as_u64);
        let inner = envelope
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match status {
            Some(s) if s != 200 => {
                let msg: String = inner
                    .chars()
                    .take(200)
                    .collect::<String>()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let mut out = String::new();
                out.push_str(&format!("\n[qoder error {s}: {msg}]\n\n"));
                out.push_str("data: [DONE]\n\n");
                Some(out)
            }
            _ => {
                if inner == "[DONE]" {
                    return Some("data: [DONE]\n\n".to_string());
                }
                let stripped = inner.replace(['\n', '\r'], "");
                Some(format!("data: {stripped}\n\n"))
            }
        }
    }
}

/// SSE usage coalescer state machine (9router `createQoderSseCoalescer` in
/// sse.js). Qoder sends usage on a later `choices: []` frame — after
/// finish_reason, which often lives on `delta.finish_reason`. Hold empty
/// finish + usage-only frames, then emit one OpenAI include_usage-style
/// chunk `{choices:[{delta:{}, finish_reason}], usage}`.
pub struct QoderSseCoalescer {
    model: String,
    pending_finish: Option<String>,
    pending_usage: Option<Value>,
    last_id: Option<String>,
    last_created: Option<u64>,
    last_model: Option<String>,
    done_emitted: bool,
    finish_forwarded: bool,
}

impl QoderSseCoalescer {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            pending_finish: None,
            pending_usage: None,
            last_id: None,
            last_created: None,
            last_model: None,
            done_emitted: false,
            finish_forwarded: false,
        }
    }

    pub fn done_emitted(&self) -> bool {
        self.done_emitted
    }

    fn finish_reason_of(parsed: &Value) -> Option<String> {
        if let Some(choice) = parsed
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
        {
            if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
                return Some(f.to_string());
            }
            if let Some(f) = choice
                .get("delta")
                .and_then(|d| d.get("finish_reason"))
                .and_then(Value::as_str)
            {
                return Some(f.to_string());
            }
        }
        parsed
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(String::from)
    }

    fn has_valuable_delta(parsed: &Value) -> bool {
        let Some(delta) = parsed
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("delta"))
        else {
            return false;
        };
        if delta
            .get("content")
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            == Some(true)
        {
            return true;
        }
        if delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            == Some(true)
        {
            return true;
        }
        if delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|a| !a.is_empty())
            == Some(true)
        {
            return true;
        }
        if delta.get("role").is_some() {
            return true;
        }
        false
    }

    fn terminal_frame(&mut self) -> Option<String> {
        if self.pending_finish.is_none() && self.pending_usage.is_none() {
            return None;
        }
        let finish = self
            .pending_finish
            .clone()
            .unwrap_or_else(|| "stop".to_string());
        let mut obj = serde_json::json!({
            "id": self.last_id.clone().unwrap_or_else(|| format!("qoder-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis())),
            "object": "chat.completion.chunk",
            "created": self.last_created.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
            "model": self.last_model.clone().unwrap_or_else(|| self.model.clone()),
            "choices": [{ "index": 0, "delta": {}, "finish_reason": finish }],
        });
        if let Some(usage) = self.pending_usage.clone() {
            obj["usage"] = usage;
        }
        self.pending_finish = None;
        self.pending_usage = None;
        Some(format!(
            "data: {}\n\n",
            obj.to_string().replace(['\n', '\r'], "")
        ))
    }

    /// Feed one unwrapped inner body (already extracted from the envelope).
    /// Returns frames to emit; `terminal` signals the caller to close with
    /// `data: [DONE]` and stop reading upstream (keepalive drop).
    pub fn handle_inner(&mut self, inner: &str) -> (Vec<String>, bool) {
        if self.done_emitted {
            return (vec![], true);
        }
        if inner == "[DONE]" {
            let mut out = Vec::new();
            if self.pending_usage.is_some()
                || (self.pending_finish.is_some() && !self.finish_forwarded)
            {
                if let Some(t) = self.terminal_frame() {
                    out.push(t);
                }
            }
            self.done_emitted = true;
            return (out, true);
        }
        let parsed: Value = match serde_json::from_str(inner) {
            Ok(v) => v,
            Err(_) => return (vec![inner.replace(['\n', '\r'], "")], false),
        };
        if !parsed.is_object() {
            return (vec![], false);
        }
        if let Some(id) = parsed.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                self.last_id = Some(id.to_string());
            }
        }
        if let Some(c) = parsed.get("created").and_then(Value::as_u64) {
            self.last_created = Some(c);
        }
        if let Some(m) = parsed.get("model").and_then(Value::as_str) {
            if !m.is_empty() {
                self.last_model = Some(m.to_string());
            }
        }
        if let Some(usage) = parsed.get("usage") {
            if let Some(canonical) = QoderExecutor::canonicalize_qoder_usage(usage) {
                self.pending_usage = Some(canonical);
            }
        }
        let finish = Self::finish_reason_of(&parsed);
        if Self::has_valuable_delta(&parsed) {
            let mut out = vec![format!("data: {}\n\n", inner.replace(['\n', '\r'], ""))];
            if let Some(f) = finish {
                self.finish_forwarded = true;
                self.pending_finish = if self.pending_usage.is_some() {
                    Some(f)
                } else {
                    None
                };
            }
            if self.pending_finish.is_some() && self.pending_usage.is_some() {
                if let Some(t) = self.terminal_frame() {
                    out.push(t);
                }
                self.done_emitted = true;
                return (out, true);
            }
            return (out, false);
        }
        if let Some(f) = finish.clone() {
            self.pending_finish = Some(f);
        }
        if (self.pending_finish.is_some() || self.finish_forwarded) && self.pending_usage.is_some()
        {
            if self.pending_finish.is_none() {
                self.pending_finish = Some("stop".to_string());
            }
            if let Some(t) = self.terminal_frame() {
                self.done_emitted = true;
                return (vec![t], true);
            }
        }
        (vec![], false)
    }

    /// Flush at end-of-stream: terminal frame if usage/finish is still held.
    pub fn flush(&mut self) -> Option<String> {
        if self.done_emitted {
            return None;
        }
        if self.pending_usage.is_some() || (self.pending_finish.is_some() && !self.finish_forwarded)
        {
            self.terminal_frame()
        } else {
            None
        }
    }
}

/// Outcome of peeking the first SSE frame of a Qoder response.
enum QoderPeek {
    /// Billing block → synthetic 403 JSON response (combo fallback).
    Billing { response: reqwest::Response },
    /// Normal → original response rebuilt with consumed bytes prepended.
    Passthrough { response: reqwest::Response },
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Credential fields needed for COSY signing.
struct QoderCreds {
    user_id: String,
    auth_token: String,
    name: String,
    email: String,
    machine_id: String,
}

/// All 17+ COSY headers ready to insert into the request.
struct CosyHeaders {
    authorization: String,
    cosy_key: String,
    cosy_user: String,
    cosy_date: String,
    cosy_version: String,
    cosy_machineid: String,
    cosy_machinetoken: String,
    cosy_machinetype: String,
    cosy_machineos: String,
    cosy_clienttype: String,
    cosy_clientip: String,
    cosy_bodyhash: String,
    cosy_bodylength: String,
    cosy_sigpath: String,
    cosy_data_policy: String,
    cosy_organization_id: String,
    cosy_organization_tags: String,
    login_version: String,
    x_request_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qoder_encode_body_empty() {
        assert_eq!(QoderExecutor::qoder_encode_body(b""), "");
    }

    #[test]
    fn test_qoder_encode_body_hello() {
        let encoded = QoderExecutor::qoder_encode_body(b"Hello, World!");
        // Should produce a non-empty string that is NOT standard base64
        assert!(!encoded.is_empty());
        // Verify it differs from standard base64
        let std_b64 = B64.encode(b"Hello, World!");
        assert_ne!(encoded, std_b64);
    }

    #[test]
    fn test_qoder_encode_roundtrip_structure() {
        // The encoding is deterministic and reversible on the server side.
        // Just verify it doesn't panic and produces output.
        let input = serde_json::json!({
            "messages": [{"role": "user", "content": "test"}],
            "stream": true
        });
        let body = serde_json::to_vec(&input).unwrap();
        let encoded = QoderExecutor::qoder_encode_body(&body);
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_normalize_messages_extracts_system() {
        let messages = vec![
            serde_json::json!({"role": "system", "content": "You are helpful."}),
            serde_json::json!({"role": "user", "content": "Hello"}),
            serde_json::json!({"role": "assistant", "content": "Hi!"}),
        ];
        let (normalized, system_text) = QoderExecutor::normalize_messages(&messages);
        assert_eq!(system_text, "You are helpful.");
        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized[0]["role"], "user");
        assert_eq!(normalized[1]["role"], "assistant");
    }

    #[test]
    fn test_normalize_messages_no_system() {
        let messages = vec![serde_json::json!({"role": "user", "content": "Hello"})];
        let (normalized, system_text) = QoderExecutor::normalize_messages(&messages);
        assert_eq!(system_text, "");
        assert_eq!(normalized.len(), 1);
    }

    #[test]
    fn test_normalize_messages_multipart_content() {
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "Part 1"},
                {"type": "text", "text": "Part 2"}
            ]
        })];
        let (normalized, _) = QoderExecutor::normalize_messages(&messages);
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0]["content"], "Part 1\nPart 2");
    }

    #[test]
    fn test_compute_sig_path() {
        assert_eq!(
            QoderExecutor::compute_sig_path(
                "https://api3.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result"
            ),
            "/api/v2/service/pro/sse/agent_chat_generation"
        );
    }

    #[test]
    fn test_compute_sig_path_no_algo_prefix() {
        assert_eq!(
            QoderExecutor::compute_sig_path("https://example.com/api/test"),
            "/api/test"
        );
    }

    #[test]
    fn test_md5_hex() {
        let hash = QoderExecutor::md5_hex(b"");
        assert_eq!(hash, "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(QoderExecutor::truncate("hello", 10), "hello");
        assert_eq!(
            QoderExecutor::truncate("hello world this is long", 8),
            "hello wo..."
        );
    }

    #[test]
    fn test_last_user_text() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "first"}),
            serde_json::json!({"role": "assistant", "content": "reply"}),
            serde_json::json!({"role": "user", "content": "second"}),
        ];
        assert_eq!(QoderExecutor::last_user_text(&messages), "second");
    }

    #[test]
    fn test_last_user_text_empty() {
        let messages = vec![serde_json::json!({"role": "assistant", "content": "hi"})];
        assert_eq!(QoderExecutor::last_user_text(&messages), "");
    }

    #[test]
    fn test_aes_cbc_encrypt_base64() {
        let key = "1234567890abcdef";
        let plaintext = b"hello world";
        let result = QoderExecutor::aes_cbc_encrypt_base64(plaintext, key);
        assert!(result.is_ok());
        let encrypted = result.unwrap();
        // Should be valid base64
        assert!(B64.decode(&encrypted).is_ok());
    }

    #[test]
    fn test_aes_cbc_encrypt_wrong_key_length() {
        let result = QoderExecutor::aes_cbc_encrypt_base64(b"test", "short");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_text_string() {
        let content = Value::String("hello".to_string());
        assert_eq!(QoderExecutor::extract_text(&content), "hello");
    }

    #[test]
    fn test_extract_text_array() {
        let content = serde_json::json!([
            {"type": "text", "text": "part1"},
            {"type": "text", "text": "part2"}
        ]);
        assert_eq!(QoderExecutor::extract_text(&content), "part1\npart2");
    }

    #[test]
    fn test_extract_text_null() {
        assert_eq!(QoderExecutor::extract_text(&Value::Null), "");
    }

    #[test]
    fn test_stable_hash() {
        let h1 = QoderExecutor::stable_hash(b"prefix", &["a", "b"]);
        let h2 = QoderExecutor::stable_hash(b"prefix", &["a", "b"]);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn test_stable_hash_different_inputs() {
        let h1 = QoderExecutor::stable_hash(b"prefix", &["a"]);
        let h2 = QoderExecutor::stable_hash(b"prefix", &["b"]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_build_url_jt_token_uses_api2() {
        let executor = QoderExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let mut creds = ProviderConnection::default();
        creds.api_key = Some("jt-abc".to_string());
        let url = executor.build_url(&creds);
        assert!(
            url.starts_with("https://api2.qoder.sh"),
            "jt- token must route to api2, got: {url}"
        );

        creds.api_key = Some("dt-abc".to_string());
        let url = executor.build_url(&creds);
        assert!(
            url.starts_with("https://api3.qoder.sh"),
            "non-jt token must use api3, got: {url}"
        );

        // pt- tokens never use api2 even though they start with 't-'.
        creds.api_key = Some("pt-abc".to_string());
        let url = executor.build_url(&creds);
        assert!(url.starts_with("https://api3.qoder.sh"), "got: {url}");

        // access_token fallback
        creds.api_key = None;
        creds.access_token = Some("jt-tok".to_string());
        let url = executor.build_url(&creds);
        assert!(url.starts_with("https://api2.qoder.sh"), "got: {url}");
    }

    #[test]
    fn test_wrap_qoder_sse_unwraps_envelope() {
        // Guard test: input envelope → unwrapped OpenAI chunk.
        let line = r#"data: {"statusCodeValue":200,"body":"{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}"}"#;
        let out = QoderExecutor::wrap_qoder_sse_line(line).unwrap();
        assert_eq!(
            out,
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"
        );

        // Inner [DONE] → [DONE] frame.
        let line = r#"data: {"statusCodeValue":200,"body":"[DONE]"}"#;
        let out = QoderExecutor::wrap_qoder_sse_line(line).unwrap();
        assert_eq!(out, "data: [DONE]\n\n");

        // Non-200 → error chunk + [DONE].
        let line = r#"data: {"statusCodeValue":500,"body":"{\"error\":\"boom\"}"}"#;
        let out = QoderExecutor::wrap_qoder_sse_line(line).unwrap();
        assert!(out.contains("[qoder error 500"), "got: {out}");
        assert!(out.ends_with("data: [DONE]\n\n"), "got: {out}");

        // Embedded newlines in the inner body are stripped so the frame stays
        // one SSE event.
        let line = r#"data: {"statusCodeValue":200,"body":"line1\nline2"}"#;
        let out = QoderExecutor::wrap_qoder_sse_line(line).unwrap();
        assert!(
            !out.contains('\n') || out == "data: line1line2\n\n",
            "got: {out:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Billing block detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_detect_billing_block_code_112() {
        // 9router qoder.js:376 requires statusCodeValue !== 200.
        let body =
            r#"{"statusCodeValue":403,"body":"{\"code\":112,\"message\":\"Quota exhausted\"}"}"#;
        let result = detect_qoder_billing_block(body);
        assert!(
            result.is_some(),
            "code 112 should be detected as billing block"
        );
        assert!(result.unwrap().contains("112"));
    }

    #[test]
    fn test_detect_billing_block_code_10605() {
        let body =
            r#"{"statusCodeValue":429,"body":"{\"code\":10605,\"message\":\"Queue throttle\"}"}"#;
        let result = detect_qoder_billing_block(body);
        assert!(
            result.is_some(),
            "code 10605 should be detected as billing block"
        );
    }

    #[test]
    fn test_detect_billing_block_pricing_url() {
        let body = r#"{"statusCodeValue":403,"body":"{\"pricingUrl\":\"https://qoder.sh/pricing\",\"message\":\"Upgrade required\"}"}"#;
        let result = detect_qoder_billing_block(body);
        assert!(
            result.is_some(),
            "pricingUrl should be detected as billing block"
        );
        assert!(result.unwrap().contains("pricingUrl"));
    }

    #[test]
    fn test_detect_billing_block_pricing_url_case_insensitive() {
        // 9router checks lowerMsg.includes("pricingurl") — mixed case must fire.
        let body =
            r#"{"statusCodeValue":403,"body":"{\"PricingURL\":\"https://qoder.sh/pricing\"}"}"#;
        let result = detect_qoder_billing_block(body);
        assert!(
            result.is_some(),
            "mixed-case PricingURL should be detected as billing block"
        );
    }

    #[test]
    fn test_detect_billing_block_200_with_billing_code_is_not_billing() {
        // A normal 200 chunk merely mentioning "code":"112" (e.g. model
        // output text echoed in the inner body) must NOT be flagged —
        // 9router qoder.js:376 gates on statusVal !== 200.
        let body = r#"{"statusCodeValue":200,"body":"{\"code\":\"112\",\"message\":\"Quota exhausted\"}"}"#;
        let result = detect_qoder_billing_block(body);
        assert!(
            result.is_none(),
            "200-status frame with billing code must not be flagged"
        );
    }

    #[test]
    fn test_detect_billing_block_normal_response() {
        let body =
            r#"{"statusCodeValue":200,"body":"{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}"}"#;
        let result = detect_qoder_billing_block(body);
        assert!(
            result.is_none(),
            "normal response should not be billing block"
        );
    }

    #[test]
    fn test_detect_billing_block_error_non_billing() {
        let body = r#"{"statusCodeValue":500,"body":"{\"error\":\"internal server error\"}"}"#;
        let result = detect_qoder_billing_block(body);
        assert!(result.is_none(), "non-billing error should not be detected");
    }

    #[test]
    fn test_check_billing_in_sse_line_billing() {
        let line = r#"data: {"statusCodeValue":403,"body":"{\"code\":112,\"message\":\"Quota exhausted\"}"}"#;
        let result = check_billing_in_sse_line(line);
        assert!(result.is_some(), "billing SSE line should be detected");
        let err_frame = result.unwrap();
        assert!(err_frame.contains("403"), "error frame should contain 403");
        assert!(
            err_frame.contains("billing"),
            "error frame should mention billing"
        );
    }

    #[test]
    fn test_check_billing_in_sse_line_200_with_code_is_not_billing() {
        // 200-status frame containing "code":"112" must NOT be flagged.
        let line =
            r#"data: {"statusCodeValue":200,"body":"{\"code\":\"112\",\"message\":\"hi\"}"}"#;
        let result = check_billing_in_sse_line(line);
        assert!(
            result.is_none(),
            "200-status SSE line must not trigger billing"
        );
    }

    #[test]
    fn test_check_billing_in_sse_line_normal() {
        let line = r#"data: {"statusCodeValue":200,"body":"{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}"}"#;
        let result = check_billing_in_sse_line(line);
        assert!(
            result.is_none(),
            "normal SSE line should not trigger billing"
        );
    }

    #[test]
    fn test_check_billing_in_sse_line_non_data() {
        let line = ": keepalive";
        let result = check_billing_in_sse_line(line);
        assert!(result.is_none(), "non-data line should be ignored");
    }

    // -----------------------------------------------------------------------
    // Gap 1: COSY RSA is SPKI + PKCS#1 v1.5 (JS cosy.js RSA_PKCS1_PADDING).
    // -----------------------------------------------------------------------

    #[test]
    fn test_cosy_key_is_spki_pkcs1v15() {
        // The hardcoded key must parse as SPKI ("BEGIN PUBLIC KEY").
        let key = RsaPublicKey::from_public_key_pem(QODER_RSA_PUBLIC_KEY_PEM);
        assert!(key.is_ok(), "SPKI key must parse");
        // PKCS#1 v1.5 encrypts to exactly the 128-byte modulus size.
        let ct = QoderExecutor::rsa_encrypt_base64("0123456789abcdef").unwrap();
        let raw = B64.decode(&ct).unwrap();
        assert_eq!(
            raw.len(),
            128,
            "1024-bit PKCS#1 v1.5 ciphertext is 128 bytes"
        );
    }

    // -----------------------------------------------------------------------
    // Gap 2: catalog parses body.chat first (JS fetchQoderCatalogRaw).
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_catalog_prefers_chat_array() {
        let catalog = serde_json::json!({
            "chat": [
                {"key": "qmodel", "display_name": "Q", "max_input_tokens": 200000, "enable": true}
            ],
            "data": [
                {"key": "stale", "display_name": "S"}
            ]
        });
        let (models, raw) = QoderExecutor::parse_qoder_catalog(&catalog);
        assert!(raw.contains_key("qmodel"));
        assert!(
            !raw.contains_key("stale"),
            "data fallback must not win over chat"
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "qmodel");
    }

    #[test]
    fn test_parse_catalog_keeps_hidden_models() {
        // enable:false entries are cached but hidden from the UI list.
        let catalog = serde_json::json!({
            "chat": [
                {"key": "hidden", "display_name": "H", "enable": false}
            ]
        });
        let (models, raw) = QoderExecutor::parse_qoder_catalog(&catalog);
        assert!(raw.contains_key("hidden"));
        assert!(models.is_empty());
    }

    // -----------------------------------------------------------------------
    // Gap 3: billing codes match as string OR number (JS regex on "112").
    // -----------------------------------------------------------------------

    #[test]
    fn test_detect_billing_block_string_code() {
        let body = r#"{"statusCodeValue":403,"body":"{\"code\":\"112\",\"message\":\"Quota exhausted\"}"}"#;
        let result = detect_qoder_billing_block(body);
        assert!(result.is_some(), "string code \"112\" must be detected");
    }

    #[test]
    fn test_detect_billing_block_number_code_10605() {
        let body = r#"{"statusCodeValue":429,"body":"{\"code\":10605,\"message\":\"throttle\"}"}"#;
        let result = detect_qoder_billing_block(body);
        assert!(result.is_some(), "numeric code 10605 must be detected");
    }

    #[test]
    fn test_billing_code_helper_accepts_both_shapes() {
        assert_eq!(
            qoder_billing_code(&serde_json::json!({"code": "112"})),
            Some("112".to_string())
        );
        assert_eq!(
            qoder_billing_code(&serde_json::json!({"code": 10605})),
            Some("10605".to_string())
        );
        assert_eq!(qoder_billing_code(&serde_json::json!({})), None);
    }

    #[test]
    fn test_detect_billing_block_numeric_code_gated_by_status() {
        // Numeric {"code":112} is a benign Rust-side superset (JS matches
        // strings only), but it is still gated behind status != 200.
        let ok = r#"{"statusCodeValue":403,"body":"{\"code\":112}"}"#;
        assert!(detect_qoder_billing_block(ok).is_some());
        let gated = r#"{"statusCodeValue":200,"body":"{\"code\":112}"}"#;
        assert!(
            detect_qoder_billing_block(gated).is_none(),
            "numeric code on a 200 frame must not fire"
        );
    }

    // -----------------------------------------------------------------------
    // Review fix: apply_qoder_context_tier creates objects when absent.
    // -----------------------------------------------------------------------

    #[test]
    fn test_apply_context_tier_creates_missing_objects() {
        // Minimal payload with none of parameters/chat_context/model_config —
        // mirrors JS spread-defaults (`payload.parameters = {...}` etc.).
        let mut payload = serde_json::json!({"messages": []});
        let tier = ("large".to_string(), 200_000u64, "auto:fits".to_string());
        apply_qoder_context_tier(&mut payload, &tier);
        assert_eq!(payload["parameters"]["context_length"], 200_000);
        assert_eq!(
            payload["chat_context"]["extra"]["ideModelConfigOverride"]["max_input_tokens"],
            200_000
        );
        // model_config stays absent — JS only touches it when already an object.
        assert!(payload.get("model_config").is_none());
        // Existing model_config object gets the tier written in.
        let mut payload2 = serde_json::json!({
            "parameters": {"context_length": 32_000},
            "model_config": {"max_input_tokens": 32_000}
        });
        apply_qoder_context_tier(&mut payload2, &tier);
        assert_eq!(payload2["parameters"]["context_length"], 200_000);
        assert_eq!(payload2["model_config"]["max_input_tokens"], 200_000);
        assert_eq!(
            payload2["chat_context"]["extra"]["ideModelConfigOverride"]["max_input_tokens"],
            200_000
        );
    }

    // -----------------------------------------------------------------------
    // Gap 5: attachments — data-URI parse, stubs, multipart shape.
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_data_uri_roundtrip() {
        let (mime, b64) = parse_qoder_data_uri("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(b64, "iVBORw0KGgo=");
        assert!(parse_qoder_data_uri("https://example.com/a.png").is_none());
        assert!(parse_qoder_data_uri("not-a-uri").is_none());
    }

    #[test]
    fn test_normalize_content_preserves_image_url() {
        let content = serde_json::json!([
            {"type": "text", "text": "describe"},
            {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}}
        ]);
        let out = QoderExecutor::normalize_content(&content);
        assert!(out.is_array(), "image content stays an array, got: {out}");
        let arr = out.as_array().unwrap();
        assert!(arr.iter().any(|b| b["type"] == "image_url"));
    }

    #[test]
    fn test_normalize_content_stubs_file_blocks() {
        let content = serde_json::json!([
            {"type": "text", "text": "see"},
            {"type": "file", "file": {"filename": "big.pdf", "file_data": "data:application/pdf;base64,AAA"}}
        ]);
        let out = QoderExecutor::normalize_content(&content);
        let s = out.as_str().unwrap_or("");
        assert!(s.contains("big.pdf"), "stub keeps the filename, got: {s}");
        assert!(!s.contains("AAA"), "stub drops the bytes");
    }

    #[test]
    fn test_extract_upload_url_variants() {
        let v = serde_json::json!({"result": {"imageUrls": ["https://oss/x.png"]}});
        assert_eq!(
            extract_qoder_upload_url(&v).as_deref(),
            Some("https://oss/x.png")
        );
        let v = serde_json::json!({"url": "https://oss/y.png"});
        assert_eq!(
            extract_qoder_upload_url(&v).as_deref(),
            Some("https://oss/y.png")
        );
        assert!(extract_qoder_upload_url(&serde_json::json!({"ok": true})).is_none());
    }

    #[test]
    fn test_multipart_body_shape() {
        let (boundary, body) = build_qoder_multipart_file(b"bytes", "image.png", "image/png");
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"file\""));
        assert!(text.contains("filename=\"image.png\""));
        assert!(text.contains("image/png"));
        assert!(text.starts_with(&format!("--{boundary}")));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
    }

    // -----------------------------------------------------------------------
    // Gap 6: context tiers (JS contextTier.js vectors).
    // -----------------------------------------------------------------------

    #[test]
    fn test_tier_tokens_parse() {
        assert_eq!(parse_qoder_tier_tokens(&serde_json::json!(204800)), 204800);
        assert_eq!(parse_qoder_tier_tokens(&serde_json::json!("200K")), 200_000);
        assert_eq!(parse_qoder_tier_tokens(&serde_json::json!("1M")), 1_000_000);
        assert_eq!(parse_qoder_tier_tokens(&serde_json::json!("big")), 0);
    }

    #[test]
    fn test_tier_escalation_auto() {
        let cfg = serde_json::json!({
            "max_input_tokens": 180_000,
            "context_config": [
                {"name": "200K", "tokenCount": 200_000, "isDefault": true},
                {"name": "400K", "tokenCount": 400_000},
                {"name": "1M", "tokenCount": 1_000_000}
            ]
        });
        // Small prompt → no escalation.
        assert!(resolve_qoder_context_tier(&cfg, "", &serde_json::json!([]), None).is_none());
        // ~300k-token prompt → 400K tier.
        let big = serde_json::json!([{"role": "user", "content": "abcd".repeat(300_000)}]);
        let choice = resolve_qoder_context_tier(&cfg, "", &big, None).unwrap();
        assert_eq!(choice.1, 400_000, "got: {choice:?}");
    }

    #[test]
    fn test_apply_tier_writes_three_places() {
        let mut payload = serde_json::json!({
            "parameters": {},
            "chat_context": {"extra": {}},
            "model_config": {"key": "qmodel_38max", "max_input_tokens": 180_000}
        });
        apply_qoder_context_tier(
            &mut payload,
            &("400K".to_string(), 400_000, "auto:fits".to_string()),
        );
        assert_eq!(payload["parameters"]["context_length"], 400_000);
        assert_eq!(
            payload["chat_context"]["extra"]["ideModelConfigOverride"]["max_input_tokens"],
            400_000
        );
        assert_eq!(payload["model_config"]["max_input_tokens"], 400_000);
    }

    // -----------------------------------------------------------------------
    // Gap 7: SSE coalescer (JS sse.js vectors).
    // -----------------------------------------------------------------------

    #[test]
    fn test_canonicalize_usage() {
        let u =
            serde_json::json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15});
        let out = QoderExecutor::canonicalize_qoder_usage(&u).unwrap();
        assert_eq!(out["prompt_tokens"], 10);
        assert_eq!(out["total_tokens"], 15);
        assert!(QoderExecutor::canonicalize_qoder_usage(&serde_json::json!({"foo": 1})).is_none());
    }

    #[test]
    fn test_coalescer_holds_finish_until_usage() {
        let mut coal = QoderSseCoalescer::new("qoder/qmodel");
        // Empty finish frame → held, nothing emitted.
        let (frames, terminal) =
            coal.handle_inner(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        assert!(frames.is_empty());
        assert!(!terminal);
        // Usage-only frame → terminal chunk with usage.
        let (frames, terminal) = coal.handle_inner(
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
        );
        assert!(terminal);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("\"usage\""), "got: {}", frames[0]);
        assert!(frames[0].contains("stop"), "got: {}", frames[0]);
    }

    #[test]
    fn test_coalescer_streams_content() {
        let mut coal = QoderSseCoalescer::new("qoder/qmodel");
        let (frames, terminal) = coal.handle_inner(r#"{"choices":[{"delta":{"content":"hi"}}]}"#);
        assert!(!terminal);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("hi"));
    }

    // -----------------------------------------------------------------------
    // Gap 8: full catalog entry sent as model_config.
    // -----------------------------------------------------------------------

    #[test]
    fn test_transform_sends_full_catalog_entry() {
        let exec = QoderExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let entry = serde_json::json!({
            "key": "qmodel_38max",
            "display_name": "Qwen3.8-Max",
            "is_reasoning": true,
            "max_input_tokens": 180_000,
            "max_output_tokens": 32_768,
            "context_config": [{"name": "200K", "tokenCount": 200_000}],
            "source": "system",
            "custom_field": "kept",
        });
        let mut creds = ProviderConnection::default();
        creds
            .provider_specific_data
            .insert("userId".to_string(), serde_json::json!("u1"));
        let body = serde_json::json!({"messages": [{"role": "user", "content": "hi"}]});
        let payload = exec
            .transform_request(&body, "qoder/qmodel_38max", &creds, &entry)
            .unwrap();
        assert_eq!(payload["model_config"]["custom_field"], "kept");
        assert_eq!(payload["model_config"]["display_name"], "Qwen3.8-Max");
        assert_eq!(payload["model_config"]["max_input_tokens"], 180_000);
        // Caller cap respected: model default 32768, body asks 100.
        let body2 = serde_json::json!({
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100
        });
        let payload2 = exec
            .transform_request(&body2, "qoder/qmodel_38max", &creds, &entry)
            .unwrap();
        assert_eq!(payload2["parameters"]["max_tokens"], 100);
    }

    // -----------------------------------------------------------------------
    // COSY signature must hash the raw multipart bytes (cosy.js:146-149).
    // -----------------------------------------------------------------------

    fn cosy_test_creds() -> QoderCreds {
        QoderCreds {
            user_id: "u1".to_string(),
            auth_token: "tok".to_string(),
            name: "N".to_string(),
            email: "e@example.com".to_string(),
            machine_id: "m1".to_string(),
        }
    }

    /// `Bearer COSY.<payloadB64>.<sig>` — recompute the signature over the same
    /// byte layout cosy.js builds and compare with what the executor produced.
    fn assert_cosy_sig_hashes_raw_body(body: &[u8], cosy: &CosyHeaders) {
        let auth = cosy
            .authorization
            .strip_prefix("Bearer COSY.")
            .expect("authorization must be a COSY bearer");
        let (payload_b64, sig) = auth.rsplit_once('.').expect("COSY.<payload>.<sig>");
        let mut expected: Vec<u8> = Vec::new();
        for part in [
            payload_b64.to_string(),
            cosy.cosy_key.clone(),
            cosy.cosy_date.clone(),
        ] {
            expected.extend_from_slice(part.as_bytes());
            expected.push(b'\n');
        }
        expected.extend_from_slice(body);
        expected.push(b'\n');
        expected.extend_from_slice(cosy.cosy_sigpath.as_bytes());
        assert_eq!(*sig, QoderExecutor::md5_hex(&expected));
    }

    #[test]
    fn test_cosy_signature_preserves_non_utf8_body_bytes() {
        let body = b"\xff\xfe\x80binary".as_slice();
        let cosy = QoderExecutor::build_cosy_headers(
            body,
            "https://api3.qoder.sh/api/v2/service/pro/sse/agent_chat_generation",
            &cosy_test_creds(),
        )
        .unwrap();
        assert_cosy_sig_hashes_raw_body(body, &cosy);
    }

    #[test]
    fn test_cosy_signature_unchanged_for_ascii_body() {
        let body = br#"{"model":"qmodel","messages":[]}"#;
        let cosy = QoderExecutor::build_cosy_headers(
            body,
            "https://api3.qoder.sh/api/v2/service/pro/sse/agent_chat_generation",
            &cosy_test_creds(),
        )
        .unwrap();
        assert_cosy_sig_hashes_raw_body(body, &cosy);
        assert_eq!(cosy.cosy_bodyhash, QoderExecutor::md5_hex(body));
    }

    #[test]
    fn test_estimate_qoder_prompt_tokens_counts_utf16_code_units() {
        let messages = serde_json::json!([{"role": "user", "content": "x"}]);
        // JS `text.length` counts UTF-16 code units, so 100 astral-plane emoji
        // are 200 — twice what a scalar count would report. A BMP-only string
        // is the control: same value under either accounting.
        let bmp = estimate_qoder_prompt_tokens(&"a".repeat(100), &messages, None);
        let astral = estimate_qoder_prompt_tokens(&"\u{1F600}".repeat(100), &messages, None);
        assert!(
            astral > bmp,
            "surrogate pairs count 2 UTF-16 units: {astral} should exceed {bmp}"
        );
        let envelope = serde_json::json!({
            "system": "a".repeat(100),
            "messages": messages,
            "tools": [],
        })
        .to_string();
        assert_eq!(
            bmp,
            (envelope.encode_utf16().count() as u64).div_ceil(4),
            "pure ASCII must keep the plain char count"
        );
    }

    #[test]
    fn test_over_budget_pass_strips_small_data_uris_too() {
        let mut msg = serde_json::json!({
            "role": "user",
            "content": format!("look: data:image/png;base64,{}", "A".repeat(1024))
        });
        strip_qoder_message_data_uris(&mut msg);
        let content = msg["content"].as_str().unwrap();
        assert!(
            !content.contains("data:"),
            "small URI must still be stripped: {content}"
        );
        assert!(content.contains("[file omitted: attachment"));
        assert!(content.contains("payload over Qoder size budget"));
    }

    #[test]
    fn test_main_rewrite_pass_still_keeps_small_data_uris() {
        let text = format!("look: data:image/png;base64,{}", "A".repeat(1024));
        assert!(strip_qoder_data_uris(&text).contains("data:"));
    }
}
