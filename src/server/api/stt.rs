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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::{engine::general_purpose::STANDARD as B64, Engine};

use super::cors::{cors_preflight_response, with_cors_response};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tracing::debug;

use tokio::io::AsyncReadExt;

use crate::core::model::{get_model_info, ModelRouteKind};
use crate::server::auth::require_api_key_with_reload;
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
    /// Codex OAuth transcription — `chatgpt.com/backend-api/transcribe`.
    /// Reuses Codex chat OAuth connections; audio is converted to 24 kHz
    /// PCM16 WAV via ffmpeg before upload (upstream `codexStt.js` +
    /// `codexSttAudio.js`).
    CodexTranscribe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SttAuthHeader {
    Bearer,
    Token,
    XApiKey,
    Key,
    None,
}

#[derive(Clone, Debug)]
pub struct SttProviderConfig {
    pub base_url: String,
    pub auth_type_none: bool,
    pub auth_header: SttAuthHeader,
    pub format: SttFormat,
}

/// Default base URL for selfhosted STT (mirrors `selfhosted-stt.js`
/// `sttConfig.baseUrl`). Overridden per connection via
/// `provider_specific_data["baseUrl"]`.
pub const SELFHOSTED_STT_DEFAULT_URL: &str = "http://localhost:8080/v1/audio/transcriptions";

/// Returns the STT config for a built-in provider, or `None` if the provider
/// does not support STT (or is a custom node — those go through the `openai`
/// fall-through path with their own `baseUrl`).
///
/// `selfhosted-stt` uses the default URL here; the per-connection
/// `provider_specific_data["baseUrl"]` override is applied at dispatch time
/// (see [`resolve_stt_config`]).
pub fn stt_config(provider: &str) -> Option<SttProviderConfig> {
    Some(match provider {
        "openai" => SttProviderConfig {
            base_url: "https://api.openai.com/v1/audio/transcriptions".to_string(),
            auth_type_none: false,
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::OpenaiCompatible,
        },
        "groq" => SttProviderConfig {
            base_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            auth_type_none: false,
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::OpenaiCompatible,
        },
        "deepgram" => SttProviderConfig {
            base_url: "https://api.deepgram.com/v1/listen".to_string(),
            auth_type_none: false,
            auth_header: SttAuthHeader::Token,
            format: SttFormat::Deepgram,
        },
        "assemblyai" => SttProviderConfig {
            base_url: "https://api.assemblyai.com/v2/transcript".to_string(),
            auth_type_none: false,
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::AssemblyAi,
        },
        // Inference Providers router, hf-inference ASR route — keeps the Hub
        // model id (`router.huggingface.co/hf-inference/models/<hubId>`).
        "huggingface" => SttProviderConfig {
            base_url: "https://router.huggingface.co/hf-inference/models".to_string(),
            auth_type_none: false,
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::HuggingfaceAsr,
        },
        "gemini" => SttProviderConfig {
            base_url: "https://generativelanguage.googleapis.com/v1beta/models".to_string(),
            auth_type_none: false,
            auth_header: SttAuthHeader::Key,
            format: SttFormat::GeminiStt,
        },
        // Upstream `providers/registry/codex.js` sttConfig — ChatGPT backend
        // transcription reusing the Codex OAuth chat connection.
        "codex" => SttProviderConfig {
            base_url: "https://chatgpt.com/backend-api/transcribe".to_string(),
            auth_type_none: false,
            // Bearer is applied inside `transcribe_codex` together with the
            // `ChatGPT-Account-Id` header; this value is unused for the format.
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::CodexTranscribe,
        },
        "selfhosted-stt" => SttProviderConfig {
            base_url: SELFHOSTED_STT_DEFAULT_URL.to_string(),
            auth_type_none: false,
            auth_header: SttAuthHeader::Bearer,
            format: SttFormat::OpenaiCompatible,
        },
        _ => return None,
    })
}

/// Resolve the effective STT config for a provider + connection.
///
/// Mirrors 9router `sttCore.js:176-182`: a per-connection
/// `provider_specific_data["baseUrl"]` overrides the registry's fixed base
/// URL (trailing slashes stripped) for ANY provider. It's opt-in — absent
/// unless the connection sets it, so cloud providers are untouched.
/// `selfhosted-stt` additionally never falls back to a cloud endpoint.
fn resolve_stt_config(
    provider: &str,
    connection: &crate::types::ProviderConnection,
) -> Option<SttProviderConfig> {
    let mut cfg = stt_config(provider)?;
    // Upstream sttCore.js:281 — "OAuth transcription uses a fixed ChatGPT
    // endpoint; never honor connection URL overrides." A per-connection
    // `baseUrl` must not redirect Codex STT traffic to an arbitrary host
    // (it would also leak the OAuth bearer token).
    if provider != "codex" {
        if let Some(base_url) = connection
            .provider_specific_data
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            cfg.base_url = base_url.trim_end_matches('/').to_string();
        }
    }
    Some(cfg)
}

// ---------------------------------------------------------------------------
// Public Axum handler.
// ---------------------------------------------------------------------------

pub async fn cors_options() -> Response {
    cors_preflight_response()
}

/// `POST /v1/audio/transcriptions` — content-type aware:
/// * `multipart/form-data` → real STT pipeline.
/// * `application/json` → legacy CLI shape (`{ model, file_b64, file_name, … }`).
pub async fn audio_transcriptions(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let headers = parts.headers.clone();

    if state.db.snapshot().settings.require_login {
        if let Err(error) = require_api_key_with_reload(&headers, &state.db).await {
            return with_cors_response(auth_error_response(error));
        }
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
                    return with_cors_response(json_error(
                        StatusCode::BAD_REQUEST,
                        &format!("Invalid multipart body: {}", err),
                    ));
                }
            };
        match parse_multipart_request(&mut multipart).await {
            Ok(req) => req,
            Err(err) => return with_cors_response(json_error(err.status, &err.message)),
        }
    } else if content_type.starts_with("application/json") {
        let body_bytes = match axum::body::to_bytes(body, MAX_JSON_BODY).await {
            Ok(b) => b,
            Err(err) => {
                return with_cors_response(json_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("Body too large or unreadable: {}", err),
                ));
            }
        };
        match parse_json_request(&body_bytes) {
            Ok(req) => req,
            Err(err) => return with_cors_response(json_error(err.status, &err.message)),
        }
    } else {
        return with_cors_response(json_error(
            StatusCode::BAD_REQUEST,
            "Content-Type must be multipart/form-data or application/json",
        ));
    };

    let snapshot = state.db.snapshot();
    let resolved = get_model_info(&req.model, &snapshot);
    match resolved.route_kind {
        ModelRouteKind::Combo => with_cors_response(json_error(
            StatusCode::BAD_REQUEST,
            "Combos not supported for audio/transcriptions",
        )),
        ModelRouteKind::Direct => {
            let provider = match resolved.provider.as_deref() {
                Some(p) if !p.is_empty() => p.to_string(),
                _ => {
                    return with_cors_response(json_error(
                        StatusCode::BAD_REQUEST,
                        "Invalid model format",
                    ));
                }
            };
            let model = resolved.model.clone();
            with_cors_response(
                dispatch_with_fallback(&state, &snapshot, &provider, &model, &req).await,
            )
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
    /// Deepgram-only: `smart_format` query parameter override.
    /// `None` = use default (currently hardcoded to `true`, see TODO).
    deepgram_smart_format: Option<String>,
    /// Deepgram-only: `punctuate` query parameter override.
    /// `None` = use default (currently hardcoded to `true`, see TODO).
    deepgram_punctuate: Option<String>,
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
    let mut deepgram_smart_format: Option<String> = None;
    let mut deepgram_punctuate: Option<String> = None;

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
            "deepgram_smart_format" => {
                deepgram_smart_format = Some(read_text_field(field).await?);
            }
            "deepgram_punctuate" => {
                deepgram_punctuate = Some(read_text_field(field).await?);
            }
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
        deepgram_smart_format: trim_opt(deepgram_smart_format),
        deepgram_punctuate: trim_opt(deepgram_punctuate),
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
        deepgram_smart_format: value
            .get("deepgram_smart_format")
            .and_then(Value::as_str)
            .map(str::to_string)
            .and_then(non_empty),
        deepgram_punctuate: value
            .get("deepgram_punctuate")
            .and_then(Value::as_str)
            .map(str::to_string)
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
        match transcribe(state, provider, &cfg, model, req, None, None).await {
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
        // selfhosted-stt resolves its base URL from the connection, not the
        // static config. Never falls back to a cloud endpoint.
        let effective_cfg = match resolve_stt_config(provider, &connection) {
            Some(cfg) => cfg,
            None => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    &format!("Provider '{provider}' does not support STT"),
                );
            }
        };
        match transcribe(
            state,
            provider,
            &effective_cfg,
            model,
            req,
            token_ref,
            Some(&connection),
        )
        .await
        {
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
    connection: Option<&ProviderConnection>,
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
        SttFormat::CodexTranscribe => match connection {
            Some(conn) => transcribe_codex(&client, cfg, model, req, conn).await,
            None => DispatchResult::Err {
                status: StatusCode::UNAUTHORIZED,
                message: "Codex transcription requires an OAuth connection".to_string(),
            },
        },
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

    let mut request = client.post(&cfg.base_url).multipart(form);
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
    let url = build_deepgram_url(
        &cfg.base_url,
        model,
        req.language.as_deref(),
        req.deepgram_smart_format.as_deref(),
        req.deepgram_punctuate.as_deref(),
    );
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

pub fn build_deepgram_url(
    base: &str,
    model: &str,
    language: Option<&str>,
    smart_format: Option<&str>,
    punctuate: Option<&str>,
) -> String {
    let mut url = url::Url::parse(base).unwrap_or_else(|_| {
        url::Url::parse("https://api.deepgram.com/v1/listen").expect("valid fallback URL")
    });
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("model", model);
        // TODO(deepgram-smartfmt): `smart_format` and `punctuate` default to
        // `"true"` for backward compatibility. Once the upstream JS port wires
        // these through, the fallback value should be derived from the
        // provider-level config rather than hardcoded here.
        q.append_pair("smart_format", smart_format.unwrap_or("true"));
        q.append_pair("punctuate", punctuate.unwrap_or("true"));
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
        .post(&cfg.base_url)
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

    let mut request = client.post(&cfg.base_url).multipart(form);
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

// --- codex (ChatGPT backend transcription; OAuth + ffmpeg → 24 kHz WAV) ---
//
// Port of upstream `open-sse/handlers/codexStt.js` + `codexSttAudio.js`
// (v0.5.109) and `providers/registry/codex.js` sttConfig. Reuses the Codex
// OAuth chat connection: `access_token` goes in `Authorization: Bearer` and
// the account id (`provider_specific_data.workspaceId` → `chatgptAccountId`
// → `accountId`) in `ChatGPT-Account-Id`.

/// Upstream `codexSttAudio.js` `MAX_AUDIO_BYTES` — raw upload cap (25 MiB).
const CODEX_STT_MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
/// Upstream `MAX_AUDIO_SECONDS` — ffmpeg `-t` cap.
const CODEX_STT_MAX_AUDIO_SECONDS: u32 = 120;
/// Upstream `MAX_PCM_BYTES` — PCM16 mono @ 24 kHz for `MAX_AUDIO_SECONDS`.
const CODEX_STT_MAX_PCM_BYTES: usize = CODEX_STT_MAX_AUDIO_SECONDS as usize * 24_000 * 2;
/// Upstream `readBounded` response cap (1 MiB).
const CODEX_STT_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Upstream transcript text cap.
const CODEX_STT_MAX_TEXT_LEN: usize = 250_000;
/// Upstream `AbortSignal.timeout(60000)` covers convert + upload + response.
const CODEX_STT_TIMEOUT: Duration = Duration::from_secs(60);
/// Upstream ffmpeg spawn `timeout: 15000`.
const CODEX_STT_CONVERT_TIMEOUT: Duration = Duration::from_secs(15);
/// Upstream `if (active >= 2)` in-flight cap.
const CODEX_STT_MAX_CONCURRENT: usize = 2;
/// Upstream `codex.transport.headers["User-Agent"]` for CLI 0.159.0
/// (`providers/registry/codex.js` `CODEX_CLI_VERSION`).
const CODEX_STT_USER_AGENT: &str = "codex_cli_rs/0.159.0";

static CODEX_STT_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// RAII slot for the upstream `active < 2` concurrency cap.
struct CodexSttSlot;

impl CodexSttSlot {
    fn acquire() -> Option<Self> {
        let mut current = CODEX_STT_ACTIVE.load(Ordering::Relaxed);
        loop {
            if current >= CODEX_STT_MAX_CONCURRENT {
                return None;
            }
            match CODEX_STT_ACTIVE.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self),
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for CodexSttSlot {
    fn drop(&mut self) {
        CODEX_STT_ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

fn codex_stt_err(status: StatusCode, message: impl Into<String>) -> DispatchResult {
    DispatchResult::Err {
        status,
        message: message.into(),
    }
}

/// Upstream credential gate (`codexStt.js`): OAuth auth type, printable-ASCII
/// access token (`/^[\x21-\x7e]+$/`), and a printable account id
/// (`/^[\x21-\x7e]{1,256}$/`) from `workspaceId` → `chatgptAccountId` →
/// `accountId`.
fn codex_stt_credentials<'a>(
    connection: &'a ProviderConnection,
) -> Result<(&'a str, &'a str), DispatchResult> {
    let invalid = || {
        codex_stt_err(
            StatusCode::UNAUTHORIZED,
            "A valid Codex OAuth account is required",
        )
    };
    if connection.auth_type != "oauth" {
        return Err(invalid());
    }
    let token = connection.access_token.as_deref().ok_or_else(invalid)?;
    if token.is_empty() || !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(invalid());
    }
    let account_id = ["workspaceId", "chatgptAccountId", "accountId"]
        .iter()
        .find_map(|key| {
            connection
                .provider_specific_data
                .get(*key)
                .and_then(Value::as_str)
        })
        .ok_or_else(invalid)?;
    if account_id.is_empty()
        || account_id.len() > 256
        || !account_id.bytes().all(|b| (0x21..=0x7e).contains(&b))
    {
        return Err(invalid());
    }
    Ok((token, account_id))
}

/// Upstream field gate: only `response_format` ∈ {json, text} (default json);
/// `language`, `prompt`, `temperature` must be absent or empty.
fn codex_stt_response_format(req: &SttRequest) -> Result<String, DispatchResult> {
    for (present, key) in [
        (req.language.is_some(), "language"),
        (req.prompt.is_some(), "prompt"),
        (req.temperature.is_some(), "temperature"),
    ] {
        if present {
            return Err(codex_stt_err(
                StatusCode::BAD_REQUEST,
                format!("{key} is not supported by Codex transcription"),
            ));
        }
    }
    let format = req
        .response_format
        .clone()
        .unwrap_or_else(|| "json".to_string());
    if format != "json" && format != "text" {
        return Err(codex_stt_err(
            StatusCode::BAD_REQUEST,
            "Only json and text response formats are supported",
        ));
    }
    Ok(format)
}

/// Wrap raw little-endian PCM16 mono @ 24 kHz in a canonical 44-byte WAV
/// header (mirrors the `Buffer.alloc(44 + pcm.length)` block in
/// `codexSttAudio.js`).
fn wav_wrap_pcm16_24k(pcm: &[u8]) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let riff_len = 36u32.saturating_add(data_len);
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // channels = mono
    wav.extend_from_slice(&24_000u32.to_le_bytes()); // sample rate
    wav.extend_from_slice(&48_000u32.to_le_bytes()); // byte rate (24000*2)
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// Convert arbitrary audio input to 24 kHz mono PCM16 WAV via ffmpeg,
/// mirroring `convertAudio` in `codexSttAudio.js`: bounded temp dir (input
/// written mode 0600), `-t 120.01`, `-ac 1 -ar 24000 -c:a pcm_s16le`,
/// stdout capped at `MAX_PCM_BYTES`, 15 s spawn timeout. Requires ffmpeg on
/// PATH (503 when absent, same as upstream).
async fn codex_convert_audio_to_wav(file_bytes: &[u8]) -> Result<Vec<u8>, DispatchResult> {
    let dir = std::env::temp_dir().join(format!("openproxy-codex-stt-{}", uuid::Uuid::new_v4()));
    let result = codex_convert_inner(&dir, file_bytes).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn codex_convert_inner(
    dir: &std::path::Path,
    file_bytes: &[u8],
) -> Result<Vec<u8>, DispatchResult> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| codex_stt_err(StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir tmp: {e}")))?;
    let input = dir.join("input");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&input).map_err(|e| {
            codex_stt_err(StatusCode::INTERNAL_SERVER_ERROR, format!("tmpfile: {e}"))
        })?;
        use std::io::Write;
        file.write_all(file_bytes).map_err(|e| {
            codex_stt_err(StatusCode::INTERNAL_SERVER_ERROR, format!("tmpfile: {e}"))
        })?;
    }

    let mut child = match tokio::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-threads",
            "1",
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            "wav,mp3,mov,matroska,webm,ogg,flac,aac",
            "-i",
        ])
        .arg(&input)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-t",
            &format!("{:.2}", CODEX_STT_MAX_AUDIO_SECONDS as f64 + 0.01),
            "-threads",
            "1",
            "-ac",
            "1",
            "-ar",
            "24000",
            "-c:a",
            "pcm_s16le",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(codex_stt_err(
                StatusCode::SERVICE_UNAVAILABLE,
                "ffmpeg is required for transcription",
            ));
        }
        Err(_) => {
            return Err(codex_stt_err(
                StatusCode::GATEWAY_TIMEOUT,
                "Audio conversion timed out",
            ));
        }
    };

    let conversion = async {
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut pcm: Vec<u8> = Vec::new();
        let mut oversized = false;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if pcm.len() + n > CODEX_STT_MAX_PCM_BYTES {
                        oversized = true;
                        break;
                    }
                    pcm.extend_from_slice(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        if oversized {
            let _ = child.kill().await;
        }
        let status = child.wait().await;
        (pcm, oversized, status)
    };

    match tokio::time::timeout(CODEX_STT_CONVERT_TIMEOUT, conversion).await {
        Err(_) => Err(codex_stt_err(
            StatusCode::GATEWAY_TIMEOUT,
            "Audio conversion timed out",
        )),
        Ok((pcm, oversized, status)) => {
            if oversized {
                return Err(codex_stt_err(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "Audio duration exceeds {} seconds",
                        CODEX_STT_MAX_AUDIO_SECONDS
                    ),
                ));
            }
            match status {
                Ok(s) if s.success() && !pcm.is_empty() => Ok(wav_wrap_pcm16_24k(&pcm)),
                _ => Err(codex_stt_err(
                    StatusCode::BAD_REQUEST,
                    "Invalid or unsupported audio",
                )),
            }
        }
    }
}

/// Read a response body with a hard byte cap (upstream `readBounded`).
async fn codex_read_bounded(res: &mut reqwest::Response) -> Result<Vec<u8>, DispatchResult> {
    let mut body = Vec::new();
    loop {
        match res.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > CODEX_STT_MAX_RESPONSE_BYTES {
                    return Err(codex_stt_err(
                        StatusCode::BAD_GATEWAY,
                        "Body exceeds size limit",
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(e) => {
                return Err(codex_stt_err(
                    StatusCode::BAD_GATEWAY,
                    format!("Failed reading response: {e}"),
                ));
            }
        }
    }
}

async fn transcribe_codex(
    client: &reqwest::Client,
    cfg: &SttProviderConfig,
    model: &str,
    req: &SttRequest,
    connection: &ProviderConnection,
) -> DispatchResult {
    // Codex STT is only exposed through the registry `transcribe` model;
    // other codex models are chat/image and must not hit this endpoint.
    if model != "transcribe" {
        return codex_stt_err(
            StatusCode::BAD_REQUEST,
            "Codex transcription requires the 'transcribe' model",
        );
    }
    let (access_token, account_id) = match codex_stt_credentials(connection) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let format = match codex_stt_response_format(req) {
        Ok(f) => f,
        Err(e) => return e,
    };
    if req.file_bytes.is_empty() {
        return codex_stt_err(
            StatusCode::BAD_REQUEST,
            "A non-empty audio file is required",
        );
    }
    if req.file_bytes.len() > CODEX_STT_MAX_AUDIO_BYTES {
        return codex_stt_err(StatusCode::PAYLOAD_TOO_LARGE, "Audio exceeds 25 MiB");
    }
    let _slot = match CodexSttSlot::acquire() {
        Some(s) => s,
        None => {
            return codex_stt_err(
                StatusCode::SERVICE_UNAVAILABLE,
                "Transcription capacity reached",
            );
        }
    };

    // Upstream `AbortSignal.timeout(60000)` bounds convert + upload + response.
    let work = async {
        let wav = codex_convert_audio_to_wav(&req.file_bytes).await?;

        // "audio/wav" is a static valid MIME — mime_str cannot fail here.
        let part = reqwest::multipart::Part::bytes(wav)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .expect("static audio/wav MIME is valid");
        let form = reqwest::multipart::Form::new().part("file", part);

        let request = client
            .post(&cfg.base_url)
            .multipart(form)
            .timeout(CODEX_STT_TIMEOUT)
            .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
            .header("ChatGPT-Account-Id", account_id)
            .header(header::USER_AGENT, CODEX_STT_USER_AGENT);
        // `redirect: "error"` upstream — reqwest follows redirects at the
        // client level, so a 3xx surfaces as a non-success status below.
        // NOTE: do not set Content-Type manually — `.multipart()` generates
        // the boundary parameter.
        let mut res = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                return Err(codex_stt_err(
                    StatusCode::BAD_GATEWAY,
                    format!("Request failed: {e}"),
                ));
            }
        };
        if !res.status().is_success() {
            // Upstream maps any non-2xx outside 400..=599 to 502 and answers
            // with a fixed message (the error body is discarded).
            let code = res.status().as_u16();
            let status = if (400..=599).contains(&code) {
                StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY)
            } else {
                StatusCode::BAD_GATEWAY
            };
            return Err(codex_stt_err(status, "Codex transcription failed"));
        }
        let body = codex_read_bounded(&mut res).await?;
        let text = match serde_json::from_slice::<Value>(&body) {
            Ok(v) => v.get("text").and_then(Value::as_str).map(str::to_string),
            Err(_) => None,
        };
        match text {
            Some(t) if t.len() <= CODEX_STT_MAX_TEXT_LEN => Ok(t),
            _ => Err(codex_stt_err(
                StatusCode::BAD_GATEWAY,
                "Invalid transcription response",
            )),
        }
    };

    match tokio::time::timeout(CODEX_STT_TIMEOUT, work).await {
        Err(_) => codex_stt_err(StatusCode::GATEWAY_TIMEOUT, "Codex transcription failed"),
        Ok(Err(e)) => e,
        Ok(Ok(text)) => {
            if format == "text" {
                DispatchResult::Ok(
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                        text,
                    )
                        .into_response(),
                )
            } else {
                ok_json(json!({ "text": text }))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CORS + error helpers.
// ---------------------------------------------------------------------------

/// 9router's `errorResponse` (open-sse/utils/error.js:30-38) answers with the
/// status it is handed and the message it is handed — the status is an input,
/// never re-derived from the text. Re-deriving it here turned a 400 "Combos
/// not supported for audio/transcriptions" into a 406 and a 400 "insufficient
/// balance" into a 403, so a client could not tell a malformed request from a
/// rejected one. The message still goes through `friendly_error_message`,
/// which only sanitises (HTML strip, whitespace collapse, length clamp).
fn json_error(status: StatusCode, message: &str) -> Response {
    let friendly = crate::core::utils::error::friendly_error_message(status.as_u16(), message);
    let body = Json(json!({
        "error": {
            "message": friendly,
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
        let url = build_deepgram_url(
            "https://api.deepgram.com/v1/listen",
            "nova-3",
            None,
            None,
            None,
        );
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("smart_format=true"));
        assert!(url.contains("punctuate=true"));
        assert!(url.contains("detect_language=true"));
        // Should not include an explicit `language=...` param when nothing is supplied.
        assert!(!url.contains("&language=") && !url.contains("?language="));
    }

    #[test]
    fn deepgram_url_uses_explicit_language_when_set() {
        let url = build_deepgram_url(
            "https://api.deepgram.com/v1/listen",
            "nova-3",
            Some("en"),
            None,
            None,
        );
        assert!(url.contains("&language=en") || url.contains("?language=en"));
        assert!(!url.contains("detect_language=true"));
    }

    #[test]
    fn deepgram_url_smart_format_punctuate_can_be_overridden() {
        let url = build_deepgram_url(
            "https://api.deepgram.com/v1/listen",
            "nova-3",
            None,
            Some("false"),
            Some("false"),
        );
        assert!(url.contains("&smart_format=false"));
        assert!(url.contains("&punctuate=false"));
    }

    #[test]
    fn deepgram_url_defaults_smart_format_punctuate_to_true_when_none() {
        let url = build_deepgram_url(
            "https://api.deepgram.com/v1/listen",
            "nova-3",
            None,
            None,
            None,
        );
        assert!(url.contains("&smart_format=true"));
        assert!(url.contains("&punctuate=true"));
    }

    #[test]
    fn huggingface_stt_uses_router_hf_inference_route() {
        // 9router cf663f5: api-inference.huggingface.co is retired; ASR goes
        // through the router's hf-inference provider, Hub model id appended.
        let cfg = stt_config("huggingface").unwrap();
        assert_eq!(
            cfg.base_url,
            "https://router.huggingface.co/hf-inference/models"
        );
        assert_eq!(cfg.format, SttFormat::HuggingfaceAsr);
        assert_eq!(cfg.auth_header, SttAuthHeader::Bearer);
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

    #[test]
    fn selfhosted_stt_uses_provider_specific_base_url() {
        use crate::types::ProviderConnection;
        use std::collections::BTreeMap;

        // With a per-connection override, the resolved URL is that override.
        let mut psd = BTreeMap::new();
        psd.insert(
            "baseUrl".to_string(),
            serde_json::Value::String(
                "http://192.168.1.5:8080/v1/audio/transcriptions".to_string(),
            ),
        );
        let conn = ProviderConnection {
            id: "selfhosted-1".into(),
            provider: "selfhosted-stt".into(),
            auth_type: "apikey".into(),
            provider_specific_data: psd,
            ..Default::default()
        };
        let cfg = resolve_stt_config("selfhosted-stt", &conn).expect("selfhosted config");
        assert_eq!(
            cfg.base_url,
            "http://192.168.1.5:8080/v1/audio/transcriptions"
        );
        assert_eq!(cfg.format, SttFormat::OpenaiCompatible);
        assert_eq!(cfg.auth_header, SttAuthHeader::Bearer);
    }

    #[test]
    fn selfhosted_stt_falls_back_to_default_base_url() {
        use crate::types::ProviderConnection;

        let conn = ProviderConnection {
            id: "selfhosted-1".into(),
            provider: "selfhosted-stt".into(),
            auth_type: "apikey".into(),
            ..Default::default()
        };
        let cfg = resolve_stt_config("selfhosted-stt", &conn).expect("selfhosted config");
        assert_eq!(cfg.base_url, SELFHOSTED_STT_DEFAULT_URL);
        // Must never resolve to a cloud endpoint.
        assert!(!cfg.base_url.contains("openai.com"));
        assert!(!cfg.base_url.contains("googleapis.com"));
    }

    #[test]
    fn selfhosted_stt_catalog_entry_exists() {
        let cfg = stt_config("selfhosted-stt").expect("selfhosted-stt in catalog");
        assert_eq!(cfg.base_url, SELFHOSTED_STT_DEFAULT_URL);
        assert_eq!(cfg.format, SttFormat::OpenaiCompatible);
    }

    #[test]
    fn stt_openai_resolves_connection_base_url_override() {
        use crate::types::ProviderConnection;
        use std::collections::BTreeMap;

        // Per-connection override applies to ANY provider (sttCore.js:176-182).
        let mut psd = BTreeMap::new();
        psd.insert(
            "baseUrl".to_string(),
            serde_json::Value::String(
                "http://192.168.1.5:8080/v1/audio/transcriptions/".to_string(),
            ),
        );
        let conn = ProviderConnection {
            id: "openai-1".into(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            provider_specific_data: psd,
            ..Default::default()
        };
        let cfg = resolve_stt_config("openai", &conn).expect("openai config");
        // Trailing slash stripped, override wins over the static default.
        assert_eq!(
            cfg.base_url,
            "http://192.168.1.5:8080/v1/audio/transcriptions"
        );

        // Without an override, the static catalog URL is used.
        let plain = ProviderConnection {
            id: "openai-1".into(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            ..Default::default()
        };
        let cfg = resolve_stt_config("openai", &plain).expect("openai config");
        assert_eq!(
            cfg.base_url,
            "https://api.openai.com/v1/audio/transcriptions"
        );
    }

    #[test]
    fn stt_openai_passthrough_preserves_content_type() {
        // 9router sttCore.js:150-153: the raw body + upstream content-type are
        // echoed unchanged (never re-parsed/re-serialized). ok_passthrough must
        // copy the content-type and pass the body verbatim.
        let body = "{\"text\":\"Hello world\"}".to_string();

        // JSON content-type.
        let resp = ok_passthrough(Some("application/json".into()), body.clone());
        let DispatchResult::Ok(resp) = resp else {
            panic!("expected Ok");
        };
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );

        // Non-JSON content-type (SRT/VTT) must be preserved too.
        let resp = ok_passthrough(Some("application/x-subrip".into()), body.clone());
        let DispatchResult::Ok(resp) = resp else {
            panic!("expected Ok");
        };
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/x-subrip")
        );

        // Missing content-type → a content-type is always set (never dropped).
        let resp = ok_passthrough(None, body.clone());
        let DispatchResult::Ok(resp) = resp else {
            panic!("expected Ok");
        };
        assert!(resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some());
    }

    /// The status is an input, never re-derived from the text
    /// (open-sse/utils/error.js:30-38). `infer_status_from_message` matched
    /// `"insufficient"` here and answered 403, so a client that sent a
    /// well-formed request could not tell a bad request from a rejected one.
    #[test]
    fn stt_error_keeps_the_upstream_status() {
        for (status, message) in [
            (StatusCode::BAD_REQUEST, "insufficient balance"),
            (
                StatusCode::BAD_REQUEST,
                "Combos not supported for audio/transcriptions",
            ),
            (StatusCode::BAD_REQUEST, "Model x is not supported"),
        ] {
            let response = json_error(status, message);
            assert_eq!(
                response.status(),
                status,
                "errorResponse answers with the status it is given, so {message:?} stays {status}"
            );
        }
    }

    /// …and the sanitiser still runs, so the message a client reads is the
    /// clean one. `status_to_type` reads the same unrewritten status, so the
    /// envelope's `type` can no longer report a 400 family as something else.
    #[tokio::test]
    async fn stt_error_keeps_the_sanitized_message_and_the_matching_type() {
        let response = json_error(
            StatusCode::BAD_REQUEST,
            "Error from provider (Console): Combos not supported",
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body: Value = serde_json::from_slice(&bytes).expect("json body");
        assert_eq!(body["error"]["message"], "Combos not supported");
        assert_eq!(body["error"]["type"], "invalid_request_error");
    }

    // --- codex transcription ---

    fn codex_conn(psd: &[(&str, &str)]) -> ProviderConnection {
        use std::collections::BTreeMap;
        ProviderConnection {
            id: "codex-1".into(),
            provider: "codex".into(),
            auth_type: "oauth".into(),
            access_token: Some("codex-access-token".into()),
            provider_specific_data: psd
                .iter()
                .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
                .collect::<BTreeMap<_, _>>(),
            ..Default::default()
        }
    }

    fn stt_req() -> SttRequest {
        SttRequest {
            model: "transcribe".into(),
            file_bytes: vec![1, 2, 3],
            file_name: "audio.wav".into(),
            file_content_type: Some("audio/wav".into()),
            language: None,
            prompt: None,
            response_format: None,
            temperature: None,
            deepgram_smart_format: None,
            deepgram_punctuate: None,
        }
    }

    #[test]
    fn codex_stt_catalog_entry() {
        let cfg = stt_config("codex").expect("codex STT config");
        assert_eq!(cfg.format, SttFormat::CodexTranscribe);
        assert_eq!(cfg.base_url, "https://chatgpt.com/backend-api/transcribe");
        assert!(!cfg.auth_type_none);
    }

    #[test]
    fn codex_stt_ignores_connection_base_url_override() {
        // Upstream sttCore.js:281 — the OAuth transcription endpoint is fixed;
        // a connection baseUrl must never redirect it (bearer-token leak).
        use std::collections::BTreeMap;
        let mut psd = BTreeMap::new();
        psd.insert(
            "baseUrl".to_string(),
            Value::String("https://evil.example.com/transcribe".to_string()),
        );
        let conn = ProviderConnection {
            id: "codex-1".into(),
            provider: "codex".into(),
            auth_type: "oauth".into(),
            access_token: Some("tok".into()),
            provider_specific_data: psd,
            ..Default::default()
        };
        let cfg = resolve_stt_config("codex", &conn).expect("codex config");
        assert_eq!(cfg.base_url, "https://chatgpt.com/backend-api/transcribe");
    }

    #[test]
    fn codex_credentials_require_oauth_and_printable_token() {
        let conn = codex_conn(&[("workspaceId", "acct-123")]);
        let (token, account) = codex_stt_credentials(&conn).expect("valid creds");
        assert_eq!(token, "codex-access-token");
        assert_eq!(account, "acct-123");

        // Non-OAuth auth type rejected.
        let mut bad = codex_conn(&[("workspaceId", "acct-123")]);
        bad.auth_type = "apikey".into();
        assert!(codex_stt_credentials(&bad).is_err());

        // Missing access token rejected.
        let mut bad = codex_conn(&[("workspaceId", "acct-123")]);
        bad.access_token = None;
        assert!(codex_stt_credentials(&bad).is_err());

        // Non-printable token rejected (upstream /^[\x21-\x7e]+$/).
        let mut bad = codex_conn(&[("workspaceId", "acct-123")]);
        bad.access_token = Some("tok en".into());
        assert!(codex_stt_credentials(&bad).is_err());

        // Missing account id rejected.
        let bad = codex_conn(&[]);
        assert!(codex_stt_credentials(&bad).is_err());
    }

    #[test]
    fn codex_account_id_prefers_workspace_id() {
        // Upstream: workspaceId || chatgptAccountId || accountId.
        let conn = codex_conn(&[
            ("accountId", "a1"),
            ("chatgptAccountId", "a2"),
            ("workspaceId", "a3"),
        ]);
        let (_, account) = codex_stt_credentials(&conn).unwrap();
        assert_eq!(account, "a3");

        let conn = codex_conn(&[("accountId", "a1"), ("chatgptAccountId", "a2")]);
        let (_, account) = codex_stt_credentials(&conn).unwrap();
        assert_eq!(account, "a2");

        // Over-long account id rejected (upstream {1,256}).
        let long = "x".repeat(257);
        let conn = codex_conn(&[("workspaceId", long.as_str())]);
        assert!(codex_stt_credentials(&conn).is_err());
    }

    #[test]
    fn codex_response_format_only_json_or_text() {
        let mut req = stt_req();
        assert_eq!(codex_stt_response_format(&req).unwrap(), "json");

        req.response_format = Some("text".into());
        assert_eq!(codex_stt_response_format(&req).unwrap(), "text");

        for bad in ["srt", "vtt", "verbose_json", "JSON"] {
            req.response_format = Some(bad.into());
            assert!(codex_stt_response_format(&req).is_err(), "{bad}");
        }
    }

    #[test]
    fn codex_rejects_language_prompt_temperature() {
        let mut req = stt_req();
        req.language = Some("en".into());
        assert!(codex_stt_response_format(&req).is_err());

        let mut req = stt_req();
        req.prompt = Some("translate".into());
        assert!(codex_stt_response_format(&req).is_err());

        let mut req = stt_req();
        req.temperature = Some("0.5".into());
        assert!(codex_stt_response_format(&req).is_err());
    }

    #[test]
    fn wav_wrap_pcm16_24k_header_is_canonical() {
        let pcm = vec![0u8; 480]; // 10 ms of silence
        let wav = wav_wrap_pcm16_24k(&pcm);
        assert_eq!(wav.len(), 44 + pcm.len());
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        // RIFF chunk size = 36 + data len.
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 480);
        // PCM, mono, 24 kHz, byte rate 48000, align 2, 16-bit.
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 48_000);
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 480);
        assert_eq!(&wav[44..], pcm.as_slice());
    }

    #[test]
    fn codex_slot_caps_concurrency_at_two() {
        let s1 = CodexSttSlot::acquire().expect("slot 1");
        let s2 = CodexSttSlot::acquire().expect("slot 2");
        assert!(CodexSttSlot::acquire().is_none(), "third slot rejected");
        drop(s1);
        assert!(CodexSttSlot::acquire().is_some(), "drop releases a slot");
        drop(s2);
    }

    /// End-to-end conversion check against the real ffmpeg binary (same
    /// dependency upstream requires). Skipped when ffmpeg is not on PATH.
    #[tokio::test]
    async fn codex_convert_produces_24k_wav() {
        // 0.5 s of 16 kHz mono PCM silence → valid WAV input for ffmpeg.
        let samples = 8_000usize;
        let mut wav_in = Vec::with_capacity(44 + samples * 2);
        wav_in.extend_from_slice(b"RIFF");
        wav_in.extend_from_slice(&((36 + samples * 2) as u32).to_le_bytes());
        wav_in.extend_from_slice(b"WAVEfmt ");
        wav_in.extend_from_slice(&16u32.to_le_bytes());
        wav_in.extend_from_slice(&1u16.to_le_bytes());
        wav_in.extend_from_slice(&1u16.to_le_bytes());
        wav_in.extend_from_slice(&16_000u32.to_le_bytes());
        wav_in.extend_from_slice(&32_000u32.to_le_bytes());
        wav_in.extend_from_slice(&2u16.to_le_bytes());
        wav_in.extend_from_slice(&16u16.to_le_bytes());
        wav_in.extend_from_slice(b"data");
        wav_in.extend_from_slice(&((samples * 2) as u32).to_le_bytes());
        wav_in.resize(44 + samples * 2, 0);

        if !tokio::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
        {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }

        let wav = codex_convert_audio_to_wav(&wav_in)
            .await
            .expect("conversion should succeed");
        assert_eq!(&wav[0..4], b"RIFF");
        // 24 kHz mono output.
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        // ~0.5 s * 24000 * 2 bytes ≈ 24000 bytes of PCM.
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert!(
            data_len > 20_000 && data_len < 30_000,
            "data_len={data_len}"
        );
    }
}
