use std::sync::Arc;

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use md5::{Digest, Md5};
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use rsa::{pkcs1::DecodeRsaPublicKey, Oaep, RsaPublicKey};
use serde_json::Value;
use sha1::Sha1;
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
const QODER_RSA_PUBLIC_KEY_PEM: &str = "-----BEGIN RSA PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
XcW+ML9FoCI6AOvOzwIDAQAB
-----END RSA PUBLIC KEY-----";

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

    /// RSA-OAEP (SHA-1) encrypt the AES key with the hardcoded public key,
    /// returns base64.
    fn rsa_encrypt_base64(data: &str) -> Result<String, QoderExecutorError> {
        let public_key = RsaPublicKey::from_pkcs1_pem(QODER_RSA_PUBLIC_KEY_PEM)
            .map_err(|e| QoderExecutorError::CryptoError(format!("RSA key parse error: {e}")))?;

        let mut rng = rand::thread_rng();
        let padding = Oaep::new::<Sha1>();
        let encrypted = public_key
            .encrypt(&mut rng, padding, data.as_bytes())
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

        // sigInput = payloadB64 + "\n" + cosyKey + "\n" + timestamp + "\n" + body + "\n" + sigPath
        let sig_input = format!(
            "{}\n{}\n{}\n{}\n{}",
            payload_b64,
            cosy_key,
            timestamp,
            String::from_utf8_lossy(body),
            sig_path
        );
        let sig = Self::md5_hex(sig_input.as_bytes());

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
    /// system in messages) and flatten any multipart content arrays.
    fn normalize_messages(messages: &[Value]) -> (Vec<Value>, String) {
        let mut system_parts = Vec::new();
        let mut out = Vec::new();

        for msg in messages {
            let obj = match msg.as_object() {
                Some(o) => o,
                None => continue,
            };
            let text = Self::extract_text(msg.get("content").unwrap_or(&Value::Null));
            let role = obj.get("role").and_then(|v| v.as_str()).unwrap_or("");

            if role == "system" || role == "developer" {
                if !text.is_empty() {
                    system_parts.push(text);
                }
                continue;
            }

            let mut cloned = msg.clone();
            if let Some(obj) = cloned.as_object_mut() {
                obj.insert("content".to_string(), Value::String(text));
            }
            out.push(cloned);
        }

        (out, system_parts.join("\n\n"))
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

    /// Fetch the live model catalog and resolve the entry for `qoder_key`
    /// (9router getQoderModelConfig). Returns `(is_reasoning, max_output_tokens,
    /// source)`. Hard error when the model is unknown after a forced refresh —
    /// silently downgrading the upstream model would be wrong. A network/
    /// HTTP failure on the catalog (no catalog access) falls back to defaults
    /// so the chat path does not break.
    async fn fetch_model_config(
        &self,
        qoder_key: &str,
        creds: &QoderCreds,
    ) -> Result<(bool, u64, String), QoderExecutorError> {
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
                return Ok((false, 32768, "system".to_string()));
            }
        };
        if !response.status().is_success() {
            tracing::warn!(
                target: "openproxy::executor",
                "qoder model/list returned HTTP {}",
                response.status().as_u16()
            );
            return Ok((false, 32768, "system".to_string()));
        }
        let catalog: Value = response.json().await.map_err(|e| {
            QoderExecutorError::MissingCredentials(format!("qoder model/list JSON error: {e}"))
        })?;
        // Catalog shape: { data: [...] } or { models: [...] } — find the entry
        // whose key/model matches.
        let entries = catalog
            .get("data")
            .or_else(|| catalog.get("models"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let entry = entries.iter().find(|e| {
            e.get("key").and_then(Value::as_str) == Some(qoder_key)
                || e.get("model").and_then(Value::as_str) == Some(qoder_key)
        });
        let Some(entry) = entry else {
            return Err(QoderExecutorError::MissingCredentials(format!(
                "qoder: model_config for \"{qoder_key}\" not yet known"
            )));
        };
        let is_reasoning = entry
            .get("is_reasoning")
            .or_else(|| entry.get("reasoning"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let max_output_tokens = entry
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(32768);
        let source = entry
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("system")
            .to_string();
        Ok((is_reasoning, max_output_tokens, source))
    }

    /// Map the OpenAI-style request body into the exact shape Qoder expects.
    /// `is_reasoning`/`source` come from the live model config (9router embeds
    /// `model_config` and `chat_context.extra.modelConfig.is_reasoning`).
    fn transform_request(
        &self,
        body: &Value,
        model: &str,
        credentials: &ProviderConnection,
        is_reasoning: bool,
        model_source: &str,
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

        let max_tokens = body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(32768);

        let tools = body.get("tools").cloned().unwrap_or(Value::Array(vec![]));

        // 9router: embed model_config + chat_context.extra.modelConfig.is_reasoning.
        let model_config = serde_json::json!({
            "key": qoder_key,
            "is_reasoning": is_reasoning,
            "source": model_source,
        });

        Ok(serde_json::json!({
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
        }))
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

        // Live model config → is_reasoning + source (9router getQoderModelConfig).
        let (is_reasoning, _, model_source) =
            match self.fetch_model_config(&qoder_key, &creds).await {
                Ok(cfg) => cfg,
                Err(e) => {
                    // Unknown model after refresh is a hard error (JS throws).
                    return Err(e);
                }
            };

        // Transform the OpenAI-compatible body into Qoder's format
        let transformed_body = self.transform_request(
            &request.body,
            &request.model,
            &request.credentials,
            is_reasoning,
            &model_source,
        )?;

        // Encode body with Qoder's WAF-bypass scheme
        let plain_body = serde_json::to_vec(&transformed_body)?;
        let encoded_body_str = Self::qoder_encode_body(&plain_body);
        let encoded_body = encoded_body_str.as_bytes();

        // Build COSY-signed headers from the *encoded* body
        let headers = self.build_headers(encoded_body, &url, &creds, &qoder_key, &model_source)?;

        let client = self.pool.get("qoder", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(encoded_body.to_vec())
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
                let stripped = inner.replace('\n', "").replace('\r', "");
                Some(format!("data: {stripped}\n\n"))
            }
        }
    }
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
}
