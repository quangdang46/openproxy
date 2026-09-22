//! DeepSeek Web executor — `chat.deepseek.com` userToken web API.
//!
//! Port of OmniRoute `open-sse/executors/deepseek-web.ts` (1223 lines) +
//! `deepseek-web/stream-format.ts` + `deepseek-web-done-terminator.ts`
//! (the auto-refresh subclass is folded in: 401/403 triggers one token
//! refresh + retry).
//!
//! Flow per request:
//! 1. `extract_user_token` (api_key, JSON-wrapped ok) → `GET
//!    /api/v0/users/current` → short-lived access token (cached ~1h,
//!    process-local, keyed by userToken).
//! 2. Build the single `prompt` string (tool-trajectory replay when the
//!    request carries `tools[]`, else rolling-window transcript).
//! 3. `POST /api/v0/chat_session/create` → session id.
//! 4. `POST /api/v0/chat/create_pow_challenge` → solve DeepSeekHashV1 PoW
//!    (see [`super::deepseek_pow`]) → `X-Ds-Pow-Response` header.
//! 5. `POST /api/v0/chat/completion` with browser-fingerprint headers.
//! 6. Convert the DeepSeek event stream (`p`/`o`/`v` envelopes) to OpenAI
//!    SSE chunks or a chat.completion JSON; tool replies are buffered and
//!    parsed into `tool_calls`.
//!
//! Auth: the `userToken` (DeepSeek localStorage) is stored in
//! `credentials.api_key` (web-cookie provider convention).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hyper::http;
use hyper::http::uri::InvalidUri;
use rand::RngCore;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::deepseek_web_tools::{
    append_search_citations, build_tool_conversation_prompt, extract_user_token,
    format_stream_content, is_search_model, is_thinking_model, messages_to_prompt,
    parse_deepseek_tool_calls, resolve_model_options, serialize_deepseek_tool_prompt,
    DeepSeekSearchResult,
};
use crate::types::ProviderConnection;

use super::deepseek_pow::{find_pow_nonce, validate_challenge};
use super::{ClientPool, TransportKind, UpstreamResponse};

pub const DEEPSEEK_WEB_BASE: &str = "https://chat.deepseek.com";
const DEEPSEEK_API_BASE: &str = "https://chat.deepseek.com/api";
const COMPLETION_URL: &str = "https://chat.deepseek.com/api/v0/chat/completion";
const USERS_CURRENT_URL: &str = "https://chat.deepseek.com/api/v0/users/current";
const SESSION_CREATE_URL: &str = "https://chat.deepseek.com/api/v0/chat_session/create";
const SESSION_DELETE_URL: &str = "https://chat.deepseek.com/api/v0/chat_session/delete";
const POW_CHALLENGE_URL: &str = "https://chat.deepseek.com/api/v0/chat/create_pow_challenge";

const DEEPSEEK_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

/// Finish drain after FINISHED before closing SSE (deepseek-web-done-terminator.ts).
const FINISHED_DRAIN_MS: u64 = 750;

pub struct DeepSeekWebExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug)]
pub enum DeepSeekWebExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidHeader(String),
    InvalidUri(InvalidUri),
    InvalidRequest(hyper::http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
}

impl From<reqwest::Error> for DeepSeekWebExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<InvalidUri> for DeepSeekWebExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for DeepSeekWebExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for DeepSeekWebExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for DeepSeekWebExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for DeepSeekWebExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl std::fmt::Display for DeepSeekWebExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::InvalidCredentials(m) => write!(f, "Invalid credentials: {}", m),
            Self::InvalidHeader(m) => write!(f, "Invalid header: {}", m),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
        }
    }
}

impl std::error::Error for DeepSeekWebExecutorError {}

pub struct DeepSeekWebExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for DeepSeekWebExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekWebExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

struct TokenInfo {
    access_token: String,
    expires_at: u64,
}

static TOKEN_CACHE: std::sync::LazyLock<Mutex<HashMap<String, TokenInfo>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static SESSION_CACHE: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
const CACHE_MAX_SIZE: usize = 100;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn evict_oldest<K: Clone + Eq + std::hash::Hash, V>(map: &mut HashMap<K, V>) {
    if map.len() >= CACHE_MAX_SIZE {
        if let Some(k) = map.keys().next().cloned() {
            map.remove(&k);
        }
    }
}

pub struct DeepSeekWebExecutor {
    pool: Arc<ClientPool>,
}

impl DeepSeekWebExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    pub async fn execute_request(
        &self,
        request: DeepSeekWebExecutionRequest,
    ) -> Result<DeepSeekWebExecutorResponse, DeepSeekWebExecutorError> {
        let body_obj = request.body.as_object().cloned().unwrap_or_default();
        let get = |k: &str| body_obj.get(k);

        let has_tools = get("tools")
            .and_then(Value::as_array)
            .is_some_and(|t| !t.is_empty());
        let tool_prompt = if has_tools {
            get("tools")
                .and_then(|t| serialize_deepseek_tool_prompt(t))
                .unwrap_or_default()
        } else {
            (String::new(), String::new())
        };

        let messages: Vec<Value> = get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let user_token = extract_user_token(
            request.credentials.api_key.as_deref(),
            request.credentials.access_token.as_deref(),
        )
        .ok_or_else(|| {
            DeepSeekWebExecutorError::MissingCredentials(
                "Invalid credentials: paste your userToken from DeepSeek localStorage (DevTools → Application → Local Storage → chat.deepseek.com → userToken)".to_string(),
            )
        })?;

        let (model_type, thinking_enabled, search_enabled) =
            resolve_model_options(&request.model, &request.body);

        let psd = &request.credentials.provider_specific_data;
        let persist_session = psd
            .get("persistSession")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let history_window = psd
            .get("historyWindow")
            .and_then(Value::as_u64)
            .filter(|w| *w > 0)
            .unwrap_or(0) as usize;

        let prompt = if has_tools {
            build_tool_conversation_prompt(&messages, &tool_prompt.0)
        } else {
            let mut prompt_messages = messages.clone();
            if !tool_prompt.0.is_empty() {
                prompt_messages.insert(0, json!({"role": "system", "content": tool_prompt.0}));
            }
            messages_to_prompt(&prompt_messages, history_window)
        };
        let ref_file_ids: Vec<Value> = get("ref_file_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // Token with one auto-refresh retry on 401/403 (auto-refresh subclass folded in).
        let mut access_token = self
            .acquire_access_token(&user_token, request.proxy.as_ref())
            .await?;
        let mut refreshed_once = false;

        loop {
            let attempt = self
                .attempt_completion(
                    &request,
                    &access_token,
                    &model_type,
                    thinking_enabled,
                    search_enabled,
                    &prompt,
                    &ref_file_ids,
                    &user_token,
                    persist_session,
                    has_tools,
                    tool_prompt.1.clone(),
                )
                .await;
            match attempt {
                Ok(resp) => return Ok(resp),
                Err(Retry::RefreshAndRetry) if !refreshed_once => {
                    refreshed_once = true;
                    {
                        let mut cache = TOKEN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                        cache.remove(&user_token);
                    }
                    access_token = self
                        .acquire_access_token(&user_token, request.proxy.as_ref())
                        .await?;
                    continue;
                }
                Err(Retry::RefreshAndRetry) => {
                    return self.error_result(502, "DeepSeek error: retry exhausted");
                }
                Err(Retry::Fail(msg)) => {
                    return self.error_result(502, &format!("DeepSeek error: {msg}"));
                }
            }
        }
    }

    fn error_result(
        &self,
        status: u16,
        message: &str,
    ) -> Result<DeepSeekWebExecutorResponse, DeepSeekWebExecutorError> {
        Ok(DeepSeekWebExecutorResponse {
            response: json_error(status, message, "upstream_error", None),
            url: COMPLETION_URL.to_string(),
            headers: HeaderMap::new(),
            transformed_body: json!({}),
            transport: TransportKind::Reqwest,
        })
    }

    /// One completion POST with a fresh PoW answer per attempt.
    #[allow(clippy::too_many_arguments)]
    async fn post_completion(
        &self,
        client: &reqwest::Client,
        access_token: &str,
        session_id: &str,
        model_type: &str,
        prompt: &str,
        ref_file_ids: &[Value],
        thinking_enabled: bool,
        search_enabled: bool,
    ) -> Result<(reqwest::Response, HeaderMap, Value), String> {
        let challenge = get_pow_challenge(client, access_token).await?;
        let prefix = format!("{}_{}_", challenge.salt, challenge.expire_at);
        validate_challenge(
            &challenge.algorithm,
            &challenge.challenge,
            &challenge.salt,
            challenge.difficulty,
        )
        .map_err(|e| format!("PoW validation: {e}"))?;
        let answer = find_pow_nonce(&prefix, &challenge.challenge, challenge.difficulty)
            .ok_or_else(|| "PoW solver failed".to_string())?;
        let pow_response = STANDARD.encode(
            serde_json::to_string(&json!({
                "algorithm": challenge.algorithm,
                "challenge": challenge.challenge,
                "salt": challenge.salt,
                "answer": answer,
                "signature": challenge.signature,
                "target_path": challenge.target_path,
            }))
            .map_err(|e| e.to_string())?,
        );
        let mut headers = fake_headers();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access_token}")).map_err(|e| e.to_string())?,
        );
        headers.insert(
            "X-Ds-Pow-Response",
            HeaderValue::from_str(&pow_response).map_err(|e| e.to_string())?,
        );
        headers.insert(
            "X-Client-Timezone-Offset",
            HeaderValue::from_str(&chrono::Local::now().offset().local_minus_utc().to_string())
                .map_err(|e| e.to_string())?,
        );
        headers.insert(
            reqwest::header::COOKIE,
            HeaderValue::from_str(&generate_fake_cookie()).map_err(|e| e.to_string())?,
        );
        let payload = json!({
            "chat_session_id": session_id,
            "parent_message_id": Value::Null,
            "model_type": model_type,
            "prompt": prompt,
            "ref_file_ids": ref_file_ids,
            "thinking_enabled": thinking_enabled,
            "search_enabled": search_enabled,
            "preempt": false,
        });
        let resp = client
            .post(COMPLETION_URL)
            .headers(headers.clone())
            .json(&payload)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Ok((resp, headers, payload))
    }

    async fn acquire_access_token(
        &self,
        user_token: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Result<String, DeepSeekWebExecutorError> {
        {
            let cache = TOKEN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(info) = cache.get(user_token) {
                if info.expires_at > now_secs() {
                    return Ok(info.access_token.clone());
                }
            }
        }
        let client = self.pool.get("deepseek-web", proxy)?;
        let resp = client
            .get(USERS_CURRENT_URL)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {user_token}"),
            )
            .headers(fake_headers())
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED
            || resp.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(DeepSeekWebExecutorError::InvalidCredentials(
                "Token invalid or expired — get a new userToken from DeepSeek localStorage"
                    .to_string(),
            ));
        }
        if !resp.status().is_success() {
            return Err(DeepSeekWebExecutorError::InvalidCredentials(format!(
                "users/current HTTP {}",
                resp.status()
            )));
        }
        let json: Value = resp
            .json()
            .await
            .map_err(DeepSeekWebExecutorError::Request)?;
        if let Some(code) = json.get("code").and_then(Value::as_i64) {
            if code != 0 {
                let msg = json
                    .get("msg")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        json.get("data")
                            .and_then(|d| d.get("biz_msg"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("Unknown error");
                return Err(DeepSeekWebExecutorError::InvalidCredentials(format!(
                    "DeepSeek rejected token: {msg}"
                )));
            }
        }
        let token = json
            .get("data")
            .and_then(|d| d.get("biz_data"))
            .and_then(|b| b.get("token"))
            .and_then(Value::as_str)
            .or_else(|| {
                json.get("biz_data")
                    .and_then(|b| b.get("token"))
                    .and_then(Value::as_str)
            })
            .ok_or_else(|| {
                DeepSeekWebExecutorError::InvalidCredentials("Failed to acquire token".to_string())
            })?;
        let access = token.to_string();
        {
            let mut cache = TOKEN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            evict_oldest(&mut cache);
            cache.insert(
                user_token.to_string(),
                TokenInfo {
                    access_token: access.clone(),
                    expires_at: now_secs() + 3600,
                },
            );
        }
        Ok(access)
    }

    #[allow(clippy::too_many_arguments)]
    async fn attempt_completion(
        &self,
        request: &DeepSeekWebExecutionRequest,
        access_token: &str,
        model_type: &str,
        thinking_enabled: bool,
        search_enabled: bool,
        prompt: &str,
        ref_file_ids: &[Value],
        user_token: &str,
        persist_session: bool,
        has_tools: bool,
        tool_nonce: String,
    ) -> Result<DeepSeekWebExecutorResponse, Retry> {
        let client = self
            .pool
            .get("deepseek-web", request.proxy.as_ref())
            .map_err(|e| Retry::Fail(e.to_string()))?;

        // Session: reuse when persistSession, else fresh per request.
        // A reused id may be stale (user deleted the chat in the DeepSeek
        // UI) — tracked so a failure triggers one fresh-session retry.
        let mut session_id = if persist_session {
            SESSION_CACHE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(user_token)
                .cloned()
        } else {
            None
        };
        let reused = session_id.is_some();
        if session_id.is_none() {
            session_id = Some(
                create_session(&client, access_token)
                    .await
                    .map_err(|e| Retry::Fail(e))?,
            );
            if persist_session {
                let mut cache = SESSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                evict_oldest(&mut cache);
                cache.insert(user_token.to_string(), session_id.clone().unwrap());
            }
        }
        let session_id = session_id.unwrap();

        // One completion POST (fresh PoW per attempt). Extracted as a method
        // (not a closure) so borrows don't outlive the call.
        let (resp, req_headers, request_payload) = match self
            .post_completion(
                &client,
                access_token,
                &session_id,
                model_type,
                prompt,
                ref_file_ids,
                thinking_enabled,
                search_enabled,
            )
            .await
        {
            Ok(t) => t,
            Err(e) => return Err(Retry::Fail(e)),
        };

        // Stale reused session → fresh session + one retry.
        if !resp.status().is_success() && persist_session && reused {
            {
                SESSION_CACHE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(user_token);
            }
            let fresh_sid = create_session(&client, access_token)
                .await
                .map_err(|e| Retry::Fail(e))?;
            {
                let mut cache = SESSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                evict_oldest(&mut cache);
                cache.insert(user_token.to_string(), fresh_sid.clone());
            }
            let (resp2, headers2, payload2) = match self
                .post_completion(
                    &client,
                    access_token,
                    &fresh_sid,
                    model_type,
                    prompt,
                    ref_file_ids,
                    thinking_enabled,
                    search_enabled,
                )
                .await
            {
                Ok(t) => t,
                Err(e) => return Err(Retry::Fail(e)),
            };
            if !resp2.status().is_success() {
                return Err(self
                    .map_error_status(resp2, headers2, payload2, user_token)
                    .await);
            }
            return self
                .build_success(
                    request,
                    resp2,
                    headers2,
                    payload2,
                    has_tools,
                    tool_nonce,
                    &fresh_sid,
                    access_token,
                    persist_session,
                )
                .await;
        }

        if !resp.status().is_success() {
            return Err(self
                .map_error_status(resp, req_headers, request_payload, user_token)
                .await);
        }

        // HTTP 200 with JSON error envelope?
        if resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("application/json"))
        {
            // Peek by buffering (small JSON bodies only).
            let bytes = resp.bytes().await.map_err(|e| Retry::Fail(e.to_string()))?;
            if let Ok(json) = serde_json::from_slice::<Value>(&bytes) {
                if let Some((code, msg)) = parse_deepseek_error(&json) {
                    let status = match code {
                        40003 => 401,
                        40002 => 429,
                        _ => 502,
                    };
                    if code == 40003 {
                        TOKEN_CACHE
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(user_token);
                    }
                    if persist_session {
                        SESSION_CACHE
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(user_token);
                    }
                    delete_session(&client, access_token, &session_id).await;
                    let body = json!({"error": {"message": format!("DeepSeek error {code}: {msg}"), "type": "upstream_error", "code": code}});
                    let bytes = serde_json::to_vec(&body).unwrap_or_default();
                    let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
                    *http_resp.status_mut() = reqwest::StatusCode::from_u16(status)
                        .unwrap_or(reqwest::StatusCode::BAD_GATEWAY);
                    http_resp.headers_mut().insert(
                        reqwest::header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                    return Ok(DeepSeekWebExecutorResponse {
                        response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
                        url: COMPLETION_URL.to_string(),
                        headers: req_headers,
                        transformed_body: request_payload,
                        transport: TransportKind::Reqwest,
                    });
                }
                // Plain JSON success — pass through.
                if !persist_session {
                    delete_session(&client, access_token, &session_id).await;
                }
                let mut http_resp = http::Response::new(ReqwestBody::from(bytes.to_vec()));
                *http_resp.status_mut() = reqwest::StatusCode::OK;
                http_resp.headers_mut().insert(
                    reqwest::header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                return Ok(DeepSeekWebExecutorResponse {
                    response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
                    url: COMPLETION_URL.to_string(),
                    headers: req_headers,
                    transformed_body: request_payload,
                    transport: TransportKind::Reqwest,
                });
            }
            // Not JSON after all — fall through with the buffered bytes as stream.
            return self
                .build_success_from_bytes(
                    request,
                    &bytes,
                    req_headers,
                    request_payload,
                    has_tools,
                    tool_nonce,
                    &session_id,
                    access_token,
                    persist_session,
                    &client,
                    reused,
                )
                .await;
        }

        // SSE stream path: buffer the body, convert, synthesize response.
        let bytes = resp.bytes().await.map_err(|e| Retry::Fail(e.to_string()))?;
        self.build_success_from_bytes(
            request,
            &bytes,
            req_headers,
            request_payload,
            has_tools,
            tool_nonce,
            &session_id,
            access_token,
            persist_session,
            &client,
            reused,
        )
        .await
    }

    async fn map_error_status(
        &self,
        resp: reqwest::Response,
        req_headers: HeaderMap,
        request_payload: Value,
        user_token: &str,
    ) -> Retry {
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            {
                TOKEN_CACHE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(user_token);
            }
            return Retry::RefreshAndRetry;
        }
        let mut msg = match status {
            429 => "DeepSeek rate limited. Wait and retry.".to_string(),
            _ => format!("DeepSeek API error ({status})"),
        };
        if let Ok(json) = resp.json::<Value>().await {
            if let Some(code) = json.get("code").and_then(Value::as_i64) {
                if code != 0 {
                    msg = format!(
                        "DeepSeek error {code}: {}",
                        json.get("msg").and_then(Value::as_str).unwrap_or("")
                    );
                }
            }
        }
        Retry::Fail(msg)
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_success(
        &self,
        request: &DeepSeekWebExecutionRequest,
        resp: reqwest::Response,
        req_headers: HeaderMap,
        request_payload: Value,
        has_tools: bool,
        tool_nonce: String,
        session_id: &str,
        access_token: &str,
        persist_session: bool,
    ) -> Result<DeepSeekWebExecutorResponse, Retry> {
        let bytes = resp.bytes().await.map_err(|e| Retry::Fail(e.to_string()))?;
        let client = self
            .pool
            .get("deepseek-web", request.proxy.as_ref())
            .map_err(|e| Retry::Fail(e.to_string()))?;
        self.build_success_from_bytes(
            request,
            &bytes,
            req_headers,
            request_payload,
            has_tools,
            tool_nonce,
            session_id,
            access_token,
            persist_session,
            &client,
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_success_from_bytes(
        &self,
        request: &DeepSeekWebExecutionRequest,
        bytes: &[u8],
        req_headers: HeaderMap,
        request_payload: Value,
        has_tools: bool,
        tool_nonce: String,
        session_id: &str,
        access_token: &str,
        persist_session: bool,
        client: &reqwest::Client,
        _reused: bool,
    ) -> Result<DeepSeekWebExecutorResponse, Retry> {
        let text = String::from_utf8_lossy(bytes);
        let client_model = if request.model.trim().is_empty() {
            "deepseek-web"
        } else {
            request.model.trim()
        };
        let parsed = parse_deepseek_stream(&text, client_model);

        if !persist_session {
            delete_session(client, access_token, session_id).await;
        }

        if !parsed.saw_finished {
            return Err(Retry::Fail(
                "DeepSeek web session ended before completion (no FINISHED signal received)"
                    .to_string(),
            ));
        }

        // Tool path: buffer + parse into tool_calls (with one fresh-session
        // retry already handled upstream; here we do a single parse).
        if has_tools {
            let tools_val = request.body.get("tools").cloned().unwrap_or(Value::Null);
            let (content, tool_calls) = parse_deepseek_tool_calls(
                &parsed.content_with_think_markers(),
                &format!("call-{}", now_secs()),
                &tools_val,
                &tool_nonce,
            );
            return Ok(build_tool_aware_result(
                request.stream,
                client_model,
                &content,
                &parsed.reasoning,
                tool_calls,
                req_headers,
                request_payload,
            ));
        }

        if request.stream {
            let sse = render_openai_sse(
                client_model,
                None,
                Some(&parsed.content),
                if parsed.reasoning.is_empty() {
                    None
                } else {
                    Some(&parsed.reasoning)
                },
                &parsed.citations,
                "stop",
            );
            let mut http_resp = http::Response::new(ReqwestBody::from(sse));
            *http_resp.status_mut() = reqwest::StatusCode::OK;
            http_resp.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            http_resp.headers_mut().insert(
                reqwest::header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache"),
            );
            Ok(DeepSeekWebExecutorResponse {
                response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
                url: COMPLETION_URL.to_string(),
                headers: req_headers,
                transformed_body: request_payload,
                transport: TransportKind::Reqwest,
            })
        } else {
            let mut message = json!({"role": "assistant", "content": parsed.content});
            if !parsed.reasoning.is_empty() {
                message["reasoning_content"] = json!(parsed.reasoning);
            }
            let body = json!({
                "id": format!("chatcmpl-{}", now_secs()),
                "object": "chat.completion",
                "created": now_secs(),
                "model": client_model,
                "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
            });
            let bytes = serde_json::to_vec(&body).unwrap_or_default();
            let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
            *http_resp.status_mut() = reqwest::StatusCode::OK;
            http_resp.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            Ok(DeepSeekWebExecutorResponse {
                response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
                url: COMPLETION_URL.to_string(),
                headers: req_headers,
                transformed_body: request_payload,
                transport: TransportKind::Reqwest,
            })
        }
    }
}

enum Retry {
    RefreshAndRetry,
    Fail(String),
}

struct PowChallenge {
    algorithm: String,
    challenge: String,
    salt: String,
    signature: String,
    difficulty: u64,
    expire_at: i64,
    target_path: String,
}

async fn get_pow_challenge(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<PowChallenge, String> {
    let mut headers = fake_headers();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        reqwest::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}")).map_err(|e| e.to_string())?,
    );
    let resp = client
        .post(POW_CHALLENGE_URL)
        .headers(headers)
        .json(&json!({"target_path": "/api/v0/chat/completion"}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("create_pow_challenge HTTP {}", resp.status()));
    }
    let json: Value = resp.json().await.map_err(|e| e.to_string())?;
    let biz = json
        .get("data")
        .and_then(|d| d.get("biz_data"))
        .or_else(|| json.get("biz_data"))
        .and_then(|b| b.get("challenge"))
        .ok_or_else(|| {
            format!(
                "No PoW challenge: code={}",
                json.get("code").unwrap_or(&Value::Null)
            )
        })?;
    Ok(PowChallenge {
        algorithm: biz
            .get("algorithm")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        challenge: biz
            .get("challenge")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        salt: biz
            .get("salt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        signature: biz
            .get("signature")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        difficulty: biz.get("difficulty").and_then(Value::as_u64).unwrap_or(0),
        expire_at: biz.get("expire_at").and_then(Value::as_i64).unwrap_or(0),
        target_path: biz
            .get("target_path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

async fn create_session(client: &reqwest::Client, access_token: &str) -> Result<String, String> {
    let mut headers = fake_headers();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        reqwest::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}")).map_err(|e| e.to_string())?,
    );
    headers.insert(
        reqwest::header::COOKIE,
        HeaderValue::from_str(&generate_fake_cookie()).map_err(|e| e.to_string())?,
    );
    let resp = client
        .post(SESSION_CREATE_URL)
        .headers(headers)
        .json(&json!({}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("chat_session/create HTTP {}", resp.status()));
    }
    let json: Value = resp.json().await.map_err(|e| e.to_string())?;
    json.get("data")
        .and_then(|d| d.get("biz_data"))
        .and_then(|b| b.get("chat_session"))
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .or_else(|| {
            json.get("biz_data")
                .and_then(|b| b.get("chat_session"))
                .and_then(|s| s.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "No session id: code={}",
                json.get("code").unwrap_or(&Value::Null)
            )
        })
}

async fn delete_session(client: &reqwest::Client, access_token: &str, session_id: &str) {
    let mut headers = fake_headers();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(auth) = HeaderValue::from_str(&format!("Bearer {access_token}")) {
        headers.insert(reqwest::header::AUTHORIZATION, auth);
    }
    let _ = client
        .post(SESSION_DELETE_URL)
        .headers(headers)
        .json(&json!({"chat_session_id": session_id}))
        .send()
        .await;
}

/// Browser-fingerprint headers of the chat.deepseek.com web client v2.0.0.
/// NOTE: no legacy `X-App-Version`; `X-Client-Bundle-Id` is required.
fn fake_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(ACCEPT, HeaderValue::from_static("*/*"));
    h.insert(
        reqwest::header::ACCEPT_ENCODING,
        HeaderValue::from_static("gzip, deflate, br, zstd"),
    );
    h.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9"),
    );
    h.insert(
        reqwest::header::ORIGIN,
        HeaderValue::from_static(DEEPSEEK_WEB_BASE),
    );
    h.insert(
        reqwest::header::REFERER,
        HeaderValue::from_static("https://chat.deepseek.com/"),
    );
    h.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_static(DEEPSEEK_USER_AGENT),
    );
    h.insert(
        "X-Client-Bundle-Id",
        HeaderValue::from_static("com.deepseek.chat"),
    );
    h.insert("X-Client-Locale", HeaderValue::from_static("en-US"));
    h.insert("X-Client-Platform", HeaderValue::from_static("web"));
    h.insert("X-Client-Version", HeaderValue::from_static("2.0.0"));
    h
}

fn generate_fake_cookie() -> String {
    let mut rng = rand::thread_rng();
    let mut hex = |n: usize| -> String {
        let mut bytes = vec![0u8; n];
        rng.fill_bytes(&mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()[..n].to_string()
    };
    let ts = now_secs() * 1000;
    format!(
        "intercom-HWWAFSESTIME={ts}; HWWAFSESID={}; Hm_lvt_{}={}; _frid={}",
        hex(18),
        Uuid::new_v4(),
        now_secs(),
        Uuid::new_v4()
    )
}

fn parse_deepseek_error(payload: &Value) -> Option<(i64, String)> {
    let code = payload.get("code")?.as_i64()?;
    if code == 0 {
        return None;
    }
    let msg = payload
        .get("msg")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("data")
                .and_then(|d| d.get("biz_msg"))
                .and_then(Value::as_str)
        })
        .unwrap_or("")
        .to_string();
    Some((
        code,
        if msg.is_empty() {
            format!("DeepSeek error {code}")
        } else {
            msg
        },
    ))
}

fn json_error(status: u16, message: &str, err_type: &str, code: Option<&str>) -> UpstreamResponse {
    let mut body = serde_json::json!({ "error": { "message": message, "type": err_type } });
    if let Some(code) = code {
        body["error"]["code"] = Value::String(code.to_string());
    }
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
    *http_resp.status_mut() =
        reqwest::StatusCode::from_u16(status).unwrap_or(reqwest::StatusCode::BAD_GATEWAY);
    http_resp.headers_mut().insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    UpstreamResponse::Reqwest(reqwest::Response::from(http_resp))
}

struct ParsedStream {
    content: String,
    reasoning: String,
    citations: String,
    saw_finished: bool,
}

impl ParsedStream {
    fn content_with_think_markers(&self) -> String {
        // Tool parsing operates on plain content; reasoning is passed
        // separately by the caller. Kept as a helper for symmetry with JS.
        self.content.clone()
    }
}

/// Parse one DeepSeek `p`/`o`/`v` envelope stream into content + reasoning.
fn parse_deepseek_stream(text: &str, model: &str) -> ParsedStream {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut current_path = String::new();
    let thinking_model = is_thinking_model(model);
    let mut search_results: Vec<DeepSeekSearchResult> = Vec::new();
    let mut saw_finished = false;

    let mut append_by_path =
        |raw: &str, content: &mut String, reasoning: &mut String, current_path: &str| {
            let text = format_stream_content(raw, model);
            if text.is_empty() {
                return;
            }
            let mut path = current_path.to_string();
            if path.is_empty() && thinking_model {
                path = "thinking".to_string();
            } else if path.is_empty() && is_search_model(model) {
                path = "content".to_string();
            }
            if path == "thinking" {
                reasoning.push_str(&text);
            } else {
                content.push_str(&text);
            }
        };

    for line in text.lines() {
        let payload = if let Some(p) = line.strip_prefix("data: ") {
            p.trim()
        } else if let Some(p) = line.strip_prefix("data:") {
            p.trim()
        } else {
            continue;
        };
        if payload == "[DONE]" {
            break;
        }
        let Ok(data) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        let p = data.get("p").and_then(Value::as_str).unwrap_or("");
        let o = data.get("o").and_then(Value::as_str).unwrap_or("");
        let v = data.get("v").cloned().unwrap_or(Value::Null);

        if v.is_object() && v.get("response").is_some() {
            let resp = &v["response"];
            if resp.get("thinking_enabled") == Some(&json!(true)) {
                current_path = "thinking".to_string();
            } else if resp.get("thinking_enabled") == Some(&json!(false)) {
                current_path = "content".to_string();
            }
            if let Some(frags) = resp.get("fragments").and_then(Value::as_array) {
                for frag in frags {
                    handle_fragment(
                        frag,
                        false,
                        &mut current_path,
                        &mut append_by_path,
                        &mut content,
                        &mut reasoning,
                    );
                }
            }
        }
        if p == "response/fragments" {
            match &v {
                Value::Array(arr) => {
                    for frag in arr {
                        handle_fragment(
                            frag,
                            true,
                            &mut current_path,
                            &mut append_by_path,
                            &mut content,
                            &mut reasoning,
                        );
                    }
                }
                Value::Object(_) => {
                    handle_fragment(
                        &v,
                        true,
                        &mut current_path,
                        &mut append_by_path,
                        &mut content,
                        &mut reasoning,
                    );
                }
                _ => {}
            }
        }
        if p == "response" && v.is_array() {
            if let Some(arr) = v.as_array() {
                for entry in arr {
                    if entry.get("p").and_then(Value::as_str) == Some("response")
                        && entry.get("v").and_then(|vv| vv.get("thinking_enabled"))
                            == Some(&json!(true))
                    {
                        current_path = "thinking".to_string();
                    }
                }
            }
        }
        if p == "response/search_results" {
            if let Some(arr) = v.as_array() {
                if o != "BATCH" {
                    search_results = arr.iter().map(DeepSeekSearchResult::from_value).collect();
                } else {
                    for op in arr {
                        if let Some(path) = op.get("p").and_then(Value::as_str) {
                            if let Some(idx) = path
                                .strip_suffix("/cite_index")
                                .and_then(|n| n.parse::<usize>().ok())
                            {
                                if let Some(r) = search_results.get_mut(idx) {
                                    r.cite_index = op.get("v").and_then(Value::as_i64);
                                }
                            }
                        }
                    }
                }
            }
            continue;
        }
        if p == "response/search_status" {
            continue;
        }
        if let Some(s) = v.as_str() {
            append_by_path(s, &mut content, &mut reasoning, &current_path);
        } else if v.is_array() && p == "response" {
            if let Some(arr) = v.as_array() {
                for entry in arr {
                    if let Some(inner) = entry.get("v").and_then(Value::as_array) {
                        let joined = inner
                            .iter()
                            .filter_map(|item| item.get("content").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("");
                        if !joined.is_empty() {
                            append_by_path(&joined, &mut content, &mut reasoning, &current_path);
                        }
                    }
                }
            }
        }
        if p == "response/status" && v == json!("FINISHED") {
            saw_finished = true;
        }
    }

    let citations = append_search_citations(&search_results, model);
    if !citations.is_empty() {
        content.push_str(&format!("\n\n{citations}"));
    }
    ParsedStream {
        content,
        reasoning,
        citations: String::new(),
        saw_finished,
    }
}

fn handle_fragment(
    frag: &Value,
    set_path_from_type: bool,
    current_path: &mut String,
    append: &mut impl FnMut(&str, &mut String, &mut String, &str),
    content: &mut String,
    reasoning: &mut String,
) {
    let frag_type = frag
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_uppercase();
    if set_path_from_type
        || frag_type == "THINK"
        || frag_type == "ANSWER"
        || frag_type == "RESPONSE"
    {
        if frag_type == "THINK" {
            *current_path = "thinking".to_string();
        } else if frag_type == "ANSWER" || frag_type == "RESPONSE" {
            *current_path = "content".to_string();
        }
    }
    if let Some(text) = frag.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            append(text, content, reasoning, current_path);
        }
    }
}

fn sse_chunk(cid: &str, created: u64, model: &str, delta: Value, finish: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    )
}

/// Render a full OpenAI SSE body (role → reasoning → content → citations → stop → DONE).
fn render_openai_sse(
    model: &str,
    role: Option<&str>,
    content: Option<&str>,
    reasoning: Option<&str>,
    citations: &str,
    finish: &str,
) -> String {
    let cid = format!(
        "chatcmpl-{}-{}",
        now_secs(),
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let created = now_secs();
    let mut sse = String::new();
    sse.push_str(&sse_chunk(
        &cid,
        created,
        model,
        json!({"role": role.unwrap_or("assistant"), "content": ""}),
        None,
    ));
    if let Some(r) = reasoning {
        if !r.is_empty() {
            sse.push_str(&sse_chunk(
                &cid,
                created,
                model,
                json!({"reasoning_content": r}),
                None,
            ));
        }
    }
    if let Some(c) = content {
        if !c.is_empty() {
            sse.push_str(&sse_chunk(
                &cid,
                created,
                model,
                json!({"content": c}),
                None,
            ));
        }
    }
    if !citations.is_empty() {
        sse.push_str(&sse_chunk(
            &cid,
            created,
            model,
            json!({"content": format!("\n\n{citations}")}),
            None,
        ));
    }
    sse.push_str(&sse_chunk(&cid, created, model, json!({}), Some(finish)));
    sse.push_str("data: [DONE]\n\n");
    sse
}

/// Build the executor result for a tool-translated reply (#2820): OpenAI
/// `tool_calls` with `finish_reason: "tool_calls"` when parsed, else plain
/// content. Streaming clients get a synthetic SSE; others get JSON.
fn build_tool_aware_result(
    stream: bool,
    client_model: &str,
    content: &str,
    reasoning_content: &str,
    tool_calls: Option<Vec<crate::core::translator::helpers::web_tools::OpenAIToolCall>>,
    req_headers: HeaderMap,
    request_payload: Value,
) -> DeepSeekWebExecutorResponse {
    let has_calls = tool_calls.as_ref().is_some_and(|c| !c.is_empty());
    let finish_reason = if has_calls { "tool_calls" } else { "stop" };
    if stream {
        let cid = format!("chatcmpl-{}", now_secs());
        let created = now_secs();
        let mut sse = String::new();
        sse.push_str(&sse_chunk(
            &cid,
            created,
            client_model,
            json!({"role": "assistant", "content": ""}),
            None,
        ));
        if !reasoning_content.is_empty() {
            sse.push_str(&sse_chunk(
                &cid,
                created,
                client_model,
                json!({"reasoning_content": reasoning_content}),
                None,
            ));
        }
        if !content.is_empty() {
            sse.push_str(&sse_chunk(
                &cid,
                created,
                client_model,
                json!({"content": content}),
                None,
            ));
        }
        if let Some(calls) = &tool_calls {
            if !calls.is_empty() {
                sse.push_str(&sse_chunk(
                    &cid,
                    created,
                    client_model,
                    json!({"tool_calls": calls.iter().enumerate().map(|(i, tc)| json!({"index": i, "id": tc.id, "type": "function", "function": {"name": tc.name, "arguments": tc.arguments}})).collect::<Vec<_>>()}),
                    None,
                ));
            }
        }
        sse.push_str(&sse_chunk(
            &cid,
            created,
            client_model,
            json!({}),
            Some(finish_reason),
        ));
        sse.push_str("data: [DONE]\n\n");
        let mut http_resp = http::Response::new(ReqwestBody::from(sse));
        *http_resp.status_mut() = reqwest::StatusCode::OK;
        http_resp.headers_mut().insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        http_resp.headers_mut().insert(
            reqwest::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        );
        DeepSeekWebExecutorResponse {
            response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
            url: COMPLETION_URL.to_string(),
            headers: req_headers,
            transformed_body: request_payload,
            transport: TransportKind::Reqwest,
        }
    } else {
        let mut message = json!({"role": "assistant", "content": content});
        if !reasoning_content.is_empty() {
            message["reasoning_content"] = json!(reasoning_content);
        }
        if let Some(calls) = tool_calls {
            if !calls.is_empty() {
                message["tool_calls"] =
                    json!(calls.iter().map(|tc| tc.to_json()).collect::<Vec<_>>());
                if content.is_empty() {
                    message["content"] = Value::Null;
                }
            }
        }
        let body = json!({
            "id": format!("chatcmpl-{}", now_secs()),
            "object": "chat.completion",
            "created": now_secs(),
            "model": client_model,
            "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
        });
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
        *http_resp.status_mut() = reqwest::StatusCode::OK;
        http_resp.headers_mut().insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        DeepSeekWebExecutorResponse {
            response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
            url: COMPLETION_URL.to_string(),
            headers: req_headers,
            transformed_body: request_payload,
            transport: TransportKind::Reqwest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_token_rejected_when_empty() {
        assert!(extract_user_token(None, None).is_none());
        assert!(extract_user_token(Some("  "), Some("")).is_none());
    }

    #[test]
    fn fingerprint_headers_match_web_client_v200() {
        // OmniRoute FAKE_HEADERS parity: v2.0.0 bundle id, no legacy X-App-Version.
        let h = fake_headers();
        assert_eq!(h["X-Client-Version"], "2.0.0");
        assert_eq!(h["X-Client-Bundle-Id"], "com.deepseek.chat");
        assert!(!h.contains_key("X-App-Version"));
        assert_eq!(h[reqwest::header::ORIGIN], DEEPSEEK_WEB_BASE);
    }

    #[test]
    fn stream_parser_routes_think_vs_answer() {
        let raw = "data: {\"p\":\"response/fragments\",\"v\":[{\"type\":\"THINK\",\"content\":\"hmm\"},{\"type\":\"ANSWER\",\"content\":\"hi\"}]}\n\ndata: {\"p\":\"response/status\",\"v\":\"FINISHED\"}\n";
        let parsed = parse_deepseek_stream(raw, "deepseek-v4-pro-think");
        assert_eq!(parsed.reasoning, "hmm");
        assert_eq!(parsed.content, "hi");
        assert!(parsed.saw_finished);
    }

    #[test]
    fn stream_parser_rejects_truncated_session() {
        // No FINISHED → caller surfaces 502 instead of a fake "stop".
        let raw = "data: {\"p\":\"response/fragments\",\"v\":[{\"type\":\"ANSWER\",\"content\":\"partial\"}]}\n";
        let parsed = parse_deepseek_stream(raw, "deepseek-chat");
        assert!(!parsed.saw_finished);
    }

    #[test]
    fn completion_url_is_web_endpoint() {
        assert_eq!(
            COMPLETION_URL,
            "https://chat.deepseek.com/api/v0/chat/completion"
        );
    }
}
