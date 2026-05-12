//! Runtime HTTP client for CLI commands that need a live `openproxy` server.
//!
//! M4 (observability, runtime usage/logs/quota/chat, oauth) commands cannot
//! work against `db.json` alone — they need the in-process state of the
//! running server (live usage events, oauth device flows, chat completions
//! that route through the proxy). This module owns the small reqwest client
//! the M4 subcommands share.
//!
//! Design points:
//!
//! 1. **Endpoint resolution**. We prefer the remote `--url` from the resolved
//!    config (if set), otherwise we read the `openproxy.endpoint` sidecar
//!    written by `server start` and dial `http://127.0.0.1:<port>`. This
//!    matches what `server status` already does (see `cli::server`).
//! 2. **Auth**. We send `x-api-key` if the resolved config has one, otherwise
//!    we try to pick the first active API key out of `db.json` (local mode
//!    only, agent-friendly). Remote mode without a key is an error.
//! 3. **Failure mode**. Every command calls `Runtime::ensure_alive()` before
//!    doing any real work; if the health probe fails we emit a `server_unreachable`
//!    error (exit code 6) with a hint pointing the user at `server start --detach`.
//! 4. **NDJSON streams**. `Runtime::stream_sse` reads `text/event-stream`
//!    responses, splits on `data:` payloads, and yields each one as `Bytes`
//!    so the caller can re-emit it as a single NDJSON envelope. Streams run
//!    until Ctrl+C or until the server closes the connection — no timeout.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use futures_util::stream::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde_json::Value;

use crate::cli::config::ResolvedConfig;
use crate::cli::server::{read_endpoint, PID_FILE};
use crate::db::Db;

/// Default port a local OpenProxy server binds to (matches `Cli::port`).
pub const DEFAULT_LOCAL_PORT: u16 = 4623;

/// Wall-clock timeout for a single non-streaming HTTP call. Streaming calls
/// override the request timeout to `None` so they can run indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Timeout for the initial `/api/health` probe in `ensure_alive`. Kept short
/// so a dead server fails fast instead of stalling the user.
const HEALTH_TIMEOUT: Duration = Duration::from_millis(1500);

/// The shared HTTP client + base URL + auth header used by all runtime
/// commands. Cheap to clone — `reqwest::Client` is `Arc` internally.
#[derive(Clone)]
pub struct Runtime {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl Runtime {
    /// Build a runtime client from the resolved config. Does not perform any
    /// I/O; call `ensure_alive` before issuing real requests if you want the
    /// canonical "server not running" exit code 6 behavior.
    pub async fn from_config(cfg: &ResolvedConfig) -> anyhow::Result<Self> {
        let base_url = resolve_base_url(cfg)?;
        let api_key = resolve_api_key(cfg).await;

        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("build runtime http client")?;

        Ok(Self {
            client,
            base_url,
            api_key,
        })
    }

    /// Same as `from_config` but does not auto-resolve an API key from the
    /// local `db.json`. Used by commands like `provider oauth start` that
    /// hit unauthenticated endpoints.
    pub fn from_config_no_auth(cfg: &ResolvedConfig) -> anyhow::Result<Self> {
        let base_url = resolve_base_url(cfg)?;
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("build runtime http client")?;
        Ok(Self {
            client,
            base_url,
            api_key: cfg.api_key.clone(),
        })
    }

    /// Base URL the client is dialing (without trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Whether the runtime has an API key to send.
    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// Probe `/api/health`. Returns `Err(RuntimeError::Unreachable)` if the
    /// server is down so callers can map to exit code 6 uniformly.
    pub async fn ensure_alive(&self) -> Result<(), RuntimeError> {
        let url = format!("{}/api/health", self.base_url);
        let res = self.client.get(&url).timeout(HEALTH_TIMEOUT).send().await;
        match res {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => Err(RuntimeError::Unreachable {
                url,
                detail: format!("HTTP {}", r.status()),
            }),
            Err(e) => Err(RuntimeError::Unreachable {
                url,
                detail: e.to_string(),
            }),
        }
    }

    /// GET a JSON resource. Returns the parsed body or a `RuntimeError`.
    pub async fn get_json(&self, path: &str) -> Result<Value, RuntimeError> {
        let res = self
            .request(Method::GET, path)
            .send()
            .await
            .map_err(map_err)?;
        decode_json(res).await
    }

    /// GET with query string parameters (encoded by reqwest).
    pub async fn get_json_query<Q: serde::Serialize + ?Sized>(
        &self,
        path: &str,
        query: &Q,
    ) -> Result<Value, RuntimeError> {
        let res = self
            .request(Method::GET, path)
            .query(query)
            .send()
            .await
            .map_err(map_err)?;
        decode_json(res).await
    }

    /// POST a JSON body and decode the JSON response.
    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value, RuntimeError> {
        let res = self
            .request(Method::POST, path)
            .json(body)
            .send()
            .await
            .map_err(map_err)?;
        decode_json(res).await
    }

    /// POST without a body — useful for action endpoints like `/api/observability/clear`.
    pub async fn post_empty(&self, path: &str) -> Result<Value, RuntimeError> {
        let res = self
            .request(Method::POST, path)
            .send()
            .await
            .map_err(map_err)?;
        decode_json(res).await
    }

    /// Open an SSE / NDJSON stream against `path`. Each `data: ...` chunk is
    /// yielded as raw bytes; comment / ping lines (`: ...`) are skipped.
    /// The stream runs until the server closes or the caller drops it.
    pub async fn stream_sse(
        &self,
        path: &str,
    ) -> Result<impl Stream<Item = Result<Bytes, RuntimeError>> + Send + Unpin, RuntimeError> {
        let res = self
            .stream_client()?
            .request(Method::GET, format!("{}{}", self.base_url, path))
            .headers(self.auth_headers())
            .header(ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(map_err)?;

        if !res.status().is_success() {
            return Err(map_http_status(res).await);
        }

        let byte_stream = res.bytes_stream().map(|chunk| chunk.map_err(map_err));
        Ok(Box::pin(SseFrames::new(byte_stream)))
    }

    /// POST a JSON body and consume the response as SSE.
    pub async fn post_stream_sse(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<impl Stream<Item = Result<Bytes, RuntimeError>> + Send + Unpin, RuntimeError> {
        let res = self
            .stream_client()?
            .request(Method::POST, format!("{}{}", self.base_url, path))
            .headers(self.auth_headers())
            .header(ACCEPT, "text/event-stream")
            .json(body)
            .send()
            .await
            .map_err(map_err)?;

        if !res.status().is_success() {
            return Err(map_http_status(res).await);
        }

        let byte_stream = res.bytes_stream().map(|chunk| chunk.map_err(map_err));
        Ok(Box::pin(SseFrames::new(byte_stream)))
    }

    fn stream_client(&self) -> Result<Client, RuntimeError> {
        Client::builder()
            .build()
            .map_err(|e| RuntimeError::Network(e.to_string()))
    }

    /// Build a request with the canonical auth/accept headers attached.
    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{}", self.base_url, path))
            .headers(self.auth_headers())
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if let Some(key) = &self.api_key {
            if let Ok(v) = HeaderValue::from_str(key) {
                h.insert("x-api-key", v.clone());
                if let Ok(bearer) = HeaderValue::from_str(&format!("Bearer {key}")) {
                    h.insert(AUTHORIZATION, bearer);
                }
            }
        }
        h
    }
}

/// Resolve the base URL we should dial.
///
/// 1. `--url` / `OPENPROXY_URL` if set on the resolved config.
/// 2. The `openproxy.endpoint` sidecar written by `server start --detach`.
/// 3. `http://127.0.0.1:<DEFAULT_LOCAL_PORT>` as a last-ditch default so the
///    `usage` etc. commands still produce a deterministic "not running"
///    error rather than panicking.
fn resolve_base_url(cfg: &ResolvedConfig) -> anyhow::Result<String> {
    if let Some(url) = &cfg.remote_url {
        return Ok(url.trim_end_matches('/').to_string());
    }
    if let Some((host, port)) = read_endpoint(&cfg.data_dir) {
        let dial_host = if host == "0.0.0.0" || host == "::" || host.is_empty() {
            "127.0.0.1".to_string()
        } else {
            host
        };
        return Ok(format!("http://{dial_host}:{port}"));
    }
    Ok(format!("http://127.0.0.1:{DEFAULT_LOCAL_PORT}"))
}

/// Pick an API key to authenticate runtime calls.
///
/// 1. `--api-key` / `OPENPROXY_API_KEY` if set.
/// 2. First active key from `db.json` (local mode only — assumes the CLI
///    user already has filesystem access to the data dir).
async fn resolve_api_key(cfg: &ResolvedConfig) -> Option<String> {
    if let Some(key) = &cfg.api_key {
        if !key.trim().is_empty() {
            return Some(key.trim().to_string());
        }
    }
    let db = Db::load_from(&cfg.data_dir).await.ok()?;
    let snap = db.snapshot();
    snap.api_keys
        .iter()
        .find(|k| k.is_active())
        .map(|k| k.key.clone())
}

/// Path of the pid sidecar file. Re-exported for debugging.
pub fn pid_file_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(PID_FILE)
}

/// Errors the runtime client can produce. Mapped to exit codes by the caller
/// via `RuntimeError::exit_code`.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("server unreachable at {url}: {detail}")]
    Unreachable { url: String, detail: String },
    #[error("auth error: {0}")]
    Auth(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("server returned HTTP {status}: {message}")]
    Http { status: StatusCode, message: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("decode error: {0}")]
    Decode(String),
}

impl RuntimeError {
    /// Stable error code string used by `emit_error`. Maps via
    /// `output::exit_code_for` to the conventional CLI exit codes.
    pub fn code(&self) -> &'static str {
        match self {
            RuntimeError::Unreachable { .. } => "server_unreachable",
            RuntimeError::Auth(_) => "auth",
            RuntimeError::NotFound(_) => "not_found",
            RuntimeError::Http { status, .. } if status.as_u16() == 404 => "not_found",
            RuntimeError::Http { status, .. } if status.as_u16() == 401 => "auth",
            RuntimeError::Http { status, .. } if status.as_u16() == 403 => "auth",
            RuntimeError::Http { .. } => "other",
            RuntimeError::Network(_) => "network",
            RuntimeError::Decode(_) => "other",
        }
    }

    pub fn exit_code(&self) -> i32 {
        crate::cli::output::exit_code_for(self.code())
    }
}

fn map_err(err: reqwest::Error) -> RuntimeError {
    if err.is_timeout() || err.is_connect() {
        RuntimeError::Unreachable {
            url: err
                .url()
                .map(|u| u.to_string())
                .unwrap_or_else(|| "<unknown>".to_string()),
            detail: err.to_string(),
        }
    } else {
        RuntimeError::Network(err.to_string())
    }
}

async fn map_http_status(res: Response) -> RuntimeError {
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    let message = if text.is_empty() {
        status.canonical_reason().unwrap_or("").to_string()
    } else {
        text
    };
    RuntimeError::Http { status, message }
}

async fn decode_json(res: Response) -> Result<Value, RuntimeError> {
    if !res.status().is_success() {
        return Err(map_http_status(res).await);
    }
    let bytes = res.bytes().await.map_err(map_err)?;
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| RuntimeError::Decode(format!("invalid JSON response: {e}")))
}

/// Adapter that converts a raw byte stream of SSE chunks into a stream of
/// `data:` payload bytes. Comment lines (`:`-prefixed) and event headers are
/// dropped so callers see only the JSON body each event carries.
struct SseFrames<S> {
    inner: S,
    buf: String,
    pending: std::collections::VecDeque<Bytes>,
}

impl<S> SseFrames<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            buf: String::new(),
            pending: std::collections::VecDeque::new(),
        }
    }

    fn drain_buffer(&mut self) {
        // SSE frames are delimited by a blank line. Each frame is a sequence
        // of `field: value` lines; we only care about `data:` lines and we
        // concatenate consecutive `data:` lines per the spec.
        while let Some(idx) = find_blank_line(&self.buf) {
            let frame = self.buf[..idx].to_string();
            // Trim the blank line delimiter (\n\n or \r\n\r\n).
            let cut = idx + blank_line_len(&self.buf, idx);
            self.buf.drain(..cut);

            let mut data = String::new();
            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(rest.trim_start());
                }
            }
            if !data.is_empty() {
                self.pending.push_back(Bytes::from(data));
            }
        }
    }
}

fn find_blank_line(buf: &str) -> Option<usize> {
    // Accept LF-only or CRLF terminators.
    let lf = buf.find("\n\n");
    let crlf = buf.find("\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn blank_line_len(buf: &str, idx: usize) -> usize {
    if buf[idx..].starts_with("\r\n\r\n") {
        4
    } else {
        2
    }
}

impl<S> Stream for SseFrames<S>
where
    S: Stream<Item = Result<Bytes, RuntimeError>> + Send + Unpin,
{
    type Item = Result<Bytes, RuntimeError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        loop {
            if let Some(b) = self.pending.pop_front() {
                return std::task::Poll::Ready(Some(Ok(b)));
            }
            match self.inner.poll_next_unpin(cx) {
                std::task::Poll::Pending => return std::task::Poll::Pending,
                std::task::Poll::Ready(None) => {
                    if !self.buf.is_empty() {
                        // Flush any trailing frame that did not end with a blank line.
                        let leftover = std::mem::take(&mut self.buf);
                        if let Some(rest) = leftover.strip_prefix("data:") {
                            return std::task::Poll::Ready(Some(Ok(Bytes::from(
                                rest.trim_start().to_string(),
                            ))));
                        }
                    }
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    return std::task::Poll::Ready(Some(Err(e)))
                }
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    match std::str::from_utf8(&chunk) {
                        Ok(s) => self.buf.push_str(s),
                        Err(_) => {
                            return std::task::Poll::Ready(Some(Err(RuntimeError::Decode(
                                "non-UTF8 SSE chunk".to_string(),
                            ))))
                        }
                    }
                    self.drain_buffer();
                }
            }
        }
    }
}

/// Read raw stdin into a string. Used by commands that take `--prompt -`,
/// `--from-file -`, etc.
pub fn read_stdin_to_string() -> anyhow::Result<String> {
    use std::io::Read;
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .context("read stdin")?;
    Ok(s)
}

/// Read stdin (or a file path) into a string. `-` means stdin.
pub fn read_input(from: &str) -> anyhow::Result<String> {
    if from == "-" {
        read_stdin_to_string()
    } else {
        std::fs::read_to_string(from).with_context(|| format!("read input file '{from}'"))
    }
}

/// Helper: convert `RuntimeError` to a user-facing `emit_error` exit code.
/// Always returns `Ok(exit_code)` so callers can do
/// `let exit = rt_error_to_exit(ctx, e)?;` and propagate cleanly.
pub fn rt_error_to_exit(
    ctx: crate::cli::output::OutputCtx,
    err: RuntimeError,
) -> anyhow::Result<i32> {
    let code = err.code();
    let message = match &err {
        RuntimeError::Unreachable { url, detail } => format!(
            "server not running ({url}: {detail}). Start it with: openproxy server start --detach"
        ),
        RuntimeError::Auth(m) => format!("auth: {m}"),
        RuntimeError::NotFound(m) => format!("not found: {m}"),
        RuntimeError::Http { status, message } => format!("server returned {status}: {message}"),
        RuntimeError::Network(m) => format!("network: {m}"),
        RuntimeError::Decode(m) => format!("decode: {m}"),
    };
    Ok(crate::cli::output::emit_error(ctx, code, &message)?)
}

/// Convenience: build a `Runtime`, probe `/api/health`, and return the
/// canonical "server not running" error on failure. Returns the runtime if
/// the server is up.
pub async fn require_runtime(cfg: &ResolvedConfig) -> Result<Runtime, RuntimeError> {
    let rt = Runtime::from_config(cfg)
        .await
        .map_err(|e| RuntimeError::Network(e.to_string()))?;
    rt.ensure_alive().await?;
    Ok(rt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    #[test]
    fn find_blank_line_detects_both_lf_and_crlf() {
        assert_eq!(find_blank_line("data: a\n\nrest"), Some(7));
        assert_eq!(find_blank_line("data: a\r\n\r\nrest"), Some(7));
        assert_eq!(find_blank_line("no frame yet"), None);
    }

    #[tokio::test]
    async fn sse_frames_extracts_data_payloads() {
        let chunks = vec![
            Ok(Bytes::from_static(b"data: {\"a\":1}\n\n")),
            Ok(Bytes::from_static(b": ping\n\n")),
            Ok(Bytes::from_static(b"data: hel")),
            Ok(Bytes::from_static(b"lo\n\n")),
        ];
        let raw = stream::iter(chunks);
        let mut frames = SseFrames::new(raw);

        let first = frames.next().await.unwrap().unwrap();
        assert_eq!(&first[..], b"{\"a\":1}");

        let second = frames.next().await.unwrap().unwrap();
        assert_eq!(&second[..], b"hello");

        assert!(frames.next().await.is_none());
    }

    #[test]
    fn runtime_error_maps_known_codes() {
        let unreach = RuntimeError::Unreachable {
            url: "http://x".into(),
            detail: "refused".into(),
        };
        assert_eq!(unreach.code(), "server_unreachable");
        assert_eq!(unreach.exit_code(), 6);

        let nf = RuntimeError::NotFound("missing".into());
        assert_eq!(nf.exit_code(), 3);

        let auth = RuntimeError::Http {
            status: StatusCode::UNAUTHORIZED,
            message: "bad".into(),
        };
        assert_eq!(auth.code(), "auth");
    }
}
