//! Speech-to-text (`/v1/audio/transcriptions`) pipeline.
//!
//! Ported from the upstream 9router `open-sse/handlers/sttCore.js` + `src/sse/handlers/stt.js`.
//!
//! Accepts either OpenAI-compatible **multipart/form-data** (with `file`, `model`,
//! and optional `language`/`prompt`/`response_format`/`temperature`) or a JSON body
//! with `file_b64` + `file_name` (legacy OpenProxy CLI shape — kept for backwards
//! compatibility). Resolves the model to a provider, then dispatches by the
//! provider's STT `format`:
//!
//! * **`openai`** — multipart POST to the provider's `/audio/transcriptions`.
//! * **`deepgram`** — raw binary POST + query params (`smart_format`, `punctuate`,
//!   `language`/`detect_language`).
//! * **`assemblyai`** — upload bytes, submit transcript job, poll until done.
//! * **`nvidia-asr`** — multipart POST, normalize response to `{ text }`.
//! * **`huggingface-asr`** — raw binary POST to `{baseUrl}/{model_id}`.
//! * **`gemini-stt`** — `generateContent` with `inline_data` audio + transcription prompt.
//!
//! On per-connection failures the pipeline falls back through other active
//! credentialed connections for the same provider, skipping accounts that are
//! currently rate-limited.

use std::collections::HashSet;
use std::time::Duration;

use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tracing::debug;

use crate::core::model::{get_model_info, ModelRouteKind};
use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::{AppDb, ProviderConnection};

use super::auth_error_response;

// ---------------------------------------------------------------------------
// STT provider catalog (Rust-side mirror of web/src/shared/constants/providers.ts).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SttFormat {
    /// OpenAI-compatible multipart (`file`, `model`, optional `language`, `prompt`, …).
    OpenaiCompatible,
    /// Deepgram `/v1/listen` — raw binary body + query params.
    Deepgram,
    /// AssemblyAI v2 — upload, submit, poll.
    AssemblyAi,
    /// NVIDIA NIM ASR — multipart, response normalized to `{ text }`.
    NvidiaAsr,
    /// HuggingFace inference API — raw binary to `{baseUrl}/{model_id}`.
    HuggingfaceAsr,
    /// Gemini `generateContent` with `inline_data` audio.
    GeminiStt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SttAuthHeader {
    Bearer,
    Token,
    XApiKey,
    Key,
    None,
}

#[derive(Clone, Copy, Debug)]
pub struct SttProviderConfig {
    pub base_url: &'static str,
    pub auth_type_none: bool,
    pub auth_header: SttAuthHeader,
    pub format: SttFormat,
}

/// Returns the STT config for a built-in provider, or `None` if the provider
/// does not support STT (or is a custom node — those go through the `openai`
/// fall-through path with their own `baseUrl`).
pub fn stt_config(provider: &str) -> Option<SttProviderConfig> {
    Some(match provider {
        "openai" => SttProviderConfig {
            base_url: "https://api.openai.com/v1/audio/transcriptions",
            auth_type_none: false,
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::OpenaiCompatible,
        },
        "groq" => SttProviderConfig {
            base_url: "https://api.groq.com/openai/v1/audio/transcriptions",
            auth_type_none: false,
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::OpenaiCompatible,
        },
        "deepgram" => SttProviderConfig {
            base_url: "https://api.deepgram.com/v1/listen",
            auth_type_none: false,
            auth_header: SttAuthHeader::Token,
            format: SttFormat::Deepgram,
        },
        "assemblyai" => SttProviderConfig {
            base_url: "https://api.assemblyai.com/v2/transcript",
            auth_type_none: false,
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::AssemblyAi,
        },
        "huggingface" => SttProviderConfig {
            base_url: "https://api-inference.huggingface.co/models",
            auth_type_none: false,
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::HuggingfaceAsr,
        },
        "gemini" => SttProviderConfig {
            base_url: "https://generativelanguage.googleapis.com/v1beta/models",
            auth_type_none: false,
            auth_header: SttAuthHeader::Key,
            format: SttFormat::GeminiStt,
        },
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Public Axum handler.
// ---------------------------------------------------------------------------

pub async fn cors_options() -> Response {
    cors_preflight_response("POST, OPTIONS")
}

/// `POST /v1/audio/transcriptions` — content-type aware:
/// * `multipart/form-data` → real STT pipeline.
/// * `application/json` → legacy CLI shape (`{ model, file_b64, file_name, … }`).
pub async fn audio_transcriptions(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let headers = parts.headers.clone();

    if let Err(error) = require_api_key(&headers, &state.db) {
        return with_cors(auth_error_response(error));
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let req = if content_type.starts_with("multipart/") {
        let mut multipart =
            match Multipart::from_request(Request::from_parts(parts, body), &state).await {
                Ok(m) => m,
                Err(err) => {
                    return with_cors(json_error(
                        StatusCode::BAD_REQUEST,
                        &format!("Invalid multipart body: {}", err),
                    ));
                }
            };
        match parse_multipart_request(&mut multipart).await {
            Ok(req) => req,
            Err(err) => return with_cors(json_error(err.status, &err.message)),
        }
    } else if content_type.starts_with("application/json") {
        let body_bytes = match axum::body::to_bytes(body, MAX_JSON_BODY).await {
            Ok(b) => b,
            Err(err) => {
                return with_cors(json_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("Body too large or unreadable: {}", err),
                ));
            }
        };
        match parse_json_request(&body_bytes) {
            Ok(req) => req,
            Err(err) => return with_cors(json_error(err.status, &err.message)),
        }
    } else {
        return with_cors(json_error(
            StatusCode::BAD_REQUEST,
            "Content-Type must be multipart/form-data or application/json",
        ));
    };

    let snapshot = state.db.snapshot();
    let resolved = get_model_info(&req.model, &snapshot);
    match resolved.route_kind {
        ModelRouteKind::Combo => with_cors(json_error(
            StatusCode::BAD_REQUEST,
            "Combos not supported for audio/transcriptions",
        )),
        ModelRouteKind::Direct => {
            let provider = match resolved.provider.as_deref() {
                Some(p) if !p.is_empty() => p.to_string(),
                _ => {
                    return with_cors(json_error(StatusCode::BAD_REQUEST, "Invalid model format"));
                }
            };
            let model = resolved.model.clone();
            with_cors(dispatch_with_fallback(&state, &snapshot, &provider, &model, &req).await)
        }
    }
}

const MAX_JSON_BODY: usize = 200 * 1024 * 1024; // 200 MiB — audio uploads can be large.

// ---------------------------------------------------------------------------
// Parsed-request shape.
// ---------------------------------------------------------------------------

struct SttRequest {
    model: String,
    file_bytes: Vec<u8>,
    file_name: String,
    file_content_type: Option<String>,
    language: Option<String>,
    prompt: Option<String>,
    response_format: Option<String>,
    temperature: Option<String>,
}

struct RequestError {
    status: StatusCode,
    message: String,
}

fn req_err(status: StatusCode, message: impl Into<String>) -> RequestError {
    RequestError {
        status,
        message: message.into(),
    }
}

async fn parse_multipart_request(mp: &mut Multipart) -> Result<SttRequest, RequestError> {
    let mut model: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = None;
    let mut file_content_type: Option<String> = None;
    let mut language: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut response_format: Option<String> = None;
    let mut temperature: Option<String> = None;

    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| req_err(StatusCode::BAD_REQUEST, format!("Invalid multipart: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                file_name = field.file_name().map(str::to_string);
                file_content_type = field.content_type().map(str::to_string);
                let bytes = field.bytes().await.map_err(|e| {
                    req_err(
                        StatusCode::BAD_REQUEST,
                        format!("Failed reading file field: {}", e),
                    )
                })?;
                file_bytes = Some(bytes.to_vec());
            }
            "model" => model = Some(read_text_field(field).await?),
            "language" => language = Some(read_text_field(field).await?),
            "prompt" => prompt = Some(read_text_field(field).await?),
            "response_format" => response_format = Some(read_text_field(field).await?),
            "temperature" => temperature = Some(read_text_field(field).await?),
            _ => {
                // Drop unknown fields silently to keep parity with upstream behavior.
                let _ = field.bytes().await;
            }
        }
    }

    let model = model
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| req_err(StatusCode::BAD_REQUEST, "Missing model"))?;
    let file_bytes = file_bytes
        .ok_or_else(|| req_err(StatusCode::BAD_REQUEST, "Missing required field: file"))?;
    let file_name = file_name.unwrap_or_else(|| "audio.wav".to_string());

    Ok(SttRequest {
        model,
        file_bytes,
        file_name,
        file_content_type,
        language: trim_opt(language),
        prompt: trim_opt(prompt),
        response_format: trim_opt(response_format),
        temperature: trim_opt(temperature),
    })
}

async fn read_text_field(
    field: axum::extract::multipart::Field<'_>,
) -> Result<String, RequestError> {
    let bytes = field
        .bytes()
        .await
        .map_err(|e| req_err(StatusCode::BAD_REQUEST, format!("Bad field: {}", e)))?;
    String::from_utf8(bytes.to_vec())
        .map_err(|e| req_err(StatusCode::BAD_REQUEST, format!("Non-UTF8 field: {}", e)))
}

fn parse_json_request(bytes: &[u8]) -> Result<SttRequest, RequestError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| req_err(StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?;

    let model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| req_err(StatusCode::BAD_REQUEST, "Missing model"))?
        .to_string();
    let file_b64 = value
        .get("file_b64")
        .and_then(Value::as_str)
        .ok_or_else(|| req_err(StatusCode::BAD_REQUEST, "Missing required field: file_b64"))?;
    let file_bytes = B64
        .decode(file_b64.trim().as_bytes())
        .map_err(|e| req_err(StatusCode::BAD_REQUEST, format!("Invalid base64: {}", e)))?;
    let file_name = value
        .get("file_name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "audio.wav".to_string());

    Ok(SttRequest {
        model,
        file_bytes,
        file_name,
        file_content_type: None,
        language: value
            .get("language")
            .and_then(Value::as_str)
            .map(str::to_string)
            .and_then(non_empty),
        prompt: value
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::to_string)
            .and_then(non_empty),
        response_format: value
            .get("response_format")
            .and_then(Value::as_str)
            .map(str::to_string)
            .and_then(non_empty),
        temperature: value
            .get("temperature")
            .map(|v| v.to_string())
            .and_then(non_empty),
    })
}

fn trim_opt(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).and_then(non_empty)
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ---------------------------------------------------------------------------
// Provider dispatch + fallback loop.
// ---------------------------------------------------------------------------

async fn dispatch_with_fallback(
    state: &AppState,
    snapshot: &AppDb,
    provider: &str,
    model: &str,
    req: &SttRequest,
) -> Response {
    let Some(cfg) = stt_config(provider) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("Provider '{}' does not support STT", provider),
        );
    };

    if cfg.auth_type_none {
        match transcribe(state, provider, &cfg, model, req, None).await {
            DispatchResult::Ok(resp) => return resp,
            DispatchResult::Err { status, message } => {
                return json_error(status, &message);
            }
        }
    }

    let mut excluded: HashSet<String> = HashSet::new();
    let mut last_message: Option<String> = None;
    let mut last_status: Option<StatusCode> = None;
    let now = Utc::now();

    loop {
        let Some(connection) = select_stt_connection(snapshot, provider, &excluded, now) else {
            if excluded.is_empty() {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    &format!("No credentials for provider: {}", provider),
                );
            }
            return json_error(
                last_status.unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
                last_message
                    .as_deref()
                    .unwrap_or("All accounts unavailable"),
            );
        };

        let token = connection
            .api_key
            .as_deref()
            .or(connection.access_token.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let token_ref = token.as_deref();
        match transcribe(state, provider, &cfg, model, req, token_ref).await {
            DispatchResult::Ok(resp) => return resp,
            DispatchResult::Err { status, message } => {
                if should_fallback(status) {
                    debug!(provider, model, connection = %connection.id, status = %status, "stt: marking connection failed, falling back");
                    excluded.insert(connection.id.clone());
                    last_message = Some(message);
                    last_status = Some(status);
                    continue;
                }
                return json_error(status, &message);
            }
        }
    }
}

fn should_fallback(status: StatusCode) -> bool {
    // Per upstream: fallback on auth, quota, rate-limit, and 5xx errors.
    matches!(status.as_u16(), 401 | 402 | 403 | 408 | 429) || status.is_server_error()
}

fn select_stt_connection(
    snapshot: &AppDb,
    provider: &str,
    excluded: &HashSet<String>,
    now: DateTime<Utc>,
) -> Option<ProviderConnection> {
    let mut candidates: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|c| {
            c.provider == provider
                && c.is_active()
                && connection_has_credentials(c)
                && !excluded.contains(&c.id)
                && !is_rate_limited(c, now)
        })
        .cloned()
        .collect();
    candidates.sort_by_key(|c| c.priority.unwrap_or(999));
    candidates.into_iter().next()
}

fn connection_has_credentials(connection: &ProviderConnection) -> bool {
    connection
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some()
        || connection
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_some()
}

fn is_rate_limited(connection: &ProviderConnection, now: DateTime<Utc>) -> bool {
    connection
        .rate_limited_until
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .is_some_and(|until| until > now)
}

// ---------------------------------------------------------------------------
// Per-format transcription implementations.
// ---------------------------------------------------------------------------

enum DispatchResult {
    Ok(Response),
    Err { status: StatusCode, message: String },
}

async fn transcribe(
    state: &AppState,
    provider: &str,
    cfg: &SttProviderConfig,
    model: &str,
    req: &SttRequest,
    token: Option<&str>,
) -> DispatchResult {
    if !cfg.auth_type_none && token.is_none() {
        return DispatchResult::Err {
            status: StatusCode::UNAUTHORIZED,
            message: format!("No credentials for STT provider: {}", provider),
        };
    }

    let client = match state.client_pool.get(provider, None) {
        Ok(c) => c,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: format!("Client error: {}", e),
            };
        }
    };

    match cfg.format {
        SttFormat::OpenaiCompatible => transcribe_openai(&client, cfg, model, req, token).await,
        SttFormat::Deepgram => transcribe_deepgram(&client, cfg, model, req, token).await,
        SttFormat::AssemblyAi => transcribe_assemblyai(&client, cfg, model, req, token).await,
        SttFormat::NvidiaAsr => transcribe_nvidia(&client, cfg, model, req, token).await,
        SttFormat::HuggingfaceAsr => transcribe_huggingface(&client, cfg, model, req, token).await,
        SttFormat::GeminiStt => transcribe_gemini(&client, cfg, model, req, token).await,
    }
}

fn build_auth_header(cfg: &SttProviderConfig, token: Option<&str>) -> Option<(String, String)> {
    let token = token?;
    match cfg.auth_header {
        SttAuthHeader::Bearer => Some(("Authorization".into(), format!("Bearer {}", token))),
        SttAuthHeader::Token => Some(("Authorization".into(), format!("Token {}", token))),
        SttAuthHeader::XApiKey => Some(("x-api-key".into(), token.to_string())),
        SttAuthHeader::Key => Some(("Authorization".into(), format!("Key {}", token))),
        SttAuthHeader::None => None,
    }
}

fn audio_mime_for(req: &SttRequest) -> String {
    if let Some(ct) = req.file_content_type.as_deref() {
        let lower = ct.to_ascii_lowercase();
        if lower.starts_with("audio/") {
            return lower;
        }
    }
    audio_mime_from_filename(&req.file_name)
}

pub fn audio_mime_from_filename(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "mp3" => "audio/mpeg",
        "mp4" | "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        "aac" => "audio/aac",
        "opus" => "audio/opus",
        _ => "application/octet-stream",
    }
    .to_string()
}

async fn upstream_error(res: reqwest::Response) -> DispatchResult {
    let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let text = res.text().await.unwrap_or_default();
    let message = parse_upstream_error_message(&text)
        .unwrap_or_else(|| format!("Upstream error ({})", status));
    DispatchResult::Err { status, message }
}

fn parse_upstream_error_message(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        if let Some(m) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            return Some(m.to_string());
        }
        if let Some(m) = v.get("error").and_then(Value::as_str) {
            return Some(m.to_string());
        }
        if let Some(m) = v.get("message").and_then(Value::as_str) {
            return Some(m.to_string());
        }
    }
    Some(text.to_string())
}

fn ok_json(body: Value) -> DispatchResult {
    DispatchResult::Ok(
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            body.to_string(),
        )
            .into_response(),
    )
}

fn ok_passthrough(content_type: Option<String>, body: String) -> DispatchResult {
    let mut response = (StatusCode::OK, body).into_response();
    if let Some(ct) = content_type {
        if let Ok(v) = HeaderValue::from_str(&ct) {
            response.headers_mut().insert(header::CONTENT_TYPE, v);
        }
    }
    DispatchResult::Ok(response)
}

// --- openai-compatible (multipart) ---

async fn transcribe_openai(
    client: &reqwest::Client,
    cfg: &SttProviderConfig,
    model: &str,
    req: &SttRequest,
    token: Option<&str>,
) -> DispatchResult {
    let mut form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(req.file_bytes.clone())
            .file_name(req.file_name.clone())
            .mime_str(&audio_mime_for(req))
            .unwrap_or_else(|_| {
                reqwest::multipart::Part::bytes(req.file_bytes.clone())
                    .file_name(req.file_name.clone())
            }),
    );
    form = form.text("model", model.to_string());
    if let Some(lang) = &req.language {
        form = form.text("language", lang.clone());
    }
    if let Some(prompt) = &req.prompt {
        form = form.text("prompt", prompt.clone());
    }
    if let Some(rf) = &req.response_format {
        form = form.text("response_format", rf.clone());
    }
    if let Some(temp) = &req.temperature {
        form = form.text("temperature", temp.clone());
    }

    let mut request = client.post(cfg.base_url).multipart(form);
    if let Some((k, v)) = build_auth_header(cfg, token) {
        request = request.header(k, v);
    }

    let res = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Request failed: {}", e),
            };
        }
    };
    if !res.status().is_success() {
        return upstream_error(res).await;
    }
    let ct = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = res.text().await.unwrap_or_default();
    ok_passthrough(ct, body)
}

// --- deepgram (raw bytes + query string) ---

async fn transcribe_deepgram(
    client: &reqwest::Client,
    cfg: &SttProviderConfig,
    model: &str,
    req: &SttRequest,
    token: Option<&str>,
) -> DispatchResult {
    let url = build_deepgram_url(cfg.base_url, model, req.language.as_deref());
    let mime = audio_mime_for(req);

    let mut request = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, mime)
        .body(req.file_bytes.clone());
    if let Some((k, v)) = build_auth_header(cfg, token) {
        request = request.header(k, v);
    }

    let res = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Request failed: {}", e),
            };
        }
    };
    if !res.status().is_success() {
        return upstream_error(res).await;
    }
    let value: Value = match res.json().await {
        Ok(v) => v,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Failed parsing Deepgram response: {}", e),
            };
        }
    };
    let text = value
        .pointer("/results/channels/0/alternatives/0/transcript")
        .and_then(Value::as_str)
        .unwrap_or("");
    ok_json(json!({ "text": text }))
}

pub fn build_deepgram_url(base: &str, model: &str, language: Option<&str>) -> String {
    let mut url = url::Url::parse(base).unwrap_or_else(|_| {
        url::Url::parse("https://api.deepgram.com/v1/listen").expect("valid fallback URL")
    });
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("model", model);
        q.append_pair("smart_format", "true");
        q.append_pair("punctuate", "true");
        match language {
            Some(lang) if !lang.trim().is_empty() => {
                q.append_pair("language", lang.trim());
            }
            _ => {
                q.append_pair("detect_language", "true");
            }
        }
    }
    url.to_string()
}

// --- assemblyai (upload → submit → poll) ---

async fn transcribe_assemblyai(
    client: &reqwest::Client,
    cfg: &SttProviderConfig,
    model: &str,
    req: &SttRequest,
    token: Option<&str>,
) -> DispatchResult {
    let auth = match build_auth_header(cfg, token) {
        Some(h) => h,
        None => {
            return DispatchResult::Err {
                status: StatusCode::UNAUTHORIZED,
                message: "AssemblyAI requires credentials".to_string(),
            };
        }
    };

    // 1. Upload audio bytes.
    let up = match client
        .post("https://api.assemblyai.com/v2/upload")
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .header(&auth.0, &auth.1)
        .body(req.file_bytes.clone())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("AssemblyAI upload failed: {}", e),
            };
        }
    };
    if !up.status().is_success() {
        return upstream_error(up).await;
    }
    let upload_url = match up.json::<Value>().await {
        Ok(v) => v
            .get("upload_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        Err(_) => None,
    };
    let Some(upload_url) = upload_url else {
        return DispatchResult::Err {
            status: StatusCode::BAD_GATEWAY,
            message: "AssemblyAI upload returned no upload_url".to_string(),
        };
    };

    // 2. Submit transcript job.
    let sub = match client
        .post(cfg.base_url)
        .header(&auth.0, &auth.1)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&json!({
            "audio_url": upload_url,
            "speech_models": [model],
            "language_detection": true,
        }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("AssemblyAI submit failed: {}", e),
            };
        }
    };
    if !sub.status().is_success() {
        return upstream_error(sub).await;
    }
    let id = match sub.json::<Value>().await {
        Ok(v) => v.get("id").and_then(Value::as_str).map(str::to_string),
        Err(_) => None,
    };
    let Some(id) = id else {
        return DispatchResult::Err {
            status: StatusCode::BAD_GATEWAY,
            message: "AssemblyAI submit returned no transcript id".to_string(),
        };
    };

    // 3. Poll up to 120s.
    let poll_url = format!("{}/{}", cfg.base_url.trim_end_matches('/'), id);
    let start = tokio::time::Instant::now();
    while start.elapsed() < Duration::from_secs(120) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let poll = match client.get(&poll_url).header(&auth.0, &auth.1).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !poll.status().is_success() {
            continue;
        }
        let v: Value = match poll.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v.get("status").and_then(Value::as_str) {
            Some("completed") => {
                let text = v.get("text").and_then(Value::as_str).unwrap_or("");
                return ok_json(json!({ "text": text }));
            }
            Some("error") => {
                let msg = v
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("AssemblyAI failed");
                return DispatchResult::Err {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: msg.to_string(),
                };
            }
            _ => continue,
        }
    }
    DispatchResult::Err {
        status: StatusCode::GATEWAY_TIMEOUT,
        message: "AssemblyAI timeout after 120s".to_string(),
    }
}

// --- nvidia-asr (multipart, normalized response) ---

async fn transcribe_nvidia(
    client: &reqwest::Client,
    cfg: &SttProviderConfig,
    model: &str,
    req: &SttRequest,
    token: Option<&str>,
) -> DispatchResult {
    let form = reqwest::multipart::Form::new()
        .part(
            "file",
            reqwest::multipart::Part::bytes(req.file_bytes.clone())
                .file_name(req.file_name.clone())
                .mime_str(&audio_mime_for(req))
                .unwrap_or_else(|_| {
                    reqwest::multipart::Part::bytes(req.file_bytes.clone())
                        .file_name(req.file_name.clone())
                }),
        )
        .text("model", model.to_string());

    let mut request = client.post(cfg.base_url).multipart(form);
    if let Some((k, v)) = build_auth_header(cfg, token) {
        request = request.header(k, v);
    }

    let res = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Request failed: {}", e),
            };
        }
    };
    if !res.status().is_success() {
        return upstream_error(res).await;
    }
    let value: Value = match res.json().await {
        Ok(v) => v,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Failed parsing NVIDIA response: {}", e),
            };
        }
    };
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.get("transcript").and_then(Value::as_str))
        .unwrap_or("");
    ok_json(json!({ "text": text }))
}

// --- huggingface-asr (raw bytes to {baseUrl}/{model_id}) ---

async fn transcribe_huggingface(
    client: &reqwest::Client,
    cfg: &SttProviderConfig,
    model: &str,
    req: &SttRequest,
    token: Option<&str>,
) -> DispatchResult {
    if model.contains("..") || model.contains("//") {
        return DispatchResult::Err {
            status: StatusCode::BAD_REQUEST,
            message: "Invalid model ID".to_string(),
        };
    }
    let url = format!("{}/{}", cfg.base_url.trim_end_matches('/'), model);
    let mime = audio_mime_for(req);
    let mut request = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, mime)
        .body(req.file_bytes.clone());
    if let Some((k, v)) = build_auth_header(cfg, token) {
        request = request.header(k, v);
    }

    let res = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Request failed: {}", e),
            };
        }
    };
    if !res.status().is_success() {
        return upstream_error(res).await;
    }
    let value: Value = match res.json().await {
        Ok(v) => v,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Failed parsing HF response: {}", e),
            };
        }
    };
    let text = value.get("text").and_then(Value::as_str).unwrap_or("");
    ok_json(json!({ "text": text }))
}

// --- gemini-stt (generateContent with inline_data audio) ---

async fn transcribe_gemini(
    client: &reqwest::Client,
    cfg: &SttProviderConfig,
    model: &str,
    req: &SttRequest,
    token: Option<&str>,
) -> DispatchResult {
    let token = match token {
        Some(t) => t,
        None => {
            return DispatchResult::Err {
                status: StatusCode::UNAUTHORIZED,
                message: "Gemini requires an API key".to_string(),
            };
        }
    };
    let mime = audio_mime_for(req);
    let b64 = B64.encode(&req.file_bytes);
    let mut prompt_text = req
        .prompt
        .clone()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            "Generate a transcript of the speech. Return only the transcribed text, no commentary."
                .to_string()
        });
    if let Some(lang) = req.language.as_deref().filter(|s| !s.is_empty()) {
        prompt_text.push_str(&format!(" Language: {}.", lang));
    }
    let url = format!(
        "{}/{}:generateContent?key={}",
        cfg.base_url.trim_end_matches('/'),
        model,
        urlencoding::encode(token),
    );
    let body = json!({
        "contents": [{
            "parts": [
                { "text": prompt_text },
                { "inline_data": { "mime_type": mime, "data": b64 } }
            ]
        }]
    });

    let res = match client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Request failed: {}", e),
            };
        }
    };
    if !res.status().is_success() {
        return upstream_error(res).await;
    }
    let value: Value = match res.json().await {
        Ok(v) => v,
        Err(e) => {
            return DispatchResult::Err {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Failed parsing Gemini response: {}", e),
            };
        }
    };
    let text = value
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    ok_json(json!({ "text": text }))
}

// ---------------------------------------------------------------------------
// CORS + error helpers.
// ---------------------------------------------------------------------------

fn json_error(status: StatusCode, message: &str) -> Response {
    let body = Json(json!({
        "error": {
            "message": message,
            "type": status_to_type(status),
        }
    }));
    (status, body).into_response()
}

fn status_to_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 422 => "invalid_request_error",
        401 | 403 => "authentication_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        408 | 504 => "timeout_error",
        500..=599 => "server_error",
        _ => "api_error",
    }
}

fn with_cors(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    response
}

fn cors_preflight_response(methods: &str) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_str(methods).unwrap_or(HeaderValue::from_static("POST, OPTIONS")),
    );
    response
}

// ---------------------------------------------------------------------------
// Unit tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stt_catalog_has_expected_providers() {
        for p in [
            "openai",
            "groq",
            "deepgram",
            "assemblyai",
            "huggingface",
            "gemini",
        ] {
            assert!(stt_config(p).is_some(), "{} should have STT config", p);
        }
        assert!(stt_config("anthropic").is_none());
        assert!(stt_config("nonexistent").is_none());
    }

    #[test]
    fn audio_mime_inferred_from_extension() {
        assert_eq!(audio_mime_from_filename("clip.mp3"), "audio/mpeg");
        assert_eq!(audio_mime_from_filename("clip.WAV"), "audio/wav");
        assert_eq!(audio_mime_from_filename("clip.m4a"), "audio/mp4");
        assert_eq!(audio_mime_from_filename("clip.opus"), "audio/opus");
        assert_eq!(
            audio_mime_from_filename("noext"),
            "application/octet-stream"
        );
    }

    #[test]
    fn deepgram_url_uses_smart_format_and_detects_language_when_unset() {
        let url = build_deepgram_url("https://api.deepgram.com/v1/listen", "nova-3", None);
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("smart_format=true"));
        assert!(url.contains("punctuate=true"));
        assert!(url.contains("detect_language=true"));
        // Should not include an explicit `language=...` param when nothing is supplied.
        assert!(!url.contains("&language=") && !url.contains("?language="));
    }

    #[test]
    fn deepgram_url_uses_explicit_language_when_set() {
        let url = build_deepgram_url("https://api.deepgram.com/v1/listen", "nova-3", Some("en"));
        assert!(url.contains("&language=en") || url.contains("?language=en"));
        assert!(!url.contains("detect_language=true"));
    }

    #[test]
    fn auth_header_token_styles_match_upstream() {
        let bearer_cfg = stt_config("openai").unwrap();
        assert_eq!(
            build_auth_header(&bearer_cfg, Some("sk-test")),
            Some(("Authorization".into(), "Bearer sk-test".into()))
        );
        let token_cfg = stt_config("deepgram").unwrap();
        assert_eq!(
            build_auth_header(&token_cfg, Some("dg-test")),
            Some(("Authorization".into(), "Token dg-test".into()))
        );
        let key_cfg = stt_config("gemini").unwrap();
        assert_eq!(
            build_auth_header(&key_cfg, Some("ai-test")),
            Some(("Authorization".into(), "Key ai-test".into()))
        );
    }

    #[test]
    fn auth_header_returns_none_when_no_token() {
        let cfg = stt_config("openai").unwrap();
        assert_eq!(build_auth_header(&cfg, None), None);
    }

    #[test]
    fn should_fallback_classifies_errors_correctly() {
        assert!(should_fallback(StatusCode::UNAUTHORIZED));
        assert!(should_fallback(StatusCode::FORBIDDEN));
        assert!(should_fallback(StatusCode::PAYMENT_REQUIRED));
        assert!(should_fallback(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_fallback(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(should_fallback(StatusCode::BAD_GATEWAY));
        assert!(!should_fallback(StatusCode::BAD_REQUEST));
        assert!(!should_fallback(StatusCode::NOT_FOUND));
        assert!(!should_fallback(StatusCode::OK));
    }

    #[test]
    fn parse_upstream_error_prefers_error_message_field() {
        let body = r#"{"error":{"message":"Invalid API key","code":"invalid_api_key"}}"#;
        assert_eq!(
            parse_upstream_error_message(body),
            Some("Invalid API key".into())
        );
    }

    #[test]
    fn parse_upstream_error_falls_back_to_string_error() {
        let body = r#"{"error":"quota exceeded"}"#;
        assert_eq!(
            parse_upstream_error_message(body),
            Some("quota exceeded".into())
        );
    }

    #[test]
    fn parse_upstream_error_returns_raw_text_when_not_json() {
        let body = "Internal Server Error";
        assert_eq!(
            parse_upstream_error_message(body),
            Some("Internal Server Error".into())
        );
    }

    #[test]
    fn parse_upstream_error_returns_none_for_empty_body() {
        assert!(parse_upstream_error_message("").is_none());
    }
}
