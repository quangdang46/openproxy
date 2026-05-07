use std::collections::{BTreeMap, HashSet};

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::TryStreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};

use crate::core::chat::RequestPlan;
use crate::core::combo::{
    check_fallback_error, execute_combo_strategy, get_combo_models_from_data, ComboAttemptError,
    ComboExecutionError, ComboStrategy,
};
use crate::core::executor::UpstreamResponse;
use crate::core::model::{get_model_info, ModelRouteKind};
use crate::core::proxy::resolve_proxy_target;
use crate::core::rtk::apply_request_preprocessing;
use crate::core::translator::registry::{self, Format};
use crate::core::translator::response_transform::{transform_sse_stream, transformer_for_provider};
use crate::server::auth::{extract_api_key, require_api_key};
use crate::server::state::AppState;
use crate::types::{AppDb, ProviderConnection, TokenUsage};

use super::auth_error_response;

pub async fn cors_options() -> Response {
    cors_preflight_response("GET, POST, OPTIONS")
}

pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    with_cors_response(
        chat_completions_for_endpoint(state, headers, body, Some("/v1/chat/completions")).await,
    )
}

pub async fn dashboard_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let body = normalize_dashboard_chat_request_body(&state, body);

    chat_completions_impl(
        state,
        headers,
        body,
        Some("/api/dashboard/chat/completions"),
        false,
    )
    .await
}

fn normalize_dashboard_chat_request_body(
    state: &AppState,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, JsonRejection> {
    let Ok(Json(mut value)) = body else {
        return body;
    };

    let dashboard_stream = value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if dashboard_stream {
        if let Some(fields) = value.as_object_mut() {
            fields.insert("stream".into(), Value::Bool(false));
            fields.insert("__dashboard_stream".into(), Value::Bool(true));
        }
    }

    let Some(model) = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return Ok(Json(value));
    };

    if model.contains('/') {
        return Ok(Json(value));
    }

    let snapshot = state.db.snapshot();
    if snapshot.combos.iter().any(|combo| combo.name == model) {
        return Ok(Json(value));
    }
    if snapshot.model_aliases.contains_key(model) {
        return Ok(Json(value));
    }

    let mut matches = snapshot
        .provider_connections
        .iter()
        .filter(|connection| connection.is_active.unwrap_or(true))
        .filter(|connection| provider_connection_supports_model(connection, model))
        .map(|connection| format!("{}/{}", connection.provider, model));

    let Some(rewritten_model) = matches.next() else {
        return Ok(Json(value));
    };
    if matches.next().is_some() {
        return Ok(Json(value));
    }

    if let Some(fields) = value.as_object_mut() {
        fields.insert("model".into(), Value::String(rewritten_model));
    }

    Ok(Json(value))
}

fn provider_connection_supports_model(connection: &ProviderConnection, model: &str) -> bool {
    if connection.default_model.as_deref() == Some(model) {
        return true;
    }

    connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(Value::as_array)
        .is_some_and(|models| {
            models
                .iter()
                .filter_map(Value::as_str)
                .any(|item| item == model)
        })
}

pub async fn chat_completions_for_endpoint(
    state: AppState,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
    endpoint: Option<&'static str>,
) -> Response {
    chat_completions_impl(state, headers, body, endpoint, true).await
}

async fn chat_completions_impl(
    state: AppState,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
    endpoint: Option<&'static str>,
    require_api_key_auth: bool,
) -> Response {
    let presented_api_key = extract_api_key(&headers);
    if require_api_key_auth {
        if let Err(error) = require_api_key(&headers, &state.db) {
            return auth_error_response(error);
        }
    }

    let Json(body) = match body {
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
        ModelRouteKind::Combo => {
            let combo_name = resolved.model;
            let Some(combo_models) = get_combo_models_from_data(&combo_name, &snapshot.combos)
            else {
                return json_error_response(StatusCode::BAD_REQUEST, "Unknown combo model");
            };

            let strategy = combo_strategy_for(&snapshot, &combo_name);
            let combo_body = body.clone();
            let combo_state = state.clone();
            let combo_api_key = presented_api_key.clone();
            match execute_combo_strategy(
                &combo_models,
                Some(&combo_name),
                strategy,
                move |combo_model| {
                    let state = combo_state.clone();
                    let body = combo_body.clone();
                    let combo_model = combo_model.to_string();
                    let api_key = combo_api_key.clone();
                    // Build plan for this combo model
                    let combo_provider_str = resolved.provider.as_deref().unwrap_or("unknown");
                    let combo_plan =
                        RequestPlan::new(endpoint, &body, combo_provider_str, &combo_model);
                    let plan_for_combo = combo_plan.clone();
                    async move {
                        execute_single_model(
                            &state,
                            &body,
                            &combo_model,
                            api_key.as_deref(),
                            endpoint,
                            &plan_for_combo,
                        )
                        .await
                    }
                },
            )
            .await
            {
                Ok(response) => response,
                Err(error) => combo_error_response(error),
            }
        }
        ModelRouteKind::Direct => {
            let plan = RequestPlan::new(
                endpoint,
                &body,
                resolved.provider.as_deref().unwrap_or(model_str),
                &resolved.model,
            );
            match execute_single_model(
                &state,
                &body,
                model_str,
                presented_api_key.as_deref(),
                endpoint,
                &plan,
            )
            .await
            {
                Ok(response) => response,
                Err(error) => attempt_error_response(error),
            }
        }
    }
}

async fn execute_single_model(
    state: &AppState,
    request_body: &Value,
    model_str: &str,
    api_key: Option<&str>,
    endpoint: Option<&'static str>,
    plan: &RequestPlan,
) -> Result<Response, ComboAttemptError> {
    let snapshot = state.db.snapshot();

    let mut body = request_body.clone();
    if let Some(fields) = body.as_object_mut() {
        fields.insert("model".into(), Value::String(plan.model.clone()));
    } else {
        return Err(ComboAttemptError {
            status: 400,
            message: "Request body must be a JSON object".into(),
            retry_after: None,
        });
    }

    let _ = apply_request_preprocessing(&mut body, &snapshot.settings, &plan.model);

    tracing::debug!(
        "PLAN provider={} model={} source={:?} target={:?} stream={} translate={}",
        plan.provider,
        plan.model,
        plan.source_format,
        plan.target_format,
        plan.stream,
        plan.needs_translation(),
    );

    forward_with_provider_fallback(
        state,
        &plan.provider,
        &plan.model,
        body,
        api_key,
        endpoint,
        plan,
    )
    .await
}

async fn forward_with_provider_fallback(
    state: &AppState,
    provider: &str,
    model: &str,
    mut request_body: Value,
    api_key: Option<&str>,
    endpoint: Option<&'static str>,
    plan: &RequestPlan,
) -> Result<Response, ComboAttemptError> {
    let mut excluded = HashSet::new();
    let mut last_error: Option<ComboAttemptError> = None;
    let registry = &state.account_registry;

    loop {
        let snapshot = state.db.snapshot();
        let Some(connection) = select_connection(&snapshot, provider, model, &excluded) else {
            let retry_after = earliest_retry_after(&snapshot, provider, model, &excluded);
            if let Some(mut error) = last_error {
                if retry_after.is_some() {
                    error.retry_after = retry_after;
                }
                return Err(error);
            }

            return Err(ComboAttemptError {
                status: if retry_after.is_some() { 503 } else { 400 },
                message: if retry_after.is_some() {
                    format!("All accounts for {provider}/{model} are cooling down")
                } else {
                    format!("No credentials for provider: {provider}")
                },
                retry_after,
            });
        };

        let provider_node = snapshot
            .provider_nodes
            .iter()
            .find(|node| node.id == provider)
            .cloned();
        let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);

        let (rate_limit_remaining, rate_limit_reset) = registry.rate_limit_info(&connection.id);
        let slot =
            registry.acquire_slot(&connection.id, 10, rate_limit_remaining, rate_limit_reset);

        let Some(_slot) = slot else {
            excluded.insert(connection.id.clone());
            continue;
        };

        let dashboard_stream = request_body
            .get("__dashboard_stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(fields) = request_body.as_object_mut() {
            fields.remove("__dashboard_stream");
        }

        let stream = request_body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        state
            .usage_live
            .start_request(model, provider, Some(connection.id.as_str()))
            .await;

        use crate::core::executor::{
            CodexExecutionRequest, CodexExecutor, CursorExecutionRequest, CursorExecutor,
            DefaultExecutor, ExecutionRequest, KiroExecutionRequest, KiroExecutor,
            KiroExecutorResponse, VertexExecutionRequest, VertexExecutor,
        };

        let is_codex_model = model.starts_with("codex/");
        let is_cursor_model = model.starts_with("cursor/");
        let executor_result: Result<KiroExecutorResponse, ComboAttemptError> =
            if provider == "kiro" {
                let executor = KiroExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Kiro executor creation failed: {:?}", e),
                        retry_after: None,
                    })?;
                executor
                    .execute_request(KiroExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Kiro execution failed: {:?}", e),
                        retry_after: None,
                    })
            } else if provider == "vertex" {
                let executor = VertexExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Vertex executor creation failed: {:?}", e),
                        retry_after: None,
                    })?;
                let result = executor
                    .execute_request(VertexExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Vertex execution failed: {:?}", e),
                        retry_after: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if is_codex_model {
                let executor = CodexExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Codex executor creation failed: {:?}", e),
                        retry_after: None,
                    })?;
                let result = executor
                    .execute(CodexExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Codex execution failed: {:?}", e),
                        retry_after: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if is_cursor_model {
                let executor = CursorExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Cursor executor creation failed: {:?}", e),
                        retry_after: None,
                    })?;
                let result = executor
                    .execute(CursorExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Cursor execution failed: {:?}", e),
                        retry_after: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else {
                let executor = DefaultExecutor::new(
                    provider.to_string(),
                    state.client_pool.clone(),
                    provider_node,
                )
                .map_err(|e| ComboAttemptError {
                    status: 500,
                    message: format!("Default executor creation failed: {:?}", e),
                    retry_after: None,
                })?;
                let result = executor
                    .execute(ExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|err| ComboAttemptError {
                        status: 500,
                        message: format!("Execution failed: {:?}", err),
                        retry_after: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            };

        let execution = executor_result;

        match execution {
            Ok(result) => {
                let status = result.response.status();
                if status.is_success() {
                    if let Some(retry_after) = retry_after_from_headers(result.response.headers()) {
                        let remaining = 0;
                        let reset = retry_after.timestamp();
                        registry.update_rate_limit(&connection.id, remaining, reset);
                    }
                    clear_connection_error(state, &connection.id).await;
                    if dashboard_stream {
                        return Ok(proxy_dashboard_sse_with_usage_tracking(
                            result.response,
                            state,
                            provider,
                            model,
                            Some(connection.id.as_str()),
                            api_key,
                            endpoint,
                        )
                        .await);
                    }
                    if !stream {
                        return Ok(proxy_response_with_usage_tracking(
                            result.response,
                            state,
                            provider,
                            model,
                            Some(connection.id.as_str()),
                            api_key,
                            endpoint,
                        )
                        .await);
                    }
                    let normalize_for_dashboard =
                        endpoint == Some("/api/dashboard/chat/completions");
                    return Ok(proxy_response_with_pending_tracking(
                        result.response,
                        state.clone(),
                        provider.to_string(),
                        model.to_string(),
                        Some(connection.id.clone()),
                        normalize_for_dashboard,
                    )
                    .await);
                }

                let retry_after = retry_after_from_headers(result.response.headers());
                let message = extract_error_message(result.response).await;
                state
                    .usage_live
                    .finish_request(model, provider, Some(connection.id.as_str()), true)
                    .await;
                let current_backoff = connection.backoff_level.unwrap_or(0);
                let decision = check_fallback_error(status.as_u16(), &message, current_backoff);
                let cooldown = retry_after
                    .map(|timestamp| (timestamp - Utc::now()).to_std().unwrap_or_default())
                    .unwrap_or(decision.cooldown);
                last_error = Some(ComboAttemptError {
                    status: status.as_u16(),
                    message: message.clone(),
                    retry_after,
                });

                if decision.should_fallback {
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        model,
                        status.as_u16(),
                        &message,
                        cooldown,
                        decision.new_backoff_level.unwrap_or(current_backoff + 1),
                    )
                    .await;
                    excluded.insert(connection.id.clone());
                    continue;
                }

                return Err(last_error.expect("set last error"));
            }
            Err(error) => {
                let message = format!("{:?}", error);
                state
                    .usage_live
                    .finish_request(model, provider, Some(connection.id.as_str()), true)
                    .await;
                let current_backoff = connection.backoff_level.unwrap_or(0);
                let decision = check_fallback_error(502, &message, current_backoff);
                last_error = Some(error);

                if decision.should_fallback {
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        model,
                        502,
                        &message,
                        decision.cooldown,
                        decision.new_backoff_level.unwrap_or(current_backoff + 1),
                    )
                    .await;
                    excluded.insert(connection.id.clone());
                    continue;
                }

                return Err(last_error.expect("set last error"));
            }
        }
    }
}

async fn proxy_dashboard_sse_with_usage_tracking(
    response: UpstreamResponse,
    state: &AppState,
    provider: &str,
    model: &str,
    connection_id: Option<&str>,
    api_key: Option<&str>,
    endpoint: Option<&str>,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let (body_bytes, body_complete) = collect_upstream_response_bytes(response).await;

    let token_usage = if body_complete {
        let usage = extract_token_usage_from_bytes(&body_bytes);
        state
            .usage_tracker()
            .track_request(
                provider,
                model,
                usage.as_ref(),
                connection_id,
                api_key,
                endpoint,
            )
            .await;
        state.usage_live.notify_update();
        usage
    } else {
        None
    };

    state
        .usage_live
        .finish_request(model, provider, connection_id, false)
        .await;

    let text = extract_dashboard_assistant_text_from_bytes(&body_bytes);
    let sse_body = build_dashboard_sse_body(text.as_deref(), token_usage.as_ref());
    build_dashboard_sse_response(status, &headers, sse_body)
}

fn select_connection(
    snapshot: &AppDb,
    provider: &str,
    model: &str,
    excluded: &HashSet<String>,
) -> Option<ProviderConnection> {
    let now = Utc::now();
    let mut candidates: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection.provider == provider
                && connection.is_active()
                && connection_has_credentials(connection)
                && !excluded.contains(&connection.id)
                && connection_supports_model(connection, model)
                && !is_connection_rate_limited(connection, now)
                && !is_model_locked(connection, model, now)
        })
        .cloned()
        .collect();

    candidates.sort_by_key(|connection| connection.priority.unwrap_or(999));
    candidates.into_iter().next()
}

fn connection_has_credentials(connection: &ProviderConnection) -> bool {
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

fn is_connection_rate_limited(connection: &ProviderConnection, now: DateTime<Utc>) -> bool {
    connection
        .rate_limited_until
        .as_deref()
        .and_then(parse_timestamp)
        .is_some_and(|until| until > now)
}

fn is_model_locked(connection: &ProviderConnection, model: &str, now: DateTime<Utc>) -> bool {
    [format!("modelLock_{model}"), "modelLock___all".to_string()]
        .into_iter()
        .filter_map(|key| connection.extra.get(&key))
        .filter_map(Value::as_str)
        .filter_map(parse_timestamp)
        .any(|until| until > now)
}

fn connection_supports_model(connection: &ProviderConnection, model: &str) -> bool {
    let enabled_models: Vec<_> = connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();

    if !enabled_models.is_empty() {
        return enabled_models
            .iter()
            .any(|value| model_ids_match(value, model));
    }

    connection
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none_or(|value| model_ids_match(value, model))
}

fn model_ids_match(advertised: &str, requested: &str) -> bool {
    let advertised = advertised.trim();
    let requested = requested.trim();

    advertised == requested || advertised.ends_with(&format!("/{requested}"))
}

fn earliest_retry_after(
    snapshot: &AppDb,
    provider: &str,
    model: &str,
    _excluded: &HashSet<String>,
) -> Option<DateTime<Utc>> {
    snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection.provider == provider
                && connection.is_active()
                && connection_has_credentials(connection)
                && connection_supports_model(connection, model)
        })
        .flat_map(|connection| {
            let mut retry_after = Vec::new();
            if let Some(until) = connection
                .rate_limited_until
                .as_deref()
                .and_then(parse_timestamp)
            {
                retry_after.push(until);
            }
            for key in [format!("modelLock_{model}"), "modelLock___all".to_string()] {
                if let Some(until) = connection
                    .extra
                    .get(&key)
                    .and_then(Value::as_str)
                    .and_then(parse_timestamp)
                {
                    retry_after.push(until);
                }
            }
            retry_after
        })
        .min()
}

fn combo_strategy_for(snapshot: &AppDb, combo_name: &str) -> ComboStrategy {
    let value = snapshot
        .settings
        .combo_strategies
        .get(combo_name)
        .map(String::as_str)
        .unwrap_or(snapshot.settings.combo_strategy.as_str());

    if value.eq_ignore_ascii_case("round-robin") {
        ComboStrategy::RoundRobin
    } else {
        ComboStrategy::Fallback
    }
}

async fn mark_connection_unavailable(
    state: &AppState,
    connection_id: &str,
    model: &str,
    status: u16,
    message: &str,
    cooldown: std::time::Duration,
    backoff_level: u32,
) {
    let until = ChronoDuration::from_std(cooldown)
        .map(|duration| Utc::now() + duration)
        .unwrap_or_else(|_| Utc::now());

    let connection_id = connection_id.to_string();
    let model_lock_key = format!("modelLock_{model}");
    let message = message.to_string();
    let _ = state
        .db
        .update(move |db| {
            if let Some(connection) = db
                .provider_connections
                .iter_mut()
                .find(|connection| connection.id == connection_id)
            {
                connection
                    .extra
                    .insert(model_lock_key.clone(), Value::String(until.to_rfc3339()));
                connection.last_error = Some(message.clone());
                connection.last_error_at = Some(Utc::now().to_rfc3339());
                connection.error_code = Some(status.to_string());
                connection.backoff_level = Some(backoff_level);
                connection.consecutive_errors = connection
                    .consecutive_errors
                    .map(|e| e.saturating_add(1))
                    .or(Some(1));
                connection.test_status = Some("unavailable".into());
            }
        })
        .await;
}

async fn clear_connection_error(state: &AppState, connection_id: &str) {
    let connection_id = connection_id.to_string();
    let _ = state
        .db
        .update(move |db| {
            if let Some(connection) = db
                .provider_connections
                .iter_mut()
                .find(|connection| connection.id == connection_id)
            {
                connection.last_error = None;
                connection.last_error_at = None;
                connection.error_code = None;
                connection.backoff_level = Some(0);
                connection.consecutive_errors = Some(0);
                connection.test_status = None;
            }
        })
        .await;
}

async fn proxy_response_with_usage_tracking(
    response: UpstreamResponse,
    state: &AppState,
    provider: &str,
    model: &str,
    connection_id: Option<&str>,
    api_key: Option<&str>,
    endpoint: Option<&str>,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let (body_bytes, body_complete) = collect_upstream_response_bytes(response).await;

    if body_complete {
        let token_usage = extract_token_usage_from_bytes(&body_bytes);
        state
            .usage_tracker()
            .track_request(
                provider,
                model,
                token_usage.as_ref(),
                connection_id,
                api_key,
                endpoint,
            )
            .await;
        state.usage_live.notify_update();
    }

    state
        .usage_live
        .finish_request(model, provider, connection_id, false)
        .await;

    build_proxied_response(status, &headers, Body::from(body_bytes))
}

async fn proxy_response_with_pending_tracking(
    response: UpstreamResponse,
    state: AppState,
    provider: String,
    model: String,
    connection_id: Option<String>,
    normalize_for_dashboard: bool,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let transformer = normalize_for_dashboard
        .then(|| transformer_for_provider(&provider))
        .flatten();
    let body = match response {
        UpstreamResponse::Reqwest(response) => {
            let state = state.clone();
            let provider = provider.clone();
            let model = model.clone();
            let connection_id = connection_id.clone();
            let mut transformer = transformer;
            let mut pending_text = String::new();
            let stream = async_stream::stream! {
                let mut upstream = response.bytes_stream();
                loop {
                    match upstream.try_next().await {
                        Ok(Some(chunk)) => {
                            if let Some(transformer) = transformer.as_mut() {
                                for line in transform_dashboard_sse_chunk(&chunk, transformer.as_mut(), &mut pending_text) {
                                    if let Some(frame) = sse_frame_for_dashboard(&line) {
                                        yield Ok::<Bytes, std::io::Error>(frame);
                                    }
                                }
                            } else {
                                yield Ok::<Bytes, std::io::Error>(chunk);
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {
                            state
                                .usage_live
                                .finish_request(&model, &provider, connection_id.as_deref(), true)
                                .await;
                            return;
                        }
                    }
                }
                if let Some(transformer) = transformer.as_mut() {
                    for line in flush_dashboard_sse_chunk(transformer.as_mut(), &mut pending_text) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                state
                    .usage_live
                    .finish_request(&model, &provider, connection_id.as_deref(), false)
                    .await;
            };
            Body::from_stream(stream)
        }
        UpstreamResponse::Hyper(response) => {
            let (_, mut body) = response.into_parts();
            let state = state.clone();
            let provider = provider.clone();
            let model = model.clone();
            let connection_id = connection_id.clone();
            let mut transformer = transformer;
            let mut pending_text = String::new();
            let stream = async_stream::stream! {
                while let Some(frame_result) = body.frame().await {
                    match frame_result {
                        Ok(frame) => {
                            if let Ok(data) = frame.into_data() {
                                if let Some(transformer) = transformer.as_mut() {
                                    for line in transform_dashboard_sse_chunk(&data, transformer.as_mut(), &mut pending_text) {
                                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                                            yield Ok::<Bytes, std::io::Error>(frame);
                                        }
                                    }
                                } else {
                                    yield Ok::<Bytes, std::io::Error>(data);
                                }
                            }
                        }
                        Err(_) => {
                            state
                                .usage_live
                                .finish_request(&model, &provider, connection_id.as_deref(), true)
                                .await;
                            return;
                        }
                    }
                }
                if let Some(transformer) = transformer.as_mut() {
                    for line in flush_dashboard_sse_chunk(transformer.as_mut(), &mut pending_text) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                state
                    .usage_live
                    .finish_request(&model, &provider, connection_id.as_deref(), false)
                    .await;
            };
            Body::from_stream(stream)
        }
    };

    build_proxied_response(status, &headers, body)
}

fn sse_frame_for_dashboard(line: &str) -> Option<Bytes> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let framed = if trimmed.starts_with(':') || trimmed.starts_with("data:") {
        format!("{trimmed}\n\n")
    } else {
        format!("data: {trimmed}\n\n")
    };

    Some(Bytes::from(framed))
}

fn build_dashboard_sse_body(text: Option<&str>, usage: Option<&TokenUsage>) -> Bytes {
    let mut frames = String::new();

    if let Some(text) = text.filter(|text| !text.is_empty()) {
        let escaped = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
        frames.push_str("data: {\"choices\":[{\"delta\":{\"content\":");
        frames.push_str(&escaped);
        frames.push_str("},\"finish_reason\":null}]}\n\n");
    }

    frames.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]");
    if let Some(usage) = usage {
        let usage_json = serde_json::to_string(usage).unwrap_or_else(|_| "{}".to_string());
        frames.push_str(",\"usage\":");
        frames.push_str(&usage_json);
    }
    frames.push_str("}\n\n");
    frames.push_str("data: [DONE]\n\n");

    Bytes::from(frames)
}

fn build_dashboard_sse_response(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: Bytes,
) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;

    for (name, value) in headers {
        if should_preserve_dashboard_sse_header(name.as_str()) {
            response.headers_mut().insert(name, value.clone());
        }
    }

    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    response
}

fn should_preserve_dashboard_sse_header(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    lowered == "trace-id"
        || lowered.starts_with("x-")
        || lowered.ends_with("-request-id")
        || lowered == "alb_receive_time"
        || lowered == "alb_request_id"
}

fn extract_dashboard_assistant_text_from_bytes(body: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(body).ok()?;

    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
    {
        return Some(text.to_string());
    }

    let content = value.get("content")?.as_array()?;
    let mut text_parts = Vec::new();
    let mut thinking_parts = Vec::new();
    for item in content {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                text_parts.push(text.to_string());
            }
            continue;
        }
        if let Some(thinking) = item.get("thinking").and_then(Value::as_str) {
            if !thinking.is_empty() {
                thinking_parts.push(thinking.to_string());
            }
        }
    }

    if !text_parts.is_empty() {
        return Some(text_parts.join(""));
    }

    if thinking_parts.is_empty() {
        None
    } else {
        Some(thinking_parts.join("\n"))
    }
}

fn transform_dashboard_sse_chunk(
    chunk: &Bytes,
    transformer: &mut dyn crate::core::translator::response_transform::StreamingTransformer,
    pending_text: &mut String,
) -> Vec<String> {
    pending_text.push_str(&String::from_utf8_lossy(chunk));
    let mut ready_lines = Vec::new();

    while let Some(newline_index) = pending_text.find('\n') {
        let mut line = pending_text[..newline_index].to_string();
        if line.ends_with('\r') {
            line.pop();
        }
        pending_text.drain(..=newline_index);
        if line.is_empty() {
            continue;
        }
        ready_lines.extend(transform_sse_stream(&Bytes::from(line), transformer));
    }

    ready_lines
}

fn flush_dashboard_sse_chunk(
    transformer: &mut dyn crate::core::translator::response_transform::StreamingTransformer,
    pending_text: &mut String,
) -> Vec<String> {
    if pending_text.trim().is_empty() {
        pending_text.clear();
        return Vec::new();
    }
    let mut line = std::mem::take(pending_text);
    if line.ends_with('\r') {
        line.pop();
    }
    transform_sse_stream(&Bytes::from(line), transformer)
}

fn build_proxied_response(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: Body,
) -> Response {
    let mut proxied = Response::new(body);
    *proxied.status_mut() = status;
    let connection_tokens = connection_header_tokens(&headers);

    for (name, value) in headers {
        if is_hop_by_hop_header(name.as_str())
            || connection_tokens.contains(&name.as_str().to_ascii_lowercase())
        {
            continue;
        }
        proxied.headers_mut().insert(name, value.clone());
    }

    proxied
}

async fn collect_upstream_response_bytes(response: UpstreamResponse) -> (Bytes, bool) {
    match response {
        UpstreamResponse::Reqwest(response) => {
            let mut stream = response.bytes_stream();
            let mut collected = Vec::new();
            let mut complete = true;

            loop {
                match stream.try_next().await {
                    Ok(Some(chunk)) => collected.extend_from_slice(&chunk),
                    Ok(None) => break,
                    Err(_) => {
                        complete = false;
                        break;
                    }
                }
            }

            (Bytes::from(collected), complete)
        }
        UpstreamResponse::Hyper(response) => {
            let (_, mut body) = response.into_parts();
            let mut collected = Vec::new();
            let mut complete = true;

            while let Some(frame_result) = body.frame().await {
                match frame_result {
                    Ok(frame) => {
                        if let Ok(data) = frame.into_data() {
                            collected.extend_from_slice(&data);
                        }
                    }
                    Err(_) => {
                        complete = false;
                        break;
                    }
                }
            }

            (Bytes::from(collected), complete)
        }
    }
}

fn extract_token_usage_from_bytes(body: &[u8]) -> Option<TokenUsage> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let usage = value.get("usage")?.as_object()?;

    let known_fields = [
        "prompt_tokens",
        "input_tokens",
        "completion_tokens",
        "output_tokens",
        "total_tokens",
    ];

    Some(TokenUsage {
        prompt_tokens: usage.get("prompt_tokens").and_then(Value::as_u64),
        input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
        completion_tokens: usage.get("completion_tokens").and_then(Value::as_u64),
        output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
        total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
        extra: usage
            .iter()
            .filter(|(key, _)| !known_fields.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
    })
}

async fn extract_error_message(response: UpstreamResponse) -> String {
    let status = response.status();
    let text = match response {
        UpstreamResponse::Reqwest(response) => response.text().await.unwrap_or_default(),
        UpstreamResponse::Hyper(response) => {
            let (_, body) = response.into_parts();
            body.collect()
                .await
                .map(|collected| String::from_utf8_lossy(&collected.to_bytes()).into_owned())
                .unwrap_or_default()
        }
    };
    if let Ok(value) = serde_json::from_str::<Value>(&text) {
        if let Some(message) = value
            .get("error")
            .and_then(|error| error.get("message").or(Some(error)))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return message.to_string();
        }

        if let Some(message) = value
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return message.to_string();
        }
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        status
            .canonical_reason()
            .unwrap_or("Upstream request failed")
            .to_string()
    } else {
        trimmed.to_string()
    }
}

fn retry_after_from_headers(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    let value = headers.get("retry-after")?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(seconds) = value.parse::<i64>() {
        return Some(Utc::now() + ChronoDuration::seconds(seconds.max(0)));
    }

    DateTime::parse_from_rfc2822(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .ok()
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

fn connection_header_tokens(headers: &reqwest::header::HeaderMap) -> HashSet<String> {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .ok()
}

fn combo_error_response(error: ComboExecutionError) -> Response {
    with_cors_response(attempt_error_response(ComboAttemptError {
        status: error.status,
        message: error.message,
        retry_after: error.earliest_retry_after,
    }))
}

fn attempt_error_response(error: ComboAttemptError) -> Response {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = (
        status,
        Json(json!({
            "error": {
                "message": error.message
            }
        })),
    )
        .into_response();

    if let Some(retry_after) = error.retry_after {
        let seconds = (retry_after - Utc::now()).num_seconds().max(1).to_string();
        if let Ok(value) = seconds.parse() {
            response.headers_mut().insert("retry-after", value);
        }
    }

    response
}

fn json_error_response(status: StatusCode, message: &str) -> Response {
    with_cors_response(
        (
            status,
            Json(json!({
                "error": {
                    "message": message
                }
            })),
        )
            .into_response(),
    )
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
        HeaderValue::from_static("GET, POST, OPTIONS"),
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
        HeaderValue::from_str(methods).unwrap_or(HeaderValue::from_static("GET, POST, OPTIONS")),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use axum::http::StatusCode;
    use bytes::Bytes;
    use chrono::{Duration as ChronoDuration, Utc};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};

    use super::{
        build_dashboard_sse_response, build_proxied_response, earliest_retry_after,
        select_connection,
    };
    use crate::types::{AppDb, ProviderConnection};

    fn connection(id: &str, priority: u32) -> ProviderConnection {
        ProviderConnection {
            id: id.to_string(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            name: Some(id.into()),
            priority: Some(priority),
            is_active: Some(true),
            created_at: None,
            updated_at: None,
            display_name: None,
            email: None,
            global_priority: None,
            default_model: Some("gpt-4.1".into()),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: Some(format!("sk-{id}")),
            test_status: None,
            last_tested: None,
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: None,
            error_code: None,
            consecutive_use_count: None,
            backoff_level: None,
            consecutive_errors: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            provider_specific_data: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn select_connection_skips_excluded_and_locked_accounts() {
        let locked_until = (Utc::now() + ChronoDuration::seconds(90)).to_rfc3339();
        let mut excluded_connection = connection("excluded", 1);
        excluded_connection.default_model = Some("gpt-4.1".into());

        let mut locked_connection = connection("locked", 2);
        locked_connection
            .extra
            .insert("modelLock_gpt-4.1".into(), Value::String(locked_until));

        let chosen_connection = connection("chosen", 3);

        let snapshot = AppDb {
            provider_connections: vec![
                excluded_connection.clone(),
                locked_connection,
                chosen_connection.clone(),
            ],
            ..AppDb::default()
        };

        let excluded = HashSet::from([excluded_connection.id]);
        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &excluded)
            .expect("third account should remain selectable");

        assert_eq!(selected.id, chosen_connection.id);
    }

    #[test]
    fn earliest_retry_after_reports_locked_model_deadline() {
        let early = Utc::now() + ChronoDuration::seconds(30);
        let late = Utc::now() + ChronoDuration::seconds(90);
        let mut early_locked = connection("early", 1);
        early_locked.extra.insert(
            "modelLock_gpt-4.1".into(),
            Value::String(early.to_rfc3339()),
        );

        let mut late_rate_limited = connection("late", 2);
        late_rate_limited.rate_limited_until = Some(late.to_rfc3339());

        let snapshot = AppDb {
            provider_connections: vec![late_rate_limited, early_locked],
            ..AppDb::default()
        };

        let retry_after = earliest_retry_after(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("retry-after should be derived from the earliest blocked account");

        assert!(retry_after <= early + ChronoDuration::seconds(1));
    }

    #[test]
    fn select_connection_skips_rate_limited_accounts() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut rate_limited = connection("rate-limited", 1);
        rate_limited.rate_limited_until = Some(future);

        let available = connection("available", 2);

        let snapshot = AppDb {
            provider_connections: vec![rate_limited, available.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should find available connection");

        assert_eq!(selected.id, "available");
    }

    #[test]
    fn select_connection_respects_model_locks_for_specific_model() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut locked = connection("locked-model", 1);
        locked
            .extra
            .insert("modelLock_gpt-4.1".into(), Value::String(future));

        let available = connection("available", 2);

        let snapshot = AppDb {
            provider_connections: vec![locked, available.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should skip locked model and find available");

        assert_eq!(selected.id, "available");
    }

    #[test]
    fn select_connection_skips_account_level_lock() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut all_locked = connection("all-locked", 1);
        all_locked
            .extra
            .insert("modelLock___all".into(), Value::String(future));

        let available = connection("available", 2);

        let snapshot = AppDb {
            provider_connections: vec![all_locked, available.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should skip account-level lock and find available");

        assert_eq!(selected.id, "available");
    }

    #[test]
    fn select_connection_skips_inactive_connections() {
        let mut inactive = connection("inactive", 1);
        inactive.is_active = Some(false);

        let available = connection("active", 2);

        let snapshot = AppDb {
            provider_connections: vec![inactive, available.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should find active connection");

        assert_eq!(selected.id, "active");
    }

    #[test]
    fn select_connection_skips_connections_without_credentials() {
        let mut no_creds = connection("no-creds", 1);
        no_creds.api_key = None;
        no_creds.access_token = None;

        let with_creds = connection("with-creds", 2);

        let snapshot = AppDb {
            provider_connections: vec![no_creds, with_creds.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should find connection with credentials");

        assert_eq!(selected.id, "with-creds");
    }

    #[test]
    fn select_connection_prioritizes_by_priority_field() {
        let low_priority = connection("low-priority", 2);
        let high_priority = connection("high-priority", 1);

        let snapshot = AppDb {
            provider_connections: vec![low_priority, high_priority.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should select highest priority connection");

        assert_eq!(selected.id, "high-priority");
    }

    #[test]
    fn select_connection_filters_by_model_support() {
        let mut conn_a = connection("conn-a", 1);
        conn_a.default_model = None;
        conn_a
            .provider_specific_data
            .insert("enabledModels".into(), json!(["gpt-4o"]));

        let mut conn_b = connection("conn-b", 2);
        conn_b.default_model = None;
        conn_b
            .provider_specific_data
            .insert("enabledModels".into(), json!(["gpt-4.1"]));

        let snapshot = AppDb {
            provider_connections: vec![conn_a, conn_b.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should select connection supporting gpt-4.1");

        assert_eq!(selected.id, "conn-b");
    }

    #[test]
    fn select_connection_returns_none_when_all_excluded() {
        let conn_a = connection("conn-a", 1);
        let conn_b = connection("conn-b", 2);

        let snapshot = AppDb {
            provider_connections: vec![conn_a, conn_b],
            ..AppDb::default()
        };

        let excluded: HashSet<String> = ["conn-a".to_string(), "conn-b".to_string()]
            .into_iter()
            .collect();

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &excluded);

        assert!(
            selected.is_none(),
            "should return None when all accounts excluded"
        );
    }

    #[test]
    fn select_connection_returns_none_when_no_connections_match() {
        let snapshot = AppDb::default();

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new());

        assert!(
            selected.is_none(),
            "should return None when no connections exist"
        );
    }

    #[test]
    fn is_connection_rate_limited_detects_expired_timestamp() {
        let past = (Utc::now() - ChronoDuration::seconds(10)).to_rfc3339();
        let mut conn = connection("conn", 1);
        conn.rate_limited_until = Some(past);

        assert!(
            !super::is_connection_rate_limited(&conn, Utc::now()),
            "expired rate_limited_until should not block connection"
        );
    }

    #[test]
    fn is_connection_rate_limited_allows_null_timestamp() {
        let conn = connection("conn", 1);
        assert!(
            !super::is_connection_rate_limited(&conn, Utc::now()),
            "null rate_limited_until should not block connection"
        );
    }

    #[test]
    fn is_model_locked_returns_false_when_no_lock() {
        let conn = connection("conn", 1);
        assert!(
            !super::is_model_locked(&conn, "gpt-4.1", Utc::now()),
            "connection without lock should not be locked"
        );
    }

    #[test]
    fn is_model_locked_checks_specific_model_key() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut conn = connection("conn", 1);
        conn.extra
            .insert("modelLock_gpt-4.1".into(), Value::String(future));

        assert!(
            super::is_model_locked(&conn, "gpt-4.1", Utc::now()),
            "specific model lock should block that model"
        );
        assert!(
            !super::is_model_locked(&conn, "gpt-4o", Utc::now()),
            "specific model lock should not block different model"
        );
    }

    #[test]
    fn is_model_locked_checks_account_level_all_key() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut conn = connection("conn", 1);
        conn.extra
            .insert("modelLock___all".into(), Value::String(future));

        assert!(
            super::is_model_locked(&conn, "any-model", Utc::now()),
            "account-level lock should block any model"
        );
    }

    #[test]
    fn is_model_locked_expired_lock_allows_connection() {
        let past = (Utc::now() - ChronoDuration::seconds(10)).to_rfc3339();
        let mut conn = connection("conn", 1);
        conn.extra
            .insert("modelLock_gpt-4.1".into(), Value::String(past));

        assert!(
            !super::is_model_locked(&conn, "gpt-4.1", Utc::now()),
            "expired model lock should not block"
        );
    }

    #[tokio::test]
    async fn build_dashboard_sse_response_returns_collectable_sse_body() {
        let body = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        );
        let response = build_dashboard_sse_response(
            StatusCode::OK,
            &reqwest::header::HeaderMap::new(),
            body.clone(),
        );

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "text/event-stream; charset=utf-8"
        );
        assert_eq!(
            response.headers()[axum::http::header::CACHE_CONTROL],
            "no-cache"
        );

        let collected = response
            .into_body()
            .collect()
            .await
            .expect("dashboard SSE body should collect");

        assert_eq!(collected.to_bytes(), body);
    }

    #[tokio::test]
    async fn build_proxied_response_preserves_plain_body_roundtrip() {
        let body = Bytes::from_static(b"hello world");
        let response = build_proxied_response(
            StatusCode::OK,
            &reqwest::header::HeaderMap::new(),
            axum::body::Body::from(body.clone()),
        );

        let collected = response
            .into_body()
            .collect()
            .await
            .expect("plain proxied body should collect");

        assert_eq!(collected.to_bytes(), body);
    }
}
