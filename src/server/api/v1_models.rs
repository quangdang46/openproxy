use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};

use crate::core::model::catalog::provider_catalog;
use crate::core::model::resolve_provider_alias;
use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::{AppDb, ModelAliasTarget, ProviderConnection};

const LLM_KIND: &str = "llm";
const OPENAI_COMPATIBLE_PREFIX: &str = "openai-compatible-";
const ANTHROPIC_COMPATIBLE_PREFIX: &str = "anthropic-compatible-";

static UPSTREAM_CONNECTION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[-_][0-9a-f]{8,}$").expect("valid upstream connection regex"));

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1", get(list_default_models).options(cors_options))
        .route("/v1/models", get(list_default_models).options(cors_options))
        .route(
            "/v1/models/{kind}",
            get(list_models_by_kind).options(cors_options),
        )
}

async fn cors_options() -> Response {
    cors_preflight_response("GET, OPTIONS")
}

async fn list_default_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    list_models_for_kinds(state, headers, &[LLM_KIND]).await
}

async fn list_models_by_kind(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(kind): Path<String>,
) -> Response {
    let kind_filter = match kind.as_str() {
        "image" => vec!["image"],
        "tts" => vec!["tts"],
        "stt" => vec!["stt"],
        "embedding" => vec!["embedding"],
        "image-to-text" => vec!["imageToText"],
        "web" => vec!["webSearch", "webFetch"],
        _ => {
            return with_cors_json(
                StatusCode::NOT_FOUND,
                json!({
                    "error": {
                        "message": format!(
                            "Unknown model kind: {kind}. Supported: image, tts, stt, embedding, image-to-text, web"
                        ),
                        "type": "invalid_request_error"
                    }
                }),
            );
        }
    };

    list_models_for_kinds(state, headers, &kind_filter).await
}

async fn list_models_for_kinds(
    state: AppState,
    headers: HeaderMap,
    kind_filter: &[&str],
) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return with_cors_response(super::auth_error_response(error));
    }

    let snapshot = state.db.snapshot();
    let data = build_models_list(&snapshot, kind_filter).await;

    with_cors_response(
        Json(ModelListResponse {
            object: "list",
            data,
        })
        .into_response(),
    )
}

async fn build_models_list(snapshot: &AppDb, kind_filter: &[&str]) -> Vec<ModelCard> {
    let catalog = provider_catalog();
    let alias_to_provider_id = catalog.alias_to_provider_id();
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let active_connections: Vec<&ProviderConnection> = snapshot
        .provider_connections
        .iter()
        .filter(|connection| connection.is_active())
        .collect();

    let mut seen_providers = HashSet::new();
    let mut active_connection_by_provider = Vec::new();
    for connection in &active_connections {
        if seen_providers.insert(connection.provider.as_str()) {
            active_connection_by_provider.push(*connection);
        }
    }

    let mut models = Vec::new();

    for combo in &snapshot.combos {
        if !combo_matches_kinds(combo.kind.as_deref(), kind_filter) {
            continue;
        }

        let kind = match combo.kind.as_deref() {
            Some("webSearch") => Some("webSearch".to_string()),
            Some("webFetch") => Some("webFetch".to_string()),
            _ => None,
        };

        models.push(model_card(
            combo.name.clone(),
            "combo".to_string(),
            created,
            kind,
        ));
    }

    if active_connections.is_empty() {
        for provider_entry in catalog.iter_provider_models() {
            let provider_id = alias_to_provider_id
                .get(&provider_entry.alias)
                .map(String::as_str)
                .unwrap_or(provider_entry.alias.as_str());

            if !provider_matches_kinds(catalog, provider_id, kind_filter) {
                continue;
            }

            for model in &provider_entry.models {
                if !kind_filter.iter().any(|kind| *kind == model.kind) {
                    continue;
                }

                models.push(model_card(
                    format!("{}/{}", provider_entry.alias, model.id),
                    provider_entry.alias.clone(),
                    created,
                    None,
                ));
            }
        }

        if kind_filter.iter().any(|kind| *kind == LLM_KIND) {
            for custom_model in snapshot
                .custom_models
                .iter()
                .filter(|model| model.r#type.is_empty() || model.r#type == LLM_KIND)
            {
                let model_id = custom_model.id.trim();
                let provider_alias = custom_model.provider_alias.trim();
                if model_id.is_empty() || provider_alias.is_empty() {
                    continue;
                }

                models.push(model_card(
                    format!("{provider_alias}/{model_id}"),
                    provider_alias.to_string(),
                    created,
                    None,
                ));
            }
        }
    } else {
        for connection in active_connection_by_provider {
            let provider_id = connection.provider.as_str();
            if !provider_matches_kinds(catalog, provider_id, kind_filter) {
                continue;
            }

            let static_alias = catalog
                .static_alias_for_provider(provider_id)
                .unwrap_or(provider_id);
            let output_alias = output_alias(catalog, connection, provider_id, static_alias);
            let provider_models = catalog.models_for_alias(static_alias).unwrap_or(&[]);
            let static_model_kind_by_id: HashMap<_, _> = provider_models
                .iter()
                .map(|model| (model.id.as_str(), model.kind.as_str()))
                .collect();

            let (mut raw_model_ids, had_enabled_models) = enabled_model_ids(connection);
            if !had_enabled_models {
                raw_model_ids = provider_models
                    .iter()
                    .map(|model| model.id.clone())
                    .collect::<Vec<_>>();
            }

            if is_compatible_provider(provider_id)
                && raw_model_ids.is_empty()
                && !UPSTREAM_CONNECTION_RE.is_match(provider_id)
            {
                raw_model_ids =
                    super::provider_models::fetch_compatible_model_ids(connection).await;
            }

            let prefixes = [output_alias.as_str(), static_alias, provider_id];
            let model_ids = raw_model_ids
                .into_iter()
                .filter_map(|model_id| strip_provider_prefix(&model_id, &prefixes))
                .collect::<Vec<_>>();

            let custom_model_ids = snapshot
                .custom_models
                .iter()
                .filter(|model| model.r#type.is_empty() || model.r#type == LLM_KIND)
                .filter_map(|model| {
                    let provider_alias = model.provider_alias.trim();
                    let model_id = model.id.trim();
                    if model_id.is_empty()
                        || (provider_alias != static_alias
                            && provider_alias != output_alias
                            && provider_alias != provider_id)
                    {
                        return None;
                    }

                    Some(model_id.to_string())
                })
                .collect::<Vec<_>>();

            let alias_model_ids = snapshot
                .model_aliases
                .values()
                .flat_map(model_alias_paths)
                .filter(|path| {
                    path.starts_with(&format!("{output_alias}/"))
                        || path.starts_with(&format!("{static_alias}/"))
                        || path.starts_with(&format!("{provider_id}/"))
                })
                .filter_map(|path| strip_provider_prefix(&path, &prefixes))
                .collect::<Vec<_>>();

            let merged_model_ids = dedupe_strings(
                model_ids
                    .into_iter()
                    .chain(custom_model_ids.into_iter())
                    .chain(alias_model_ids.into_iter())
                    .collect(),
            );

            for model_id in merged_model_ids {
                let kind = static_model_kind_by_id
                    .get(model_id.as_str())
                    .copied()
                    .unwrap_or_else(|| infer_kind_from_unknown_model_id(&model_id));
                if !kind_filter.iter().any(|candidate| *candidate == kind) {
                    continue;
                }

                models.push(model_card(
                    format!("{output_alias}/{model_id}"),
                    output_alias.clone(),
                    created,
                    None,
                ));
            }

            if let Some(provider_info) = catalog.provider_info(provider_id) {
                if kind_filter.iter().any(|kind| *kind == "tts") {
                    for model_id in &provider_info.tts_models {
                        models.push(model_card(
                            format!("{output_alias}/{model_id}"),
                            output_alias.clone(),
                            created,
                            None,
                        ));
                    }
                }

                if kind_filter.iter().any(|kind| *kind == "embedding") {
                    for model_id in &provider_info.embedding_models {
                        models.push(model_card(
                            format!("{output_alias}/{model_id}"),
                            output_alias.clone(),
                            created,
                            None,
                        ));
                    }
                }

                if kind_filter.iter().any(|kind| *kind == "webSearch") && provider_info.has_search {
                    models.push(model_card(
                        format!("{output_alias}/search"),
                        output_alias.clone(),
                        created,
                        Some("webSearch".to_string()),
                    ));
                }

                if kind_filter.iter().any(|kind| *kind == "webFetch") && provider_info.has_fetch {
                    models.push(model_card(
                        format!("{output_alias}/fetch"),
                        output_alias.clone(),
                        created,
                        Some("webFetch".to_string()),
                    ));
                }
            }
        }
    }

    let mut deduped_models = Vec::new();
    let mut seen_ids = HashSet::new();
    for model in models {
        if seen_ids.insert(model.id.clone()) {
            deduped_models.push(model);
        }
    }

    deduped_models
}

fn provider_matches_kinds(
    catalog: &crate::core::model::catalog::ProviderCatalog,
    provider_id: &str,
    kind_filter: &[&str],
) -> bool {
    let service_kinds = catalog
        .provider_info(provider_id)
        .map(|provider| provider.service_kinds.as_slice())
        .unwrap_or(&[]);

    if service_kinds.is_empty() {
        return kind_filter.iter().any(|kind| *kind == LLM_KIND);
    }

    kind_filter
        .iter()
        .any(|candidate| service_kinds.iter().any(|kind| kind == candidate))
}

fn combo_matches_kinds(kind: Option<&str>, kind_filter: &[&str]) -> bool {
    let combo_kind = kind.unwrap_or(LLM_KIND);
    kind_filter.iter().any(|candidate| *candidate == combo_kind)
}

fn output_alias(
    catalog: &crate::core::model::catalog::ProviderCatalog,
    connection: &ProviderConnection,
    provider_id: &str,
    static_alias: &str,
) -> String {
    connection
        .provider_specific_data
        .get("prefix")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .map(str::to_string)
        .or_else(|| {
            catalog
                .provider_info(provider_id)
                .map(|provider| provider.alias.trim().to_string())
                .filter(|alias| !alias.is_empty())
        })
        .unwrap_or_else(|| static_alias.to_string())
}

fn enabled_model_ids(connection: &ProviderConnection) -> (Vec<String>, bool) {
    let enabled_models = connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(Value::as_array);

    let had_enabled_models = enabled_models
        .map(|models| !models.is_empty())
        .unwrap_or(false);
    let models = enabled_models
        .map(|values| {
            dedupe_strings(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect(),
            )
        })
        .unwrap_or_default();

    (models, had_enabled_models)
}

fn is_compatible_provider(provider_id: &str) -> bool {
    provider_id.starts_with(OPENAI_COMPATIBLE_PREFIX)
        || provider_id.starts_with(ANTHROPIC_COMPATIBLE_PREFIX)
}

fn strip_provider_prefix(model_id: &str, prefixes: &[&str]) -> Option<String> {
    let value = model_id.trim();
    if value.is_empty() {
        return None;
    }

    for prefix in prefixes {
        let needle = format!("{prefix}/");
        if value.starts_with(&needle) {
            let stripped = value[needle.len()..].trim();
            if stripped.is_empty() {
                return None;
            }
            return Some(stripped.to_string());
        }
    }

    Some(value.to_string())
}

fn model_alias_paths(target: &ModelAliasTarget) -> Vec<String> {
    match target {
        ModelAliasTarget::Path(path) => {
            let value = path.trim();
            if value.is_empty() {
                Vec::new()
            } else {
                vec![value.to_string()]
            }
        }
        ModelAliasTarget::Mapping(mapping) => {
            let provider = mapping.provider.trim();
            let model = mapping.model.trim();
            if provider.is_empty() || model.is_empty() {
                return Vec::new();
            }

            let mut values = vec![format!("{provider}/{model}")];
            let resolved = resolve_provider_alias(provider);
            if resolved != provider {
                values.push(format!("{resolved}/{model}"));
            }
            dedupe_strings(values)
        }
    }
}

fn infer_kind_from_unknown_model_id(model_id: &str) -> &str {
    let lower = model_id.to_ascii_lowercase();
    if lower.contains("embed") {
        "embedding"
    } else if ["tts", "speech", "audio", "voice"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        "tts"
    } else if [
        "image",
        "imagen",
        "dall-e",
        "dalle",
        "flux",
        "sdxl",
        "sd-",
        "stable-diffusion",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        "image"
    } else {
        LLM_KIND
    }
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            deduped.push(value);
        }
    }
    deduped
}

fn model_card(id: String, owned_by: String, created: u64, kind: Option<String>) -> ModelCard {
    let root = id.split('/').next_back().unwrap_or(&id).to_string();
    ModelCard {
        id,
        object: "model",
        created,
        owned_by,
        permission: Vec::new(),
        root,
        parent: None,
        kind,
    }
}

fn with_cors_json(status: StatusCode, payload: Value) -> Response {
    with_cors_response((status, Json(payload)).into_response())
}

fn with_cors_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, OPTIONS"),
    );
    response
}

fn cors_preflight_response(methods: &str) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_str(methods).unwrap_or(HeaderValue::from_static("GET, OPTIONS")),
    );
    response
}

#[derive(Debug, Serialize)]
struct ModelListResponse {
    object: &'static str,
    data: Vec<ModelCard>,
}

#[derive(Debug, Serialize)]
struct ModelCard {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: String,
    permission: Vec<Value>,
    root: String,
    parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
}
