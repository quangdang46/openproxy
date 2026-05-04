use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::state::AppState;
use crate::types::ProxyPool;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

fn supports_direct_api_key_update(auth_type: &str) -> bool {
    matches!(auth_type, "apikey" | "api_key")
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/providers/{id}",
            get(get_provider)
                .put(update_provider)
                .delete(delete_provider),
        )
        .route(
            "/api/combos/{id}",
            get(get_combo).put(update_combo).delete(delete_combo),
        )
        .route(
            "/api/keys/{id}",
            get(get_key).put(update_key).delete(delete_key),
        )
        .route(
            "/api/proxy-pools/{id}",
            get(get_proxy_pool)
                .put(update_proxy_pool)
                .delete(delete_proxy_pool),
        )
}

async fn get_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(connection) = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
    else {
        return not_found("Connection not found");
    };

    Json(json!({
        "connection": super::redact_provider_connection(&connection)
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProviderRequest {
    name: Option<String>,
    priority: Option<u32>,
    global_priority: Option<u32>,
    default_model: Option<String>,
    is_active: Option<bool>,
    api_key: Option<String>,
    test_status: Option<String>,
    last_error: Option<String>,
    last_error_at: Option<String>,
    provider_specific_data: Option<serde_json::Map<String, Value>>,
    connection_proxy_enabled: Option<bool>,
    connection_proxy_url: Option<String>,
    connection_no_proxy: Option<String>,
    proxy_pool_id: Option<Value>,
}

async fn update_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateProviderRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(existing) = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
    else {
        return not_found("Connection not found");
    };

    let proxy_config = match normalize_connection_proxy(&req) {
        Ok(config) => config,
        Err(message) => return bad_request(&message),
    };

    let proxy_pool_update =
        match normalize_proxy_pool_update(&snapshot.proxy_pools, req.proxy_pool_id.as_ref()) {
            Ok(update) => update,
            Err(message) => return bad_request(&message),
        };

    let updated = state
        .db
        .update({
            let id = id.clone();
            move |db| {
                if let Some(connection) = db
                    .provider_connections
                    .iter_mut()
                    .find(|connection| connection.id == id)
                {
                    if let Some(name) = req.name.clone() {
                        connection.name = Some(name);
                    }
                    if let Some(priority) = req.priority {
                        connection.priority = Some(priority);
                    }
                    if let Some(global_priority) = req.global_priority {
                        connection.global_priority = Some(global_priority);
                    }
                    if let Some(default_model) = req.default_model.clone() {
                        connection.default_model = Some(default_model);
                    }
                    if let Some(is_active) = req.is_active {
                        connection.is_active = Some(is_active);
                    }
                    if supports_direct_api_key_update(&existing.auth_type) {
                        if let Some(api_key) = req.api_key.clone() {
                            connection.api_key = Some(api_key);
                        }
                    }
                    if let Some(test_status) = req.test_status.clone() {
                        connection.test_status = Some(test_status);
                    }
                    if let Some(last_error) = req.last_error.clone() {
                        connection.last_error = Some(last_error);
                    }
                    if let Some(last_error_at) = req.last_error_at.clone() {
                        connection.last_error_at = Some(last_error_at);
                    }

                    if let Some(provider_specific_data) = req.provider_specific_data.clone() {
                        for (key, value) in provider_specific_data {
                            connection.provider_specific_data.insert(key, value);
                        }
                    }

                    if let Some(enabled) = proxy_config.connection_proxy_enabled {
                        connection
                            .provider_specific_data
                            .insert("connectionProxyEnabled".to_string(), Value::Bool(enabled));
                    }
                    if let Some(url) = proxy_config.connection_proxy_url.clone() {
                        connection
                            .provider_specific_data
                            .insert("connectionProxyUrl".to_string(), Value::String(url));
                    }
                    if let Some(no_proxy) = proxy_config.connection_no_proxy.clone() {
                        connection
                            .provider_specific_data
                            .insert("connectionNoProxy".to_string(), Value::String(no_proxy));
                    }

                    if proxy_pool_update.has_field {
                        if let Some(proxy_pool_id) = proxy_pool_update.proxy_pool_id.clone() {
                            connection
                                .provider_specific_data
                                .insert("proxyPoolId".to_string(), Value::String(proxy_pool_id));
                        } else {
                            connection.provider_specific_data.remove("proxyPoolId");
                        }
                    }

                    connection.updated_at = Some(Utc::now().to_rfc3339());
                }
            }
        })
        .await;

    match updated {
        Ok(snapshot) => {
            let Some(connection) = snapshot
                .provider_connections
                .iter()
                .find(|connection| connection.id == id)
            else {
                return not_found("Connection not found");
            };

            Json(json!({
                "connection": super::redact_provider_connection(connection)
            }))
            .into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn delete_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    if !snapshot
        .provider_connections
        .iter()
        .any(|connection| connection.id == id)
    {
        return not_found("Connection not found");
    }

    match state
        .db
        .update(move |db| {
            db.provider_connections
                .retain(|connection| connection.id != id);
        })
        .await
    {
        Ok(_) => Json(json!({ "message": "Connection deleted successfully" })).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn get_combo(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(combo) = snapshot.combos.iter().find(|combo| combo.id == id).cloned() else {
        return not_found("Combo not found");
    };

    Json(combo).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateComboRequest {
    name: Option<String>,
    models: Option<Vec<String>>,
    kind: Option<String>,
}

async fn update_combo(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateComboRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(existing) = snapshot.combos.iter().find(|combo| combo.id == id).cloned() else {
        return not_found("Combo not found");
    };

    if let Some(name) = req.name.as_deref() {
        if !valid_combo_name(name) {
            return bad_request("Name can only contain letters, numbers, -, _ and .");
        }

        if snapshot
            .combos
            .iter()
            .any(|combo| combo.id != id && combo.name == name)
        {
            return bad_request("Combo name already exists");
        }
    }

    match state
        .db
        .update(move |db| {
            if let Some(combo) = db.combos.iter_mut().find(|combo| combo.id == id) {
                if let Some(name) = req.name.clone() {
                    combo.name = name;
                }
                if let Some(models) = req.models.clone() {
                    combo.models = models;
                }
                if let Some(kind) = req.kind.clone() {
                    combo.kind = Some(kind);
                }
                combo.updated_at = Some(Utc::now().to_rfc3339());
            }
        })
        .await
    {
        Ok(snapshot) => {
            let Some(combo) = snapshot.combos.iter().find(|combo| combo.id == existing.id) else {
                return not_found("Combo not found");
            };
            Json(combo).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn delete_combo(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    if !snapshot.combos.iter().any(|combo| combo.id == id) {
        return not_found("Combo not found");
    }

    match state
        .db
        .update(move |db| {
            db.combos.retain(|combo| combo.id != id);
        })
        .await
    {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn get_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(key) = snapshot.api_keys.iter().find(|key| key.id == id).cloned() else {
        return not_found("Key not found");
    };

    Json(json!({ "key": key })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateKeyRequest {
    is_active: Option<bool>,
}

async fn update_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateKeyRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    if !snapshot.api_keys.iter().any(|key| key.id == id) {
        return not_found("Key not found");
    }

    let key_id = id.clone();
    match state
        .db
        .update(move |db| {
            if let Some(key) = db.api_keys.iter_mut().find(|key| key.id == key_id) {
                if let Some(is_active) = req.is_active {
                    key.is_active = Some(is_active);
                }
            }
        })
        .await
    {
        Ok(snapshot) => {
            let Some(key) = snapshot.api_keys.iter().find(|key| key.id == id) else {
                return not_found("Key not found");
            };
            Json(json!({ "key": key })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    if !snapshot.api_keys.iter().any(|key| key.id == id) {
        return not_found("Key not found");
    }

    match state
        .db
        .update(move |db| {
            db.api_keys.retain(|key| key.id != id);
        })
        .await
    {
        Ok(_) => Json(json!({ "message": "Key deleted successfully" })).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn get_proxy_pool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(proxy_pool) = snapshot
        .proxy_pools
        .iter()
        .find(|proxy_pool| proxy_pool.id == id)
        .cloned()
    else {
        return not_found("Proxy pool not found");
    };

    Json(json!({ "proxyPool": proxy_pool })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProxyPoolRequest {
    name: Option<String>,
    proxy_url: Option<String>,
    no_proxy: Option<String>,
    is_active: Option<bool>,
    strict_proxy: Option<bool>,
    r#type: Option<String>,
}

async fn update_proxy_pool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateProxyPoolRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    if !snapshot
        .proxy_pools
        .iter()
        .any(|proxy_pool| proxy_pool.id == id)
    {
        return not_found("Proxy pool not found");
    }

    let updates = match normalize_proxy_pool_request(&req) {
        Ok(updates) => updates,
        Err(message) => return bad_request(&message),
    };

    let proxy_pool_id = id.clone();
    match state
        .db
        .update(move |db| {
            if let Some(proxy_pool) = db
                .proxy_pools
                .iter_mut()
                .find(|proxy_pool| proxy_pool.id == proxy_pool_id)
            {
                if let Some(name) = updates.name.clone() {
                    proxy_pool.name = name;
                }
                if let Some(proxy_url) = updates.proxy_url.clone() {
                    proxy_pool.proxy_url = proxy_url;
                }
                if let Some(no_proxy) = updates.no_proxy.clone() {
                    proxy_pool.no_proxy = no_proxy;
                }
                if let Some(is_active) = updates.is_active {
                    proxy_pool.is_active = Some(is_active);
                }
                if let Some(strict_proxy) = updates.strict_proxy {
                    proxy_pool.strict_proxy = Some(strict_proxy);
                }
                if let Some(pool_type) = updates.r#type.clone() {
                    proxy_pool.r#type = pool_type;
                }
                proxy_pool.updated_at = Some(Utc::now().to_rfc3339());
            }
        })
        .await
    {
        Ok(snapshot) => {
            let Some(proxy_pool) = snapshot
                .proxy_pools
                .iter()
                .find(|proxy_pool| proxy_pool.id == id)
            else {
                return not_found("Proxy pool not found");
            };
            Json(json!({ "proxyPool": proxy_pool })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn delete_proxy_pool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    if !snapshot
        .proxy_pools
        .iter()
        .any(|proxy_pool| proxy_pool.id == id)
    {
        return not_found("Proxy pool not found");
    }

    let bound_connection_count = snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection
                .provider_specific_data
                .get("proxyPoolId")
                .and_then(Value::as_str)
                .is_some_and(|proxy_pool_id| proxy_pool_id == id)
        })
        .count();

    if bound_connection_count > 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Proxy pool is currently in use",
                "boundConnectionCount": bound_connection_count
            })),
        )
            .into_response();
    }

    match state
        .db
        .update(move |db| {
            db.proxy_pools.retain(|proxy_pool| proxy_pool.id != id);
        })
        .await
    {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(error) => internal_error(error),
    }
}

#[derive(Clone, Default)]
struct ConnectionProxyConfig {
    connection_proxy_enabled: Option<bool>,
    connection_proxy_url: Option<String>,
    connection_no_proxy: Option<String>,
}

fn normalize_connection_proxy(
    req: &UpdateProviderRequest,
) -> Result<ConnectionProxyConfig, String> {
    let has_any_proxy_field = req.connection_proxy_enabled.is_some()
        || req.connection_proxy_url.is_some()
        || req.connection_no_proxy.is_some();

    if !has_any_proxy_field {
        return Ok(ConnectionProxyConfig::default());
    }

    let enabled = req.connection_proxy_enabled.unwrap_or(false);
    let url = req
        .connection_proxy_url
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let no_proxy = req
        .connection_no_proxy
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();

    if enabled && url.is_empty() {
        return Err("Connection proxy URL is required when connection proxy is enabled".into());
    }

    Ok(ConnectionProxyConfig {
        connection_proxy_enabled: Some(enabled),
        connection_proxy_url: Some(url),
        connection_no_proxy: Some(no_proxy),
    })
}

#[derive(Clone, Default)]
struct ProxyPoolFieldUpdate {
    has_field: bool,
    proxy_pool_id: Option<String>,
}

fn normalize_proxy_pool_update(
    proxy_pools: &[ProxyPool],
    proxy_pool_id_input: Option<&Value>,
) -> Result<ProxyPoolFieldUpdate, String> {
    let Some(proxy_pool_id_input) = proxy_pool_id_input else {
        return Ok(ProxyPoolFieldUpdate::default());
    };

    if proxy_pool_id_input.is_null() {
        return Ok(ProxyPoolFieldUpdate {
            has_field: true,
            proxy_pool_id: None,
        });
    }

    let raw = proxy_pool_id_input
        .as_str()
        .map(str::trim)
        .unwrap_or_default();

    if raw.is_empty() || raw == "__none__" {
        return Ok(ProxyPoolFieldUpdate {
            has_field: true,
            proxy_pool_id: None,
        });
    }

    if !proxy_pools.iter().any(|proxy_pool| proxy_pool.id == raw) {
        return Err("Proxy pool not found".into());
    }

    Ok(ProxyPoolFieldUpdate {
        has_field: true,
        proxy_pool_id: Some(raw.to_string()),
    })
}

fn valid_combo_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

#[derive(Clone, Default)]
struct ProxyPoolUpdates {
    name: Option<String>,
    proxy_url: Option<String>,
    no_proxy: Option<String>,
    is_active: Option<bool>,
    strict_proxy: Option<bool>,
    r#type: Option<String>,
}

fn normalize_proxy_pool_request(req: &UpdateProxyPoolRequest) -> Result<ProxyPoolUpdates, String> {
    let mut updates = ProxyPoolUpdates::default();

    if let Some(name) = req.name.as_deref() {
        let name = name.trim();
        if name.is_empty() {
            return Err("Name is required".into());
        }
        updates.name = Some(name.to_string());
    }

    if let Some(proxy_url) = req.proxy_url.as_deref() {
        let proxy_url = proxy_url.trim();
        if proxy_url.is_empty() {
            return Err("Proxy URL is required".into());
        }
        updates.proxy_url = Some(proxy_url.to_string());
    }

    if let Some(no_proxy) = req.no_proxy.as_deref() {
        updates.no_proxy = Some(no_proxy.trim().to_string());
    }

    if let Some(is_active) = req.is_active {
        updates.is_active = Some(is_active);
    }

    if let Some(strict_proxy) = req.strict_proxy {
        updates.strict_proxy = Some(strict_proxy);
    }

    if let Some(pool_type) = req.r#type.as_deref() {
        updates.r#type = Some(match pool_type {
            "http" | "vercel" => pool_type.to_string(),
            _ => "http".to_string(),
        });
    }

    Ok(updates)
}

fn not_found(message: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": message }))).into_response()
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

fn internal_error(error: impl ToString) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_name_validation_matches_baseline_shape() {
        assert!(valid_combo_name("combo_1.v2"));
        assert!(!valid_combo_name("combo name"));
        assert!(!valid_combo_name(""));
    }

    #[test]
    fn proxy_pool_update_rejects_missing_url_when_enabled() {
        let req = UpdateProviderRequest {
            name: None,
            priority: None,
            global_priority: None,
            default_model: None,
            is_active: None,
            api_key: None,
            test_status: None,
            last_error: None,
            last_error_at: None,
            provider_specific_data: None,
            connection_proxy_enabled: Some(true),
            connection_proxy_url: Some(" ".into()),
            connection_no_proxy: None,
            proxy_pool_id: None,
        };

        assert!(normalize_connection_proxy(&req).is_err());
    }
}
