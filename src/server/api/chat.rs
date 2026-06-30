use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

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

use crate::core::account_fallback::{build_model_lock_update, filter_available_accounts};
use crate::core::chat::RequestPlan;
use crate::core::combo::fusion::handle_fusion_chat;
use crate::core::combo::{
    check_fallback_error, detect_required_capabilities, execute_combo_strategy_with_capacity,
    get_combo_models_from_data, get_disabled_members_for_combo, mark_combo_member_quarantined,
    reorder_by_capabilities, ComboAttemptError, ComboExecutionError, ComboStrategy, FusionConfig,
    ModelCapacity,
};
use crate::core::executor::UpstreamResponse;
use crate::core::model::{get_model_info, ModelRouteKind};
use crate::core::proxy::resolve_proxy_target;
use crate::core::rtk::headroom::{compress_with_headroom, HeadroomConfig};
use crate::core::rtk::{apply_request_preprocessing, compress_messages};
use crate::core::translator::helpers::image_helper::fetch_image_as_base64;
use crate::core::translator::helpers::modality_helper::{
    capabilities_for_format, strip_unsupported_modalities, ModalityCapabilities,
};
use crate::core::translator::registry::{self, Format};
use crate::core::translator::response_transform::{transform_sse_stream, transformer_for_provider};
use crate::core::utils::bypass_handler::{detect_bypass, BypassDecision, DEFAULT_BYPASS_TEXT};
use crate::core::utils::claude_cloaking::{cloak_claude_tools, CloakedRequest};
use crate::core::utils::client_detector::{detect_client_tool, is_native_passthrough, ClientTool};
use crate::core::utils::tool_deduper::dedupe_tools;
use crate::payload_rules::{apply_request_rules, apply_system_prompt};
use crate::server::auth::{extract_api_key, require_api_key};
use crate::server::state::AppState;
use crate::types::{AppDb, ProviderConnection, TokenUsage};

use super::auth_error_response;

/// Maximum time we'll wait for the next byte from an upstream SSE stream before
/// considering the connection stalled. 3 minutes matches what most providers
/// use for their keep-alive heartbeats (OpenAI sends a comment every ~30s,
/// Anthropic every ~60s, Gemini every ~30s — 180s is well past any of them).
const SSE_STALL_TIMEOUT: Duration = Duration::from_secs(180);

/// Maximum number of concurrent in-flight requests per provider account.
///
/// Used both as the per-account slot cap inside
/// [`forward_with_provider_fallback`] and as the round-robin capacity
/// threshold when deciding whether a combo member is `Available` or `Busy`.
const MAX_IN_FLIGHT_PER_ACCOUNT: usize = 10;

pub async fn cors_options() -> Response {
    cors_preflight_response("GET, POST, OPTIONS")
}

pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let model = body
        .as_ref()
        .ok()
        .and_then(|b| b.get("model").and_then(|m| m.as_str()));
    let _log =
        crate::server::request_logger::RequestLog::start("POST", "/v1/chat/completions", model);
    let response = with_cors_response(
        chat_completions_for_endpoint(state, headers, body, Some("/v1/chat/completions")).await,
    );
    _log.finish(response.status().as_u16());
    response
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

    let Json(mut body) = match body {
        Ok(body) => body,
        Err(_) => return json_error_response(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    let Some(model_str) = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return json_error_response(StatusCode::BAD_REQUEST, "Missing model");
    };
    let model_str = model_str.as_str();

    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model_str, &snapshot);

    // Payload-rules + system-prompt override (OmniRoute-style).
    // Applied here, after the model field has been validated but before
    // we fan out into combo / direct dispatch — so both branches see the
    // same transformed body. Wildcard matching uses the user-facing
    // `model` field; the protocol tag is left empty for now (it can be
    // wired in once we surface upstream protocol metadata at this layer).
    apply_system_prompt(&mut body, &snapshot.settings.system_prompt);
    apply_request_rules(&mut body, model_str, None, &snapshot.settings.payload_rules);

    // Convert headers once for client-tool detection shared by both
    // Direct and Combo dispatch paths.
    let headers_map: std::collections::HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    // 9router parity: cache Claude-specific headers from incoming request
    // for replay on subsequent requests (claudeHeaderCache).
    crate::core::utils::claude_header_cache::cache_claude_headers(&headers_map);

    let client_tool = detect_client_tool(&headers_map, &body);

    // 9router parity: Accept-header streaming preference (chatCore.js:87-94).
    // When client explicitly prefers application/json over text/event-stream,
    // force non-streaming so AI SDK / curl-style callers get a synchronous response.
    if let Some(accept_val) = headers.get("accept") {
        if let Ok(accept_str) = accept_val.to_str() {
            let a = accept_str.to_lowercase();
            let wants_json = a.contains("application/json");
            let wants_sse = a.contains("text/event-stream");
            if wants_json && !wants_sse && a != "*/*" {
                body.as_object_mut()
                    .and_then(|obj| obj.insert("stream".to_string(), Value::Bool(false)));
            }
        }
    }

    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    // 9router parity: ccFilterNaming setting — used by bypass handler to
    // intercept Claude Code's isNewTopic / topic-extraction requests before
    // they reach a provider (matches handleChat in 9router).
    let cc_filter_naming = snapshot
        .settings
        .extra
        .get("ccFilterNaming")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match detect_bypass(&body, &user_agent, cc_filter_naming) {
        BypassDecision::Bypass => {
            let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(true);
            return bypass_response(model_str, DEFAULT_BYPASS_TEXT, stream);
        }
        BypassDecision::Naming { title } => {
            let naming_text = serde_json::to_string(&json!({
                "isNewTopic": true,
                "title": title,
            }))
            .unwrap_or_else(|_| String::new());
            let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(true);
            return bypass_response(model_str, &naming_text, stream);
        }
        BypassDecision::Pass => {}
    }
    match resolved.route_kind {
        ModelRouteKind::Combo => {
            let combo_name = resolved.model;
            let Some(combo_models) = get_combo_models_from_data(&combo_name, &snapshot.combos)
            else {
                return json_error_response(StatusCode::BAD_REQUEST, "Unknown combo model");
            };

            // 9router parity: capability auto-switch — reorder models so that
            // vision/pdf-capable providers are tried first when the request
            // contains multimodal blocks. Models without required hard caps
            // remain as last-resort fallback rather than being excluded.
            let required_caps = detect_required_capabilities(&body);
            let combo_models = if !required_caps.is_empty() {
                reorder_by_capabilities(&combo_models, &required_caps)
            } else {
                combo_models
            };

            let disabled_members = get_disabled_members_for_combo(&combo_name, &snapshot.combos);
            let strategy = combo_strategy_for(&snapshot, &combo_name);
            let combo_body = body.clone();
            let combo_state = state.clone();
            let combo_api_key = presented_api_key.clone();
            let capacity_snapshot = snapshot.clone();
            let capacity_registry = state.account_registry.clone();
            let capacity_check = move |combo_model: &str| -> ModelCapacity {
                model_capacity(&capacity_snapshot, &capacity_registry, combo_model)
            };
            // Track every member we attempted so that on a full combo
            // failure (the closure returned `Err` for every member) we
            // can register them in the auto-quarantine map. Anything in
            // this list bubbled up an error, so quarantining them stops
            // the very next request from immediately re-attempting the
            // same broken member and making the CLI agent hang.
            let attempted_members: std::sync::Arc<parking_lot::Mutex<Vec<String>>> =
                std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
            let combo_name_for_quarantine = combo_name.clone();
            let client_tool_for_combo = client_tool;
            let result = if strategy == ComboStrategy::Fusion {
                let combo_data: serde_json::Map<String, serde_json::Value> = snapshot
                    .combos
                    .iter()
                    .find(|c| c.name == combo_name)
                    .map(|c| {
                        c.extra
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();

                let f_state = state.clone();
                let f_body = body.clone();
                let f_api_key = presented_api_key.clone();
                let f_client_tool = client_tool;

                let panel_count = combo_models.len();
                let fusion_result = handle_fusion_chat(
                    &mut body.clone(),
                    &combo_models,
                    &FusionConfig::from_extra(&combo_data, panel_count),
                    None,
                    move |model: String, panel_body: Value| {
                        let state = f_state.clone();
                        let body = f_body.clone();
                        let api_key = f_api_key.clone();
                        let client_tool = f_client_tool;
                        async move {
                            let snapshot = state.db.snapshot();
                            let resolved = get_model_info(&model, &snapshot);
                            let provider = resolved
                                .provider
                                .as_deref()
                                .unwrap_or("unknown")
                                .to_string();
                            let resolved_model = resolved.model.clone();
                            let mut plan =
                                RequestPlan::new(endpoint, &body, &provider, &resolved_model);
                            plan.passthrough = is_native_passthrough(client_tool, &provider);
                            plan.stream = false;
                            let response = execute_single_model(
                                &state,
                                &panel_body,
                                &resolved_model,
                                api_key.as_deref(),
                                endpoint,
                                &plan,
                                client_tool,
                            )
                            .await
                            .map_err(|e| anyhow::anyhow!("Fusion panel failed: {}", e.message))?;
                            let body_bytes =
                                axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
                                    .await
                                    .map_err(|e| {
                                        anyhow::anyhow!("Failed to read panel body: {}", e)
                                    })?;
                            serde_json::from_slice(&body_bytes)
                                .map_err(|e| anyhow::anyhow!("Failed to parse panel body: {}", e))
                        }
                    },
                )
                .await;

                match fusion_result {
                    Ok(value) => {
                        let json_str = serde_json::to_string(&value).unwrap_or_default();
                        Ok(axum::response::Response::new(axum::body::Body::from(
                            json_str,
                        )))
                    }
                    Err(e) => Err(ComboExecutionError {
                        status: e.status,
                        message: e.message,
                        earliest_retry_after: None,
                    }),
                }
            } else {
                let attempted_members = attempted_members.clone();
                execute_combo_strategy_with_capacity(
                    &combo_models,
                    Some(&combo_name),
                    strategy,
                    &disabled_members,
                    capacity_check,
                    move |combo_model| {
                        let state = combo_state.clone();
                        let body = combo_body.clone();
                        let combo_model = combo_model.to_string();
                        let api_key = combo_api_key.clone();
                        attempted_members.lock().push(combo_model.clone());
                        // Re-resolve provider/model for this combo entry so each
                        // iteration dispatches against the correct provider node
                        // (e.g. "custom/gpt-fail" -> provider "node-openai", model "gpt-fail").
                        let inner_snapshot = state.db.snapshot();
                        let combo_resolved = get_model_info(&combo_model, &inner_snapshot);
                        let combo_provider_str = combo_resolved
                            .provider
                            .as_deref()
                            .unwrap_or("unknown")
                            .to_string();
                        let resolved_model = combo_resolved.model.clone();
                        let mut combo_plan =
                            RequestPlan::new(endpoint, &body, &combo_provider_str, &resolved_model);
                        combo_plan.passthrough =
                            is_native_passthrough(client_tool_for_combo, &combo_provider_str);
                        let plan_for_combo = combo_plan.clone();
                        async move {
                            execute_single_model(
                                &state,
                                &body,
                                &resolved_model,
                                api_key.as_deref(),
                                endpoint,
                                &plan_for_combo,
                                client_tool_for_combo,
                            )
                            .await
                        }
                    },
                )
                .await
            };
            match result {
                Ok(response) => response,
                Err(error) => {
                    // Auto-quarantine every combo member we just tried so
                    // the next request doesn't immediately reroll the same
                    // failure. We reuse `check_fallback_error`'s cooldown
                    // so the TTL matches the per-account lock that
                    // `forward_with_provider_fallback` just applied — this
                    // is the "hook / pre-gate" that stops the CLI agent
                    // from appearing to hang on a known-broken combo
                    // member.
                    let cooldown = check_fallback_error(error.status, &error.message, 0).cooldown;
                    let attempted = attempted_members.lock().clone();
                    for member in attempted {
                        mark_combo_member_quarantined(
                            &combo_name_for_quarantine,
                            &member,
                            cooldown,
                        );
                    }
                    combo_error_response(error)
                }
            }
        }
        ModelRouteKind::Direct => {
            let mut plan = RequestPlan::new(
                endpoint,
                &body,
                resolved.provider.as_deref().unwrap_or(model_str),
                &resolved.model,
            );
            plan.passthrough = is_native_passthrough(client_tool, &plan.provider);
            match execute_single_model(
                &state,
                &body,
                model_str,
                presented_api_key.as_deref(),
                endpoint,
                &plan,
                client_tool,
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
    client_tool: Option<ClientTool>,
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
            upstream_body: None,
        });
    }

    // 1. RTK tool-result compression (gated by rtk_enabled)
    compress_messages(&mut body, snapshot.settings.rtk_enabled);

    // 2. Headroom external token compression (gated by headroom_enabled)
    {
        let headroom_cfg = HeadroomConfig {
            enabled: snapshot.settings.headroom_enabled,
            url: snapshot.settings.headroom_url.clone(),
            timeout_ms: snapshot.settings.headroom_timeout_ms,
            compress_user_messages: snapshot.settings.headroom_compress_user_messages,
        };
        let headroom_format =
            if plan.source_format == crate::core::translator::registry::Format::Claude {
                "claude"
            } else {
                "openai"
            };
        if let Some(stats) =
            compress_with_headroom(&mut body, &headroom_cfg, &plan.model, headroom_format).await
        {
            tracing::debug!("{}", stats.format_headroom_log().unwrap_or_default());
        }
    }

    // 9router parity: provider-level thinking config override (chatCore.js:44-57).
    if let Some(provider_thinking) = snapshot
        .settings
        .extra
        .get("providerThinking")
        .and_then(|v| v.as_object())
    {
        if let Some(mode_val) = provider_thinking.get(plan.provider.as_str()) {
            let mode = mode_val.as_str().unwrap_or("auto");
            if mode != "auto" {
                if mode == "on" && !body.get("thinking").and_then(|v| v.as_object()).is_some() {
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert(
                            "thinking".to_string(),
                            json!({"type": "enabled", "budget_tokens": 10000}),
                        );
                    }
                } else if mode == "off"
                    && !body.get("thinking").and_then(|v| v.as_object()).is_some()
                {
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert("thinking".to_string(), json!({"type": "disabled"}));
                    }
                } else if mode != "on" && mode != "off" {
                    if !body
                        .get("reasoning_effort")
                        .and_then(|v| v.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                    {
                        if let Some(obj) = body.as_object_mut() {
                            obj.insert(
                                "reasoning_effort".to_string(),
                                Value::String(mode.to_string()),
                            );
                        }
                    }
                }
            }
        }
    }

    // 3. Strip unsupported modalities in source format before translation
    let caps = capabilities_for_format(plan.source_format);
    strip_unsupported_modalities(&mut body, plan.source_format, &caps);

    // 4. Translate request body from source format to target format
    //     before applying token savers (9router order: translate first, then preprocess).
    if plan.needs_translation() {
        registry::global_registry().translate_request(
            plan.source_format,
            plan.target_format,
            &plan.model,
            &mut body,
            plan.stream,
            None,
        );
    }

    // 5. Caveman + Ponytail prompt injection (after translate — 9router parity)
    let _ = apply_request_preprocessing(&mut body, &snapshot.settings, &plan.model);

    // Prefetch remote images for providers that need inline base64
    // (9router prefetchRemoteImages parity: gate on target format).
    if plan.target_format.needs_image_prefetch() {
        if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            let client = reqwest::Client::new();
            for msg in messages.iter_mut() {
                let content_array = match msg.get_mut("content") {
                    Some(Value::Array(arr)) => arr,
                    _ => continue,
                };
                for part in content_array.iter_mut() {
                    // OpenAI format: image_url.url
                    if let Some(url) = part
                        .get("image_url")
                        .and_then(|iu| iu.get("url"))
                        .and_then(|u| u.as_str())
                    {
                        if url.starts_with("http://") || url.starts_with("https://") {
                            if let Some(fetched) = fetch_image_as_base64(&client, url).await {
                                if let Some(img) =
                                    part.get_mut("image_url").and_then(|iu| iu.as_object_mut())
                                {
                                    img.insert("url".into(), Value::String(fetched.data_url));
                                }
                            }
                        }
                    }
                    // Claude format: image.source.url
                    if let Some(source) = part.get("image").and_then(|im| im.get("source")) {
                        if source.get("type").and_then(|t| t.as_str()) == Some("url") {
                            if let Some(url) = source.get("url").and_then(|u| u.as_str()) {
                                if url.starts_with("http://") || url.starts_with("https://") {
                                    if let Some(fetched) = fetch_image_as_base64(&client, url).await
                                    {
                                        if let Some(src) = part
                                            .get_mut("image")
                                            .and_then(|im| im.get_mut("source"))
                                            .and_then(|s| s.as_object_mut())
                                        {
                                            src.insert(
                                                "data".into(),
                                                Value::String(fetched.data_url),
                                            );
                                            src.insert(
                                                "type".into(),
                                                Value::String("base64".into()),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Deduplicate tool definitions when MCP equivalents are present
    // (9router parity: only strips for Claude client).
    if client_tool == Some(ClientTool::Claude) {
        if let Some(tools_val) = body.get("tools").and_then(|t| t.as_array()) {
            let result = dedupe_tools(tools_val);
            if !result.stripped.is_empty() {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("tools".into(), Value::Array(result.tools));
                }
            }
        }
    }

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
        client_tool,
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
    client_tool: Option<ClientTool>,
) -> Result<Response, ComboAttemptError> {
    let mut excluded = HashSet::new();
    let mut last_error: Option<ComboAttemptError> = None;
    let registry = &state.account_registry;

    // Extract tool name map from body (set by Claude cloaking).
    // Remove from body before dispatch to avoid serializing it upstream.
    let tool_name_map: Option<std::collections::BTreeMap<String, String>> = request_body
        .as_object_mut()
        .and_then(|obj| obj.remove("_toolNameMap"))
        .and_then(|v| serde_json::from_value(v).ok());

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
                upstream_body: None,
            });
        };

        let provider_node = snapshot
            .provider_nodes
            .iter()
            .find(|node| node.id == provider)
            .cloned();
        let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);

        let (rate_limit_remaining, rate_limit_reset) = registry.rate_limit_info(&connection.id);
        let slot = registry.acquire_slot(
            &connection.id,
            MAX_IN_FLIGHT_PER_ACCOUNT,
            rate_limit_remaining,
            rate_limit_reset,
        );

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
            .unwrap_or(true);

        // DeepSeek TUI non-interactive (-p) mode can't parse SSE
        // (9router chatCore.js:81-86 parity).
        let stream = if client_tool == Some(ClientTool::DeepseekTui) {
            false
        } else {
            stream
        };

        state
            .usage_live
            .start_request(model, provider, Some(connection.id.as_str()))
            .await;

        use crate::core::executor::{
            AntigravityExecutionRequest, AntigravityExecutor, AzureExecutionRequest, AzureExecutor,
            CodexExecutionRequest, CodexExecutor, CommandCodeExecutionRequest, CommandCodeExecutor,
            CursorExecutionRequest, CursorExecutor, DefaultExecutor, ExecutionRequest,
            GeminiCliExecutionRequest, GeminiCliExecutor, GithubExecutionRequest, GithubExecutor,
            GrokWebExecutionRequest, GrokWebExecutor, IFlowExecutionRequest, IFlowExecutor,
            KiroExecutionRequest, KiroExecutor, KiroExecutorResponse, OpenCodeExecutionRequest,
            OpenCodeExecutor, OpenCodeGoExecutionRequest, OpenCodeGoExecutor,
            PerplexityWebExecutionRequest, PerplexityWebExecutor, QoderExecutionRequest,
            QoderExecutor, QwenExecutionRequest, QwenExecutor, VertexExecutionRequest,
            VertexExecutor,
        };

        let is_codex_model = model.starts_with("codex/") || provider == "codex";
        let is_cursor_model =
            model.starts_with("cursor/") || provider == "cu" || provider == "cursor";
        let executor_result: Result<KiroExecutorResponse, ComboAttemptError> =
            if provider == "kiro" {
                let executor = KiroExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Kiro executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
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
                        upstream_body: None,
                    })
            } else if provider == "vertex" {
                let executor = VertexExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Vertex executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
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
                        upstream_body: None,
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
                        upstream_body: None,
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
                        upstream_body: None,
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
                        upstream_body: None,
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
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "github" {
                let executor = GithubExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Github executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(GithubExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Github execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "azure" {
                let executor = AzureExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Azure executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(AzureExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Azure execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "qwen" {
                let executor = QwenExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Qwen executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(QwenExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Qwen execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "iflow" {
                let executor = IFlowExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("IFlow executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(IFlowExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("IFlow execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "gemini-cli" {
                let executor = GeminiCliExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("GeminiCli executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(GeminiCliExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("GeminiCli execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "opencode" {
                let executor = OpenCodeExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("OpenCode executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(OpenCodeExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("OpenCode execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "opencode-go" {
                let executor = OpenCodeGoExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("OpenCodeGo executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(OpenCodeGoExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("OpenCodeGo execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "qoder" {
                let executor = QoderExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Qoder executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(QoderExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Qoder execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "commandcode" {
                let executor = CommandCodeExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                    status: 500,
                    message: format!("CommandCode executor creation failed: {:?}", e),
                    retry_after: None,
                    upstream_body: None,
                })?;
                let result = executor
                    .execute_request(CommandCodeExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("CommandCode execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "antigravity" {
                let executor = AntigravityExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                    status: 500,
                    message: format!("Antigravity executor creation failed: {:?}", e),
                    retry_after: None,
                    upstream_body: None,
                })?;
                let result = executor
                    .execute_request(AntigravityExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Antigravity execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "grok-web" {
                let executor = GrokWebExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(GrokWebExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("GrokWeb execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "perplexity-web" {
                let executor = PerplexityWebExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(PerplexityWebExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("PerplexityWeb execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
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
                    upstream_body: None,
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
                        upstream_body: None,
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
                            plan,
                            tool_name_map.as_ref(),
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
                        plan,
                        tool_name_map.as_ref(),
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
                    upstream_body: None,
                });

                // 404 (model not found) should set a model-specific lock without
                // excluding the connection — other models on the same connection
                // should still be routable.
                if status.as_u16() == 404 {
                    let model_cooldown = std::time::Duration::from_secs(300);
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        model,
                        status.as_u16(),
                        &message,
                        model_cooldown,
                        current_backoff,
                    )
                    .await;
                }

                // Token refresh: on 401/403, try to refresh the access token
                // before giving up on this connection (9router parity).
                // On success, update the DB and continue the loop so the
                // fresh snapshot picks up the renewed token.
                if (status.as_u16() == 401 || status.as_u16() == 403)
                    && connection.refresh_token.is_some()
                {
                    if let Some(ref rt) = connection.refresh_token.clone() {
                        let refresh_provider = plan.provider.as_str();
                        match crate::oauth::token_refresh::dispatch_oauth_refresh(
                            refresh_provider,
                            &rt,
                            &connection.provider_specific_data,
                        )
                        .await
                        {
                            Ok(result) => {
                                let conn_id = connection.id.clone();
                                let new_access = result.access_token.clone();
                                let new_refresh = result.refresh_token.clone();
                                let _ = state
                                    .db
                                    .update(move |db| {
                                        if let Some(conn) = db
                                            .provider_connections
                                            .iter_mut()
                                            .find(|c| c.id == conn_id)
                                        {
                                            conn.access_token = Some(new_access);
                                            if let Some(rt) = new_refresh {
                                                conn.refresh_token = Some(rt);
                                            }
                                            conn.last_error = None;
                                            conn.last_error_at = None;
                                            conn.error_code = None;
                                            conn.backoff_level = Some(0);
                                        }
                                    })
                                    .await;
                                continue;
                            }
                            Err(_) => {}
                        }
                    }
                }

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

/// Peek-only capacity check for a single combo member model.
///
/// Mirrors the filtering in [`select_connection`] but does NOT acquire a slot:
/// it just asks whether at least one eligible provider account has a free
/// in-flight slot under [`MAX_IN_FLIGHT_PER_ACCOUNT`]. Used by the round-robin
/// strategy to skip combo members whose backing providers are currently
/// saturated, so we don't pin a coding agent's request on a provider that
/// would either fail fast through the inner per-account fallback or block
/// other repos' requests.
///
/// Returns `Available` for combo models we can't statically resolve to a
/// specific provider (e.g. alias-only lookups that depend on runtime
/// resolution) so we don't accidentally exclude them - the existing
/// per-account fallback inside [`forward_with_provider_fallback`] still
/// applies once we actually attempt the request.
fn model_capacity(
    snapshot: &AppDb,
    registry: &crate::core::account_fallback::AccountRegistry,
    combo_model: &str,
) -> ModelCapacity {
    let resolved = get_model_info(combo_model, snapshot);
    let Some(provider) = resolved.provider.as_deref() else {
        return ModelCapacity::Available;
    };

    let now = Utc::now();
    let has_capacity = snapshot.provider_connections.iter().any(|connection| {
        connection.provider == provider
            && connection.is_active()
            && connection_has_credentials(connection)
            && connection_supports_model(connection, &resolved.model)
            && !is_connection_rate_limited(connection, now)
            && !is_model_locked(connection, &resolved.model, now)
            && registry.in_flight_count(&connection.id) < MAX_IN_FLIGHT_PER_ACCOUNT
    });

    if has_capacity {
        ModelCapacity::Available
    } else {
        ModelCapacity::Busy
    }
}

fn select_connection(
    snapshot: &AppDb,
    provider: &str,
    model: &str,
    excluded: &HashSet<String>,
) -> Option<ProviderConnection> {
    let now = Utc::now();

    // First: use filter_available_accounts to get accounts not in cooldown / not locked.
    let available =
        filter_available_accounts(&snapshot.provider_connections, provider, model, None, now);

    // Then: apply remaining filters that filter_available_accounts does not cover:
    //   - credentials presence
    //   - model support
    //   - excluded set (the call above passes None for exclude_id since we need
    //     to apply it separately alongside the other per-request filters)
    let mut candidates: Vec<_> = available
        .into_iter()
        .filter(|connection| {
            connection_has_credentials(connection)
                && !excluded.contains(&connection.id)
                && connection_supports_model(connection, model)
        })
        .cloned()
        .collect();

    candidates.sort_by_key(|connection| connection.priority.unwrap_or(999));
    if let Some(connection) = candidates.into_iter().next() {
        return Some(connection);
    }

    // No stored connection. Inject a virtual one for noAuth free providers
    // (matches 9router's getProviderCredentials behavior). Lets OpenCode Free,
    // edge-tts, google-tts, etc. route requests without manual setup.
    if is_no_auth_provider(provider) && !excluded.contains("noauth") {
        return Some(virtual_no_auth_connection(provider));
    }

    None
}

fn is_no_auth_provider(provider: &str) -> bool {
    matches!(
        provider,
        "opencode"
            | "opencode-go"
            | "edge-tts"
            | "google-tts"
            | "local-device"
            | "ollama-local"
            | "sdwebui"
            | "comfyui"
            | "grok-web"
            | "perplexity-web"
    )
}

fn virtual_no_auth_connection(provider: &str) -> ProviderConnection {
    let mut connection = ProviderConnection::default();
    connection.id = "noauth".to_string();
    connection.provider = provider.to_string();
    connection.auth_type = "none".to_string();
    connection.name = Some("Public".to_string());
    connection.is_active = Some(true);
    connection.access_token = Some("public".to_string());
    connection
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
    let now = Utc::now();
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
        .filter(|until| *until > now)
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
    } else if value.eq_ignore_ascii_case("fusion") {
        ComboStrategy::Fusion
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
    let connection_id = connection_id.to_string();
    let (model_lock_key, until_str) = build_model_lock_update(model, cooldown.as_secs() as i64);
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
                    .insert(model_lock_key, Value::String(until_str));
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
    plan: &RequestPlan,
    tool_name_map: Option<&std::collections::BTreeMap<String, String>>,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let (body_bytes, body_complete) = collect_upstream_response_bytes(response).await;

    // 9router parity: decloak tool names when Claude cloaking was applied.
    let decloaked_body = if let Some(map) = tool_name_map {
        if !map.is_empty() {
            let body_val: serde_json::Value =
                serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
            if !body_val.is_null() {
                let decloaked =
                    crate::core::utils::claude_cloaking::decloak_tool_names(&body_val, map);
                serde_json::to_vec(&decloaked)
                    .map(Bytes::from)
                    .unwrap_or(body_bytes.clone())
            } else {
                body_bytes.clone()
            }
        } else {
            body_bytes.clone()
        }
    } else {
        body_bytes.clone()
    };

    let final_body = if body_complete {
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

        // 9router parity: translate non-streaming response body when source
        // and target formats differ (handleNonStreamingResponse).
        let translated_body = if plan.needs_translation() {
            let mut state = crate::core::translator::registry::ResponseTransformState::default();
            let chunks = registry::global_registry().translate_response(
                plan.target_format,
                plan.source_format,
                decloaked_body.as_ref(),
                &mut state,
            );
            if !chunks.is_empty() {
                let mut result = String::new();
                for chunk in &chunks {
                    if let Some(data) = chunk.strip_prefix("data: ") {
                        result = data.to_string();
                        if result == "[DONE]" {
                            continue;
                        }
                    }
                }
                if result.is_empty() {
                    decloaked_body.clone()
                } else {
                    Bytes::from(result)
                }
            } else {
                decloaked_body.clone()
            }
        } else {
            decloaked_body.clone()
        };

        Body::from(translated_body)
    } else {
        Body::from(decloaked_body)
    };

    build_proxied_response(status, &headers, final_body)
}

async fn proxy_response_with_pending_tracking(
    response: UpstreamResponse,
    state: AppState,
    provider: String,
    model: String,
    connection_id: Option<String>,
    normalize_for_dashboard: bool,
    plan: &RequestPlan,
    tool_name_map: Option<&std::collections::BTreeMap<String, String>>,
) -> Response {
    // Extract formats before stream closure to avoid lifetime issues
    let needs_stream_translation = plan.needs_translation();
    let stream_source_format = plan.source_format;
    let stream_target_format = plan.target_format;
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
                    let next = tokio::time::timeout(SSE_STALL_TIMEOUT, upstream.try_next()).await;
                    match next {
                        Err(_elapsed) => {
                            // Upstream went silent for SSE_STALL_TIMEOUT; treat
                            // as an error so the client can retry.
                            tracing::warn!(
                                target: "openproxy::chat::stream",
                                provider = %provider,
                                model = %model,
                                "SSE stalled, closing stream"
                            );
                            state
                                .usage_live
                                .finish_request(&model, &provider, connection_id.as_deref(), true)
                                .await;
                            return;
                        }
                        Ok(Ok(Some(chunk))) => {
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
                        Ok(Ok(None)) => break,
                        Ok(Err(_)) => {
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
                loop {
                    let next = tokio::time::timeout(SSE_STALL_TIMEOUT, body.frame()).await;
                    let frame_result = match next {
                        Err(_elapsed) => {
                            tracing::warn!(
                                target: "openproxy::chat::stream",
                                provider = %provider,
                                model = %model,
                                "SSE stalled, closing stream"
                            );
                            state
                                .usage_live
                                .finish_request(&model, &provider, connection_id.as_deref(), true)
                                .await;
                            return;
                        }
                        Ok(Some(result)) => result,
                        Ok(None) => break,
                    };
                    match frame_result {
                        Ok(frame) => {
                            if let Ok(data) = frame.into_data() {
                                if let Some(transformer) = transformer.as_mut() {
                                    for line in transform_dashboard_sse_chunk(&data, transformer.as_mut(), &mut pending_text) {
                                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                                            yield Ok::<Bytes, std::io::Error>(frame);
                                        }
                                    }
                                } else if needs_stream_translation {
                                    // 9router parity: translate SSE response chunks
                                    // from provider format back to source format
                                    // during streaming (translateResponse pipeline).
                                    let mut t_state = crate::core::translator::registry::ResponseTransformState::default();
                                    let chunks = registry::global_registry()
                                        .translate_response(
                                            stream_target_format,
                                            stream_source_format,
                                            &data,
                                            &mut t_state,
                                        );
                                    for line in chunks {
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

    let mut response = build_proxied_response(status, &headers, body);
    // SSE-specific headers (9router parity): prevent nginx/proxy buffering
    // and keep the SSE connection alive through intermediary proxies.
    response
        .headers_mut()
        .insert("Connection", "keep-alive".parse().unwrap());
    response
        .headers_mut()
        .insert("X-Accel-Buffering", "no".parse().unwrap());
    response
        .headers_mut()
        .insert("Cache-Control", "no-cache".parse().unwrap());
    response
        .headers_mut()
        .insert("Content-Type", "text/event-stream".parse().unwrap());
    response
}

fn sse_frame_for_dashboard(line: &str) -> Option<Bytes> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 9router parity: preserve all standard SSE line types without wrapping.
    // - data: {...}          → data frame
    // - event: name          → event type header
    // - id: ...              → event id
    // - retry: ...           → retry interval
    // - : comment            → comment (keep-alive)
    // Everything else gets data: prefix added.
    let framed = if trimmed.starts_with("data:")
        || trimmed.starts_with("event:")
        || trimmed.starts_with("id:")
        || trimmed.starts_with("retry:")
        || trimmed.starts_with(':')
    {
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
    let pending_len = line.len();
    let output = transform_sse_stream(&Bytes::from(line), transformer);
    if output.is_empty() {
        tracing::trace!(
            target: "openproxy::chat::stream",
            "flush_dashboard_sse_chunk: {} bytes of partial/invalid buffer content yielded no output lines",
            pending_len,
        );
    }
    output
}

fn build_proxied_response(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: Body,
) -> Response {
    let mut proxied = Response::new(body);
    *proxied.status_mut() = status;
    let connection_tokens = connection_header_tokens(headers);

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
        "reasoning_tokens",
        "cached_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ];

    Some(TokenUsage {
        prompt_tokens: usage.get("prompt_tokens").and_then(Value::as_u64),
        input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
        completion_tokens: usage.get("completion_tokens").and_then(Value::as_u64),
        output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
        total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
        reasoning_tokens: usage.get("reasoning_tokens").and_then(Value::as_u64),
        cached_tokens: usage.get("cached_tokens").and_then(Value::as_u64),
        cache_read_input_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_u64),
        cache_creation_input_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64),
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
    // Standard retry-after header (HTTP/1.1)
    if let Some(value) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
        let trimmed = value.trim();
        if let Ok(seconds) = trimmed.parse::<i64>() {
            return Some(Utc::now() + ChronoDuration::seconds(seconds.max(0)));
        }
        if let Ok(timestamp) = DateTime::parse_from_rfc2822(trimmed) {
            return Some(timestamp.with_timezone(&Utc));
        }
    }

    // Google-specific rate limit headers (used by Antigravity / Cloud Code)
    // x-ratelimit-reset-after: seconds until rate limit resets (relative)
    if let Some(value) = headers
        .get("x-ratelimit-reset-after")
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(seconds) = value.trim().parse::<i64>() {
            if seconds > 0 {
                return Some(Utc::now() + ChronoDuration::seconds(seconds));
            }
        }
    }

    // x-ratelimit-reset: unix timestamp (seconds) when rate limit resets (absolute)
    if let Some(value) = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(ts) = value.trim().parse::<i64>() {
            let now = Utc::now().timestamp();
            if ts > now {
                return Some(Utc::now() + ChronoDuration::seconds(ts - now));
            }
        }
    }

    None
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
        upstream_body: None,
    }))
}

fn attempt_error_response(error: ComboAttemptError) -> Response {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let (e_type, e_code) = match crate::core::config::error_config::error_type_for(status.as_u16())
    {
        Some(info) => (info.r#type, info.code),
        None if status.as_u16() >= 500 => ("server_error", "internal_server_error"),
        None => ("invalid_request_error", ""),
    };
    let mut response = (
        status,
        Json(json!({
            "error": {
                "message": error.message,
                "type": e_type,
                "code": e_code
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
    let (e_type, e_code) = match crate::core::config::error_config::error_type_for(status.as_u16())
    {
        Some(info) => (info.r#type, info.code),
        None if status.as_u16() >= 500 => ("server_error", "internal_server_error"),
        None => ("invalid_request_error", ""),
    };
    let msg = message;
    with_cors_response(
        (
            status,
            Json(json!({
                "error": {
                    "message": msg,
                    "type": e_type,
                    "code": e_code
                }
            })),
        )
            .into_response(),
    )
}

fn json_success_response(status: StatusCode, data: Value) -> Response {
    with_cors_response((status, Json(data)).into_response())
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

/// Build a bypass response — either streaming SSE (when `stream` is true) or
/// non-streaming JSON. 9router parity: the streaming path emits proper OpenAI
/// SSE chunks so client-side SSE parsers (Claude Code, Gemini CLI, etc.)
/// receive a valid event stream instead of unexpected JSON.
fn bypass_response(model: &str, text: &str, stream: bool) -> Response {
    let id = format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis());
    let created = chrono::Utc::now().timestamp();

    if stream {
        let content_frame = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "content": text
                },
                "finish_reason": null
            }]
        });
        let finish_frame = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            }
        });

        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::to_string(&content_frame).unwrap_or_default(),
            serde_json::to_string(&finish_frame).unwrap_or_default(),
        );

        let mut response = Response::new(Body::from(body));
        *response.status_mut() = StatusCode::OK;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        response
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        response
    } else {
        json_success_response(
            StatusCode::OK,
            json!({
                "id": id,
                "object": "chat.completion",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": text
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            }),
        )
    }
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
