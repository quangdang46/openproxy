use std::sync::Arc;

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

    fn build_url(&self) -> String {
        QODER_CHAT_URL_ENCODED.to_string()
    }

    fn build_headers(
        &self,
        encoded_body: &[u8],
        request_url: &str,
        creds: &QoderCreds,
    ) -> Result<HeaderMap, QoderExecutorError> {
        let cosy = Self::build_cosy_headers(encoded_body, request_url, creds)?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("Accept", HeaderValue::from_static("text/event-stream"));
        headers.insert("Cache-Control", HeaderValue::from_static("no-cache"));
        // gzip triggers signature validation on Qoder's CDN; force identity.
        headers.insert("Accept-Encoding", HeaderValue::from_static("identity"));

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

            if role == "system" {
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

    /// Map the OpenAI-style request body into the exact shape Qoder expects.
    fn transform_request(
        &self,
        body: &Value,
        model: &str,
        credentials: &ProviderConnection,
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
            "chat_context": {
                "chatPrompt": "",
                "imageUrls": null,
                "extra": {
                    "context": [],
                    "modelConfig": {
                        "key": qoder_key,
                        "is_reasoning": false
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
        request: QoderExecutionRequest,
    ) -> Result<QoderExecutorResponse, QoderExecutorError> {
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

        let url = self.build_url();

        // Transform the OpenAI-compatible body into Qoder's format
        let transformed_body =
            self.transform_request(&request.body, &request.model, &request.credentials)?;

        // Encode body with Qoder's WAF-bypass scheme
        let plain_body = serde_json::to_vec(&transformed_body)?;
        let encoded_body_str = Self::qoder_encode_body(&plain_body);
        let encoded_body = encoded_body_str.as_bytes();

        // Build COSY-signed headers from the *encoded* body
        let headers = self.build_headers(encoded_body, &url, &creds)?;

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
}
