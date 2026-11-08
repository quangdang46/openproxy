use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};

use crate::core::combo::capabilities::{
    aggregate_combo_capabilities, capabilities_json, combo_seat_limits, get_capabilities_for_model,
};
use crate::core::model::catalog::provider_catalog;
use crate::core::model::resolve_provider_alias;
use crate::server::state::AppState;
use crate::types::{AppDb, ModelAliasTarget, ProviderConnection};

use super::provider_models::INTERNAL_MODELS_FETCH_HEADER;

const LLM_KIND: &str = "llm";

/// Provider ids whose 9router registry entry is `category: "free"` with
/// top-level `noAuth: true` — the set `/v1/models` synthesizes a
/// `{id:"noauth"}` connection for (route.js:370-379). See the call site for
/// why the broader noAuth registry entries are excluded.
const NO_AUTH_FREE_PROVIDERS: &[&str] = &["opencode", "mimo-free", "devin-cli"];

/// The `/v1/models` counterpart of chat's `virtual_no_auth_connection`
/// (chat.rs): upstream's pseudo-connection carries only
/// `{id:"noauth", provider, isActive:true}` — no credentials, no
/// providerSpecificData — so the static catalog is what gets listed.
fn no_auth_models_connection(provider: &str) -> ProviderConnection {
    let mut connection = ProviderConnection::default();
    connection.id = "noauth".to_string();
    connection.provider = provider.to_string();
    connection.is_active = Some(true);
    connection
}

/// Router path a model of this kind is served on
/// (9router `KIND_ENDPOINT`, models/info/route.js:5-14). Unknown kinds report
/// `null`, which is what `KIND_ENDPOINT[kind] || null` yields in JS.
fn kind_endpoint(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "llm" | "imageToText" => "/v1/chat/completions",
        "image" => "/v1/images/generations",
        "tts" => "/v1/audio/speech",
        "stt" => "/v1/audio/transcriptions",
        "embedding" => "/v1/embeddings",
        "webSearch" => "/v1/search",
        "webFetch" => "/v1/fetch",
        _ => return None,
    })
}

static UPSTREAM_CONNECTION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[-_][0-9a-f]{8,}$").expect("valid upstream connection regex"));

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(list_default_models).options(cors_options))
        .route(
            "/v1/models/{kind}",
            get(list_models_by_kind).options(cors_options),
        )
        .route(
            "/v1/models/{provider}/{model}",
            get(get_model_by_id).options(cors_options),
        )
        .route("/v1/models/info", get(models_info).options(cors_options))
}

pub async fn cors_options() -> Response {
    cors_preflight_response("GET, OPTIONS")
}

/// True when the caller is a sibling proxy pulling our catalog through its own
/// `fetch_compatible_model_ids`. Fanning out to our compatible providers from
/// there would bounce the request between the two instances until one times
/// out. 9router `skipDynamicFetch` (route.js:388, :574).
fn skip_dynamic_fetch(headers: &HeaderMap) -> bool {
    headers
        .get(INTERNAL_MODELS_FETCH_HEADER)
        .is_some_and(|value| value == "1")
}

pub async fn list_default_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let skip = skip_dynamic_fetch(&headers);
    list_models_for_kinds(state, headers, &[LLM_KIND], skip).await
}

pub async fn list_models_by_kind(
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
        // 9router's `KIND_SLUG_MAP[identifier]` is simply undefined for a
        // single segment that is not a kind slug, so the request falls through
        // to the id lookup — `GET /v1/models/gpt-4o` is a model, not a typo.
        _ => return model_lookup_response(&state, &headers, &kind).await,
    };

    list_models_for_kinds(state, headers, &kind_filter, false).await
}

/// GET /v1/models/{provider}/{model} — single-model lookup.
///
/// Answers from the same LLM catalog `/v1/models` exposes and returns the card
/// bare rather than in a `{object,data}` envelope, so OpenAI clients read
/// `id`/`owned_by` off the top level (9router `[...model]/route.js:52-77`).
pub async fn get_model_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((provider, model)): Path<(String, String)>,
) -> Response {
    model_lookup_response(&state, &headers, &format!("{provider}/{model}")).await
}

async fn model_lookup_response(
    state: &AppState,
    headers: &HeaderMap,
    identifier: &str,
) -> Response {
    let snapshot = state.db.snapshot();
    let models = filter_models_for_key(
        &snapshot,
        headers,
        build_models_list(state, &snapshot, &[LLM_KIND], false).await,
    );

    match models.into_iter().find(|card| card.id == identifier) {
        Some(card) => with_cors_response(Json(card).into_response()),
        None => with_cors_json(
            StatusCode::NOT_FOUND,
            json!({
                "error": {
                    "message": format!(
                        "The model '{identifier}' does not exist or you do not have access to it."
                    ),
                    "type": "invalid_request_error",
                    "code": "model_not_found"
                }
            }),
        ),
    }
}

async fn list_models_for_kinds(
    state: AppState,
    headers: HeaderMap,
    kind_filter: &[&str],
    skip_dynamic_fetch: bool,
) -> Response {
    let snapshot = state.db.snapshot();
    let data = filter_models_for_key(
        &snapshot,
        &headers,
        build_models_list(&state, &snapshot, kind_filter, skip_dynamic_fetch).await,
    );

    with_cors_response(
        Json(ModelListResponse {
            object: "list",
            data,
        })
        .into_response(),
    )
}

/// `/v1/models`: a restricted key sees only the combos and models it may
/// call (9router `filterModelsListForKey`, models/route.js:667). Unrestricted
/// requests — no key, unknown key, or unrestricted key — get the full list.
fn filter_models_for_key(
    snapshot: &AppDb,
    headers: &HeaderMap,
    mut data: Vec<ModelCard>,
) -> Vec<ModelCard> {
    let ctx = crate::core::key_access::context_for_presented_key(
        crate::server::auth::extract_api_key(headers).as_deref(),
        snapshot,
    );
    let Some(ctx) = ctx else {
        return data;
    };
    let sets = crate::core::key_access::allow_sets(&ctx, snapshot);
    data.retain(|card| {
        crate::core::key_access::model_card_allowed(
            &sets,
            &card.id,
            &card.owned_by,
            card.kind.as_deref(),
            snapshot,
        )
    });
    data
}

async fn build_models_list(
    state: &AppState,
    snapshot: &AppDb,
    kind_filter: &[&str],
    skip_dynamic_fetch: bool,
) -> Vec<ModelCard> {
    let catalog = provider_catalog();
    let alias_to_provider_id = catalog.alias_to_provider_id();
    // Same store the Providers page and ModelSelectModal read via
    // /api/models/disabled — a model the operator switched off must not keep
    // being advertised here, or the dashboard and the client picker disagree.
    let disabled = super::models_disabled::disabled_models_from_db(snapshot);
    let is_disabled = |alias: &str, model_id: &str| {
        disabled
            .get(alias)
            .is_some_and(|ids| ids.iter().any(|id| id == model_id))
    };
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

    // Lookup map so aggregate_combo_capabilities / combo_seat_limits can
    // recursively resolve nested combos (bare-name seats) — upstream
    // `comboByName` (models/route.js:386-388).
    let combo_lookup: HashMap<String, Vec<String>> = snapshot
        .combos
        .iter()
        .map(|combo| (combo.name.clone(), combo.models.clone()))
        .collect();

    for combo in &snapshot.combos {
        // A combo switched off with `openproxy combo disable` is no longer
        // dispatchable (get_combo_models_from_data requires is_active), so
        // advertising it in GET /v1/models would offer clients a model that
        // 400s on use. The Combos page filters the same way.
        if !combo.is_active() {
            continue;
        }
        if !combo_matches_kinds(combo.kind.as_deref(), kind_filter) {
            continue;
        }

        let kind = match combo.kind.as_deref() {
            Some("webSearch") => Some("webSearch".to_string()),
            Some("webFetch") => Some("webFetch".to_string()),
            _ => None,
        };

        let mut card = model_card(
            combo.name.clone(),
            "combo".to_string(),
            created,
            kind.clone(),
            None,
            None,
        );
        // Non-web combos publish the aggregated capability envelope plus the
        // token limits every seat can promise — upstream models/route.js:399-410.
        // Web combos have no model seats, so they get neither.
        if kind.is_none() {
            if let Some(caps) = aggregate_combo_capabilities(&combo.models, Some(&combo_lookup)) {
                card.capabilities = Some(capabilities_json(&caps));
            }
            let (context_window, max_output) = combo_seat_limits(&combo.models, &combo_lookup);
            card.context_length = context_window.and_then(|v| u32::try_from(v).ok());
            card.max_completion_tokens = max_output.and_then(|v| u32::try_from(v).ok());
        }
        models.push(card);
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
                if is_disabled(&provider_entry.alias, &model.id) {
                    continue;
                }

                let mut card = model_card(
                    format!("{}/{}", provider_entry.alias, model.id),
                    provider_entry.alias.clone(),
                    created,
                    None,
                    model.context_window,
                    None,
                );
                apply_model_capabilities(&mut card, provider_id, &model.id, model.kind == LLM_KIND);
                models.push(card);
            }
        }

        if kind_filter.contains(&LLM_KIND) {
            for custom_model in snapshot.custom_models.iter().filter(|model| {
                model.r#type.is_empty() || model.r#type == LLM_KIND || model.r#type == "chat"
            }) {
                let model_id = custom_model.id.trim();
                let provider_alias = custom_model.provider_alias.trim();
                if model_id.is_empty() || provider_alias.is_empty() {
                    continue;
                }

                let mut card = model_card(
                    format!("{provider_alias}/{model_id}"),
                    provider_alias.to_string(),
                    created,
                    None,
                    None,
                    None,
                );
                apply_model_capabilities(&mut card, provider_alias, model_id, true);
                models.push(card);
            }
        }
    } else {
        // 9router route.js:370-379 injects `{id:"noauth", provider, isActive:true}`
        // pseudo-connections for FREE_PROVIDERS entries carrying `noAuth`, so
        // keyless free providers still advertise their catalog when no stored
        // connection exists. Upstream's effective set is the registry entries
        // with `category: "free"` AND top-level `noAuth: true`
        // (open-sse/providers/registry/{opencode,mimo-free,devin-cli}.js) —
        // the freeTier/apikey noAuth providers (edge-tts, coqui, mmf, …) are
        // NOT in FREE_PROVIDERS and get no pseudo-connection upstream.
        let no_auth_connections: Vec<ProviderConnection> = NO_AUTH_FREE_PROVIDERS
            .iter()
            .filter(|provider_id| {
                !active_connections
                    .iter()
                    .any(|connection| connection.provider == **provider_id)
            })
            .map(|provider_id| no_auth_models_connection(provider_id))
            .collect();

        for connection in active_connection_by_provider
            .iter()
            .copied()
            .chain(no_auth_connections.iter())
        {
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

            // `ollama-local` is the one provider whose static catalog is a
            // *partial* advertisement: the registry only names the Phonon
            // TTS tags, embedding families and the `clef` systemone models,
            // while the chat models live on the daemon (`/api/tags`). Upstream
            // lists the static set only; OpenProxy discovers the daemon too,
            // so for this provider the discovered ids are merged into the
            // static ones instead of only filling an empty list.
            let discover = !skip_dynamic_fetch
                && !UPSTREAM_CONNECTION_RE.is_match(provider_id)
                && super::provider_models::supports_models_discovery(provider_id)
                && (raw_model_ids.is_empty()
                    || (provider_id == "ollama-local" && !had_enabled_models));
            if discover {
                let discovered =
                    super::provider_models::fetch_discovered_model_ids(state, connection).await;
                if raw_model_ids.is_empty() {
                    raw_model_ids = discovered;
                } else {
                    raw_model_ids.extend(discovered);
                }
            }

            let prefixes = [output_alias.as_str(), static_alias, provider_id];
            let model_ids = raw_model_ids
                .into_iter()
                .filter_map(|model_id| strip_provider_prefix(&model_id, &prefixes))
                .collect::<Vec<_>>();

            let custom_model_ids = snapshot
                .custom_models
                .iter()
                .filter(|model| {
                    model.r#type.is_empty() || model.r#type == LLM_KIND || model.r#type == "chat"
                })
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
                    .chain(custom_model_ids)
                    .chain(alias_model_ids)
                    .collect(),
            );

            for model_id in merged_model_ids {
                let kind = static_model_kind_by_id
                    .get(model_id.as_str())
                    .copied()
                    .unwrap_or_else(|| infer_kind_from_unknown_model_id(&model_id));
                if !kind_filter.contains(&kind) {
                    continue;
                }
                // A connection carrying a `prefix` is listed under the output
                // alias, but the operator toggles the model on the Providers
                // page under the static alias — both must hide it.
                if is_disabled(&output_alias, &model_id) || is_disabled(static_alias, &model_id) {
                    continue;
                }

                // Look up context_window from catalog for this model.
                let ctx_len = catalog
                    .find_model(provider_id, &model_id)
                    .and_then(|m| m.context_window);
                let mut card = model_card(
                    format!("{output_alias}/{model_id}"),
                    output_alias.clone(),
                    created,
                    None,
                    ctx_len,
                    None,
                );
                apply_model_capabilities(&mut card, provider_id, &model_id, kind == LLM_KIND);
                models.push(card);
            }

            if let Some(provider_info) = catalog.provider_info(provider_id) {
                if kind_filter.contains(&"tts") {
                    for model_id in &provider_info.tts_models {
                        models.push(model_card(
                            format!("{output_alias}/{model_id}"),
                            output_alias.clone(),
                            created,
                            None,
                            None,
                            None,
                        ));
                    }
                }

                if kind_filter.contains(&"embedding") {
                    for model_id in &provider_info.embedding_models {
                        models.push(model_card(
                            format!("{output_alias}/{model_id}"),
                            output_alias.clone(),
                            created,
                            None,
                            None,
                            None,
                        ));
                    }
                }

                if kind_filter.contains(&"webSearch") && provider_info.has_search {
                    models.push(model_card(
                        format!("{output_alias}/search"),
                        output_alias.clone(),
                        created,
                        Some("webSearch".to_string()),
                        None,
                        None,
                    ));
                }

                if kind_filter.contains(&"webFetch") && provider_info.has_fetch {
                    models.push(model_card(
                        format!("{output_alias}/fetch"),
                        output_alias.clone(),
                        created,
                        Some("webFetch".to_string()),
                        None,
                        None,
                    ));
                }
            }
        }
    }

    // Also include custom models whose provider_alias doesn't match any connection
    // (e.g., custom models registered on provider nodes rather than connections)
    if kind_filter.contains(&LLM_KIND) {
        for custom_model in &snapshot.custom_models {
            if !custom_model.r#type.is_empty()
                && custom_model.r#type != LLM_KIND
                && custom_model.r#type != "chat"
            {
                continue;
            }
            let model_id = custom_model.id.trim();
            if model_id.is_empty() {
                continue;
            }
            let provider_alias = custom_model.provider_alias.trim();
            if provider_alias.is_empty() {
                continue;
            }
            let mut card = model_card(
                format!("{provider_alias}/{model_id}"),
                provider_alias.to_string(),
                created,
                None,
                None,
                None,
            );
            apply_model_capabilities(&mut card, provider_alias, model_id, true);
            models.push(card);
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
        return kind_filter.contains(&LLM_KIND);
    }

    kind_filter
        .iter()
        .any(|candidate| service_kinds.iter().any(|kind| kind == candidate))
}

fn combo_matches_kinds(kind: Option<&str>, kind_filter: &[&str]) -> bool {
    let combo_kind = kind.unwrap_or(LLM_KIND);
    kind_filter.contains(&combo_kind)
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

/// Capability metadata + token limits for one provider/custom model card —
/// upstream models/route.js per-provider path: `capabilities` carries the
/// resolved table entry and `context_length` / `max_completion_tokens` are
/// published under the snake_case names OpenAI/OpenRouter clients match on.
/// `provider` may be an alias; it is resolved alias → provider id exactly
/// like `comboSeatCapabilities`. Limits are only emitted for LLM-kind cards
/// (upstream `kind === LLM_KIND || allowAsLlm` gate).
fn apply_model_capabilities(card: &mut ModelCard, provider: &str, model_id: &str, is_llm: bool) {
    let caps = get_capabilities_for_model(&resolve_provider_alias(provider), model_id);
    card.capabilities = Some(capabilities_json(&caps));
    if is_llm {
        // Upstream publishes caps.contextWindow / caps.maxOutput verbatim —
        // the capability table (hand rows + synced catalog overlay + provider
        // overrides) is authoritative here, so it wins over the static
        // catalog context_window when both exist.
        card.context_length = u32::try_from(caps.context_window)
            .ok()
            .or(card.context_length);
        card.max_completion_tokens = u32::try_from(caps.max_output).ok();
    }
}

fn model_card(
    id: String,
    owned_by: String,
    created: u64,
    kind: Option<String>,
    context_length: Option<u32>,
    max_completion_tokens: Option<u32>,
) -> ModelCard {
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
        capabilities: None,
        context_length,
        max_completion_tokens,
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
    /// Resolved capability envelope (9router `capabilities.js` shape —
    /// vision/pdf/audioInput/…/contextWindow/maxOutput), emitted on combo
    /// and LLM model cards (upstream models/route.js `entry.capabilities`).
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<Value>,
    /// Maximum context window size in tokens (9router v0.5.55 parity).
    #[serde(skip_serializing_if = "Option::is_none")]
    context_length: Option<u32>,
    /// Maximum completion/output tokens (9router v0.5.55 parity).
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
}

/// Does the router actually know this model id?
///
/// 9router's `lookup()` (models/info/route.js:45-77) answers from the static
/// catalog alone, so an unknown id 404s instead of being handed the provider
/// `get_model_info` guesses from the model name. OpenProxy also knows about
/// combos, custom models and connection-pinned models, so those count as known
/// too — otherwise the 404 would reject ids `/v1/models` advertises.
fn model_is_known(
    snapshot: &AppDb,
    catalog: &crate::core::model::catalog::ProviderCatalog,
    model_str: &str,
    resolved: &crate::core::model::ResolvedModel,
    requested_kind: Option<&str>,
) -> bool {
    if matches!(
        resolved.route_kind,
        crate::core::model::ModelRouteKind::Combo
    ) {
        return snapshot
            .combos
            .iter()
            .any(|combo| combo.name == resolved.model);
    }

    // Catalog models are addressed as `{alias}/{modelId}`; route.js:46 rejects
    // anything without a slash.
    let Some((alias, model_id)) = model_str.split_once('/').filter(|(_, id)| !id.is_empty()) else {
        return false;
    };
    let provider_id = resolved.provider.as_deref().unwrap_or(alias);

    if let Some(provider_info) = catalog.provider_info(provider_id) {
        if catalog
            .find_model(provider_id, model_id)
            .is_some_and(|model| requested_kind.is_none_or(|kind| kind == model.kind))
        {
            return true;
        }
        // `search` / `fetch` are virtual ids that /v1/models/web advertises.
        if (model_id == "search" && provider_info.has_search)
            || (model_id == "fetch" && provider_info.has_fetch)
        {
            return true;
        }
    }

    if snapshot
        .custom_models
        .iter()
        .any(|custom| custom.provider_alias.trim() == alias && custom.id.trim() == model_id)
    {
        return true;
    }

    snapshot.provider_connections.iter().any(|connection| {
        if !connection.is_active() {
            return false;
        }
        let provider = connection.provider.as_str();
        let static_alias = catalog
            .static_alias_for_provider(provider)
            .unwrap_or(provider);
        let output = output_alias(catalog, connection, provider, static_alias);
        if alias != output && alias != static_alias && alias != provider {
            return false;
        }
        // Only an explicit enabledModels list proves this id exists; a
        // connection without one already advertises the catalog, and treating
        // that as "any id is known" would make the 404 arm unreachable.
        let (ids, had_enabled) = enabled_model_ids(connection);
        had_enabled
            && ids
                .iter()
                .filter_map(|id| {
                    strip_provider_prefix(id, &[output.as_str(), static_alias, provider])
                })
                .any(|id| id == model_id)
    })
}

/// GET /v1/models/info?id={alias}/{model_id}
pub async fn models_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    use crate::core::model::{get_model_info, parse_model};
    // 9router names the param `id`; `model` stays as a fallback so callers
    // written against the older OpenProxy contract keep working.
    let model_str = params
        .get("id")
        .or_else(|| params.get("model"))
        .map(String::as_str)
        .unwrap_or("");
    if model_str.is_empty() {
        return with_cors_json(
            StatusCode::BAD_REQUEST,
            json!({
                "error": {
                    "message": "Missing required query param: id (e.g. ?id=openai/dall-e-3)",
                    "type": "invalid_request_error"
                }
            }),
        );
    }
    let requested_kind = params
        .get("kind")
        .map(String::as_str)
        .filter(|kind| !kind.is_empty());
    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model_str, &snapshot);

    // Look up catalog-level model metadata if available
    let catalog = provider_catalog();
    let (catalog_model, provider_info) = match &resolved.provider {
        Some(provider_id) => {
            let parsed = parse_model(model_str);
            let model_id = parsed.model.as_deref().unwrap_or(&resolved.model);
            let cm = catalog.find_model(provider_id, model_id);
            let pi = catalog.provider_info(provider_id);
            (cm, pi)
        }
        None => (None, None),
    };

    if !model_is_known(&snapshot, catalog, model_str, &resolved, requested_kind) {
        return with_cors_json(
            StatusCode::NOT_FOUND,
            json!({
                "error": {
                    "message": format!("Model not found: {model_str}"),
                    "type": "not_found"
                }
            }),
        );
    }

    let mut info = json!({
        "id": model_str,
        "provider": resolved.provider,
        "model": resolved.model,
        "routeKind": format!("{:?}", resolved.route_kind),
        // 9router `buildInfo` (models/info/route.js:18-36) always emits
        // `owned_by` (the catalog alias) and `endpoint` (the per-kind path),
        // so a client can route and size a request from this response alone.
        "owned_by": resolved
            .provider
            .clone()
            .and_then(|provider| catalog.static_alias_for_provider(&provider).map(str::to_string))
            .unwrap_or_default(),
    });

    // Merge catalog model fields
    if let Some(cm) = catalog_model {
        if let Some(v) = &cm.name {
            info["name"] = json!(v);
        }
        if let Some(v) = &cm.quota_family {
            info["quotaFamily"] = json!(v);
        }
        if let Some(v) = &cm.strip {
            info["strip"] = json!(v);
        }
        if let Some(v) = &cm.target_format {
            info["targetFormat"] = json!(v);
        }
        if let Some(v) = &cm.upstream_model_id {
            info["upstreamModelId"] = json!(v);
        }
        if let Some(v) = cm.context_window {
            info["contextWindow"] = json!(v);
        }
        if let Some(v) = cm.dimensions {
            info["dimensions"] = json!(v);
        }
        if let Some(v) = &cm.capabilities {
            info["capabilities"] = json!(v);
        }
        info["kind"] = json!(cm.kind);
        info["endpoint"] = json!(kind_endpoint(&cm.kind));
    }

    // Merge provider-level fields
    if let Some(pi) = provider_info {
        if let Some(v) = pi.vision {
            info["vision"] = json!(v);
        }
        if let Some(v) = pi.reasoning {
            info["reasoning"] = json!(v);
        }
        if let Some(v) = pi.context_window {
            info["providerContextWindow"] = json!(v);
        }
        if let Some(v) = pi.max_output {
            info["providerMaxOutput"] = json!(v);
        }
        if let Some(v) = pi.tools {
            info["tools"] = json!(v);
        }
    }

    with_cors_response(Json(info).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::AppState;
    use crate::types::{CustomModel, ProviderConnection};
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    async fn test_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::load_from(dir.path()).await.unwrap();
        AppState::new(Arc::new(db))
    }

    #[tokio::test]
    async fn custom_models_appear_without_connections() {
        // Issue #156 regression: custom models must appear in /v1/models
        // even when there are no provider connections.
        let snapshot = AppDb {
            custom_models: vec![CustomModel {
                provider_alias: "tr".into(),
                id: "MiniMax-M3".into(),
                r#type: String::new(),
                name: None,
                extra: BTreeMap::new(),
            }],
            ..Default::default()
        };

        let models = build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
        assert!(models.iter().any(|m| m.id == "tr/MiniMax-M3"),
            "custom model with provider_alias 'tr' should appear in /v1/models even without connections");
    }

    #[tokio::test]
    async fn custom_models_appear_with_unrelated_connections() {
        // Issue #156 regression: custom models with a provider_alias that
        // doesn't match any connection should still appear in /v1/models.
        let snapshot = AppDb {
            provider_connections: vec![ProviderConnection {
                id: "conn-opencode".into(),
                provider: "opencode-go".into(),
                auth_type: "apikey".into(),
                provider_specific_data: BTreeMap::from([
                    ("prefix".into(), json!("oc")),
                    ("enabledModels".into(), json!(["gpt-4o", "claude-3"])),
                ]),
                ..Default::default()
            }],
            custom_models: vec![CustomModel {
                provider_alias: "tr".into(),
                id: "MiniMax-M3".into(),
                r#type: String::new(),
                name: None,
                extra: BTreeMap::new(),
            }],
            ..Default::default()
        };

        let models = build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
        assert!(models.iter().any(|m| m.id == "tr/MiniMax-M3"),
            "custom model 'tr/MiniMax-M3' should appear even when it doesn't match any connection's prefix");
    }

    #[tokio::test]
    async fn no_duplicate_custom_models() {
        // When a custom model's provider_alias matches a connection's output_alias,
        // it should only appear once (deduped).
        let snapshot = AppDb {
            provider_connections: vec![ProviderConnection {
                id: "conn-opencode".into(),
                provider: "opencode-go".into(),
                auth_type: "apikey".into(),
                provider_specific_data: BTreeMap::from([
                    ("prefix".into(), json!("ocg")),
                    ("enabledModels".into(), json!(["gpt-4o"])),
                ]),
                ..Default::default()
            }],
            custom_models: vec![CustomModel {
                provider_alias: "ocg".into(),
                id: "gpt-4o".into(),
                r#type: String::new(),
                name: None,
                extra: BTreeMap::new(),
            }],
            ..Default::default()
        };

        let models = build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
        let count = models.iter().filter(|m| m.id == "ocg/gpt-4o").count();
        assert_eq!(count, 1,
            "custom model 'ocg/gpt-4o' should appear exactly once even if matched by connection AND fallback");
    }

    #[tokio::test]
    async fn custom_models_without_llm_kind_filtered() {
        // Custom models with type "image" should not appear in LLM kind filter.
        let snapshot = AppDb {
            custom_models: vec![
                CustomModel {
                    provider_alias: "tr".into(),
                    id: "MiniMax-M3".into(),
                    r#type: "llm".into(),
                    name: None,
                    extra: BTreeMap::new(),
                },
                CustomModel {
                    provider_alias: "tr".into(),
                    id: "flux-pro".into(),
                    r#type: "image".into(),
                    name: None,
                    extra: BTreeMap::new(),
                },
            ],
            ..Default::default()
        };

        let models = build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
        assert!(
            models.iter().any(|m| m.id == "tr/MiniMax-M3"),
            "llm-type custom model should appear in LLM list"
        );
        assert!(
            !models.iter().any(|m| m.id == "tr/flux-pro"),
            "image-type custom model should NOT appear in LLM list"
        );
    }

    #[tokio::test]
    async fn opencode_zen_models_appear_in_models_list() {
        // Regression: opencode-zen was registered in the executor/CLI/alias
        // maps but its provider entry + models were missing from the static
        // catalog, so /v1/models exposed nothing for it.
        let snapshot = AppDb {
            provider_connections: vec![ProviderConnection {
                id: "conn-zen".into(),
                provider: "opencode-zen".into(),
                auth_type: "apikey".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let models = build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
        assert!(
            models.iter().any(|m| m.id == "opencode-zen/gpt-5.4"),
            "catalog-registered opencode-zen model should appear in /v1/models"
        );
        assert!(
            models.iter().any(|m| m.id == "opencode-zen/kimi-k2.6"),
            "catalog-registered opencode-zen model should appear in /v1/models"
        );
    }

    #[tokio::test]
    async fn catalog_less_builtin_provider_falls_back_to_dynamic_discovery() {
        // Generic path: a built-in provider with NO static catalog models but
        // with a server-side discovery fetcher must still surface its models
        // through /v1/models (same discovery the dashboard uses).
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/tags"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "models": [
                    { "id": "local-llama-3.1" },
                    { "id": "local-qwen-2.5" }
                ]
            })))
            .mount(&server)
            .await;

        let snapshot = AppDb {
            provider_connections: vec![ProviderConnection {
                id: "conn-ollama-local".into(),
                provider: "ollama-local".into(),
                auth_type: "apikey".into(),
                provider_specific_data: BTreeMap::from([("baseUrl".into(), json!(server.uri()))]),
                ..Default::default()
            }],
            ..Default::default()
        };

        let models = build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
        assert!(
            models.iter().any(|m| m.id == "ollama-local/local-llama-3.1"),
            "dynamically discovered models of a catalog-less built-in provider should appear in /v1/models"
        );
        assert!(
            models.iter().any(|m| m.id == "ollama-local/local-qwen-2.5"),
            "dynamically discovered models of a catalog-less built-in provider should appear in /v1/models"
        );
    }

    fn disabled_snapshot(alias: &str, ids: &[&str]) -> AppDb {
        AppDb {
            extra: BTreeMap::from([("disabledModels".into(), json!({ alias: ids }))]),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn disabled_catalog_models_are_hidden_from_the_static_branch() {
        // The Providers page toggle is the operator's kill-switch for a model;
        // /v1/models must not keep advertising it to client model pickers.
        let snapshot = disabled_snapshot("openai", &["gpt-4o-mini"]);

        let models = build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
        assert!(
            !models.iter().any(|m| m.id == "openai/gpt-4o-mini"),
            "a model disabled on the Providers page must not appear in /v1/models"
        );
        assert!(
            models.iter().any(|m| m.id.starts_with("openai/")),
            "disabling one model must not empty the rest of that provider's list"
        );
    }

    #[tokio::test]
    async fn disabled_connection_models_are_hidden_under_either_alias() {
        // A prefixed connection is listed under the output alias, but the
        // operator toggles the model under the static alias on the Providers
        // page — hiding it must work for both.
        let connection = ProviderConnection {
            id: "conn-openai".into(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            provider_specific_data: BTreeMap::from([
                ("prefix".into(), json!("oa")),
                ("enabledModels".into(), json!(["gpt-4o", "gpt-4o-mini"])),
            ]),
            ..Default::default()
        };

        for (label, alias) in [("output alias", "oa"), ("static alias", "openai")] {
            let snapshot = AppDb {
                provider_connections: vec![connection.clone()],
                ..disabled_snapshot(alias, &["gpt-4o-mini"])
            };

            let models =
                build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
            assert!(
                !models.iter().any(|m| m.id == "oa/gpt-4o-mini"),
                "model disabled under the {label} must not appear in /v1/models"
            );
            assert!(
                models.iter().any(|m| m.id == "oa/gpt-4o"),
                "an enabled sibling must survive a {label} disable"
            );
        }
    }

    #[tokio::test]
    async fn disabled_models_are_hidden_from_every_kind_route() {
        // /v1/models/{kind} funnels through the same builder, so a disable
        // must bite for the non-LLM kinds too.
        let state = test_state().await;
        let snapshot = disabled_snapshot(
            "openai",
            &["text-embedding-3-small", "tts-1", "dall-e-3", "whisper-1"],
        );

        for (kind_filter, disabled_id) in [
            (vec!["embedding"], "openai/text-embedding-3-small"),
            (vec!["tts"], "openai/tts-1"),
            (vec!["image"], "openai/dall-e-3"),
            (vec!["stt"], "openai/whisper-1"),
        ] {
            let models = build_models_list(&state, &snapshot, &kind_filter, false).await;
            assert!(
                !models.iter().any(|m| m.id == disabled_id),
                "{disabled_id} disabled on the Providers page must not appear in /v1/models/{kind_filter:?}"
            );
        }
    }

    #[tokio::test]
    async fn custom_models_are_not_subject_to_the_disabled_filter() {
        // 9router's two isDisabled call sites cover the catalog and the
        // per-connection list only — custom models stay visible.
        let snapshot = AppDb {
            custom_models: vec![CustomModel {
                provider_alias: "tr".into(),
                id: "MiniMax-M3".into(),
                r#type: String::new(),
                name: None,
                extra: BTreeMap::new(),
            }],
            ..disabled_snapshot("tr", &["MiniMax-M3"])
        };

        let models = build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
        assert!(
            models.iter().any(|m| m.id == "tr/MiniMax-M3"),
            "9router does not filter custom models; matching that quirk"
        );
    }

    async fn models_info_request(state: &AppState, uri: &str) -> (StatusCode, serde_json::Value) {
        let app = routes().with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, serde_json::from_slice(&bytes).expect("json body"))
    }

    async fn openai_info_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::load_from(dir.path()).await.unwrap();
        let db = Arc::new(db);
        db.update(|state| {
            state.provider_connections = vec![ProviderConnection {
                id: "conn-openai".into(),
                provider: "openai".into(),
                auth_type: "apikey".into(),
                ..Default::default()
            }];
        })
        .await
        .unwrap();
        AppState::new(db)
    }

    /// A gemini connection, so the embedding branch of `models_info` has a
    /// catalog entry to read `dimensions` from.
    async fn gemini_info_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::load_from(dir.path()).await.unwrap();
        let db = Arc::new(db);
        db.update(|state| {
            state.provider_connections = vec![ProviderConnection {
                id: "conn-gemini".into(),
                provider: "gemini".into(),
                auth_type: "apikey".into(),
                ..Default::default()
            }];
        })
        .await
        .unwrap();
        AppState::new(db)
    }

    #[tokio::test]
    async fn models_info_accepts_the_9router_id_param_and_returns_a_bare_object() {
        let state = openai_info_state().await;
        let (status, body) = models_info_request(&state, "/v1/models/info?id=openai/gpt-4.1").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], json!("openai/gpt-4.1"));
        assert!(
            body.get("data").is_none() && body.get("object").is_none(),
            "9router returns the info object bare, not wrapped in {{object,data}}: {body}"
        );
    }

    #[tokio::test]
    async fn models_info_404s_on_an_unknown_id() {
        let state = openai_info_state().await;
        let (status, body) =
            models_info_request(&state, "/v1/models/info?id=openai/definitely-not-real").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["type"], json!("not_found"));
        assert_eq!(
            body["error"]["message"],
            json!("Model not found: openai/definitely-not-real")
        );
    }

    #[tokio::test]
    async fn models_info_400s_without_an_id() {
        let state = openai_info_state().await;
        let (status, body) = models_info_request(&state, "/v1/models/info").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["message"],
            json!("Missing required query param: id (e.g. ?id=openai/dall-e-3)")
        );
    }

    #[tokio::test]
    async fn models_info_still_accepts_the_model_param_as_a_fallback() {
        let state = openai_info_state().await;
        let (status, body) =
            models_info_request(&state, "/v1/models/info?model=openai/gpt-4.1").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], json!("openai/gpt-4.1"));
    }

    /// The kind→endpoint table 9router's `KIND_ENDPOINT` carries
    /// (models/info/route.js:5-14).
    #[test]
    fn kind_endpoint_matches_9router() {
        assert_eq!(kind_endpoint("embedding"), Some("/v1/embeddings"));
        assert_eq!(kind_endpoint("llm"), Some("/v1/chat/completions"));
        assert_eq!(kind_endpoint("tts"), Some("/v1/audio/speech"));
        assert_eq!(kind_endpoint("stt"), Some("/v1/audio/transcriptions"));
        assert_eq!(kind_endpoint("image"), Some("/v1/images/generations"));
        assert_eq!(kind_endpoint("imageToText"), Some("/v1/chat/completions"));
        assert_eq!(kind_endpoint("webSearch"), Some("/v1/search"));
        assert_eq!(kind_endpoint("webFetch"), Some("/v1/fetch"));
        assert_eq!(kind_endpoint("something-else"), None);
    }

    /// 9router's `buildInfo` always emits `owned_by` and `endpoint`; without
    /// them a client cannot size a vector index or route a call from this
    /// response alone.
    #[tokio::test]
    async fn models_info_publishes_owned_by_and_endpoint() {
        let state = openai_info_state().await;
        let (status, body) = models_info_request(&state, "/v1/models/info?id=openai/gpt-4.1").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["owned_by"], json!("openai"));
        assert_eq!(body["endpoint"], json!("/v1/chat/completions"));
    }

    #[tokio::test]
    async fn models_info_publishes_embedding_dimensions() {
        let state = gemini_info_state().await;
        let (status, body) =
            models_info_request(&state, "/v1/models/info?id=gemini/gemini-embedding-001").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["kind"], json!("embedding"));
        assert_eq!(body["endpoint"], json!("/v1/embeddings"));
        assert_eq!(body["owned_by"], json!("gemini"));
        assert_eq!(
            body["dimensions"], 768,
            "9router declares dimensions: 768 on gemini embedding-001"
        );
    }

    /// 9router models/route.js:399-410 — a combo card carries the aggregated
    /// `capabilities` envelope plus the `context_length` /
    /// `max_completion_tokens` every seat can promise (min across seats).
    #[tokio::test]
    async fn combo_card_publishes_aggregated_capabilities_and_limits() {
        let snapshot = AppDb {
            combos: vec![crate::types::Combo {
                id: "combo-1".into(),
                name: "mycombo".into(),
                models: vec![
                    "openai/gpt-4o".to_string(),             // 128k ctx / 16k out, vision
                    "anthropic/claude-opus-4.8".to_string(), // 1M ctx / 128k out
                ],
                ..Default::default()
            }],
            ..Default::default()
        };

        let models = build_models_list(&test_state().await, &snapshot, &[LLM_KIND], false).await;
        let card = models
            .iter()
            .find(|m| m.id == "mycombo")
            .expect("combo should be listed");

        let caps = card.capabilities.as_ref().expect("capabilities present");
        assert_eq!(caps["contextWindow"], json!(128_000)); // min across seats
        assert_eq!(caps["maxOutput"], json!(128_000)); // max across seats
        assert_eq!(caps["vision"], json!(true)); // union
        assert_eq!(card.context_length, Some(128_000));
        assert_eq!(card.max_completion_tokens, Some(16_384)); // min out
    }

    /// GPT-5.4+ publish the real 1.05M window on /v1/models — the capability
    /// table is authoritative for `context_length` (89ffac5 pattern rows).
    #[tokio::test]
    async fn gpt_5_4_model_card_publishes_1m_context() {
        let models =
            build_models_list(&test_state().await, &AppDb::default(), &[LLM_KIND], false).await;
        let card = models
            .iter()
            .find(|m| m.id == "openai/gpt-5.4")
            .expect("openai/gpt-5.4 should be in the static catalog");

        assert_eq!(card.context_length, Some(1_050_000));
        assert_eq!(card.max_completion_tokens, Some(128_000));
        let caps = card.capabilities.as_ref().expect("capabilities present");
        assert_eq!(caps["contextWindow"], json!(1_050_000));
        assert_eq!(caps["reasoning"], json!(true));

        // The mini tier stays at 400k.
        let mini = models
            .iter()
            .find(|m| m.id == "openai/gpt-5.4-mini")
            .expect("openai/gpt-5.4-mini should be in the static catalog");
        assert_eq!(mini.context_length, Some(400_000));
    }
}
