use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
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

pub async fn audio_transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    generic_media_handler(state, headers, body, "audio/transcriptions").await
}

pub async fn audio_speech(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    generic_media_handler(state, headers, body, "audio/speech").await
}

pub async fn embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    generic_media_handler(state, headers, body, "embeddings").await
}

pub async fn images_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    generic_media_handler(state, headers, body, "images/generations").await
}

pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    generic_media_handler(state, headers, body, "search").await
}

async fn generic_media_handler(
    state: AppState,
    headers: HeaderMap,
    body_result: Result<Json<Value>, JsonRejection>,
    route_kind: &'static str,
) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
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

fn json_error_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message
            }
        })),
    )
        .into_response()
}
