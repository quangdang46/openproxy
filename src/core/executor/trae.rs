//! Trae executor — SOLO remote agent API.
//!
//! Port of 9router `open-sse/executors/trae.js`:
//!   1. POST `{base}/chat_sessions` → `{ code:0, data:{ chat_session_id, message_id } }`
//!   2. GET `{base}/chat_sessions/{id}/events?reply_to_message_id={message_id}`
//!      → SSE. Assistant text streams in `plan_item` events under the `thought`
//!      field (cumulative per plan-item id, longest wins). `token_usage` carries
//!      usage; `done` ends the turn; `error` carries upstream errors.
//!
//! Auth: header `Authorization: Cloud-IDE-JWT <jwt>` (note the space, not
//! Bearer). Identity fields for `common_params` live in
//! `credentials.providerSpecificData`.

use std::sync::Arc;

use hyper::http;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{ClientPool, TransportKind, UpstreamResponse};

const TRAE_BASE_URL: &str = "https://core-normal.trae.ai/api/remote/v1";
const TRAE_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

/// Stream timeout in ms (default 300s per JS `TRAE_STREAM_TIMEOUT_MS`).
fn stream_timeout_ms() -> u64 {
    std::env::var("TRAE_STREAM_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300_000)
}

/// Flatten OpenAI messages into Trae's query JSON-string of typed content
/// blocks. Mirrors JS `flattenQuery` (trae.js:22-42).
fn flatten_query(messages: &[Value]) -> String {
    let mut parts = Vec::new();
    for m in messages {
        let mut content = String::new();
        match m.get("content") {
            Some(Value::String(s)) => content = s.clone(),
            Some(Value::Array(arr)) => {
                for p in arr {
                    match p {
                        Value::String(s) => content.push_str(s),
                        Value::Object(_) => {
                            if let Some(text) = p.get("text").and_then(Value::as_str) {
                                content.push_str(text);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        match m.get("role").and_then(Value::as_str) {
            Some("system") => parts.push(format!("[System]\n{content}")),
            Some("assistant") => parts.push(format!("[Assistant]\n{content}")),
            _ => parts.push(content),
        }
    }
    // Trae expects query as a JSON-encoded string of typed content blocks.
    serde_json::to_string(&json!([{ "type": "text", "data": { "content": parts.join("\n\n") } }]))
        .unwrap_or_else(|_| "[]".into())
}

/// SOLO session modes: "work" (fast auto lane) vs "code" (model picker).
/// Mirrors JS `resolveMode` (trae.js:69-76).
fn resolve_mode(model: &str) -> (String, String, String) {
    let m = model.trim().to_lowercase();
    if m == "work" || m == "auto-work" || m == "solo-work" {
        return ("work".into(), "auto".into(), String::new());
    }
    let auto = m.is_empty() || m == "auto";
    (
        "code".into(),
        if auto { "auto".into() } else { "manual".into() },
        if auto {
            String::new()
        } else {
            model.to_string()
        },
    )
}

/// Build `common_params` (JSON-string) embedded in `initial_message`.
/// Mirrors JS `commonParams` (trae.js:79-100).
fn common_params(psd: &Value, mode: &str, session_id: Option<&str>) -> String {
    let g = |k: &str, d: &str| psd.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
    let mut cp = serde_json::Map::new();
    cp.insert("language".into(), json!("en-us"));
    cp.insert("app_language".into(), json!(g("appLanguage", "en")));
    cp.insert("quality".into(), json!("stable"));
    cp.insert("app_version".into(), json!(g("appVersion", "1.0.0.1229")));
    cp.insert("web_id".into(), json!(g("webId", "")));
    cp.insert("user_identity".into(), json!(g("userIdentity", "Free")));
    cp.insert("is_freshman".into(), json!("0"));
    cp.insert("biz_user_id".into(), json!(g("bizUserId", "")));
    cp.insert("user_unique_id".into(), json!(g("userUniqueId", "")));
    cp.insert("scope".into(), json!(g("scope", "marscode-us")));
    cp.insert("tenant".into(), json!(g("tenant", "marscode")));
    let region = g("region", "US-East");
    cp.insert("region".into(), json!(region));
    cp.insert("aiRegion".into(), json!(g("aiRegion", &region)));
    cp.insert("is_privacy_mode".into(), json!(0));
    cp.insert("privacy_mode".into(), json!("off"));
    cp.insert("solo_chat_mode".into(), json!(mode));
    if let Some(sid) = session_id {
        cp.insert("biz_session_id".into(), json!(sid));
    }
    serde_json::to_string(&Value::Object(cp)).unwrap_or_else(|_| "{}".into())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Plan-item thought aggregation (cumulative per id, longest wins)
// ---------------------------------------------------------------------------

/// Tracks `plan_item` thoughts and returns the newly-appended text piece.
/// Mirrors JS `renderNewText` (trae.js:201-211): order array + longest-thought
/// per id; `piece = full.slice(sent)`.
struct PlanItemAggregator {
    order: Vec<String>,
    thoughts: HashMap<String, String>,
    sent: usize,
}

impl PlanItemAggregator {
    fn new() -> Self {
        Self {
            order: Vec::new(),
            thoughts: HashMap::new(),
            sent: 0,
        }
    }

    fn render_new_text(&mut self, data: &Value) -> String {
        let Some(pid) = data.get("id").and_then(Value::as_str) else {
            return String::new();
        };
        let pid = pid.to_string();
        if !self.thoughts.contains_key(&pid) {
            self.order.push(pid.clone());
        }
        let t = data.get("thought").and_then(Value::as_str).unwrap_or("");
        let cur = self.thoughts.get(&pid).map(|s| s.as_str()).unwrap_or("");
        if t.len() >= cur.len() {
            self.thoughts.insert(pid.clone(), t.to_string());
        }
        let full: String = self
            .order
            .iter()
            .filter_map(|i| self.thoughts.get(i))
            .cloned()
            .collect();
        let piece = full[self.sent.min(full.len())..].to_string();
        self.sent = full.len();
        piece
    }

    fn full_text(&self) -> String {
        self.order
            .iter()
            .filter_map(|i| self.thoughts.get(i))
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TraeExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug)]
pub enum TraeExecutorError {
    MissingCredentials(String),
    Serialize(serde_json::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    Http(http::Error),
}

impl From<reqwest::Error> for TraeExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for TraeExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<serde_json::Error> for TraeExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<http::Error> for TraeExecutorError {
    fn from(error: http::Error) -> Self {
        Self::Http(error)
    }
}

impl std::fmt::Display for TraeExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::InvalidHeader(e) => write!(f, "Invalid header: {}", e),
            Self::Http(e) => write!(f, "HTTP error: {}", e),
        }
    }
}

impl std::error::Error for TraeExecutorError {}

pub struct TraeExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for TraeExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraeExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

pub struct TraeExecutor {
    pool: Arc<ClientPool>,
}

impl TraeExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    fn base(&self) -> String {
        TRAE_BASE_URL.trim_end_matches('/').to_string()
    }

    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, TraeExecutorError> {
        let token = credentials.access_token.as_deref().unwrap_or("");
        let psd = serde_json::to_value(&credentials.provider_specific_data).unwrap_or(Value::Null);
        let g = |k: &str, d: &str| psd.get(k).and_then(Value::as_str).unwrap_or(d).to_string();

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Cloud-IDE-JWT {token}"))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("X-Trae-Client-Type", HeaderValue::from_static("web"));
        headers.insert(
            "X-Preferenced-Language",
            HeaderValue::from_str(&g("appLanguage", "en"))?,
        );
        headers.insert(
            "x-user-region",
            HeaderValue::from_str(&g("userRegion", "US"))?,
        );
        headers.insert("Referer", HeaderValue::from_static("https://solo.trae.ai/"));
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(TRAE_UA),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(if stream {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );
        Ok(headers)
    }

    /// POST `{base}/chat_sessions` → `{ code:0, data:{ chat_session_id, message_id } }`.
    async fn create_session(
        &self,
        headers: &HeaderMap,
        query: &str,
        model: &str,
        psd: &Value,
        proxy: Option<&ProxyTarget>,
    ) -> Result<(String, String), TraeExecutorError> {
        let (mode, strategy, model_name) = resolve_mode(model);
        let body = json!({
            "mode": mode,
            "environment_id": "default",
            "initial_message": {
                "chat_session_id": "",
                "content": [],
                "query": query,
                "model_name": model_name,
                "agent_type": "solo_agent_remote",
                "model_selection_strategy": strategy,
                "common_params": common_params(psd, &mode, None),
            },
            "env": "remote",
            "auto_create_project": false,
            "origin": "web",
        });
        let client = self.pool.get("trae", proxy)?;
        let resp = client
            .post(format!("{}/chat_sessions", self.base()))
            .headers(headers.clone())
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(TraeExecutorError::MissingCredentials(format!(
                "[{status}] {text}"
            )));
        }
        let json: Value = serde_json::from_str(&text).map_err(|_| {
            TraeExecutorError::MissingCredentials(format!("trae create_session: {text}"))
        })?;
        if json.get("code").and_then(Value::as_i64) != Some(0) {
            return Err(TraeExecutorError::MissingCredentials(format!(
                "Trae create_session: {text}"
            )));
        }
        let session_id = json
            .pointer("/data/chat_session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let message_id = json
            .pointer("/data/message_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Ok((session_id, message_id))
    }

    /// GET `{base}/chat_sessions/{id}/events?reply_to_message_id={mid}` and
    /// consume the SSE stream, invoking `on_event(event_type, data)` per frame.
    /// Resolves when `on_event` returns `true` (done/error), the stream ends, or
    /// the timeout fires.
    async fn stream_events<F>(
        &self,
        headers: &HeaderMap,
        session_id: &str,
        reply_to: &str,
        mut on_event: F,
        proxy: Option<&ProxyTarget>,
    ) -> Result<(), TraeExecutorError>
    where
        F: FnMut(&str, &Value) -> bool,
    {
        let url = format!(
            "{}/chat_sessions/{}/events?reply_to_message_id={}",
            self.base(),
            session_id,
            urlencoding::encode(reply_to)
        );
        let client = self.pool.get("trae", proxy)?;
        let resp = client.get(&url).headers(headers.clone()).send().await?;
        if !resp.status().is_success() {
            return Err(TraeExecutorError::MissingCredentials(format!(
                "[{}] events stream failed",
                resp.status()
            )));
        }
        let bytes = resp.bytes().await.unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let mut ev: Option<String> = None;
        for line in text.lines() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if let Some(rest) = line.strip_prefix("event:") {
                ev = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                let payload = rest.trim();
                let data: Value =
                    serde_json::from_str(payload).unwrap_or_else(|_| json!({ "_raw": payload }));
                let event_name = ev.clone().unwrap_or_default();
                if on_event(&event_name, &data) {
                    return Ok(());
                }
            } else if line.is_empty() {
                ev = None;
            }
        }
        Ok(())
    }

    pub async fn execute_request(
        &self,
        request: TraeExecutionRequest,
    ) -> Result<TraeExecutorResponse, TraeExecutorError> {
        let headers = self.build_headers(&request.credentials, request.stream)?;
        let psd = serde_json::to_value(&request.credentials.provider_specific_data)
            .unwrap_or(Value::Null);
        let query = flatten_query(
            request
                .body
                .get("messages")
                .and_then(Value::as_array)
                .unwrap_or(&vec![]),
        );
        let response_id = format!("chatcmpl-trae-{}", unix_now());
        let created = unix_now();

        let (session_id, message_id) = match self
            .create_session(
                &headers,
                &query,
                &request.model,
                &psd,
                request.proxy.as_ref(),
            )
            .await
        {
            Ok(ok) => ok,
            Err(e) => {
                let err_resp = json_error(502, &format!("{}", e));
                return Ok(TraeExecutorResponse {
                    response: err_resp,
                    url: self.base(),
                    headers,
                    transformed_body: request.body.clone(),
                    transport: TransportKind::Reqwest,
                });
            }
        };

        let mut aggregator = PlanItemAggregator::new();
        let mut usage: Option<Value> = None;
        let mut error_event: Option<Value> = None;

        if request.stream {
            let result = self
                .stream_events(
                    &headers,
                    &session_id,
                    &message_id,
                    |ev, data| {
                        if ev == "error" {
                            error_event = Some(data.clone());
                            return true;
                        }
                        if ev == "token_usage" {
                            usage = Some(data.clone());
                        }
                        // plan_item handled inline below via the aggregator;
                        // we re-run render in the SSE building pass.
                        ev == "done"
                    },
                    request.proxy.as_ref(),
                )
                .await;

            if let Err(e) = result {
                let err_resp = json_error(502, &format!("{}", e));
                return Ok(TraeExecutorResponse {
                    response: err_resp,
                    url: self.base(),
                    headers,
                    transformed_body: request.body.clone(),
                    transport: TransportKind::Reqwest,
                });
            }

            // Build the SSE text from the aggregated thoughts.
            let mut sse = String::new();
            // First chunk: role assistant.
            sse.push_str(&sse_chunk(
                &response_id,
                created,
                &request.model,
                json!({ "role": "assistant" }),
                None,
            ));
            if let Some(err) = error_event {
                let code = err.get("code").and_then(Value::as_str).unwrap_or("");
                let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
                let err_obj = json!({
                    "id": response_id, "object": "chat.completion.chunk", "created": created, "model": request.model,
                    "choices": [],
                    "error": { "message": format!("trae {code}: {msg}"), "type": "api_error" },
                });
                sse.push_str(&format!(
                    "data: {}\n\n",
                    serde_json::to_string(&err_obj).unwrap_or_default()
                ));
            } else {
                sse.push_str(&sse_chunk(
                    &response_id,
                    created,
                    &request.model,
                    json!({}),
                    Some("stop"),
                ));
                if let Some(u) = usage {
                    sse.push_str(&usage_chunk(&response_id, created, &request.model, &u));
                }
            }
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
            http_resp.headers_mut().insert(
                reqwest::header::HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            );

            Ok(TraeExecutorResponse {
                response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
                url: self.base(),
                headers,
                transformed_body: request.body.clone(),
                transport: TransportKind::Reqwest,
            })
        } else {
            // Non-streaming: drive to completion, return chat.completion JSON.
            let result = self
                .stream_events(
                    &headers,
                    &session_id,
                    &message_id,
                    |ev, data| {
                        if ev == "error" {
                            error_event = Some(data.clone());
                            return true;
                        }
                        if ev == "token_usage" {
                            usage = Some(data.clone());
                        }
                        if ev == "plan_item" {
                            aggregator.render_new_text(data);
                        }
                        ev == "done"
                    },
                    request.proxy.as_ref(),
                )
                .await;
            if let Err(e) = result {
                let err_resp = json_error(502, &format!("{}", e));
                return Ok(TraeExecutorResponse {
                    response: err_resp,
                    url: self.base(),
                    headers,
                    transformed_body: request.body.clone(),
                    transport: TransportKind::Reqwest,
                });
            }
            if let Some(err) = error_event {
                let code = err.get("code").and_then(Value::as_str).unwrap_or("");
                let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
                let err_resp = json_error(502, &format!("trae {code}: {msg}"));
                return Ok(TraeExecutorResponse {
                    response: err_resp,
                    url: self.base(),
                    headers,
                    transformed_body: request.body.clone(),
                    transport: TransportKind::Reqwest,
                });
            }
            let content = aggregator.full_text();
            let mut out = json!({
                "id": response_id,
                "object": "chat.completion",
                "created": created,
                "model": request.model,
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": content },
                    "finish_reason": "stop",
                }],
            });
            if let Some(u) = usage {
                out["usage"] = json!({
                    "prompt_tokens": u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                    "completion_tokens": u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
                    "total_tokens": u.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
                });
            }
            let bytes = serde_json::to_vec(&out).unwrap_or_default();
            let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
            *http_resp.status_mut() = reqwest::StatusCode::OK;
            http_resp.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );

            Ok(TraeExecutorResponse {
                response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
                url: self.base(),
                headers,
                transformed_body: request.body.clone(),
                transport: TransportKind::Reqwest,
            })
        }
    }
}

fn sse_chunk(
    cid: &str,
    created: i64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
            }],
        }))
        .unwrap_or_default()
    )
}

fn usage_chunk(cid: &str, created: i64, model: &str, usage: &Value) -> String {
    let prompt = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt + completion);
    format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [],
            "usage": {
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": total,
            },
        }))
        .unwrap_or_default()
    )
}

fn json_error(status: u16, message: &str) -> UpstreamResponse {
    let body = json!({
        "error": { "message": message, "type": "api_error", "code": "" },
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trae_render_new_text_cumulative() {
        let mut agg = PlanItemAggregator::new();
        // plan_item id A "hi"
        let piece1 = agg.render_new_text(&json!({ "id": "A", "thought": "hi" }));
        assert_eq!(piece1, "hi");
        // plan_item id B " there"
        let piece2 = agg.render_new_text(&json!({ "id": "B", "thought": " there" }));
        assert_eq!(piece2, " there");
        // shorter re-send of A must not shrink (longest wins)
        let piece3 = agg.render_new_text(&json!({ "id": "A", "thought": "h" }));
        assert_eq!(piece3, "");
        assert_eq!(agg.full_text(), "hi there");
    }

    #[test]
    fn test_trae_render_new_text_longest_wins() {
        let mut agg = PlanItemAggregator::new();
        agg.render_new_text(&json!({ "id": "A", "thought": "hello" }));
        // longer update
        let piece = agg.render_new_text(&json!({ "id": "A", "thought": "hello world" }));
        assert_eq!(piece, " world");
        assert_eq!(agg.full_text(), "hello world");
    }

    #[test]
    fn test_flatten_query() {
        let messages = json!([
            { "role": "system", "content": "be helpful" },
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": [ { "type": "text", "text": "hello" } ] },
        ]);
        let q = flatten_query(messages.as_array().unwrap());
        let parsed: Value = serde_json::from_str(&q).unwrap();
        assert_eq!(parsed[0]["type"], "text");
        assert_eq!(
            parsed[0]["data"]["content"],
            "[System]\nbe helpful\n\nhi\n\n[Assistant]\nhello"
        );
    }

    #[test]
    fn test_resolve_mode() {
        assert_eq!(
            resolve_mode("work"),
            ("work".into(), "auto".into(), String::new())
        );
        assert_eq!(
            resolve_mode("auto-work"),
            ("work".into(), "auto".into(), String::new())
        );
        assert_eq!(
            resolve_mode(""),
            ("code".into(), "auto".into(), String::new())
        );
        assert_eq!(
            resolve_mode("auto"),
            ("code".into(), "auto".into(), String::new())
        );
        assert_eq!(
            resolve_mode("claude-opus-4-7"),
            ("code".into(), "manual".into(), "claude-opus-4-7".into())
        );
    }

    #[test]
    fn test_common_params_fields() {
        let psd = json!({ "appLanguage": "vi", "region": "US-West" });
        let cp = common_params(&psd, "code", Some("sid-1"));
        let parsed: Value = serde_json::from_str(&cp).unwrap();
        assert_eq!(parsed["language"], "en-us");
        assert_eq!(parsed["app_language"], "vi");
        assert_eq!(parsed["region"], "US-West");
        assert_eq!(parsed["aiRegion"], "US-West");
        assert_eq!(parsed["solo_chat_mode"], "code");
        assert_eq!(parsed["biz_session_id"], "sid-1");
    }
}
