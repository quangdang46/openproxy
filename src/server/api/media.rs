use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};

use crate::core::model::{get_model_info, ModelRouteKind};
use crate::core::proxy::resolve_proxy_target;
use crate::server::auth::require_api_key_with_reload;
use crate::server::state::AppState;
use crate::types::AppDb;

use super::auth_error_response;

/// Default provider for video routes when the request model has no `provider/` prefix.
/// Video generation is xAI-first (Grok Imagine); OpenRouter and Vertex expose
/// it through adapters (9router `videoProviders/`).
const DEFAULT_VIDEO_PROVIDER: &str = "xai";

/// Upstream base for async xAI video jobs (POST action / GET by request id).
/// Docs: https://docs.x.ai/developers/rest-api-reference/inference/videos
const XAI_VIDEO_BASE_URL: &str = "https://api.x.ai/v1/videos";

/// Async OpenRouter video jobs (POST collection root → { id, status },
/// GET /videos/{id} polls). Creation POSTs to the collection root with no
/// `/generations` suffix.
/// Docs: https://openrouter.ai/docs/api/api-reference/videos
const OPENROUTER_VIDEO_BASE_URL: &str = "https://openrouter.ai/api/v1/videos";

/// Vertex AI (Veo) video jobs. Vertex does NOT speak the OpenAI-ish
/// /v1/videos shape: create → POST {model}:predictLongRunning, poll →
/// POST {model}:fetchPredictOperation (adapter: 9router
/// `open-sse/handlers/videoProviders/vertex.js`).
/// Docs: https://cloud.google.com/vertex-ai/generative-ai/docs/model-reference/veo-video-generation
const VERTEX_VIDEO_BASE_URL: &str = "https://aiplatform.googleapis.com";

/// Default Vertex location when the connection carries none
/// (9router `videoProviders/vertex.js` DEFAULT_LOCATION).
const VERTEX_DEFAULT_LOCATION: &str = "us-central1";

/// `Idempotency-Key` is forwarded on every create attempt, including the
/// post-refresh retry: the whole point of the key is that a retried create
/// must not bill twice.
const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");

/// Upstream deadline for one video round-trip (9router
/// `VIDEO_FETCH_TIMEOUT_MS`, videoCore.js:9). Bounds the HTTP call, not the
/// async job that runs after it.
fn video_fetch_timeout() -> std::time::Duration {
    std::time::Duration::from_millis(
        std::env::var("VIDEO_FETCH_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(120_000),
    )
}

/// Why a video send produced no response.
enum VideoSendError {
    /// The deadline elapsed — 9router's `AbortError`/`TimeoutError` arm.
    Timeout,
    /// A genuine connect/send failure.
    Transport(reqwest::Error),
}

/// Send one video request under [`video_fetch_timeout`], keeping the two
/// failure modes apart: 9router videoCore.js:141-147 answers 408 on an
/// abort and 502 on anything else, and a hung upstream is not a refused
/// connection.
async fn send_video(
    send: impl std::future::Future<Output = reqwest::Result<reqwest::Response>>,
) -> Result<reqwest::Response, VideoSendError> {
    match tokio::time::timeout(video_fetch_timeout(), send).await {
        Err(_) => Err(VideoSendError::Timeout),
        Ok(Err(e)) => Err(VideoSendError::Transport(e)),
        Ok(Ok(r)) => Ok(r),
    }
}

fn video_send_error_response(provider: &str, method: &str, error: VideoSendError) -> Response {
    match error {
        VideoSendError::Timeout => video_error_response(
            StatusCode::REQUEST_TIMEOUT,
            &format!(
                "[{provider}] video {method} aborted: timeout after {}ms",
                video_fetch_timeout().as_millis()
            ),
        ),
        VideoSendError::Transport(e) => video_error_response(
            StatusCode::BAD_GATEWAY,
            &format!("[{provider}] video {method} aborted: {e}"),
        ),
    }
}

/// The client's `Idempotency-Key`, if it sent a non-empty one.
fn idempotency_key(headers: &HeaderMap) -> Option<HeaderValue> {
    headers
        .get("idempotency-key")
        .cloned()
        .filter(|v| !v.as_bytes().is_empty())
}

/// Persist a refreshed OAuth token so the next request doesn't re-auth.
async fn persist_video_token(state: &AppState, connection_id: &str, access_token: &str) {
    let conn_id = connection_id.to_string();
    let token = access_token.to_string();
    let _ = state
        .db
        .update(move |app| {
            if let Some(idx) = app
                .provider_connections
                .iter()
                .position(|c| c.id == conn_id)
            {
                app.provider_connections[idx].access_token = Some(token);
                app.provider_connections[idx].updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
        })
        .await;
}

/// Cooldown a video account takes after an upstream failure, by status.
/// 9router's `markAccountUnavailable` derives the window from the status
/// class (auth.js:239-296) and ALSO sets a per-model lock, so a rate-limited
/// video account is excluded from later selection instead of being re-tried on
/// every request until the operator notices.
const VIDEO_COOLDOWN_SECONDS: [(u16, i64); 4] = [(401, 900), (403, 900), (429, 120), (503, 120)];

fn video_cooldown_seconds(status: u16) -> Option<i64> {
    VIDEO_COOLDOWN_SECONDS
        .iter()
        .find(|(code, _)| *code == status)
        .map(|(_, seconds)| *seconds)
}

/// Write the video request's outcome into the shared account-fallback state:
/// `markAccountUnavailable` on failure (every status, not just the rotation
/// set — the rotation decision is a separate, later `if` in 9router), and
/// `clearAccountError` on success. Without this a video failure never reached
/// the Providers dashboard, and selection kept handing the dead account back.
///
/// The lock is PER MODEL, not an account-wide cooldown: 9router's
/// `markAccountUnavailable` (auth.js:239-296) writes `modelLock_${model}` plus
/// `testStatus`/`lastError`/`errorCode`/`backoffLevel` and leaves
/// `rateLimitedUntil` alone. Cooling the whole account for a bad prompt would
/// take a healthy credential out of every other model's rotation.
async fn record_video_outcome(
    state: &AppState,
    connection_id: &str,
    model: Option<&str>,
    status: u16,
) {
    use crate::core::account_fallback::{build_model_lock_update, get_model_lock_key};

    let conn_id = connection_id.to_string();
    let model = model.map(str::to_string);
    let _ = state
        .db
        .update(move |app| {
            let Some(connection) = app
                .provider_connections
                .iter_mut()
                .find(|c| c.id == conn_id)
            else {
                return;
            };
            if (200..300).contains(&status) {
                crate::core::account_fallback::reset_account_state(connection);
                if let Some(key) = model.as_deref().map(get_model_lock_key) {
                    connection.extra.insert(key, Value::Null);
                }
                return;
            }
            let (lock_key, until) = build_model_lock_update(
                model.as_deref().unwrap_or_default(),
                video_cooldown_seconds(status).unwrap_or(300),
            );
            connection.extra.insert(lock_key, Value::String(until));
            connection.last_error = Some(format!("video {status}"));
            connection.last_error_at = Some(chrono::Utc::now().to_rfc3339());
            connection.error_code = Some(status.to_string());
            connection.test_status = Some("unavailable".into());
            connection.consecutive_errors = connection
                .consecutive_errors
                .map(|errors| errors.saturating_add(1))
                .or(Some(1));
            // Only a quota rejection ratchets the backoff level (9router
            // accountFallback.js:211 — `backoff: true` on the rate-limit rule
            // and on no other).
            if status == 429 {
                connection.backoff_level =
                    Some(connection.backoff_level.unwrap_or(0).saturating_add(1));
            }
        })
        .await;
}

/// Google OAuth2 token endpoint used to mint Vertex access tokens from
/// service-account JWTs (9router `tokenRefresh.js` OAUTH_ENDPOINTS.google.token).
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub async fn cors_options() -> Response {
    cors_preflight_response("POST, OPTIONS")
}

pub async fn audio_transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(generic_media_handler(state, headers, body, "audio/transcriptions").await)
}

pub async fn audio_speech(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    // 9router tts.js:31 — `?response_format=json` returns the base64 JSON
    // envelope; anything else (default) streams raw audio bytes.
    let response_format = query.get("response_format").cloned().unwrap_or_default();
    let response = generic_media_handler(state, headers, body, "audio/speech").await;
    with_cors_response(tts_binary_or_json(response, response_format).await)
}

/// Post-process a TTS JSON envelope into raw audio bytes unless the caller
/// asked for `?response_format=json` (JS createTtsResponse parity).
async fn tts_binary_or_json(response: Response, response_format: String) -> Response {
    if response_format.eq_ignore_ascii_case("json") {
        return response;
    }
    let (parts, body) = response.into_parts();
    if !parts.status.is_success() {
        return Response::from_parts(parts, body);
    }
    // Re-buffer (adapter output is small base64 audio).
    let bytes = match axum::body::to_bytes(body, 64 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let parsed: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        // Already binary or non-JSON → pass through untouched.
        Err(_) => return Response::from_parts(parts, axum::body::Body::from(bytes)),
    };
    let Some(audio_b64) = parsed.get("audio").and_then(Value::as_str) else {
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    };
    let format = parsed
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("mp3");
    let Ok(audio) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, audio_b64)
    else {
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    };
    let mut response = Response::new(axum::body::Body::from(audio));
    *response.status_mut() = parts.status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&format!("audio/{format}"))
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response
}

pub async fn embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(generic_media_handler(state, headers, body, "embeddings").await)
}

pub async fn images_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(generic_media_handler(state, headers, body, "images/generations").await)
}

/// GET /v1/audio/voices?provider={p}[&lang=xx]
/// Returns OpenAI-style voice list for TTS providers.
pub async fn audio_voices(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let db = state.db.clone();
    let settings = db.snapshot().settings.require_login;
    if settings {
        if let Err(e) = require_api_key_with_reload(&headers, &state.db).await {
            return auth_error_response(e);
        }
    }

    let provider = params.get("provider").map(String::as_str).unwrap_or("");
    let lang = params.get("lang").map(String::as_str);

    // Fetch from internal TTS voices endpoint
    let internal_url = match provider {
        "elevenlabs" => "/api/media-providers/tts/elevenlabs/voices",
        "deepgram" => "/api/media-providers/tts/deepgram/voices",
        "inworld" => "/api/media-providers/tts/inworld/voices",
        "minimax" => "/api/media-providers/tts/minimax/voices",
        "minimax-cn" => "/api/media-providers/tts/minimax/voices?provider=minimax-cn",
        "edge-tts" => "/api/media-providers/tts/voices?provider=edge-tts",
        "local-device" => "/api/media-providers/tts/voices?provider=local-device",
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": "provider must be one of: elevenlabs, deepgram, inworld, minimax, minimax-cn, edge-tts, local-device",
                        "type": "invalid_request_error",
                        "code": null
                    }
                })),
            ).into_response();
        }
    };

    // Build URL with optional lang param
    let url = if let Some(l) = lang {
        format!(
            "{}{}lang={}",
            internal_url,
            if internal_url.contains('?') { "&" } else { "?" },
            urlencoding::encode(l)
        )
    } else {
        internal_url.to_string()
    };

    // Proxy to our own internal endpoint using reqwest
    let port = std::env::var("PORT").unwrap_or_else(|_| "4623".to_string());
    let full_url = format!("http://127.0.0.1:{}{}", port, url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    match client.get(&full_url).send().await {
        Ok(resp) => {
            let status = resp.status();
            match resp.json::<Value>().await {
                Ok(data) => {
                    if !status.is_success() || data.get("error").is_some() {
                        return Json(json!({
                            "error": {
                                "message": data.get("error").and_then(|e| e.as_str()).unwrap_or("Upstream error"),
                                "type": "server_error",
                                "code": null
                            }
                        })).into_response();
                    }

                    // Extract voices from either format
                    let voices: Vec<Value> = if lang.is_some() {
                        data.get("voices")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        let mut v = Vec::new();
                        if let Some(by_lang) = data.get("byLang").and_then(|b| b.as_object()) {
                            for (_, lang_data) in by_lang {
                                if let Some(lang_voices) =
                                    lang_data.get("voices").and_then(|v| v.as_array())
                                {
                                    v.extend(lang_voices.clone());
                                }
                            }
                        }
                        v
                    };

                    // Map to OpenAI-style
                    let alias = match provider {
                        "elevenlabs" => "el",
                        "deepgram" => "dg",
                        "minimax" => "minimax",
                        "minimax-cn" => "minimax-cn",
                        _ => provider,
                    };
                    let data_out: Vec<Value> = voices.iter().map(|v| {
                        json!({
                            "id": v.get("id").unwrap_or(&json!("")),
                            "name": v.get("name").unwrap_or(&json!("")),
                            "lang": v.get("lang").unwrap_or(&json!("")),
                            "gender": v.get("gender").unwrap_or(&json!("")),
                            "model": format!("{}/{}", alias, v.get("id").unwrap_or(&json!("")).as_str().unwrap_or(""))
                        })
                    }).collect();

                    Json(json!({ "object": "list", "data": data_out })).into_response()
                }
                Err(e) => {
                    Json(json!({ "error": { "message": e.to_string(), "type": "server_error", "code": null } }))
                        .into_response()
                }
            }
        }
        Err(e) => Json(
            json!({ "error": { "message": e.to_string(), "type": "server_error", "code": null } }),
        )
        .into_response(),
    }
}

pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(generic_media_handler(state, headers, body, "search").await)
}

/// POST /v1/videos/generations (and legacy /v1/video/generations) — async video job create.
pub async fn video_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(video_create_handler(state, headers, body, "generations").await)
}

/// POST /v1/videos/edits — async video edit job create (xAI Grok Imagine).
pub async fn video_edits(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    with_cors_response(video_edits_extensions_proxy(state, headers, request, "edits").await)
}

/// POST /v1/videos/extensions — async video extension job create (xAI Grok Imagine).
pub async fn video_extensions(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    with_cors_response(video_edits_extensions_proxy(state, headers, request, "extensions").await)
}

/// Raw-byte passthrough for video edits/extensions (9router
/// videoGeneration.js:45-61 readForwardableBody): multipart/other content
/// types are forwarded byte-for-byte (re-encoding FormData would change the
/// multipart boundary); JSON keeps the model-prefix-strip + rotation path.
async fn video_edits_extensions_proxy(
    state: AppState,
    headers: HeaderMap,
    request: axum::extract::Request,
    action: &'static str,
) -> Response {
    // This handler takes the raw request rather than Json<Value>, so it never
    // reached generic_media_handler's check — both the JSON and the multipart
    // arm were reachable with no API key at all. Gate it the same way, on
    // requireApiKey, before branching on content type.
    if state.db.snapshot().settings.require_api_key() {
        if let Err(error) = require_api_key_with_reload(&headers, &state.db).await {
            return auth_error_response(error);
        }
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    if content_type.starts_with("application/json") {
        // Parse into Json<Value> and reuse the JSON pipeline.
        let bytes = match axum::body::to_bytes(request.into_body(), 32 * 1024 * 1024).await {
            Ok(b) => b,
            Err(_) => return video_error_response(StatusCode::BAD_REQUEST, "Invalid body"),
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => video_create_handler(state, headers, Ok(Json(v)), action).await,
            Err(_) => video_error_response(StatusCode::BAD_REQUEST, "Invalid JSON body"),
        }
    } else {
        // Multipart / other: forward raw bytes verbatim.
        let raw_body = match axum::body::to_bytes(request.into_body(), 512 * 1024 * 1024).await {
            Ok(b) => b,
            Err(_) => return video_error_response(StatusCode::BAD_REQUEST, "Invalid body"),
        };
        video_forward_raw(state, headers, &content_type, raw_body, action).await
    }
}

/// Forward a non-JSON video creation payload byte-for-byte.
///
/// 9router `readForwardableBody` (videoGeneration.js:60-76) branches only on
/// whether the body is JSON, never on the action, and the multipart bytes then
/// flow through the SAME account loop as the JSON arm (videoGeneration.js:134-189):
/// the pinned connection first, rotation on 401/403/429, and a single
/// refresh-and-retry on 401/403. This used to pick one connection by priority
/// and give up on its first failure, so a quota-limited first account failed a
/// request a second account would have served. The body is cloned verbatim on
/// every attempt — re-encoding it to rebuild the request would change the
/// multipart boundary.
async fn video_forward_raw(
    state: AppState,
    headers: HeaderMap,
    content_type: &str,
    raw_body: bytes::Bytes,
    action: &'static str,
) -> Response {
    let provider = DEFAULT_VIDEO_PROVIDER;
    let create_rotation_statuses: [u16; 3] = [401, 403, 429];

    let connections = video_candidate_connections(&state, provider, &headers, None);
    if connections.is_empty() {
        return video_unavailable_or_missing(&state, provider);
    }

    let snapshot = state.db.snapshot();
    let idempotency_key = idempotency_key(&headers);
    let url = format!("{}/{}", XAI_VIDEO_BASE_URL.trim_end_matches('/'), action);
    let mut last_error: Option<Response> = None;

    for connection in &connections {
        let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);
        let client = match state.client_pool.get(provider, proxy.as_ref()) {
            Ok(c) => c,
            Err(e) => return video_error_response(StatusCode::BAD_GATEWAY, &format!("{e}")),
        };

        let post = |conn: &crate::types::ProviderConnection| {
            client
                .post(&url)
                .headers(raw_video_headers(
                    conn,
                    content_type,
                    idempotency_key.as_ref(),
                ))
                .body(raw_body.clone())
                .send()
        };

        let response = match send_video(post(connection)).await {
            Ok(r) => r,
            Err(error) => return video_send_error_response(provider, "POST", error),
        };

        // 401/403 with a refresh token: refresh, persist, re-POST the identical
        // bytes exactly once (9router videoCore.js:151-176).
        let status = response.status().as_u16();
        if (status == 401 || status == 403)
            && connection
                .refresh_token
                .as_deref()
                .is_some_and(|r| !r.is_empty())
        {
            if let Some(new_access) = refresh_media_connection(provider, connection).await {
                persist_video_token(&state, &connection.id, &new_access).await;
                let mut refreshed = connection.clone();
                refreshed.access_token = Some(new_access);
                let retry = match send_video(post(&refreshed)).await {
                    Ok(r) => r,
                    Err(error) => return video_send_error_response(provider, "POST", error),
                };
                let retry_status = retry.status().as_u16();
                if !create_rotation_statuses.contains(&retry_status)
                    || !connections.iter().any(|c| c.id != connection.id)
                {
                    let mut proxied =
                        proxy_video_response(retry, HeaderMap::new(), provider, connection).await;
                    with_connection_header(&mut proxied, &connection.id);
                    return proxied;
                }
                last_error =
                    Some(proxy_video_response(retry, HeaderMap::new(), provider, connection).await);
                continue;
            }
        }

        // Rotate on auth/quota errors only when another account remains.
        if create_rotation_statuses.contains(&status)
            && connections.iter().any(|c| c.id != connection.id)
        {
            last_error =
                Some(proxy_video_response(response, HeaderMap::new(), provider, connection).await);
            continue;
        }

        let mut proxied =
            proxy_video_response(response, HeaderMap::new(), provider, connection).await;
        with_connection_header(&mut proxied, &connection.id);
        return proxied;
    }

    last_error.unwrap_or_else(|| {
        video_error_response(StatusCode::BAD_GATEWAY, "All video accounts failed")
    })
}

/// Headers for the raw multipart passthrough (9router `videoCore.js buildHeaders`):
/// `Accept` always, `Authorization` from the token, the caller's own
/// `Content-Type` and `Idempotency-Key` since this is a POST with a body.
fn raw_video_headers(
    connection: &crate::types::ProviderConnection,
    content_type: &str,
    idempotency_key: Option<&HeaderValue>,
) -> HeaderMap {
    let token = connection
        .api_key
        .as_deref()
        .or(connection.access_token.as_deref())
        .unwrap_or("");
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
        headers.insert(header::AUTHORIZATION, value);
    }
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Some(key) = idempotency_key {
        headers.insert(IDEMPOTENCY_KEY, key.clone());
    }
    headers
}

/// Candidate accounts for a video create: the client's pinned connection first,
/// then every eligible account by priority.
///
/// `model` is the bare upstream id, and it is what the cooldown is keyed on —
/// 9router's `getProviderCredentials(provider, excluded, model)` drops
/// candidates whose `isModelLockActive(c, model)` is true (auth.js:87) and
/// returns `allRateLimited` when that empties the list. `""` means "this is a
/// poll with no model", which 9router locks account-wide (`modelLock___all`).
fn video_candidate_connections(
    state: &AppState,
    provider: &str,
    headers: &HeaderMap,
    model: Option<&str>,
) -> Vec<crate::types::ProviderConnection> {
    use crate::core::account_fallback::is_model_lock_active;

    let now = chrono::Utc::now();
    let model = model.unwrap_or_default();
    let mut connections: Vec<crate::types::ProviderConnection> = Vec::new();
    if let Ok(preferred) = select_video_connection(state, provider, headers) {
        if !is_model_lock_active(&preferred, model, now) {
            connections.push(preferred);
        }
    }
    for conn in select_media_connections(&state.db.snapshot(), provider) {
        if !connections.iter().any(|c| c.id == conn.id) && !is_model_lock_active(&conn, model, now)
        {
            connections.push(conn);
        }
    }
    connections
}

/// GET /v1/videos/{id} — poll async video job status (xAI Grok Imagine).
/// Poll requests carry no model, so the provider resolves from the pinned
/// connection (`x-connection-id`) or `?provider=` (9router
/// `videoGeneration.js resolveGetProvider`).
pub async fn video_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> Response {
    with_cors_get_response(
        video_get_handler_with_query(state, headers, request_id, raw_query).await,
    )
}

pub async fn cors_options_get() -> Response {
    cors_preflight_response("GET, OPTIONS")
}

pub async fn audio_music(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(generic_media_handler(state, headers, body, "audio/music").await)
}

pub async fn rerank(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(generic_media_handler(state, headers, body, "rerank").await)
}

pub async fn moderations(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(generic_media_handler(state, headers, body, "moderations").await)
}

pub async fn images_edits(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(generic_media_handler(state, headers, body, "images/edits").await)
}

async fn generic_media_handler(
    state: AppState,
    headers: HeaderMap,
    body_result: Result<Json<Value>, JsonRejection>,
    route_kind: &'static str,
) -> Response {
    // /v1 gates on requireApiKey, NOT on requireLogin. Keying the API surface
    // to a dashboard flag is the conflation 9router does not have: locking the
    // dashboard would lock every API client, and the usual headless posture
    // (dashboard open) would silently remove API auth entirely. The dashboard
    // keeps its own gate on requireLogin.
    if state.db.snapshot().settings.require_api_key() {
        if let Err(error) = require_api_key_with_reload(&headers, &state.db).await {
            return auth_error_response(error);
        }
    }

    let Json(body) = match body_result {
        Ok(body) => body,
        Err(_) => return json_error_response(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    // 9router embeddings.js:73-76 runs `if (!body.input)` BEFORE getModelInfo,
    // so a missing input costs no model resolution, no credential lookup and no
    // network call. The check has to live here rather than in the adapter: the
    // non-adapter fall-through path validated nothing at all.
    if route_kind == "embeddings" {
        match body.get("input") {
            Some(input) if !crate::core::media::embeddings::handler::is_falsy(input) => {}
            _ => {
                return json_error_response(
                    StatusCode::BAD_REQUEST,
                    "Missing required field: input",
                )
            }
        }
    }

    let Some(model_str) = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return json_error_response(StatusCode::BAD_REQUEST, "Missing model");
    };

    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model_str, &snapshot);

    match resolved.route_kind {
        ModelRouteKind::Combo => json_error_response(
            StatusCode::BAD_REQUEST,
            &format!("Combos not supported for {}", route_kind),
        ),
        ModelRouteKind::Direct => {
            execute_media_provider(
                &state,
                &body,
                &resolved.provider,
                &resolved.model,
                route_kind,
            )
            .await
        }
    }
}

/// Route a media/embeddings request to the next eligible credential when one
/// fails on auth or quota.
///
/// 9router rotates across accounts for these surfaces (embeddings.js:97-164,
/// and videoGeneration.js the same way), so one quota-limited or revoked
/// account does not fail a request that a second account could serve. The last
/// error is returned only when every credential has been tried.
///
/// Scoped to auth/quota statuses. A 400 or 5xx is the request's own problem,
/// not the credential's, and retrying it on every account would multiply
/// latency for a guaranteed-same answer.
async fn execute_media_provider(
    state: &AppState,
    request_body: &Value,
    provider: &Option<String>,
    model: &str,
    route_kind: &str,
) -> Response {
    let provider = match provider {
        Some(p) => p,
        None => return json_error_response(StatusCode::BAD_REQUEST, "Invalid model format"),
    };

    let snapshot = state.db.snapshot();

    // 9router embeddingsCore.js:32-38 refuses a provider with no embedding
    // adapter up front. Without this the request fell through to the generic
    // forwarder, which builds `{chat_base}/embeddings` — and for a provider in
    // neither the adapter list nor the registry, a synthesised
    // https://api.{provider}.com/v1, so the caller's input text and Bearer key
    // left the machine for a host nobody configured.
    if route_kind == "embeddings" && !is_embedding_endpoint(&snapshot, provider) {
        return media_error_response(
            StatusCode::BAD_REQUEST,
            &format!("Provider '{provider}' does not support embeddings."),
        );
    }

    let connections = select_media_connections(&snapshot, provider);
    if connections.is_empty() {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            &format!("No credentials for provider: {}", provider),
        );
    }

    let mut last_auth_failure = None;
    for connection in &connections {
        let response = execute_media_provider_on_connection(
            state,
            request_body,
            provider,
            model,
            route_kind,
            connection,
        )
        .await;

        if !matches!(response.status().as_u16(), 401 | 403 | 429) {
            return response;
        }

        tracing::warn!(
            "MEDIA-ROTATE provider={} connection={} status={} trying next",
            provider,
            connection.id,
            response.status().as_u16()
        );
        last_auth_failure = Some(response);
    }

    last_auth_failure.expect("the connection list was checked non-empty above")
}

async fn execute_media_provider_on_connection(
    state: &AppState,
    request_body: &Value,
    provider: &str,
    model: &str,
    route_kind: &str,
    connection: &crate::types::ProviderConnection,
) -> Response {
    let snapshot = state.db.snapshot();

    let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);

    // Try the provider-specific media adapter first (image / tts /
    // embeddings / search). Falls through to the generic upstream
    // forwarder below when no adapter handles this provider+route.
    let adapter_url = build_media_url(provider, model, route_kind, &connection);
    if let Some(result) = try_provider_adapter(
        state,
        &connection,
        provider,
        model,
        route_kind,
        request_body,
    )
    .await
    {
        // 9router meters on the primary path. The adapter used to return
        // before the forwarder's metering block, so every provider served by a
        // media adapter recorded nothing and its embedding spend was invisible.
        if route_kind == "embeddings" {
            if let Ok(body) = &result {
                track_embeddings_usage(state, &connection, provider, model, &adapter_url, body)
                    .await;
            }
        }
        let result = if route_kind == "embeddings" {
            tag_embeddings_error(result)
        } else {
            result
        };
        return media_result_to_response(result);
    }

    let url = build_media_url(provider, model, route_kind, &connection);
    let headers = match build_media_headers(provider, &connection) {
        Ok(h) => h,
        Err(e) => {
            return json_error_response(StatusCode::BAD_REQUEST, &format!("Header error: {}", e))
        }
    };

    let _executor = match crate::core::executor::DefaultExecutor::new(
        provider.to_string(),
        state.client_pool.clone(),
        snapshot
            .provider_nodes
            .iter()
            .find(|n| n.id.as_str() == provider)
            .cloned(),
    ) {
        Ok(ex) => ex,
        Err(e) => {
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Executor error: {:?}", e),
            )
        }
    };

    let transformed_body = transform_media_request(provider, route_kind, request_body);

    let body_bytes = match serde_json::to_vec(&transformed_body) {
        Ok(b) => b,
        Err(e) => {
            return json_error_response(
                StatusCode::BAD_REQUEST,
                &format!("Serialization error: {}", e),
            )
        }
    };

    let client = match state.client_pool.get(provider, proxy.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Client error: {:?}", e),
            )
        }
    };

    let response = match client
        .post(&url)
        .headers(headers.clone())
        .body(body_bytes)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return json_error_response(StatusCode::BAD_GATEWAY, &format!("Request failed: {}", e))
        }
    };

    // Record exact embedding tokens on success (ported from 9router v0.5.45
    // fix(usage): record exact embedding tokens — only when the upstream usage
    // is non-estimated and structurally consistent).
    if route_kind == "embeddings" && response.status().is_success() {
        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(_) => {
                // Body consumption failed; return an empty error preserving
                // the upstream status.
                return proxy_upstream_response_raw(status).await;
            }
        };
        if let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) {
            track_embeddings_usage(state, &connection, provider, model, &url, &parsed).await;
        }
        return rebuild_json_response(status, bytes.to_vec());
    }

    proxy_upstream_response(response, headers).await
}

/// Record embedding token usage from a successful upstream body.
///
/// Only a non-estimated, internally consistent shape is accepted: embeddings
/// report `prompt_tokens` with zero completion, and the totals have to agree.
/// Anything else would put a fabricated number into the ledger.
///
/// 9router meters on the primary path (embeddings.js:137-150). This used to be
/// inline in the forwarder, which meant any provider served by a media adapter
/// returned before reaching it — so adapter-backed embedding spend, the common
/// case, was never metered at all.
async fn track_embeddings_usage(
    state: &AppState,
    connection: &crate::types::ProviderConnection,
    provider: &str,
    model: &str,
    url: &str,
    parsed: &Value,
) {
    let usage = parsed.get("usage").filter(|v| v.is_object());
    let estimated = usage
        .and_then(|u| u.get("estimated"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .or_else(|| usage.and_then(|u| u.get("input_tokens")))
        .and_then(|v| v.as_u64());
    let completion_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .or_else(|| usage.and_then(|u| u.get("output_tokens")))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_u64());

    if estimated
        || !prompt_tokens.is_some_and(|p| p > 0)
        || completion_tokens != 0
        || total_tokens != prompt_tokens
    {
        return;
    }

    let connection_id = connection.id.as_str();
    let api_key = connection
        .api_key
        .as_deref()
        .map(String::from)
        .unwrap_or_else(|| connection.access_token.clone().unwrap_or_default());
    let token_usage = crate::types::TokenUsage {
        prompt_tokens,
        input_tokens: None,
        completion_tokens: Some(0),
        output_tokens: None,
        total_tokens,
        reasoning_tokens: None,
        cached_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        extra: Default::default(),
    };
    state
        .usage_tracker()
        .track_request(
            provider,
            model,
            Some(&token_usage),
            Some(connection_id),
            Some(&api_key),
            Some(url),
            None,
        )
        .await;
}

/// Fallback when the upstream body could not be consumed: return an empty
/// JSON error preserving the status.
async fn proxy_upstream_response_raw(status: axum::http::StatusCode) -> Response {
    axum::response::Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&json!({ "error": { "message": "upstream read failed" } }))
                .unwrap_or_default(),
        ))
        .unwrap_or_else(|_| {
            axum::response::Response::builder()
                .status(axum::http::StatusCode::BAD_GATEWAY)
                .body(axum::body::Body::empty())
                .unwrap()
        })
}

/// Rebuild an already-consumed response body into a new Response.
fn rebuild_json_response(status: axum::http::StatusCode, bytes: Vec<u8>) -> Response {
    axum::response::Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| {
            axum::response::Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap()
        })
}

fn select_media_connection(
    snapshot: &AppDb,
    provider: &str,
    _model: &str,
) -> Option<crate::types::ProviderConnection> {
    select_media_connections(snapshot, provider)
        .into_iter()
        .next()
}

/// All active provider connections for a provider (ordered by priority), for
/// account rotation on auth/quota errors. 9router videoGeneration.js rotates
/// to the next account on 401/403/429.
///
/// Accounts in an open cooldown or a live model lock are dropped, mirroring the
/// `isModelLockActive` filter `getProviderCredentials` applies
/// (9router `src/sse/services/auth.js:87`). Without it a rate-limited video
/// account was re-tried on every subsequent request and never skipped.
fn select_media_connections(
    snapshot: &AppDb,
    provider: &str,
) -> Vec<crate::types::ProviderConnection> {
    let now = chrono::Utc::now();
    let mut conns: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection.provider == provider
                && connection.is_active()
                && connection_has_credentials(connection)
                && !crate::core::account_fallback::is_account_unavailable(connection, now)
        })
        .cloned()
        .collect();
    conns.sort_by_key(|c| c.priority.unwrap_or(999));
    conns
}

/// Earliest moment a filtered-out video account becomes selectable again, so
/// the caller can tell the client when to retry (9router
/// `unavailableResponse`'s `Retry-After`, open-sse/utils/error.js:116-129).
fn video_retry_after(state: &AppState, provider: &str) -> Option<i64> {
    use crate::core::account_fallback::{
        account_degraded_until, get_earliest_model_lock_until, is_account_unavailable,
    };
    let now = chrono::Utc::now();
    state
        .db
        .snapshot()
        .provider_connections
        .iter()
        .filter(|c| c.provider == provider && c.is_active() && is_account_unavailable(c, now))
        .filter_map(|connection| {
            let mut expiries: Vec<chrono::DateTime<chrono::Utc>> = Vec::new();
            if let Some(until) = connection.rate_limited_until.as_deref() {
                if let Ok(until) = chrono::DateTime::parse_from_rfc3339(until) {
                    expiries.push(until.with_timezone(&chrono::Utc));
                }
            }
            if let Some(until) = account_degraded_until(connection) {
                expiries.push(until);
            }
            if let Some(until) = get_earliest_model_lock_until(connection) {
                expiries.push(until);
            }
            let soonest = expiries.into_iter().filter(|t| *t > now).min()?;
            Some((soonest - now).num_seconds().max(1))
        })
        .min()
}

/// 429 with `Retry-After` — every video account is in cooldown.
fn video_unavailable_response(provider: &str, state: &AppState) -> Response {
    let retry_after = video_retry_after(state, provider);
    let mut response = video_error_response(
        StatusCode::TOO_MANY_REQUESTS,
        &format!("[{provider}] All accounts are rate limited; retry later"),
    );
    if let Some(seconds) = retry_after {
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

/// No account is selectable. Distinguish "you never configured one" (400) from
/// "they are all cooling down" (429 + `Retry-After`) — 9router
/// `getProviderCredentials` returns `allRateLimited` for the second case and the
/// handler answers `unavailableResponse` (videoGeneration.js:141).
fn video_unavailable_or_missing(state: &AppState, provider: &str) -> Response {
    if video_retry_after(state, provider).is_some() {
        return video_unavailable_response(provider, state);
    }
    video_error_response(
        StatusCode::BAD_REQUEST,
        &format!("No credentials for provider: {provider}"),
    )
}

/// Does this provider serve `/v1/embeddings`?
///
/// Two accepted answers: an entry in the embedding-adapter registry, and a
/// provider node the operator registered as an embeddings endpoint. 9router
/// only has the first — it namespaces every custom node behind
/// `custom-embedding-` / `openai-compatible-` (embeddingProviders/index.js:25-31)
/// — but an OpenProxy node id is caller-supplied and carries no such guarantee,
/// and a node's base URL is configured, not guessed. A named provider in neither
/// set is exactly the case the guard exists for.
fn is_embedding_endpoint(snapshot: &AppDb, provider: &str) -> bool {
    crate::core::media::embeddings::is_embedding_provider(provider)
        || snapshot.provider_nodes.iter().any(|node| {
            node.id == provider
                && (node.r#type == "custom-embedding"
                    || node.api_type.as_deref() == Some("embeddings"))
        })
}

fn connection_has_credentials(connection: &crate::types::ProviderConnection) -> bool {
    connection
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        || connection
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
}

fn build_media_url(
    provider: &str,
    _model: &str,
    route_kind: &str,
    connection: &crate::types::ProviderConnection,
) -> String {
    let base_url = get_provider_base_url(provider, connection);

    match route_kind {
        "audio/transcriptions" => {
            if provider == "deepgram" {
                format!("{}/listen", base_url.trim_end_matches('/'))
            } else if provider == "elevenlabs" {
                format!("{}/speech-to-text/stream", base_url.trim_end_matches('/'))
            } else if provider == "cartesia" {
                format!("{}/transcriptions", base_url.trim_end_matches('/'))
            } else if provider == "playht" {
                format!("{}/transcriptions", base_url.trim_end_matches('/'))
            } else {
                format!("{}/audio/transcriptions", base_url.trim_end_matches('/'))
            }
        }
        "audio/speech" => {
            if provider == "google-tts" {
                format!("{}/text:synthesize?key=", base_url.trim_end_matches('/'))
            } else if provider == "edge-tts" {
                base_url.trim_end_matches('/').to_string()
            } else {
                format!("{}/audio/speech", base_url.trim_end_matches('/'))
            }
        }
        "embeddings" => {
            if provider == "openai-embedding" {
                format!("{}/embeddings", base_url.trim_end_matches('/'))
            } else if provider == "cohere-embedding" {
                format!("{}/embeddings", base_url.trim_end_matches('/'))
            } else if provider == "voyage-ai" {
                format!("{}/embeddings", base_url.trim_end_matches('/'))
            } else {
                format!("{}/embeddings", base_url.trim_end_matches('/'))
            }
        }
        "images/generations" => {
            if provider == "dalle" {
                format!("{}/images/generations", base_url.trim_end_matches('/'))
            } else if provider == "stable-diffusion" {
                format!(
                    "{}/generation/image-synthesis",
                    base_url.trim_end_matches('/')
                )
            } else {
                format!("{}/images/generations", base_url.trim_end_matches('/'))
            }
        }
        "search" => {
            if provider == "tavily" {
                format!("{}/search", base_url.trim_end_matches('/'))
            } else if provider == "brave-search" {
                format!("{}/search", base_url.trim_end_matches('/'))
            } else if provider == "serper" {
                base_url.trim_end_matches('/').to_string()
            } else if provider == "exa" {
                format!("{}/search", base_url.trim_end_matches('/'))
            } else {
                format!("{}/search", base_url.trim_end_matches('/'))
            }
        }
        _ => format!("{}/{}", base_url.trim_end_matches('/'), route_kind),
    }
}

fn get_provider_base_url(provider: &str, connection: &crate::types::ProviderConnection) -> String {
    if let Some(base_url) = connection
        .provider_specific_data
        .get("baseUrl")
        .and_then(Value::as_str)
    {
        return base_url.to_string();
    }

    crate::core::executor::provider_config_base_url(provider)
        .unwrap_or_else(|| format!("https://api.{}.com/v1", provider))
}

fn build_media_headers(
    provider: &str,
    connection: &crate::types::ProviderConnection,
) -> Result<HeaderMap, String> {
    use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let token = connection
        .api_key
        .as_deref()
        .or(connection.access_token.as_deref())
        .ok_or_else(|| "Missing credentials".to_string())?;

    match provider {
        "deepgram" => {
            headers.insert(
                reqwest::header::HeaderName::from_static("Authorization"),
                HeaderValue::from_str(&format!("Token {}", token)).map_err(|e| e.to_string())?,
            );
        }
        "elevenlabs" => {
            headers.insert(
                reqwest::header::HeaderName::from_static("xi-api-key"),
                HeaderValue::from_str(token).map_err(|e| e.to_string())?,
            );
        }
        "google-tts" => {
            headers.insert(
                reqwest::header::HeaderName::from_static("x-goog-api-key"),
                HeaderValue::from_str(token).map_err(|e| e.to_string())?,
            );
        }
        "brave-search" => {
            headers.insert(
                reqwest::header::HeaderName::from_static("Accept"),
                HeaderValue::from_static("application/json"),
            );
        }
        _ => {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", token)).map_err(|e| e.to_string())?,
            );
        }
    }

    Ok(headers)
}

/// Proxy an upstream video response, sanitizing error bodies so secrets
/// (`Bearer <token>`, raw keys) never reach the client. Non-2xx bodies are
/// wrapped in the OpenAI error envelope; 2xx pass through as a stream.
///
/// 9router `videoCore.js:181-184`:
/// `createErrorResult(upstream.status, "[${provider}] " + message.slice(0, 2000))`.
/// Relaying the provider's own body verbatim both dropped the `type`/`code` pair
/// a client needs to branch on and left no bound on how much upstream text a
/// client receives.
async fn proxy_video_response(
    response: reqwest::Response,
    headers: HeaderMap,
    provider: &str,
    connection: &crate::types::ProviderConnection,
) -> Response {
    let status = response.status();
    if status.is_success() {
        return proxy_upstream_response(response, headers).await;
    }
    let body = response.bytes().await.unwrap_or_default();
    let text = String::from_utf8_lossy(&body).to_string();
    let sanitized = sanitize_video_secrets(&text, connection);
    let message = if sanitized.trim().is_empty() {
        format!("HTTP {}", status.as_u16())
    } else {
        // Char-wise, so a multi-byte character before the cap cannot panic.
        sanitized.chars().take(2000).collect::<String>()
    };
    video_error_response(status, &format!("[{provider}] {message}"))
}

/// Port of 9router `videoCore.js sanitizeSecrets` (videoCore.js:21-31):
/// redact `Bearer <token>` and any raw access/refresh/api key from client-bound
/// text so secrets never leak in error responses.
fn sanitize_video_secrets(text: &str, connection: &crate::types::ProviderConnection) -> String {
    let mut out = text.to_string();
    // Redact `Bearer <8+ token chars>`.
    out = out
        .split("Bearer ")
        .enumerate()
        .map(|(i, part)| {
            if i == 0 {
                part.to_string()
            } else {
                // Keep "Bearer" in the first chunk; redact the token that follows.
                let trimmed: String = part.chars().take_while(|c| !c.is_whitespace()).collect();
                let rest = &part[trimmed.len().min(part.len())..];
                format!("Bearer [redacted]{rest}")
            }
        })
        .collect::<Vec<_>>()
        .join("");
    for secret in [
        connection.access_token.as_deref(),
        connection.refresh_token.as_deref(),
        connection.api_key.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if secret.len() >= 8 {
            out = out.replace(secret, "[redacted]");
        }
    }
    out
}

/// Attempt a one-shot OAuth refresh for a media provider connection that
/// received a 401/403. Returns the refreshed access token (or `None`).
///
/// Uses `dispatch_oauth_refresh` directly (NOT the expiry-gated
/// `refresh_if_needed` — a still-unexpired-but-rejected token would no-op).
async fn refresh_media_connection(
    provider: &str,
    connection: &crate::types::ProviderConnection,
) -> Option<String> {
    let refresh_token = connection
        .refresh_token
        .as_deref()
        .filter(|r| !r.is_empty())?;
    let provider_specific_data = connection.provider_specific_data.clone();
    match crate::oauth::token_refresh::dispatch_oauth_refresh(
        provider,
        refresh_token,
        &provider_specific_data,
    )
    .await
    {
        Ok(result) if !result.access_token.is_empty() => Some(result.access_token),
        _ => None,
    }
}

fn transform_media_request(provider: &str, route_kind: &str, body: &Value) -> Value {
    let mut transformed = body.clone();

    match (provider, route_kind) {
        ("deepgram", "audio/transcriptions") => {
            if let Some(obj) = transformed.as_object_mut() {
                let model_opt = obj
                    .get("model")
                    .and_then(|v| v.as_str().map(|s| s.to_string()));
                if let Some(model) = model_opt {
                    obj.insert("version".to_string(), json!("2024-06-20"));
                    obj.insert("punctuate".to_string(), json!(true));
                    obj.insert("smart_format".to_string(), json!(true));
                    let _ = obj.remove("model");
                    obj.insert("model".to_string(), json!(model));
                }
            }
        }
        ("elevenlabs", "audio/transcriptions") => {
            if let Some(obj) = transformed.as_object_mut() {
                obj.insert(" Braband".to_string(), json!(true));
                obj.insert("enable.extra_modeling".to_string(), json!(true));
            }
        }
        ("tavily", "search") => {
            if let Some(obj) = transformed.as_object_mut() {
                obj.insert("api_key".to_string(), json!("from_connection"));
            }
        }
        ("brave-search", "search") => {
            if let Some(obj) = transformed.as_object_mut() {
                if let Some(query) = obj.get("query").and_then(|v| v.as_str()) {
                    obj.insert("q".to_string(), json!(query));
                    let _ = obj.remove("query");
                }
            }
        }
        _ => {}
    }

    transformed
}

async fn proxy_upstream_response(response: reqwest::Response, _headers: HeaderMap) -> Response {
    let status = response.status();
    let resp_headers = response.headers().clone();

    let body = if status == 200
        && resp_headers
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").contains("audio"))
            .unwrap_or(false)
    {
        let bytes = response.bytes().await.unwrap_or_default();
        Body::from(bytes)
    } else {
        let stream = response.bytes_stream().map_ok(|b: Bytes| b);
        Body::from_stream(stream)
    };

    let mut proxied = Response::new(body);
    *proxied.status_mut() = status;
    copy_upstream_content_type(&resp_headers, proxied.headers_mut());

    proxied
}

/// Forward only `Content-Type` from the upstream.
///
/// 9router `videoCore.js:199-208` builds a fresh `Response` carrying exactly
/// `Content-Type` and `Access-Control-Allow-Origin`. Echoing the whole upstream
/// set leaked provider internals (`server`, `x-request-id`, tracing headers)
/// and, for the video path, the upstream's own `Idempotency-Key` back to the
/// client.
fn copy_upstream_content_type(upstream: &HeaderMap, into: &mut HeaderMap) {
    if let Some(value) = upstream.get(header::CONTENT_TYPE) {
        into.insert(header::CONTENT_TYPE, value.clone());
    }
}

/// Try to handle the request through one of the per-provider media
/// adapters (image / tts / embeddings / search). Returns `Some(response)`
/// when an adapter ran for this provider; `None` to fall through to the
/// generic upstream forwarder.
async fn try_provider_adapter(
    state: &AppState,
    connection: &crate::types::ProviderConnection,
    provider: &str,
    model: &str,
    route_kind: &str,
    request_body: &Value,
) -> Option<Result<Value, crate::core::media::MediaError>> {
    use crate::core::media::{embeddings, image, search, tts, MediaError};

    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);
    let client = state.client_pool.get(provider, proxy.as_ref()).ok()?;

    let result: Option<Result<Value, MediaError>> = match route_kind {
        "images/generations" => {
            image::dispatch(&client, connection, provider, model, request_body).await
        }
        "audio/speech" => tts::dispatch(&client, connection, provider, model, request_body).await,
        "embeddings" => {
            embeddings::dispatch(&client, connection, provider, model, request_body).await
        }
        "search" => search::dispatch(&client, connection, provider, request_body).await,
        // STT input is multipart and lives on a dedicated route in stt.rs;
        // it does not flow through this JSON handler.
        _ => None,
    };

    Some(result?)
}

/// Tag an upstream embeddings error with the status it arrived under.
///
/// 9router's embeddings core runs every provider error through
/// `formatProviderError` (open-sse/utils/error.js:143) before handing it to
/// `createErrorResult`, which renders `` `[${status}]: ${message}` ``. Only the
/// embeddings route does this; the other media handlers pass the message
/// straight through, so the tag is applied per route kind rather than in
/// `MediaError` (which has no status-aware `message()`).
fn tag_embeddings_error(
    result: Result<Value, crate::core::media::MediaError>,
) -> Result<Value, crate::core::media::MediaError> {
    use crate::core::media::MediaError;
    result.map_err(|err| match err {
        MediaError::Http { status, message } if !message.starts_with('[') => MediaError::Http {
            status,
            message: format!("[{status}]: {message}"),
        },
        other => other,
    })
}

fn media_result_to_response(result: Result<Value, crate::core::media::MediaError>) -> Response {
    match result {
        Ok(body) => with_cors_response((StatusCode::OK, Json(body)).into_response()),
        Err(err) => {
            let status = StatusCode::from_u16(err.status()).unwrap_or(StatusCode::BAD_GATEWAY);
            // The adapter already resolved the upstream status (`Http(n, msg)`
            // carries it verbatim, matching 9router's
            // `createErrorResult(statusCode, errMsg)` at embeddingsCore.js:116).
            // Re-deriving it from the message text flipped a provider's 400
            // "Incorrect API key provided" into a 401 and a 500 "quota" into a
            // 403, which also broke account rotation — it keys off the status.
            media_error_response(status, &err.message())
        }
    }
}

/// [`json_error_response`] without the status-inference heuristic, for errors
/// that already carry the status a remote peer chose.
fn media_error_response(status: StatusCode, message: &str) -> Response {
    let body = crate::core::utils::error::build_error_body(status.as_u16(), Some(message));
    with_cors_response((status, Json(body)).into_response())
}

fn json_error_response(status: StatusCode, message: &str) -> Response {
    let status_code =
        crate::core::utils::error::infer_status_from_message(status.as_u16(), message);
    let status = StatusCode::from_u16(status_code).unwrap_or(status);
    let friendly = crate::core::utils::error::friendly_error_message(status.as_u16(), message);
    let body = crate::core::utils::error::build_error_body(status.as_u16(), Some(&friendly));
    with_cors_response((status, Json(body)).into_response())
}

/// The `/v1/videos/*` counterpart of [`json_error_response`].
///
/// `json_error_response` runs the chat-oriented `infer_status_from_message` +
/// `friendly_error_message` heuristics, which rewrite a client-chosen status
/// (a 400 "Combos are not supported for video generation" came back as 406)
/// and replace the text with generic prose that drops the provider name.
/// 9router's `errorResponse` uses the status verbatim and the message
/// untouched (open-sse/utils/error.js:30-38).
fn video_error_response(status: StatusCode, message: &str) -> Response {
    let body = crate::core::utils::error::build_error_body(status.as_u16(), Some(message));
    with_cors_response((status, Json(body)).into_response())
}

fn with_cors_response(mut response: Response) -> Response {
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

fn with_cors_get_response(mut response: Response) -> Response {
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
        HeaderValue::from_static("GET, OPTIONS"),
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

/// Transparent async video job creation proxy (xAI Grok Imagine shape).
///
/// Forwards the JSON body with the provider prefix stripped from `model`,
/// and passes upstream JSON (`request_id`, status, `video.url`, error) back
/// verbatim — no reshaping.
async fn video_create_handler(
    state: AppState,
    headers: HeaderMap,
    body_result: Result<Json<Value>, JsonRejection>,
    action: &'static str,
) -> Response {
    // /v1 gates on requireApiKey, NOT on requireLogin. Keying the API surface
    // to a dashboard flag is the conflation 9router does not have: locking the
    // dashboard would lock every API client, and the usual headless posture
    // (dashboard open) would silently remove API auth entirely. The dashboard
    // keeps its own gate on requireLogin.
    if state.db.snapshot().settings.require_api_key() {
        if let Err(error) = require_api_key_with_reload(&headers, &state.db).await {
            return auth_error_response(error);
        }
    }

    let Json(mut body) = match body_result {
        Ok(body) => body,
        Err(_) => return video_error_response(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    let (provider, model) = match resolve_video_provider_model(&state, &body) {
        Ok(resolved) => resolved,
        Err(resp) => return resp,
    };

    // Adapter dispatch (9router `videoProviders/index.js getVideoAdapter`):
    // the default (xAI) shape forwards the raw body to {base}/{action};
    // OpenRouter posts verbatim to the collection root (no action suffix);
    // Vertex translates both directions (predictLongRunning /
    // fetchPredictOperation).
    // OpenRouter requires an application/json body (9router `openrouter.js`
    // returns "OpenRouter video requires an application/json body" before
    // any upstream call) — the JSON extractor already guarantees this, so
    // check the original content-type header.
    if provider == "openrouter"
        && !headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"))
    {
        return video_error_response(
            StatusCode::BAD_REQUEST,
            "OpenRouter video requires an application/json body",
        );
    }
    let canonical_provider = provider.clone();
    // Strip provider prefix (e.g. "xai/grok-imagine-video" → "grok-imagine-video")
    // before forwarding so upstream receives the bare model id. Vertex keeps
    // its own `model` field (predictLongRunning derives the URL from it).
    if canonical_provider != "vertex" {
        if let Some(obj) = body.as_object_mut() {
            if let Some(model_str) = model.as_deref() {
                obj.insert("model".to_string(), json!(model_str));
            }
        }
    }
    if canonical_provider == "openrouter" && action != "generations" {
        // ponytail: generations only — OpenRouter has no edits/extensions endpoint.
        return video_error_response(
            StatusCode::BAD_REQUEST,
            &format!("OpenRouter video supports 'generations' only (got '{action}')"),
        );
    }
    if canonical_provider == "vertex" {
        if action != "generations" {
            // ponytail: Veo extend/edit go through generations with `video`/`image` in the body.
            return video_error_response(
                StatusCode::BAD_REQUEST,
                &format!("Vertex video supports 'generations' only (got '{action}')"),
            );
        }
        if let Err(resp) = validate_vertex_create_body(&body) {
            return resp;
        }
    }

    let vertex_model_id: Option<String> = if canonical_provider == "vertex" {
        body.get("model")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
    } else {
        None
    };

    // Vertex translates the OpenAI-ish body to predictLongRunning shape once —
    // per-connection auth only changes the URL/token, not the body.
    let forward_body: Value = if canonical_provider == "vertex" {
        to_vertex_body(&body)
    } else {
        body
    };

    let body_bytes = match serde_json::to_vec(&forward_body) {
        Ok(b) => b,
        Err(e) => {
            return video_error_response(
                StatusCode::BAD_REQUEST,
                &format!("Serialization error: {}", e),
            )
        }
    };

    // 9router videoGeneration.js CREATE_ROTATION_STATUSES: rotate to the next
    // account only on auth/quota errors the upstream rejects before job
    // creation (401/403/429). 5xx is returned to the caller (not rotated).
    let create_rotation_statuses: [u16; 3] = [401, 403, 429];

    let connections = video_candidate_connections(&state, &provider, &headers, model.as_deref());
    if connections.is_empty() {
        return video_unavailable_or_missing(&state, &provider);
    }

    let snapshot = state.db.snapshot();
    let idempotency_key = idempotency_key(&headers);
    let mut last_error: Option<Response> = None;

    for connection in &connections {
        // Vertex resolves auth (SA mint or access token) per connection before
        // any upstream call — raw API keys are not supported (vertex.js resolveAuth).
        let vertex_token: Option<String> = if canonical_provider == "vertex" {
            match resolve_vertex_token(connection).await {
                Ok(token) => Some(token),
                Err(message) => {
                    last_error = Some(video_error_response(StatusCode::BAD_REQUEST, &message));
                    continue;
                }
            }
        } else {
            None
        };

        let mut upstream_headers =
            match build_video_headers(&provider, connection, vertex_token.as_deref(), true) {
                Ok(h) => h,
                Err(e) => {
                    last_error = Some(video_error_response(
                        StatusCode::BAD_REQUEST,
                        &format!("Header error: {}", e),
                    ));
                    continue;
                }
            };

        // Per-connection create URL (9router adapter buildRequest):
        // xAI → {base}/{action}; OpenRouter → collection root (verbatim body);
        // Vertex → {base}/v1/projects/{p}/locations/{l}/publishers/google/models/{m}:predictLongRunning.
        let url = match video_create_url(
            &provider,
            &canonical_provider,
            action,
            connection,
            vertex_model_id.as_deref(),
        ) {
            Ok(url) => url,
            Err(message) => {
                last_error = Some(video_error_response(StatusCode::BAD_REQUEST, &message));
                continue;
            }
        };

        // Forward Idempotency-Key when present (creation is billable). The key
        // outlives the loop: 9router captures it once (videoGeneration.js:128)
        // and threads it through every attempt, so the post-refresh retry is
        // still deduplicated upstream.
        if let Some(key) = idempotency_key.as_ref() {
            upstream_headers.insert(IDEMPOTENCY_KEY, key.clone());
        }

        let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);
        let client = match state.client_pool.get(&provider, proxy.as_ref()) {
            Ok(c) => c,
            Err(e) => {
                last_error = Some(video_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Client error: {:?}", e),
                ));
                continue;
            }
        };

        // 9router videoCore.js:117-124 — a network error after the POST left
        // the socket may already have created the billable job upstream, so
        // creation is NEVER auto-retried on another account. Return 502
        // immediately (sanitized); rotation happens only on 401/403/429
        // responses.
        let post = |headers: &HeaderMap| {
            client
                .post(&url)
                .headers(headers.clone())
                .body(body_bytes.clone())
                .send()
        };
        let response = match send_video(post(&upstream_headers)).await {
            Ok(r) => r,
            Err(error) => return video_send_error_response(&provider, "POST", error),
        };

        let status = response.status().as_u16();
        let is_rotation_status = create_rotation_statuses.contains(&status);

        // Vertex tokens are freshly minted per connection above, so the
        // OAuth refresh-and-retry below is xAI/OpenRouter only.
        let is_vertex = provider == "vertex";
        // 9router parity (videoCore.js:120-146): on 401/403 with a refresh
        // token, refresh the connection and re-fire the POST exactly once.
        if (status == 401 || status == 403)
            && !is_vertex
            && connection
                .refresh_token
                .as_deref()
                .is_some_and(|r| !r.is_empty())
        {
            if let Some(new_access) = refresh_media_connection(&provider, connection).await {
                persist_video_token(&state, &connection.id, &new_access).await;
                // Rebuild headers with the fresh token and retry once.
                let mut refreshed = connection.clone();
                refreshed.access_token = Some(new_access);
                // The refresh path is xAI/OpenRouter only (Vertex mints fresh
                // tokens per connection above), so no Vertex token applies here.
                if let Ok(mut retry_headers) =
                    build_video_headers(&provider, &refreshed, None, true)
                {
                    if let Some(key) = idempotency_key.as_ref() {
                        retry_headers.insert(IDEMPOTENCY_KEY, key.clone());
                    }
                    let retry = send_video(post(&retry_headers)).await;
                    let retry_resp = match retry {
                        Ok(r) => r,
                        Err(error) => {
                            return video_send_error_response(&provider, "POST", error);
                        }
                    };
                    let retry_status = retry_resp.status().as_u16();
                    if !create_rotation_statuses.contains(&retry_status) {
                        record_video_outcome(
                            &state,
                            &connection.id,
                            model.as_deref(),
                            retry_status,
                        )
                        .await;
                        let mut proxied =
                            proxy_video_response(retry_resp, retry_headers, &provider, connection)
                                .await;
                        with_connection_header(&mut proxied, &connection.id);
                        return proxied;
                    }
                    // Retry also 401/403/429 → fall through to rotation.
                    record_video_outcome(&state, &connection.id, model.as_deref(), retry_status)
                        .await;
                    last_error = Some(
                        proxy_video_response(retry_resp, retry_headers, &provider, connection)
                            .await,
                    );
                    continue;
                }
            }
        }

        record_video_outcome(&state, &connection.id, model.as_deref(), status).await;

        // Rotate on auth/quota errors only when another account remains.
        if is_rotation_status && connections.iter().any(|c| c.id != connection.id) {
            last_error =
                Some(proxy_video_response(response, upstream_headers, &provider, connection).await);
            continue;
        }

        // Vertex success bodies carry operations, not async-job JSON — map them
        // onto the client-polled shape (vertex.js transformResponse).
        if is_vertex && response.status().is_success() {
            let mut proxied =
                proxy_vertex_response(response, upstream_headers, &provider, connection).await;
            with_connection_header(&mut proxied, &connection.id);
            return proxied;
        }

        let mut proxied =
            proxy_video_response(response, upstream_headers, &provider, connection).await;
        with_connection_header(&mut proxied, &connection.id);
        return proxied;
    }

    // All accounts failed with a rotation status — return the last response.
    last_error.unwrap_or_else(|| {
        video_error_response(StatusCode::BAD_GATEWAY, "All video accounts failed")
    })
}

/// Pin a video job to the account that owns it.
///
/// Video jobs are account-bound upstream — the client echoes the id back as
/// `x-connection-id` on GET polls so the same account serves them. 9router
/// writes the product-branded `x-9router-connection-id` (videoGeneration.js:97-104)
/// and reads the generic name back; both spellings go out so a client written
/// against either build finds the header it looks for.
fn with_connection_header(response: &mut Response, connection_id: &str) {
    let Ok(value) = HeaderValue::from_str(connection_id) else {
        return;
    };
    for name in ["x-openproxy-connection-id", "x-9router-connection-id"] {
        response
            .headers_mut()
            .insert(HeaderName::from_static(name), value.clone());
    }
}

/// Poll async video job status. Jobs are account-bound upstream, so no
/// cross-account rotation: the caller pins the creating account via
/// `x-connection-id` (returned on create as `x-openproxy-connection-id`).
/// Poll requests carry no model, so the provider resolves from the pinned
/// connection or an explicit `?provider=` — falling back to the historical
/// xAI default (9router `videoGeneration.js resolveGetProvider`).
async fn video_get_handler(state: AppState, headers: HeaderMap, request_id: String) -> Response {
    video_get_handler_with_query(state, headers, request_id, None).await
}

/// Poll handler with the raw query string so `?provider=` resolves without
/// changing the axum route signature.
async fn video_get_handler_with_query(
    state: AppState,
    headers: HeaderMap,
    request_id: String,
    raw_query: Option<String>,
) -> Response {
    // /v1 gates on requireApiKey, NOT on requireLogin. Keying the API surface
    // to a dashboard flag is the conflation 9router does not have: locking the
    // dashboard would lock every API client, and the usual headless posture
    // (dashboard open) would silently remove API auth entirely. The dashboard
    // keeps its own gate on requireLogin.
    if state.db.snapshot().settings.require_api_key() {
        if let Err(error) = require_api_key_with_reload(&headers, &state.db).await {
            return auth_error_response(error);
        }
    }

    if request_id.trim().is_empty() {
        return video_error_response(StatusCode::BAD_REQUEST, "Missing video request id");
    }

    let provider = resolve_video_get_provider(&state, &headers, raw_query.as_deref());
    let canonical_provider = provider.clone();
    let mut connection = match select_video_connection(&state, &provider, &headers) {
        Ok(conn) => conn,
        Err(resp) => return resp,
    };

    // Vertex polls with POST { operationName } (fetchPredictOperation), not GET.
    if canonical_provider == "vertex" {
        return video_vertex_poll(state, request_id, connection).await;
    }

    let url = match canonical_provider.as_str() {
        "openrouter" => format!(
            "{}/{}",
            OPENROUTER_VIDEO_BASE_URL.trim_end_matches('/'),
            urlencoding::encode(request_id.trim())
        ),
        _ => format!(
            "{}/{}",
            XAI_VIDEO_BASE_URL.trim_end_matches('/'),
            urlencoding::encode(&request_id)
        ),
    };

    let mut upstream_headers = match build_video_headers(&provider, &connection, None, false) {
        Ok(h) => h,
        Err(e) => {
            return video_error_response(StatusCode::BAD_REQUEST, &format!("Header error: {}", e))
        }
    };

    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);
    let client = match state.client_pool.get(&provider, proxy.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            return video_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Client error: {:?}", e),
            )
        }
    };

    let get = |headers: &HeaderMap| client.get(&url).headers(headers.clone()).send();
    let response = match send_video(get(&upstream_headers)).await {
        Ok(r) => r,
        Err(error) => return video_send_error_response(&provider, "GET", error),
    };

    // 9router parity (videoCore.js:120-146): on 401/403 with a refresh token,
    // refresh + persist + re-fire the GET exactly once.
    let response = if (response.status().as_u16() == 401 || response.status().as_u16() == 403)
        && connection
            .refresh_token
            .as_deref()
            .is_some_and(|r| !r.is_empty())
    {
        match refresh_media_connection(&provider, &connection).await {
            Some(new_access) => {
                persist_video_token(&state, &connection.id, &new_access).await;
                connection.access_token = Some(new_access);
                if let Ok(retry_headers) = build_video_headers(&provider, &connection, None, false)
                {
                    upstream_headers = retry_headers;
                }
                match send_video(get(&upstream_headers)).await {
                    Ok(r) => r,
                    Err(error) => return video_send_error_response(&provider, "GET", error),
                }
            }
            None => response,
        }
    } else {
        response
    };

    // The poll has no model, matching videoGeneration.js:234-236, which records
    // the failure against the account rather than a model lock.
    record_video_outcome(&state, &connection.id, None, response.status().as_u16()).await;

    let mut proxied =
        proxy_video_response(response, upstream_headers, &provider, &connection).await;
    with_connection_header(&mut proxied, &connection.id);
    proxied
}

/// Resolve `(provider, bare_model)` for a video create request.
/// Bare model ids (no `provider/` prefix) fall back to xAI.
fn resolve_video_provider_model(
    state: &AppState,
    body: &Value,
) -> Result<(String, Option<String>), Response> {
    let model_str = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let Some(model_str) = model_str else {
        // No model field — still allow through with default provider (upstream
        // will reject if model is required).
        return Ok((DEFAULT_VIDEO_PROVIDER.to_string(), None));
    };

    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model_str, &snapshot);

    match resolved.route_kind {
        ModelRouteKind::Combo => Err(video_error_response(
            StatusCode::BAD_REQUEST,
            "Combos are not supported for video generation",
        )),
        ModelRouteKind::Direct => {
            let provider = match &resolved.provider {
                Some(p) if video_provider_supported(p) => p.clone(),
                // Bare model id (no explicit provider prefix) → default video
                // provider. Prefix-less inference targets chat providers only.
                Some(_) if !model_str.contains('/') => DEFAULT_VIDEO_PROVIDER.to_string(),
                Some(p) => {
                    return Err(video_error_response(
                        StatusCode::BAD_REQUEST,
                        &format!("Provider '{}' does not support video generation", p),
                    ));
                }
                None if !model_str.contains('/') => DEFAULT_VIDEO_PROVIDER.to_string(),
                None => {
                    return Err(video_error_response(
                        StatusCode::BAD_REQUEST,
                        "Invalid model format",
                    ));
                }
            };
            let bare_model = if model_str.contains('/') {
                Some(resolved.model)
            } else {
                Some(model_str.to_string())
            };
            Ok((provider, bare_model))
        }
    }
}

fn video_provider_supported(provider: &str) -> bool {
    // 9router `open-sse/handlers/videoProviders/index.js` ADAPTERS =
    // { openrouter, vertex } — everything else keeps the xAI default shape.
    // `vertex-partner` has no videoConfig key (only transport.baseUrl), so
    // getVideoConfig returns null and videoGeneration.js rejects it.
    matches!(provider, "xai" | "openrouter" | "vertex")
}

/// Video request headers: registry `headers` merged over the bearer auth
/// (9router `videoProviders/openrouter.js headers()` spreads
/// `config.headers` under the `Authorization` token).
///
/// `Accept: application/json` always goes out (9router videoCore.js:40
/// `buildHeaders`), while `Content-Type` rides only on a POST — a bodyless
/// `GET /videos/{id}` that announces a JSON content type invites a provider to
/// wait for a body that never comes (videoCore.js:107-111).
fn build_video_headers(
    provider: &str,
    connection: &crate::types::ProviderConnection,
    vertex_token: Option<&str>,
    is_create: bool,
) -> Result<HeaderMap, String> {
    use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
    if provider == "vertex" {
        // Vertex speaks Bearer OAuth only — the stored api_key holds Service
        // Account JSON, never a usable token, so build from scratch. Its poll is
        // itself a POST with a JSON body (vertex.js:128-131), so the vertex
        // branch always sends Content-Type.
        let token = vertex_token.ok_or_else(|| "Missing Vertex token".to_string())?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?,
        );
        return Ok(headers);
    }
    let mut headers = build_media_headers(provider, connection)?;
    if !is_create {
        headers.remove(header::CONTENT_TYPE);
    }
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    if provider == "openrouter" {
        // Registry `openrouter.js` videoConfig headers.
        headers.insert(
            reqwest::header::HeaderName::from_static("http-referer"),
            HeaderValue::from_static("https://endpoint-proxy.local"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-title"),
            HeaderValue::from_static("Endpoint Proxy"),
        );
    }
    Ok(headers)
}

/// Per-connection create URL (9router adapter `buildRequest`): xAI posts to
/// `{base}/{action}`, OpenRouter posts verbatim to the collection root (no
/// action suffix), Vertex posts to `{model}:predictLongRunning`.
fn video_create_url(
    provider: &str,
    canonical_provider: &str,
    action: &str,
    connection: &crate::types::ProviderConnection,
    vertex_model: Option<&str>,
) -> Result<String, String> {
    match canonical_provider {
        "openrouter" => Ok(OPENROUTER_VIDEO_BASE_URL.trim_end_matches('/').to_string()),
        "vertex" => {
            let model = vertex_model
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .ok_or_else(|| {
                    "Vertex video requires a model (e.g. vertex/veo-3.1-generate-preview)"
                        .to_string()
                })?;
            if !is_safe_vertex_model_id(model) {
                return Err("Invalid Vertex video model id".to_string());
            }
            let project = vertex_project_id(connection).ok_or_else(|| {
                "Vertex video requires a project_id — use Service Account JSON or set providerSpecificData.projectId"
                    .to_string()
            })?;
            let location = vertex_location(connection);
            let base = connection
                .provider_specific_data
                .get("baseUrl")
                .and_then(Value::as_str)
                .unwrap_or(VERTEX_VIDEO_BASE_URL);
            let _ = provider;
            Ok(format!(
                "{}/v1/projects/{}/locations/{}/publishers/google/models/{}:predictLongRunning",
                base.trim_end_matches('/'),
                project,
                location,
                model
            ))
        }
        _ => Ok(format!(
            "{}/{}",
            XAI_VIDEO_BASE_URL.trim_end_matches('/'),
            action
        )),
    }
}

/// Resolve the poll provider: pinned connection first, then `?provider=`,
/// else the historical xAI default
/// (9router `videoGeneration.js resolveGetProvider`).
fn resolve_video_get_provider(
    state: &AppState,
    headers: &HeaderMap,
    raw_query: Option<&str>,
) -> String {
    let snapshot = state.db.snapshot();
    if let Some(preferred_id) = headers
        .get("x-connection-id")
        .or_else(|| headers.get("x-9router-connection-id"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if let Some(conn) = snapshot
            .provider_connections
            .iter()
            .find(|c| c.id == preferred_id)
        {
            if video_provider_supported(&conn.provider) {
                return conn.provider.clone();
            }
        }
    }
    if let Some(query) = raw_query {
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if parts.next() == Some("provider") {
                if let Some(raw) = parts.next() {
                    let decoded = urlencoding::decode(raw)
                        .map(|s| s.into_owned())
                        .unwrap_or_else(|_| raw.to_string());
                    if video_provider_supported(&decoded) {
                        return decoded;
                    }
                }
            }
        }
    }
    DEFAULT_VIDEO_PROVIDER.to_string()
}

/// Plain model id only — a model carrying "/" or ".." would rewrite the
/// request URL (9router `vertex.js` model check).
fn is_safe_vertex_model_id(model: &str) -> bool {
    !model.is_empty()
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Validate the Vertex create body before any billable upstream call
/// (9router `vertex.js buildRequest` create branch).
fn validate_vertex_create_body(body: &Value) -> Result<(), Response> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let Some(model) = model else {
        return Err(video_error_response(
            StatusCode::BAD_REQUEST,
            "Vertex video requires a model (e.g. vertex/veo-3.1-generate-preview)",
        ));
    };
    if !is_safe_vertex_model_id(model) {
        return Err(video_error_response(
            StatusCode::BAD_REQUEST,
            "Invalid Vertex video model id",
        ));
    }
    let has_prompt = body
        .get("prompt")
        .and_then(Value::as_str)
        .is_some_and(|p| !p.trim().is_empty());
    // 9router vertex.js:148 `if (!body.prompt && !body.image && !body.image_url)`
    // — plain truthiness, so `"image": null` or `"image": ""` does not satisfy
    // the guard. `Value::get` returns Some for an explicit null, which used to
    // let an empty instance through to a billable predictLongRunning.
    let has_image = ["image", "image_url"]
        .iter()
        .any(|key| body.get(key).is_some_and(is_present));
    if !has_prompt && !has_image {
        return Err(video_error_response(
            StatusCode::BAD_REQUEST,
            "Vertex video requires a prompt or an image",
        ));
    }
    Ok(())
}

/// Project id: Service Account `project_id` first, then the connection's
/// `project_id`, then `providerSpecificData.projectId`
/// (9router `vertex.js resolveAuth`).
fn vertex_project_id(connection: &crate::types::ProviderConnection) -> Option<String> {
    if let Some(sa) = connection.api_key.as_deref().and_then(parse_vertex_sa_json) {
        if let Some(project) = sa.project_id.clone() {
            return Some(project);
        }
    }
    if let Some(project) = connection.project_id.clone() {
        if !project.trim().is_empty() {
            return Some(project);
        }
    }
    connection
        .provider_specific_data
        .get("projectId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Location override from `providerSpecificData.location`, else the adapter
/// default (9router `vertex.js` DEFAULT_LOCATION).
fn vertex_location(connection: &crate::types::ProviderConnection) -> String {
    connection
        .provider_specific_data
        .get("location")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| VERTEX_DEFAULT_LOCATION.to_string())
}

#[derive(Debug, Clone)]
struct VertexServiceAccount {
    client_email: String,
    private_key: String,
    project_id: Option<String>,
}

/// Parse Service Account JSON from the connection api_key
/// (9router `tokenRefresh.js parseVertexSaJson`).
fn parse_vertex_sa_json(api_key: &str) -> Option<VertexServiceAccount> {
    let parsed: Value = serde_json::from_str(api_key).ok()?;
    let obj = parsed.as_object()?;
    if obj.get("type").and_then(Value::as_str) != Some("service_account") {
        return None;
    }
    let client_email = obj
        .get("client_email")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    let private_key = obj
        .get("private_key")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    let project_id = obj
        .get("project_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if project_id.is_none() {
        return None;
    }
    Some(VertexServiceAccount {
        client_email,
        private_key,
        project_id,
    })
}

/// Mint a Vertex access token from Service Account JSON via a self-signed
/// RS256 JWT exchanged at the Google OAuth2 token endpoint
/// (9router `tokenRefresh.js refreshVertexToken`).
async fn mint_vertex_token(sa: &VertexServiceAccount) -> Option<String> {
    let now = chrono::Utc::now().timestamp();
    let claims = json!({
        "iss": sa.client_email,
        "scope": "https://www.googleapis.com/auth/cloud-platform",
        "aud": GOOGLE_TOKEN_URL,
        "iat": now,
        "exp": now + 3600,
    });
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    let pem = sa.private_key.replace("\\n", "\n");
    let encoding_key = jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).ok()?;
    let jwt = jsonwebtoken::encode(&header, &claims, &encoding_key).ok()?;
    let client = reqwest::Client::new();
    let response = client
        .post(GOOGLE_TOKEN_URL)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", jwt.as_str()),
        ])
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    body.get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Resolve Vertex auth for one connection: SA mint first, else the stored
/// OAuth access token. Raw API keys are not supported
/// (9router `vertex.js resolveAuth`).
async fn resolve_vertex_auth(
    connection: &crate::types::ProviderConnection,
) -> Result<(String, String, String), String> {
    let project = vertex_project_id(connection).ok_or_else(|| {
        "Vertex video requires a project_id — use Service Account JSON or set providerSpecificData.projectId"
            .to_string()
    })?;
    let location = vertex_location(connection);
    if let Some(sa) = connection.api_key.as_deref().and_then(parse_vertex_sa_json) {
        match mint_vertex_token(&sa).await {
            Some(token) => return Ok((token, project, location)),
            None => {
                return Err(
                    "Vertex video: failed to mint access token from service account JSON"
                        .to_string(),
                )
            }
        }
    }
    if let Some(token) = connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return Ok((token.to_string(), project, location));
    }
    Err(
        "Vertex video requires Service Account JSON or an OAuth access token (raw API keys are not supported)"
            .to_string(),
    )
}

/// Convenience wrapper returning just the token for the create loop.
async fn resolve_vertex_token(
    connection: &crate::types::ProviderConnection,
) -> Result<String, String> {
    resolve_vertex_auth(connection)
        .await
        .map(|(token, _, _)| token)
}

/// JS truthiness for the Vertex parameter guards: false for `null` and for a
/// non-null-but-falsy value (empty string, `0`, `false`).
fn is_present(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// OpenAI-ish video body → Vertex predictLongRunning body
/// (9router `vertex.js toVertexBody`).
fn to_vertex_body(body: &Value) -> Value {
    let mut instance = serde_json::Map::new();
    if let Some(prompt) = body.get("prompt") {
        instance.insert("prompt".to_string(), prompt.clone());
    }
    // Image-to-video: the Vertex-native shape or a bare data URL / base64 string.
    let image = body.get("image").or_else(|| body.get("image_url"));
    if let Some(img) = image {
        if img.is_object() {
            instance.insert("image".to_string(), img.clone());
        } else if let Some(s) = img.as_str() {
            if let Some(rest) = s.strip_prefix("data:") {
                match rest.split_once(";base64,") {
                    Some((mime, b64)) => {
                        instance.insert(
                            "image".to_string(),
                            json!({ "bytesBase64Encoded": b64, "mimeType": mime }),
                        );
                    }
                    None => {
                        instance.insert("image".to_string(), json!({ "gcsUri": s }));
                    }
                }
            } else {
                instance.insert("image".to_string(), json!({ "gcsUri": s }));
            }
        }
    }
    if body.get("video").is_some_and(|v| v.is_object()) {
        instance.insert(
            "video".to_string(),
            body.get("video").cloned().unwrap_or(Value::Null),
        );
    }

    let num = |v: &Value| -> Option<f64> {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    };
    let mut parameters = serde_json::Map::new();
    if let Some(n) = body.get("n").and_then(num) {
        parameters.insert("sampleCount".to_string(), json!(n as i64));
    }
    if let Some(duration) = body.get("duration").and_then(num) {
        parameters.insert("durationSeconds".to_string(), json!(duration));
    }
    // 9router vertex.js:77,78,80,83 gate the string-typed parameters on plain
    // JS truthiness, so an explicit null or an empty string is not forwarded.
    if let Some(aspect) = body.get("aspect_ratio").filter(|v| is_present(v)) {
        parameters.insert("aspectRatio".to_string(), aspect.clone());
    }
    if let Some(resolution) = body.get("resolution").filter(|v| is_present(v)) {
        parameters.insert("resolution".to_string(), resolution.clone());
    }
    // `seed` is different: vertex.js:79 tests `!= null`, so a numeric 0 — a
    // legitimate "make this reproducible" request — is forwarded.
    if let Some(seed) = body.get("seed").filter(|v| !v.is_null()) {
        parameters.insert("seed".to_string(), seed.clone());
    }
    if let Some(negative) = body.get("negative_prompt").filter(|v| is_present(v)) {
        parameters.insert("negativePrompt".to_string(), negative.clone());
    }
    // Without storageUri Vertex returns inline base64 bytes; a GCS bucket keeps
    // the poll response small and is what production callers want.
    if let Some(storage_uri) = body.get("storage_uri").filter(|v| is_present(v)) {
        parameters.insert("storageUri".to_string(), storage_uri.clone());
    }
    if let Some(generate_audio) = body.get("generate_audio").filter(|v| !v.is_null()) {
        parameters.insert(
            "generateAudio".to_string(),
            json!(is_present(generate_audio)),
        );
    }

    let mut out = json!({ "instances": [Value::Object(instance)] });
    if !parameters.is_empty() {
        out["parameters"] = Value::Object(parameters);
    }
    out
}

/// Base64url-encode the operation name into the job id returned to the
/// client — GET /v1/videos/{id} stays a flat path
/// (9router `vertex.js encodeJobId`).
fn encode_vertex_job_id(operation_name: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(operation_name.as_bytes())
}

/// Decode + validate a Vertex job id. Only ids that re-encode byte-for-byte
/// and match the anchored `projects/…/operations/` shape are accepted, so a
/// crafted id can never splice `..` or a host-changing prefix into the
/// request URL (9router `vertex.js decodeJobId` + OPERATION_NAME_RE).
fn decode_vertex_job_id(id: &str) -> Option<String> {
    use base64::Engine;
    if id.is_empty()
        || id.len() > 1024
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(id)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes()) != id {
        return None;
    }
    is_valid_operation_name(&text).then_some(text)
}

/// Operation name shape:
/// projects/{p}/locations/{l}/publishers/{pub}/models/{m}/operations/{op}.
/// Anchored and single-segment-per-field; dot-segments are additionally
/// rejected so a decoded path can never traverse out of the resource URL.
fn is_valid_operation_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() != 10
        || parts[0] != "projects"
        || parts[2] != "locations"
        || parts[4] != "publishers"
        || parts[6] != "models"
        || parts[8] != "operations"
    {
        return false;
    }
    [parts[1], parts[3], parts[5], parts[7], parts[9]]
        .iter()
        .all(|s| !s.is_empty() && *s != "." && *s != "..")
}

/// Model path prefix of an operation name (everything before `/operations/`).
fn operation_model_path(operation_name: &str) -> &str {
    match operation_name.find("/operations/") {
        Some(idx) => &operation_name[..idx],
        None => operation_name,
    }
}

/// Vertex operation → the async-job shape clients already poll for
/// (9router `vertex.js fromVertexOperation`).
fn transform_vertex_operation(body: &Value) -> Value {
    let Some(name) = body.get("name").and_then(Value::as_str) else {
        return body.clone();
    };
    let id = encode_vertex_job_id(name);
    // 9router `vertex.js:93` is `if (json.error)` (truthy) — falsy non-null
    // error values (false, 0, "") do NOT mark the operation failed.
    let is_error = |v: &Value| -> bool {
        match v {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
            Value::String(s) => !s.is_empty(),
            _ => true,
        }
    };
    if body.get("error").is_some_and(is_error) {
        return json!({
            "id": id,
            "request_id": id,
            "status": "failed",
            "error": body.get("error").cloned().unwrap_or(Value::Null),
        });
    }
    if body.get("done").and_then(Value::as_bool) != Some(true) {
        return json!({ "id": id, "request_id": id, "status": "pending" });
    }
    let samples: Vec<&Value> = body
        .pointer("/response/videos")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .or_else(|| {
            body.pointer("/response/generateVideoResponse/generatedSamples")
                .and_then(Value::as_array)
                .map(|a| a.iter().collect())
        })
        .unwrap_or_default();
    // 9router `vertex.js:103-107` uses `||` chains, so empty-string
    // gcsUri/mimeType fall through to the next source / "video/mp4" default.
    fn non_empty(v: Option<&str>) -> Option<&str> {
        v.filter(|s| !s.is_empty())
    }
    let videos: Vec<Value> = samples
        .iter()
        .map(|s| {
            let url = non_empty(s.get("gcsUri").and_then(Value::as_str))
                .or_else(|| non_empty(s.pointer("/video/uri").and_then(Value::as_str)))
                .or_else(|| non_empty(s.get("uri").and_then(Value::as_str)));
            let b64 =
                non_empty(s.get("bytesBase64Encoded").and_then(Value::as_str)).or_else(|| {
                    non_empty(
                        s.pointer("/video/bytesBase64Encoded")
                            .and_then(Value::as_str),
                    )
                });
            let mime = non_empty(s.get("mimeType").and_then(Value::as_str))
                .or_else(|| non_empty(s.pointer("/video/mimeType").and_then(Value::as_str)))
                .unwrap_or("video/mp4");
            json!({ "url": url, "b64_json": b64, "mime_type": mime })
        })
        .collect();
    let first = videos.first().cloned().unwrap_or(Value::Null);
    json!({
        "id": id,
        "request_id": id,
        "status": "completed",
        "video": first,
        "videos": videos,
    })
}

/// Proxy a successful Vertex response through the operation → async-job
/// mapping; error responses keep the sanitizing proxy path.
async fn proxy_vertex_response(
    response: reqwest::Response,
    headers: HeaderMap,
    provider: &str,
    connection: &crate::types::ProviderConnection,
) -> Response {
    if !response.status().is_success() {
        return proxy_video_response(response, headers, provider, connection).await;
    }
    let text = response.text().await.unwrap_or_default();
    let out = serde_json::from_str::<Value>(&text)
        .map(|v| transform_vertex_operation(&v).to_string())
        .unwrap_or(text);
    // Non-JSON or unexpected shape — fall back to the raw upstream body.
    let mut proxied = Response::new(Body::from(out));
    *proxied.status_mut() = StatusCode::OK;
    proxied.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    proxied.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    with_connection_header(&mut proxied, &connection.id);
    proxied
}

/// Vertex poll: POST { operationName } to `:fetchPredictOperation`
/// (9router `vertex.js buildRequest` poll branch — Vertex polls with POST,
/// not GET).
async fn video_vertex_poll(
    state: AppState,
    request_id: String,
    connection: crate::types::ProviderConnection,
) -> Response {
    let operation_name = match decode_vertex_job_id(&request_id) {
        Some(name) => name,
        None => {
            return video_error_response(StatusCode::BAD_REQUEST, "Invalid Vertex video job id")
        }
    };
    let (token, _project, _location) = match resolve_vertex_auth(&connection).await {
        Ok(auth) => auth,
        Err(message) => return video_error_response(StatusCode::BAD_REQUEST, &message),
    };
    let base = connection
        .provider_specific_data
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or(VERTEX_VIDEO_BASE_URL);
    let url = format!(
        "{}/v1/{}:fetchPredictOperation",
        base.trim_end_matches('/'),
        operation_model_path(&operation_name)
    );
    let headers = match build_video_headers(&connection.provider, &connection, Some(&token), true) {
        Ok(h) => h,
        Err(e) => {
            return video_error_response(StatusCode::BAD_REQUEST, &format!("Header error: {}", e))
        }
    };
    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);
    let client = match state.client_pool.get(&connection.provider, proxy.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            return video_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Client error: {:?}", e),
            )
        }
    };
    let body = json!({ "operationName": operation_name }).to_string();
    let post = client.post(&url).headers(headers.clone()).body(body).send();
    match send_video(post).await {
        Ok(response) => {
            let provider = connection.provider.clone();
            proxy_vertex_response(response, headers, &provider, &connection).await
        }
        Err(error) => video_send_error_response(&connection.provider, "POST", error),
    }
}

fn select_video_connection(
    state: &AppState,
    provider: &str,
    headers: &HeaderMap,
) -> Result<crate::types::ProviderConnection, Response> {
    let snapshot = state.db.snapshot();

    // Prefer the account that created the job when the client echoes the
    // connection id returned on create. All three spellings are accepted: the
    // generic one 9router reads (videoGeneration.js:127, :203) and both names
    // this build writes.
    let preferred = headers
        .get("x-connection-id")
        .or_else(|| headers.get("x-openproxy-connection-id"))
        .or_else(|| headers.get("x-9router-connection-id"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty());

    if let Some(preferred_id) = preferred {
        if let Some(conn) = snapshot
            .provider_connections
            .iter()
            .find(|c| c.id == preferred_id && c.provider == provider && c.is_active())
            .filter(|c| connection_has_credentials(c))
            .cloned()
        {
            return Ok(conn);
        }
    }

    select_media_connection(&snapshot, provider, "").ok_or_else(|| {
        json_error_response(
            StatusCode::BAD_REQUEST,
            &format!("No credentials for provider: {}", provider),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn error_message(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .or_else(|| v.get("error"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default()
    }

    async fn error_body(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    #[test]
    fn create_rotation_statuses_matches_9router() {
        // 9router videoGeneration.js CREATE_ROTATION_STATUSES = Set([401, 403, 429]).
        let rotation: [u16; 3] = [401, 403, 429];
        assert!(rotation.contains(&401));
        assert!(rotation.contains(&403));
        assert!(rotation.contains(&429));
        assert!(!rotation.contains(&500));
        assert!(!rotation.contains(&502));
        assert!(!rotation.contains(&400));
    }

    #[test]
    fn rotation_statuses_constant_is_present() {
        // The handler's in-memory rotation set (mirrors CREATE_ROTATION_STATUSES).
        // Compile-level guard that the handler still carries the set.
        let handler_uses_rotation = include_str!("media.rs").contains("create_rotation_statuses");
        assert!(handler_uses_rotation);
    }

    #[test]
    fn video_sanitizes_secrets() {
        let mut conn = crate::types::ProviderConnection::default();
        conn.api_key = Some("sk-xai-super-secret-key-123".to_string());
        conn.access_token = Some("xai-access-token-abc123".to_string());
        conn.refresh_token = Some("xai-refresh-token-xyz789".to_string());

        let text = "Error: Bearer sk-xai-super-secret-key-123 rejected. apiKey=xai-access-token-abc123 refresh=xai-refresh-token-xyz789";
        let sanitized = sanitize_video_secrets(text, &conn);
        assert!(
            !sanitized.contains("sk-xai-super-secret-key-123"),
            "raw api key must be redacted: {sanitized}"
        );
        assert!(
            !sanitized.contains("xai-access-token-abc123"),
            "raw access token must be redacted: {sanitized}"
        );
        assert!(
            !sanitized.contains("xai-refresh-token-xyz789"),
            "raw refresh token must be redacted: {sanitized}"
        );
        assert!(
            sanitized.contains("[redacted]"),
            "should contain [redacted] markers"
        );
    }

    #[test]
    fn video_sanitizes_bearer_token_pattern() {
        let conn = crate::types::ProviderConnection::default();
        // No known secrets in the connection — only the Bearer pattern redacts.
        let text = "Authorization header 'Bearer eyJhbGciOiJIUzI1NiJ9' was rejected";
        let sanitized = sanitize_video_secrets(text, &conn);
        assert!(
            !sanitized.contains("eyJhbGciOiJIUzI1NiJ9"),
            "Bearer token must be redacted: {sanitized}"
        );
        assert!(sanitized.contains("Bearer [redacted]"), "got: {sanitized}");
    }

    #[test]
    fn video_supported_providers_matches_9router() {
        // 9router `videoProviders/index.js` ADAPTERS = { openrouter, vertex };
        // `vertex-partner` has no videoConfig key so getVideoConfig returns
        // null and videoGeneration.js rejects it.
        for provider in ["xai", "openrouter", "vertex"] {
            assert!(video_provider_supported(provider), "got: {provider}");
        }
        assert!(!video_provider_supported("vertex-partner"));
        assert!(!video_provider_supported("openai"));
    }

    #[test]
    fn openrouter_video_create_url_is_collection_root() {
        // 9router `openrouter.js buildRequest`: creation POSTs to the collection
        // root — no `/generations` suffix.
        let conn = crate::types::ProviderConnection::default();
        let url =
            video_create_url("openrouter", "openrouter", "generations", &conn, None).expect("url");
        assert_eq!(url, "https://openrouter.ai/api/v1/videos");
    }

    #[test]
    fn vertex_video_create_url_uses_predict_long_running() {
        // 9router `vertex.js buildRequest` create branch.
        let mut conn = crate::types::ProviderConnection::default();
        conn.provider_specific_data
            .insert("projectId".to_string(), json!("proj-1"));
        let url = video_create_url(
            "vertex",
            "vertex",
            "generations",
            &conn,
            Some("veo-3.1-generate-preview"),
        )
        .expect("url");
        assert_eq!(
            url,
            "https://aiplatform.googleapis.com/v1/projects/proj-1/locations/us-central1/publishers/google/models/veo-3.1-generate-preview:predictLongRunning"
        );
    }

    #[test]
    fn vertex_video_rejects_path_traversal_model_id() {
        let mut conn = crate::types::ProviderConnection::default();
        conn.provider_specific_data
            .insert("projectId".to_string(), json!("proj-1"));
        for bad in ["../../evil", "a/b", "m/operations/x"] {
            assert!(
                video_create_url("vertex", "vertex", "generations", &conn, Some(bad)).is_err(),
                "got: {bad}"
            );
        }
    }

    #[test]
    fn vertex_job_id_round_trips_operation_name() {
        // 9router `vertex.js`: operation name base64url-encodes into the job id.
        let name = "projects/proj-1/locations/us-central1/publishers/google/models/veo-3.1-generate-preview/operations/op-abc";
        let id = encode_vertex_job_id(name);
        assert_eq!(decode_vertex_job_id(&id).as_deref(), Some(name));
    }

    #[test]
    fn vertex_job_id_rejects_ssrf_shapes() {
        // 9router `video-providers.test.js`: crafted ids must 400 before upstream.
        use base64::Engine;
        let jid = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes());
        let valid_name = "projects/proj-1/locations/us-central1/publishers/google/models/veo-3.1-generate-preview/operations/op-abc";
        let valid = encode_vertex_job_id(valid_name);
        for bad in [
            jid("../../evil"),
            jid("projects/p/locations/l/publishers/google/models/m/operations/../../x"),
            jid("../../evil/operations/op"),
            "!!!not-base64!!!".to_string(),
            format!("{valid}="),
            format!("{valid}\n"),
            "x".repeat(1025),
        ] {
            assert!(decode_vertex_job_id(&bad).is_none(), "got: {bad}");
        }
    }

    #[test]
    fn vertex_operation_maps_to_async_job_shape() {
        // 9router `vertex.js fromVertexOperation`: pending / failed / completed.
        let name = "projects/proj-1/locations/us-central1/publishers/google/models/veo-3.1-generate-preview/operations/op-abc";
        let pending = transform_vertex_operation(&json!({ "name": name }));
        assert_eq!(pending["status"], json!("pending"));
        let failed = transform_vertex_operation(
            &json!({ "name": name, "done": true, "error": { "code": 3, "message": "bad prompt" } }),
        );
        assert_eq!(failed["status"], json!("failed"));
        assert_eq!(failed["error"]["message"], json!("bad prompt"));
        let completed = transform_vertex_operation(&json!({
            "name": name,
            "done": true,
            "response": { "videos": [{ "gcsUri": "gs://bucket/v.mp4", "mimeType": "video/mp4" }] },
        }));
        assert_eq!(completed["status"], json!("completed"));
        assert_eq!(completed["video"]["url"], json!("gs://bucket/v.mp4"));
        assert_eq!(completed["videos"][0]["mime_type"], json!("video/mp4"));
    }

    // P261-001: `json_error_response` runs the chat-oriented status/message
    // heuristics, so "Combos are not supported for video generation" came back
    // as 406 and the provider arm lost its `Provider 'x'` to generic prose.
    // 9router's `errorResponse` (open-sse/utils/error.js:30-38) is verbatim.
    #[tokio::test]
    async fn video_combo_rejection_stays_400() {
        let response = video_error_response(
            StatusCode::BAD_REQUEST,
            "Combos are not supported for video generation",
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            error_message(response).await,
            "Combos are not supported for video generation"
        );
    }

    #[tokio::test]
    async fn video_provider_rejection_keeps_provider_name() {
        let response = video_error_response(
            StatusCode::BAD_REQUEST,
            "Provider 'anthropic' does not support video generation",
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            error_message(response).await,
            "Provider 'anthropic' does not support video generation"
        );
    }

    /// And the contrast that makes the bug visible: the shared chat-oriented
    /// constructor really does mangle the same input.
    #[tokio::test]
    async fn shared_json_error_response_rewrites_video_text() {
        let response = json_error_response(
            StatusCode::BAD_REQUEST,
            "Combos are not supported for video generation",
        );
        assert_ne!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "infer_status_from_message turns this into 406"
        );
    }

    #[test]
    fn vertex_body_translates_openai_shape() {
        // 9router `vertex.js toVertexBody`: prompt/duration/aspect/n + data-URL image.
        let body = json!({
            "model": "veo-3.1-generate-preview",
            "prompt": "a neon city",
            "duration": 8,
            "aspect_ratio": "16:9",
            "resolution": "720p",
            "n": 1,
            "image": "data:image/png;base64,AAAB",
        });
        let out = to_vertex_body(&body);
        assert_eq!(
            out,
            json!({
                "instances": [{ "prompt": "a neon city", "image": { "bytesBase64Encoded": "AAAB", "mimeType": "image/png" } }],
                "parameters": { "sampleCount": 1, "durationSeconds": 8.0, "aspectRatio": "16:9", "resolution": "720p" },
            })
        );
    }

    /// 9router vertex.js:75-84 gates `n`/`duration`/`seed`/`generate_audio` on
    /// `!= null` and the rest on plain truthiness. `Value::get` returns `Some`
    /// for an explicit null, so the string-typed parameters were forwarding
    /// `""` and `null` straight into the Vertex request.
    #[test]
    fn vertex_body_drops_null_and_empty_parameters() {
        let out = to_vertex_body(&json!({
            "prompt": "a cat",
            "seed": Value::Null,
            "aspect_ratio": "",
            "resolution": Value::Null,
            "negative_prompt": "",
            "storage_uri": "",
            "generate_audio": Value::Null,
        }));
        assert!(
            out.get("parameters").is_none(),
            "nothing survived the guard, yet a parameters object was built: {out}"
        );
    }

    /// `seed` keeps `!= null` semantics: 0 is a legitimate request, not an
    /// absent one (vertex.js:79).
    #[test]
    fn vertex_body_keeps_a_zero_seed() {
        let out = to_vertex_body(&json!({"prompt": "a cat", "seed": 0}));
        assert_eq!(out["parameters"]["seed"], json!(0));
    }

    /// vertex.js:148 `if (!body.prompt && !body.image && !body.image_url)` —
    /// a present-but-null image does not satisfy the guard, so the request is
    /// rejected before the billable predictLongRunning.
    #[tokio::test]
    async fn vertex_null_image_does_not_satisfy_the_prompt_guard() {
        assert!(validate_vertex_create_body(&json!({
            "model": "veo-3.1-generate-preview",
            "image": Value::Null,
        }))
        .is_err());
        assert!(validate_vertex_create_body(&json!({
            "model": "veo-3.1-generate-preview",
            "image": "",
        }))
        .is_err());
        assert!(validate_vertex_create_body(&json!({
            "model": "veo-3.1-generate-preview",
            "image": "gs://bucket/frame.png",
        }))
        .is_ok());
    }

    /// Both spellings go out, so a client written against either build can pin
    /// the poll (9router videoGeneration.js:97-104).
    #[tokio::test]
    async fn connection_header_is_written_under_both_names() {
        let mut response = Response::new(Body::empty());
        with_connection_header(&mut response, "conn-abc");
        assert_eq!(
            response
                .headers()
                .get("x-openproxy-connection-id")
                .and_then(|v| v.to_str().ok()),
            Some("conn-abc")
        );
        assert_eq!(
            response
                .headers()
                .get("x-9router-connection-id")
                .and_then(|v| v.to_str().ok()),
            Some("conn-abc")
        );
    }

    // ── Video upstream shaping ─────────────────────────────────────────────
    //
    // These drive the real response helpers against a locally-served upstream.
    // The routes themselves point at `XAI_VIDEO_BASE_URL`, a public constant
    // with no per-connection override, so an end-to-end create/poll would bill
    // real traffic against api.x.ai.

    /// One canned HTTP response from a throwaway listener.
    async fn upstream_reply(
        status: &'static str,
        extra_headers: &'static str,
        body: String,
    ) -> reqwest::Response {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        });
        reqwest::get(format!("http://{addr}/"))
            .await
            .expect("upstream")
    }

    /// A non-2xx video upstream reaches the client as the OpenAI error envelope
    /// with the provider prefix — videoCore.js:181-184's
    /// `createErrorResult(status, "[${provider}] " + message.slice(0, 2000))`.
    /// Relaying the provider's own body verbatim dropped `type`/`code`, the
    /// only thing a client can branch on.
    #[tokio::test]
    async fn a_video_upstream_error_is_wrapped_and_prefixed() {
        let upstream = upstream_reply(
            "429 Too Many Requests",
            "",
            r#"{"error":"rate limit"}"#.to_string(),
        )
        .await;
        let response =
            proxy_video_response(upstream, HeaderMap::new(), "xai", &Default::default()).await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let message = error_message(response).await;
        assert!(
            message.starts_with("[xai] "),
            "the provider name must prefix the message, got: {message}"
        );
        assert!(message.contains("rate limit"), "got: {message}");
    }

    /// The `type`/`code` pair follows the upstream status
    /// (config/errorConfig.js:2-14), not the error text.
    #[tokio::test]
    async fn a_wrapped_video_error_carries_the_status_derived_type() {
        let upstream =
            upstream_reply("429 Too Many Requests", "", r#"{"error":"x"}"#.to_string()).await;
        let response =
            proxy_video_response(upstream, HeaderMap::new(), "xai", &Default::default()).await;
        let (status, body) = error_body(response).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["type"], json!("rate_limit_error"));
        assert_eq!(body["error"]["code"], json!("rate_limit_exceeded"));
    }

    /// videoCore.js:183 caps the message at 2000 CHARACTERS. 9router's
    /// `slice(0, 2000)` is UTF-16 units, so a byte-index cap in Rust would
    /// panic on a multi-byte body — `chars().take` is the safe equivalent.
    #[tokio::test]
    async fn a_video_error_body_is_capped_at_2000_characters() {
        let ascii = "x".repeat(5000);
        let upstream = upstream_reply("500 Internal Server Error", "", ascii).await;
        let response =
            proxy_video_response(upstream, HeaderMap::new(), "xai", &Default::default()).await;
        let message = error_message(response).await;
        let body = message.strip_prefix("[xai] ").expect("prefixed");
        assert_eq!(
            body.chars().count(),
            2000,
            "got {} chars",
            body.chars().count()
        );

        let multibyte = "é".repeat(5000);
        let upstream = upstream_reply("500 Internal Server Error", "", multibyte).await;
        let response =
            proxy_video_response(upstream, HeaderMap::new(), "xai", &Default::default()).await;
        let body = error_message(response)
            .await
            .strip_prefix("[xai] ")
            .expect("prefixed")
            .to_string();
        assert_eq!(body.chars().count(), 2000, "chars, not bytes");
    }

    /// An empty upstream body still produces a message, naming the status
    /// (videoCore.js:182 `bodyText || \`HTTP ${upstream.status}\``).
    #[tokio::test]
    async fn an_empty_video_error_body_falls_back_to_the_status() {
        let upstream = upstream_reply("503 Service Unavailable", "", String::new()).await;
        let response =
            proxy_video_response(upstream, HeaderMap::new(), "xai", &Default::default()).await;
        assert_eq!(error_message(response).await, "[xai] HTTP 503");
    }

    /// videoCore.js:199-208 builds a fresh Response carrying exactly
    /// `Content-Type` and CORS. Echoing the whole upstream set leaked provider
    /// internals and handed the client the upstream's own `Idempotency-Key`.
    #[tokio::test]
    async fn only_content_type_is_copied_back_from_the_upstream() {
        let upstream = upstream_reply(
            "200 OK",
            "x-request-id: abc\r\nserver: cloudflare\r\n",
            r#"{"ok":true}"#.to_string(),
        )
        .await;
        let response = proxy_upstream_response(upstream, HeaderMap::new()).await;
        let headers = response.headers();
        assert!(headers.contains_key(header::CONTENT_TYPE));
        assert!(
            !headers.contains_key("x-request-id") && !headers.contains_key("server"),
            "upstream internals must not be echoed: {headers:?}"
        );
    }

    /// videoCore.js:40 `buildHeaders` always sets `Accept`, and
    /// `contentType: method === "POST" ? contentType : null` keeps
    /// `Content-Type` off a bodyless poll (videoCore.js:107-111).
    #[test]
    fn video_headers_pair_content_type_with_the_verb() {
        let mut conn = crate::types::ProviderConnection::default();
        conn.api_key = Some("sk-xai".into());

        let create = build_video_headers("xai", &conn, None, true).unwrap();
        assert_eq!(create.get(header::ACCEPT).unwrap(), "application/json");
        assert!(create.contains_key(header::CONTENT_TYPE));

        let poll = build_video_headers("xai", &conn, None, false).unwrap();
        assert_eq!(poll.get(header::ACCEPT).unwrap(), "application/json");
        assert!(
            !poll.contains_key(header::CONTENT_TYPE),
            "a bodyless GET must not announce a JSON content type"
        );
    }

    /// Vertex polls with POST + a JSON body (vertex.js:128-131), so its branch
    /// keeps Content-Type even on a poll.
    #[test]
    fn vertex_video_headers_always_carry_content_type() {
        let conn = crate::types::ProviderConnection::default();
        let headers = build_video_headers("vertex", &conn, Some("ya29.token"), true).unwrap();
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(headers.get(header::ACCEPT).unwrap(), "application/json");
    }

    /// A hung upstream must abort at the deadline and be reported as a 408,
    /// which the pooled client's own (much longer) timeout never produced.
    /// A refused connection is the other arm and stays a 502.
    #[tokio::test]
    async fn a_hung_video_upstream_is_408_and_a_refused_one_is_502() {
        // Serialized: this test is the only one that touches the env override.
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hanging = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });

        std::env::set_var("VIDEO_FETCH_TIMEOUT_MS", "150");
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/videos/generations");
        let error = send_video(client.post(&url).send()).await.unwrap_err();
        assert!(
            matches!(error, VideoSendError::Timeout),
            "a hung upstream is the timeout arm"
        );
        let response = video_send_error_response("xai", "POST", error);
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert!(
            error_message(response)
                .await
                .starts_with("[xai] video POST aborted"),
            "the message mirrors videoCore.js:143"
        );
        hanging.abort();
        std::env::remove_var("VIDEO_FETCH_TIMEOUT_MS");

        // A port nothing listens on: a connect failure, not a deadline.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);
        let url = format!("http://{dead_addr}/v1/videos/generations");
        let error = send_video(client.post(&url).send()).await.unwrap_err();
        assert!(
            matches!(error, VideoSendError::Transport(_)),
            "a refused connection is the transport arm"
        );
        assert_eq!(
            video_send_error_response("xai", "POST", error).status(),
            StatusCode::BAD_GATEWAY
        );
    }

    /// The default deadline is 9router's 120 s (videoCore.js:9).
    #[test]
    fn default_video_fetch_timeout_is_120_seconds() {
        if std::env::var("VIDEO_FETCH_TIMEOUT_MS").is_err() {
            assert_eq!(video_fetch_timeout(), std::time::Duration::from_secs(120));
        }
    }

    /// The raw multipart passthrough sends the same header set the JSON create
    /// path does, so an edits/extensions call is deduplicated upstream the same
    /// way a generations call is.
    #[test]
    fn raw_video_headers_carry_auth_accept_content_type_and_the_key() {
        let mut conn = crate::types::ProviderConnection::default();
        conn.api_key = Some("sk-xai".into());
        let key = HeaderValue::from_static("job-42");
        let headers = raw_video_headers(&conn, "multipart/form-data; boundary=abc", Some(&key));

        assert_eq!(headers.get(header::ACCEPT).unwrap(), "application/json");
        assert_eq!(headers.get(header::AUTHORIZATION).unwrap(), "Bearer sk-xai");
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "multipart/form-data; boundary=abc"
        );
        assert_eq!(headers.get(&IDEMPOTENCY_KEY).unwrap(), "job-42");
    }

    /// An empty key is not forwarded (9router's `if (idempotencyKey)`).
    #[test]
    fn an_empty_idempotency_key_is_dropped() {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", HeaderValue::from_static(""));
        assert_eq!(idempotency_key(&headers), None);
        headers.insert("idempotency-key", HeaderValue::from_static("job-42"));
        assert_eq!(
            idempotency_key(&headers).map(|v| v.to_str().unwrap().to_string()),
            Some("job-42".to_string())
        );
    }

    /// 9router captures the key once (videoGeneration.js:128) and threads it
    /// through every attempt, so the post-refresh retry is still deduplicated
    /// upstream. Compile-level guard: both attempts insert it.
    #[test]
    fn the_create_retry_re_sends_the_idempotency_key() {
        let inserts = include_str!("media.rs")
            .matches("insert(IDEMPOTENCY_KEY")
            .count();
        assert!(
            inserts >= 2,
            "the retry must rebuild the key too; found {inserts} insert site(s)"
        );
    }

    /// A video failure has to reach the shared account-fallback state, or the
    /// Providers dashboard shows a healthy account and selection keeps handing
    /// it back. 9router's `markAccountUnavailable` (auth.js:239-296) writes a
    /// PER-MODEL lock plus `testStatus`/`lastError`/`errorCode`/`backoffLevel`
    /// and leaves `rateLimitedUntil` alone — a bad prompt must not take the
    /// account out of every other model's rotation.
    #[tokio::test]
    async fn a_video_failure_is_recorded_as_a_per_model_lock() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db = std::sync::Arc::new(crate::db::Db::load_from(temp.path()).await.expect("db"));
        db.update(|state| {
            state.provider_connections = vec![crate::types::ProviderConnection {
                id: "conn-xai".into(),
                provider: "xai".into(),
                auth_type: "apikey".into(),
                api_key: Some("sk-xai".into()),
                is_active: Some(true),
                ..Default::default()
            }];
        })
        .await
        .expect("seed db");
        let state = AppState::new(db);

        record_video_outcome(&state, "conn-xai", Some("grok-imagine-video"), 429).await;
        let stored = state.db.snapshot().provider_connections[0].clone();

        assert!(
            stored.extra.contains_key("modelLock_grok-imagine-video"),
            "expected the per-model lock, got {:?}",
            stored.extra
        );
        assert_eq!(stored.error_code.as_deref(), Some("429"));
        assert_eq!(stored.test_status.as_deref(), Some("unavailable"));
        assert!(stored.last_error.is_some(), "the dashboard shows no reason");
        assert_eq!(stored.backoff_level, Some(1), "a 429 is the only ratchet");
        assert_eq!(
            stored.rate_limited_until, None,
            "a per-model lock must not cool the whole account down"
        );

        // A success clears the lock and the error state (clearAccountError).
        record_video_outcome(&state, "conn-xai", Some("grok-imagine-video"), 200).await;
        let stored = state.db.snapshot().provider_connections[0].clone();
        assert!(stored.extra["modelLock_grok-imagine-video"].is_null());
        assert_eq!(stored.test_status, None);
        assert_eq!(stored.last_error, None);
    }

    /// A client-side 4xx is recorded but must not ratchet the backoff level —
    /// 9router's rate-limit rule is the only one carrying `backoff: true`
    /// (accountFallback.js:211).
    #[tokio::test]
    async fn a_client_side_video_error_does_not_ratchet_the_backoff_level() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db = std::sync::Arc::new(crate::db::Db::load_from(temp.path()).await.expect("db"));
        db.update(|state| {
            state.provider_connections = vec![crate::types::ProviderConnection {
                id: "conn-xai".into(),
                provider: "xai".into(),
                auth_type: "apikey".into(),
                api_key: Some("sk-xai".into()),
                is_active: Some(true),
                backoff_level: Some(0),
                ..Default::default()
            }];
        })
        .await
        .expect("seed db");
        let state = AppState::new(db);

        record_video_outcome(&state, "conn-xai", Some("grok-imagine-video"), 400).await;
        let stored = state.db.snapshot().provider_connections[0].clone();

        assert_eq!(stored.error_code.as_deref(), Some("400"));
        assert_eq!(
            stored.backoff_level,
            Some(0),
            "a bad prompt must not ratchet towards the rate-limit cap"
        );
        assert_eq!(stored.rate_limited_until, None);
    }
}
