use std::collections::HashSet;

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::TryStreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};

use crate::core::combo::{
    check_fallback_error, execute_combo_strategy, get_combo_models_from_data, ComboAttemptError,
    ComboExecutionError, ComboStrategy,
};
use crate::core::executor::{ExecutorError, UpstreamResponse};
use crate::core::model::{get_model_info, ModelRouteKind};
use crate::core::proxy::resolve_proxy_target;
use crate::core::rtk::apply_request_preprocessing;
use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::{AppDb, ProviderConnection};

use super::auth_error_response;

pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
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
            match execute_combo_strategy(
                &combo_models,
                Some(&combo_name),
                strategy,
                move |combo_model| {
                    let state = combo_state.clone();
                    let body = combo_body.clone();
                    let combo_model = combo_model.to_string();
                    async move { execute_single_model(&state, &body, &combo_model).await }
                },
            )
            .await
            {
                Ok(response) => response,
                Err(error) => combo_error_response(error),
            }
        }
        ModelRouteKind::Direct => match execute_single_model(&state, &body, model_str).await {
            Ok(response) => response,
            Err(error) => attempt_error_response(error),
        },
    }
}

async fn execute_single_model(
    state: &AppState,
    request_body: &Value,
    model_str: &str,
) -> Result<Response, ComboAttemptError> {
    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model_str, &snapshot);
    let Some(provider) = resolved.provider else {
        return Err(ComboAttemptError {
            status: 400,
            message: "Invalid model format".into(),
            retry_after: None,
        });
    };

    let mut body = request_body.clone();
    if let Some(fields) = body.as_object_mut() {
        fields.insert("model".into(), Value::String(resolved.model.clone()));
    } else {
        return Err(ComboAttemptError {
            status: 400,
            message: "Request body must be a JSON object".into(),
            retry_after: None,
        });
    }

    let _ = apply_request_preprocessing(&mut body, &snapshot.settings, &resolved.model);

    forward_with_provider_fallback(state, &provider, &resolved.model, body).await
}

async fn forward_with_provider_fallback(
    state: &AppState,
    provider: &str,
    model: &str,
    request_body: Value,
) -> Result<Response, ComboAttemptError> {
    let mut excluded = HashSet::new();
    let mut last_error: Option<ComboAttemptError> = None;

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
        let stream = request_body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);

use crate::core::executor::{
            DefaultExecutor, ExecutionRequest, KiroExecutor, KiroExecutionRequest,
            KiroExecutorResponse,
        };

        let executor_result: Result<_, ComboAttemptError> = if provider == "kiro" {
            let executor = KiroExecutor::new(state.client_pool.clone(), provider_node)
                .map_err(|e| ComboAttemptError {
                    status: 500,
                    message: format!("Kiro executor creation failed: {:?}", e),
                    retry_after: None,
                })?;
            executor.execute_request(KiroExecutionRequest {
                model: model.to_string(),
                body: request_body.clone(),
                stream,
                credentials: connection.clone(),
                proxy,
            }).await
                .map_err(|e| ComboAttemptError {
                    status: 500,
                    message: format!("Kiro execution failed: {:?}", e),
                    retry_after: None,
                })
        } else {
            let executor = DefaultExecutor::new(provider.to_string(), state.client_pool.clone(), provider_node)
                .map_err(|e| ComboAttemptError {
                    status: 500,
                    message: format!("Default executor creation failed: {:?}", e),
                    retry_after: None,
                })?;
            let result = executor.execute(ExecutionRequest {
                model: model.to_string(),
                body: request_body.clone(),
                stream,
                credentials: connection.clone(),
                proxy,
            }).await.map_err(|err| ComboAttemptError {
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
                    clear_connection_error(state, &connection.id).await;
                    return Ok(proxy_response(result.response));
                }

                let retry_after = retry_after_from_headers(result.response.headers());
                let message = extract_error_message(result.response).await;
                let decision = check_fallback_error(status.as_u16(), &message, 0);
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
                    )
                    .await;
                    excluded.insert(connection.id.clone());
                    continue;
                }

                return Err(last_error.expect("set last error"));
            }
            Err(error) => {
                let message = format!("{:?}", error);
                let decision = check_fallback_error(502, &message, 0);
                last_error = Some(error);

                if decision.should_fallback {
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        model,
                        502,
                        &message,
                        decision.cooldown,
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
                connection.test_status = None;
            }
        })
        .await;
}

fn proxy_response(response: UpstreamResponse) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let body = match response {
        UpstreamResponse::Reqwest(response) => {
            let stream = response.bytes_stream().map_ok(|b: Bytes| b);
            Body::from_stream(stream)
        }
        UpstreamResponse::Hyper(response) => {
            let (_, body) = response.into_parts();
            Body::new(body)
        }
    };
    let mut proxied = Response::new(body);
    *proxied.status_mut() = status;
    let connection_tokens = connection_header_tokens(&headers);

    for (name, value) in &headers {
        if is_hop_by_hop_header(name.as_str())
            || connection_tokens.contains(&name.as_str().to_ascii_lowercase())
        {
            continue;
        }
        proxied.headers_mut().insert(name, value.clone());
    }

    proxied
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

fn executor_error_message(error: &ExecutorError) -> String {
    match error {
        ExecutorError::UnsupportedProvider(provider) => format!("Unsupported provider: {provider}"),
        ExecutorError::MissingCredentials(provider) => {
            format!("Missing credentials for provider: {provider}")
        }
        ExecutorError::MissingProviderSpecificData(provider, field) => {
            format!("Missing provider-specific field {field} for: {provider}")
        }
        ExecutorError::InvalidHeader(error) => format!("Invalid upstream header: {error}"),
        ExecutorError::InvalidUri(error) => format!("Invalid upstream URL: {error}"),
        ExecutorError::InvalidRequest(error) => format!("Invalid upstream request: {error}"),
        ExecutorError::Serialize(error) => format!("Failed to encode upstream body: {error}"),
        ExecutorError::HyperClientInit(error) => {
            format!("Failed to initialize hyper client: {error}")
        }
        ExecutorError::Hyper(error) => format!("Upstream hyper request failed: {error}"),
        ExecutorError::Request(error) => format!("Upstream request failed: {error}"),
    }
}

fn combo_error_response(error: ComboExecutionError) -> Response {
    attempt_error_response(ComboAttemptError {
        status: error.status,
        message: error.message,
        retry_after: error.earliest_retry_after,
    })
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use chrono::{Duration as ChronoDuration, Utc};
    use serde_json::{json, Value};

    use super::{earliest_retry_after, select_connection};
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
        locked.extra.insert(
            "modelLock_gpt-4.1".into(),
            Value::String(future),
        );

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
        all_locked.extra.insert("modelLock___all".into(), Value::String(future));

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

        assert!(selected.is_none(), "should return None when all accounts excluded");
    }

    #[test]
    fn select_connection_returns_none_when_no_connections_match() {
        let snapshot = AppDb::default();

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new());

        assert!(selected.is_none(), "should return None when no connections exist");
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
        conn.extra.insert("modelLock___all".into(), Value::String(future));

        assert!(
            super::is_model_locked(&conn, "any-model", Utc::now()),
            "account-level lock should block any model"
        );
    }

    #[test]
    fn is_model_locked_expired_lock_allows_connection() {
        let past = (Utc::now() - ChronoDuration::seconds(10)).to_rfc3339();
        let mut conn = connection("conn", 1);
        conn.extra.insert("modelLock_gpt-4.1".into(), Value::String(past));

        assert!(
            !super::is_model_locked(&conn, "gpt-4.1", Utc::now()),
            "expired model lock should not block"
        );
    }
}
