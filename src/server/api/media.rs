use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};

use crate::core::model::{get_model_info, ModelRouteKind};
use crate::core::proxy::resolve_proxy_target;
use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::AppDb;

use super::auth_error_response;

/// Default provider for video routes when the request model has no `provider/` prefix.
/// Video generation is xAI-only today (Grok Imagine).
const DEFAULT_VIDEO_PROVIDER: &str = "xai";

/// Upstream base for async xAI video jobs (POST action / GET by request id).
/// Docs: https://docs.x.ai/developers/rest-api-reference/inference/videos
const XAI_VIDEO_BASE_URL: &str = "https://api.x.ai/v1/videos";

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
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(generic_media_handler(state, headers, body, "audio/speech").await)
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
        if let Err(e) = require_api_key(&headers, &state.db) {
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
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(video_create_handler(state, headers, body, "edits").await)
}

/// POST /v1/videos/extensions — async video extension job create (xAI Grok Imagine).
pub async fn video_extensions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(video_create_handler(state, headers, body, "extensions").await)
}

/// GET /v1/videos/{id} — poll async video job status (xAI Grok Imagine).
pub async fn video_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Response {
    with_cors_get_response(video_get_handler(state, headers, request_id).await)
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
    if state.db.snapshot().settings.require_login {
        if let Err(error) = require_api_key(&headers, &state.db) {
            return auth_error_response(error);
        }
    }

    let Json(body) = match body_result {
        Ok(body) => body,
        Err(_) => return json_error_response(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

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
    let connection = match select_media_connection(&snapshot, provider, model) {
        Some(conn) => conn,
        None => {
            return json_error_response(
                StatusCode::BAD_REQUEST,
                &format!("No credentials for provider: {}", provider),
            )
        }
    };

    let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);

    // Try the provider-specific media adapter first (image / tts /
    // embeddings / search). Falls through to the generic upstream
    // forwarder below when no adapter handles this provider+route.
    if let Some(resp) = try_provider_adapter(
        state,
        &connection,
        provider,
        model,
        route_kind,
        request_body,
    )
    .await
    {
        return resp;
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

    proxy_upstream_response(response, headers).await
}

fn select_media_connection(
    snapshot: &AppDb,
    provider: &str,
    _model: &str,
) -> Option<crate::types::ProviderConnection> {
    snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection.provider == provider
                && connection.is_active()
                && connection_has_credentials(connection)
        })
        .min_by_key(|connection| connection.priority.unwrap_or(999))
        .cloned()
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

    crate::core::executor::get_provider_config(provider)
        .map(|config| config.base_url)
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

    for (name, value) in &resp_headers {
        if !is_hop_by_hop_header(name.as_str()) {
            proxied.headers_mut().insert(name.clone(), value.clone());
        }
    }

    proxied
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "content-length"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
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
) -> Option<Response> {
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

    Some(media_result_to_response(result?))
}

fn media_result_to_response(result: Result<Value, crate::core::media::MediaError>) -> Response {
    match result {
        Ok(body) => with_cors_response((StatusCode::OK, Json(body)).into_response()),
        Err(err) => {
            let status = StatusCode::from_u16(err.status()).unwrap_or(StatusCode::BAD_GATEWAY);
            json_error_response(status, &err.message())
        }
    }
}

fn json_error_response(status: StatusCode, message: &str) -> Response {
    let status_code =
        crate::core::utils::error::infer_status_from_message(status.as_u16(), message);
    let status = StatusCode::from_u16(status_code).unwrap_or(status);
    let friendly = crate::core::utils::error::friendly_error_message(status.as_u16(), message);
    let body = crate::core::utils::error::build_error_body(status.as_u16(), Some(&friendly));
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
    if state.db.snapshot().settings.require_login {
        if let Err(error) = require_api_key(&headers, &state.db) {
            return auth_error_response(error);
        }
    }

    let Json(mut body) = match body_result {
        Ok(body) => body,
        Err(_) => return json_error_response(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    let (provider, model) = match resolve_video_provider_model(&state, &body) {
        Ok(resolved) => resolved,
        Err(resp) => return resp,
    };

    // Strip provider prefix (e.g. "xai/grok-imagine-video" → "grok-imagine-video")
    // before forwarding so upstream receives the bare model id.
    if let Some(obj) = body.as_object_mut() {
        if let Some(model_str) = model.as_deref() {
            obj.insert("model".to_string(), json!(model_str));
        }
    }

    let connection = match select_video_connection(&state, &provider, &headers) {
        Ok(conn) => conn,
        Err(resp) => return resp,
    };

    let url = format!("{}/{}", XAI_VIDEO_BASE_URL.trim_end_matches('/'), action);

    let mut upstream_headers = match build_media_headers(&provider, &connection) {
        Ok(h) => h,
        Err(e) => {
            return json_error_response(StatusCode::BAD_REQUEST, &format!("Header error: {}", e))
        }
    };

    // Forward Idempotency-Key when present (creation is billable).
    if let Some(idem) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    {
        if let Ok(val) = HeaderValue::from_str(idem) {
            upstream_headers.insert(
                reqwest::header::HeaderName::from_static("idempotency-key"),
                val,
            );
        }
    }

    let body_bytes = match serde_json::to_vec(&body) {
        Ok(b) => b,
        Err(e) => {
            return json_error_response(
                StatusCode::BAD_REQUEST,
                &format!("Serialization error: {}", e),
            )
        }
    };

    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);
    let client = match state.client_pool.get(&provider, proxy.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Client error: {:?}", e),
            )
        }
    };

    // Never auto-retry creation POSTs — a network error after the request left
    // the socket may still have created the billable job upstream.
    let response = match client
        .post(&url)
        .headers(upstream_headers.clone())
        .body(body_bytes)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return json_error_response(StatusCode::BAD_GATEWAY, &format!("Request failed: {}", e))
        }
    };

    let mut proxied = proxy_upstream_response(response, upstream_headers).await;
    // Video jobs are account-bound — clients echo this back as `x-connection-id`
    // on GET polls so the same account is used.
    if let Ok(val) = HeaderValue::from_str(&connection.id) {
        proxied
            .headers_mut()
            .insert("x-openproxy-connection-id", val);
    }
    proxied
}

/// Poll async video job status. Jobs are account-bound upstream, so no
/// cross-account rotation: the caller pins the creating account via
/// `x-connection-id` (returned on create as `x-openproxy-connection-id`).
async fn video_get_handler(state: AppState, headers: HeaderMap, request_id: String) -> Response {
    if state.db.snapshot().settings.require_login {
        if let Err(error) = require_api_key(&headers, &state.db) {
            return auth_error_response(error);
        }
    }

    if request_id.trim().is_empty() {
        return json_error_response(StatusCode::BAD_REQUEST, "Missing video request id");
    }

    let provider = DEFAULT_VIDEO_PROVIDER.to_string();
    let connection = match select_video_connection(&state, &provider, &headers) {
        Ok(conn) => conn,
        Err(resp) => return resp,
    };

    let url = format!(
        "{}/{}",
        XAI_VIDEO_BASE_URL.trim_end_matches('/'),
        urlencoding::encode(&request_id)
    );

    let upstream_headers = match build_media_headers(&provider, &connection) {
        Ok(h) => h,
        Err(e) => {
            return json_error_response(StatusCode::BAD_REQUEST, &format!("Header error: {}", e))
        }
    };

    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);
    let client = match state.client_pool.get(&provider, proxy.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Client error: {:?}", e),
            )
        }
    };

    let response = match client
        .get(&url)
        .headers(upstream_headers.clone())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return json_error_response(StatusCode::BAD_GATEWAY, &format!("Request failed: {}", e))
        }
    };

    let mut proxied = proxy_upstream_response(response, upstream_headers).await;
    if let Ok(val) = HeaderValue::from_str(&connection.id) {
        proxied
            .headers_mut()
            .insert("x-openproxy-connection-id", val);
    }
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
        ModelRouteKind::Combo => Err(json_error_response(
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
                    return Err(json_error_response(
                        StatusCode::BAD_REQUEST,
                        &format!("Provider '{}' does not support video generation", p),
                    ));
                }
                None if !model_str.contains('/') => DEFAULT_VIDEO_PROVIDER.to_string(),
                None => {
                    return Err(json_error_response(
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
    // Today only xAI exposes videoConfig. Keep this narrow so unsupported
    // providers fail closed rather than forwarding to a nonexistent endpoint.
    provider == "xai"
}

fn select_video_connection(
    state: &AppState,
    provider: &str,
    headers: &HeaderMap,
) -> Result<crate::types::ProviderConnection, Response> {
    let snapshot = state.db.snapshot();

    // Prefer the account that created the job when the client echoes the
    // connection id returned on create.
    let preferred = headers
        .get("x-connection-id")
        .or_else(|| headers.get("x-openproxy-connection-id"))
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
