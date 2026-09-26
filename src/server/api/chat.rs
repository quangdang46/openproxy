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

use crate::core::account_fallback::{
    build_model_lock_update, filter_available_accounts, StrategyType,
};
use crate::core::chat::RequestPlan;
use crate::core::combo::fusion::{handle_fusion_chat, handle_fusion_chat_deferred};
use crate::core::combo::{
    capacity_adapter::{
        augment_models_with_capacity_adapter, get_active_adapter_strategy,
        strip_history_for_context,
    },
    check_fallback_error, combo_quarantine_for, detect_required_capabilities,
    execute_combo_strategy_full, get_combo_models_from_data, get_disabled_members_for_combo,
    mark_combo_member_quarantined, strategy_for_combo, ComboAttemptError, ComboExecutionError,
    ComboStrategy, FallbackDecision, FusionConfig, ModelCapacity,
};
use crate::core::executor::UpstreamResponse;
use crate::core::model::{get_model_info, ModelRouteKind};
use crate::core::proxy::resolve_proxy_target;
use crate::core::rtk::headroom::{compress_with_headroom_diag, HeadroomConfig};
use crate::core::rtk::{apply_request_preprocessing, compress_messages};
use crate::core::translator::helpers::image_helper::fetch_image_as_base64;
use crate::core::translator::helpers::modality_helper::{
    capabilities_for_format, strip_unsupported_modalities, ModalityCapabilities,
};
use crate::core::translator::registry::{self, Format};
use crate::core::translator::response_transform::{transform_sse_stream, transformer_for_provider};
use crate::core::usage::CompressionStats;
use crate::core::utils::bypass_handler::{detect_bypass, BypassDecision, DEFAULT_BYPASS_TEXT};
use crate::core::utils::claude_cloaking::{cloak_claude_tools, CloakedRequest};
use crate::core::utils::client_detector::{detect_client_tool, is_native_passthrough, ClientTool};
use crate::core::utils::stream_flags::resolve_stream_flags;
use crate::core::utils::tool_deduper::dedupe_tools;
use crate::payload_rules::{apply_request_rules, apply_system_prompt};
use crate::server::auth::{extract_api_key, require_api_key, require_api_key_with_reload};
use crate::server::state::AppState;
use crate::types::{AppDb, ProviderConnection, TokenUsage};

use super::auth_error_response;

/// Check whether the process should trust reverse-proxy forwarding headers
/// (`X-Forwarded-For`, `X-Real-IP`, `X-Forwarded-Proto`, etc.).
///
/// Set `TRUST_PROXY=true` in the environment to enable. **Default is `false`**
/// — when disabled, all forwarding headers are stripped from the incoming
/// request so that spoofed IPs / protocols from untrusted intermediaries
/// are never propagated upstream or used for rate-limiting decisions.
///
/// # Examples
///
/// ```ignore
/// TRUST_PROXY=true            # trust reverse-proxy headers
/// TRUST_PROXY=false           # strip them (default)
///                             # not set → same as false
/// ```
fn trust_proxy_enabled() -> bool {
    matches!(
        std::env::var("TRUST_PROXY").as_deref(),
        Ok("true") | Ok("1") | Ok("yes")
    )
}

/// Remove reverse-proxy forwarding headers from `headers` when
/// [`trust_proxy_enabled`] returns `false`.
///
/// This runs at the top of every chat-completions handler so that:
///   - `X-Forwarded-For` / `X-Real-IP` are not forwarded upstream.
///   - `X-Forwarded-Proto` is not used to infer TLS state.
///   - `X-Forwarded-Host` is not used to infer the target host.
///
/// When deployed directly (not behind nginx/Caddy/Traefik), stripping
/// these headers also prevents malicious clients from injecting them.
fn strip_forwarding_headers(headers: &mut HeaderMap) {
    if trust_proxy_enabled() {
        return;
    }
    // Common headers set by reverse proxies (nginx, Caddy, Traefik, HAProxy,
    // Cloudflare, AWS ALB, …) that should not be trusted when TRUST_PROXY
    // is not explicitly enabled.
    static FORWARDING_HEADERS: &[&str] = &[
        "x-forwarded-for",
        "x-forwarded-proto",
        "x-forwarded-host",
        "x-forwarded-server",
        "x-real-ip",
    ];
    for &name in FORWARDING_HEADERS {
        headers.remove(name);
    }
}

/// Maximum time we'll wait for the next byte from an upstream SSE stream before
/// considering the connection stalled. 3 minutes matches what most providers
/// use for their keep-alive heartbeats.
///
/// The value now comes from the shared runtime config rather than a second
/// literal. `runtime_config::STREAM_STALL_TIMEOUT_MS` already carried 360s
/// with the comment "matching the 9router default" — and nothing read it, while
/// this constant said 180s. 9router sets 360s
/// (config/runtimeConfig.js:53) specifically so "slow reasoning models aren't
/// aborted mid-stream"; a reasoning turn that pauses 3-6 minutes was being cut
/// here and not there.
///
/// Kept as a function so the value is resolved once at first use rather than in
/// a const, and so the env override has somewhere to land (9router reads
/// STREAM_STALL_TIMEOUT_MS; OpenProxy has no such env var yet — the follow-up
/// is adding one, not silently keeping two constants).
fn sse_stall_timeout() -> Duration {
    Duration::from_millis(crate::core::config::runtime_config::STREAM_STALL_TIMEOUT_MS)
}

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
    mut headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
    endpoint: Option<&'static str>,
    require_api_key_auth: bool,
) -> Response {
    // Security: strip reverse-proxy forwarding headers unless TRUST_PROXY=true.
    // When running without a trusted reverse proxy (default), headers like
    // X-Forwarded-For / X-Real-IP / X-Forwarded-Proto are spoofable by any
    // client and must not be used for rate limiting, IP logging, or TLS
    // inference decisions downstream.
    strip_forwarding_headers(&mut headers);

    let presented_api_key = extract_api_key(&headers);
    // Gate on requireApiKey, NOT requireLogin (bead openproxy-d5lf). The two
    // are separate settings in 9router; reading require_login here meant
    // locking the dashboard also locked every API client, and leaving the
    // dashboard open silently removed API auth from /v1.
    if require_api_key_auth && state.db.snapshot().settings.require_api_key() {
        if let Err(error) = require_api_key_with_reload(&headers, &state.db).await {
            return auth_error_response(error);
        }
    }

    let Json(mut body) = match body {
        Ok(body) => body,
        Err(_) => return json_error_response(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    // Claude Code marks a 1M-context request as `<model>[1m]`. The marker is a
    // client-side annotation that matches no combo, alias or `provider/model`
    // pair, so it must not reach model resolution or the request dies with an
    // invalid-model error. The actual 1M capability travels in the
    // `anthropic-beta` header, which is forwarded untouched.
    // Mirrors `stripModelContextMarker` in 9router's
    // `open-sse/utils/modelMarkers.js`, called at the top of
    // `src/sse/handlers/chat.js`.
    if let Some(model) = body.get("model").and_then(Value::as_str) {
        let (stripped, marker) =
            crate::core::translator::request::claude_format::strip_model_context_marker(
                model.trim(),
            );
        if marker.is_some() {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("model".to_string(), Value::String(stripped));
            }
        }
    }

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

    // Stale-snapshot recovery: if the model name looks like a combo (no '/')
    // but wasn't found, reload from SQLite and try once more. This handles
    // combos created by the CLI process that bypasses the server's snapshot.
    let (snapshot, resolved) =
        if resolved.route_kind == ModelRouteKind::Combo || model_str.contains('/') {
            (snapshot, resolved)
        } else {
            if let Ok(fresh) = state.db.reload_snapshot().await {
                if fresh.combos.iter().any(|c| c.name == model_str) {
                    let re_resolved = get_model_info(model_str, &fresh);
                    (fresh, re_resolved)
                } else {
                    (snapshot, resolved)
                }
            } else {
                (snapshot, resolved)
            }
        };

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

    // Accept/stream preference is applied via resolve_stream_flags on the plan
    // (does NOT mutate body.stream when client set stream:true — 9router parity).
    let accept_header = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

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

    // Feature4: ResponseCache — consult before provider dispatch.
    // The cache stores a single JSON body and replays it as `application/json`,
    // so it must be gated on the SAME resolved stream decision the dispatcher
    // uses (resolve_stream_flags), never on an independent default read from the
    // raw body. An omitted `stream` field means streaming (9router parity), so a
    // second independent default here previously took the cache path while the
    // response was actually an SSE stream, poisoning the cache with SSE bytes
    // under a JSON key (openproxy-w3nh). Simulation bypass (live-E2E fix): any
    // x-openproxy-sim-* control header bypasses lookup AND store — fault/
    // override/latency are test controls, and sim responses must never poison
    // the cache for real requests.
    //
    // Resolve the plan up-front using the exact inputs the Direct dispatch
    // later feeds to apply_stream_plan (same provider/model/accept/client_tool
    // and the same source-format detection as RequestPlan::new), so the guard
    // and the dispatcher cannot drift. For a Combo the per-member provider is
    // not known until dispatch, so the top-level resolved name is used and the
    // guard errs toward the streaming default — the safe direction.
    let stream_plan = {
        let source_format = if let Some(path) = endpoint {
            registry::detect_source_format_by_endpoint_with_body(path, Some(&body))
                .unwrap_or_else(|| registry::detect_source_format(&body))
        } else {
            registry::detect_source_format(&body)
        };
        resolve_stream_flags(
            body.get("stream").and_then(Value::as_bool),
            accept_header.as_deref(),
            resolved.provider.as_deref().unwrap_or(model_str),
            &resolved.model,
            source_format,
            client_tool,
            None,
        )
    };
    // A live SSE stream reaches the client only when the plan streams upstream
    // and is NOT aggregated back to a single JSON body (sse_to_json). Those are
    // the responses that must never be stored in or served from the cache;
    // `!stream` (non-streaming) and `sse_to_json` (forceStream aggregated to
    // JSON) both yield a single cacheable JSON body.
    let is_sse_response = stream_plan.stream && !stream_plan.sse_to_json;
    let sim_controlled = headers_map
        .keys()
        .any(|k| k == "x-openproxy-sim" || k.starts_with("x-openproxy-sim-"));

    // Combo responses are never cached.
    //
    // The guard above resolves the stream decision from the TOP-LEVEL provider
    // and the real Accept header, but the combo dispatch builds its own plan
    // inside the closure with `accept: None` (the header is not in scope
    // there). The two therefore disagree whenever Accept changes the outcome:
    // a client sending `Accept: application/json` flips the guard to
    // "cacheable" while the combo leg still streams, and the SSE body gets
    // stored under a JSON key — then served back with Content-Type
    // application/json and x-cache: HIT. That is the exact defect this guard
    // exists to prevent, and it is reachable with ordinary client headers.
    //
    // Threading the header into the closure is the general fix, but until that
    // lands the two decisions cannot be proven to agree for every provider in
    // a combo. Skipping the cache for combos is conservative, costs only a
    // small amount of hit rate, and removes the entire class of drift.
    let is_cacheable_route = resolved.route_kind == ModelRouteKind::Direct;

    if is_cacheable_route && !is_sse_response && !sim_controlled {
        if let Some((cached, ttl_remaining)) = state.response_cache.get_with_ttl(&body) {
            let mut resp = Response::new(Body::from(cached));
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            resp.headers_mut()
                .insert("x-cache", HeaderValue::from_static("HIT"));

            // Robot envelope: lets agents detect a cache hit and its remaining
            // TTL without parsing the body. Carried as a header so the OpenAI-
            // compatible JSON body stays untouched.
            let envelope = json!({
                "schema": "openproxy.v1.cache.hit",
                "ok": true,
                "data": {
                    "cache_hit": true,
                    "model": body.get("model").and_then(Value::as_str).unwrap_or(""),
                    "provider": resolved.provider.as_deref().unwrap_or("unknown"),
                    "ttl_remaining": ttl_remaining,
                },
                "meta": {},
            });
            if let Ok(value) = serde_json::to_string(&envelope) {
                if let Ok(hv) = HeaderValue::from_str(&value) {
                    resp.headers_mut().insert("x-cache-envelope", hv);
                }
            }
            return resp;
        }
    }

    let cache_provider = resolved.provider.as_deref().unwrap_or("unknown");

    let response = match resolved.route_kind {
        ModelRouteKind::Combo => {
            let combo_name = resolved.model;
            let Some(combo_models) = get_combo_models_from_data(&combo_name, &snapshot.combos)
            else {
                return json_error_response(StatusCode::BAD_REQUEST, "Unknown combo model");
            };

            // Capability auto-switch is applied AFTER round-robin rotation
            // inside execute_combo_strategy_with_capacity (9router order:
            // rotate first, then reorderByCapabilities).
            let required_caps = detect_required_capabilities(&body);
            let disabled_members = get_disabled_members_for_combo(&combo_name, &snapshot.combos);

            // 9router parity (chat.js): augment the combo member list with
            // capacity-adapter pool models when no member satisfies the
            // request's hard capabilities, and remember which models were
            // added so history stripping only ever applies to them.
            let augmented_models = augment_models_with_capacity_adapter(
                &combo_models,
                &required_caps,
                &snapshot.settings.capacity_adapter,
            );
            let adapter_added: HashSet<String> = augmented_models
                .iter()
                .filter(|m| !combo_models.contains(m))
                .cloned()
                .collect();
            let mut strategy = strategy_for_combo(&snapshot, &combo_name);
            // Solo-augmented path: an adapter model was prepended to a
            // single-member combo — use the adapter pool's strategy.
            if !adapter_added.is_empty() && combo_models.len() == 1 {
                strategy = match get_active_adapter_strategy(
                    &required_caps,
                    &snapshot.settings.capacity_adapter,
                ) {
                    "round-robin" => ComboStrategy::RoundRobin,
                    _ => ComboStrategy::Fallback,
                };
            }
            let sticky_limit = snapshot.settings.combo_sticky_round_robin_limit.max(1);
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
                // openproxy-s2oc: the Fusion fan-out must honour the same
                // pre-gates as the sequential strategies. Passing the raw
                // member list here dispatched to — and billed — members the
                // operator muted, and members already parked in the
                // auto-quarantine map, even though the Combos page renders
                // them as "Disabled — never dispatched" / "cooling down".
                // Health is deliberately NOT a gate here either: 9router
                // discovers a degraded provider inside the per-panel attempt
                // (combo.js:298-305), so the panel still gets its turn once
                // the provider has recovered.
                let mut skip: HashSet<String> = disabled_members.iter().cloned().collect();
                skip.extend(
                    combo_quarantine_for(&combo_name)
                        .into_iter()
                        .map(|(model, _)| model),
                );
                let fusion_panels: Vec<String> = combo_models
                    .iter()
                    .filter(|model| !skip.contains(model.as_str()))
                    .cloned()
                    .collect();

                if fusion_panels.is_empty() {
                    // Same 400/503 split the sequential path returns when
                    // no member is dispatchable, instead of fanning out to
                    // an empty panel set.
                    let only_quarantine = disabled_members.is_empty()
                        && combo_models.iter().all(|m| skip.contains(m));
                    return combo_error_response(ComboExecutionError {
                        status: if only_quarantine { 503 } else { 400 },
                        message: if only_quarantine {
                            "All combo members are currently quarantined after recent failures"
                                .into()
                        } else {
                            "All combo members are disabled".into()
                        },
                        earliest_retry_after: None,
                        upstream_body: None,
                    });
                }

                let f_state = state.clone();
                let f_body = body.clone();
                let f_api_key = presented_api_key.clone();
                let f_client_tool = client_tool;
                let f_headers = headers_map.clone();
                // openproxy-s2oc: record the panels that actually FAILED so a
                // failed fusion parks only the broken members. A panel that
                // answered must not be quarantined just because the judge
                // leg later failed.
                let failed_panels: std::sync::Arc<parking_lot::Mutex<Vec<String>>> =
                    std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));

                let panel_count = fusion_panels.len();
                let fusion_cfg = fusion_config_for(&snapshot, &combo_name, panel_count);
                // One clone per fan-out arm: each `move` closure takes its
                // own handle, and the outer list stays readable afterwards.
                let deferred_failures = failed_panels.clone();
                let direct_failures = failed_panels.clone();
                // 9router combo.js: the judge (and single-survivor) leg runs
                // with the ORIGINAL client stream flag — a streaming client
                // must get SSE, not a buffered JSON blob. The buffered-Value
                // callback below cannot carry an SSE body, so when the client
                // asked to stream we defer the final dispatch and run it
                // ourselves after panel collection.
                let client_wants_stream =
                    body.get("stream").and_then(Value::as_bool).unwrap_or(true);
                let fusion_result = if client_wants_stream {
                    handle_fusion_chat_deferred(
                        &mut body.clone(),
                        &fusion_panels,
                        &fusion_cfg,
                        None,
                        move |model: String, panel_body: Value| {
                            let state = f_state.clone();
                            let body = f_body.clone();
                            let api_key = f_api_key.clone();
                            let client_tool = f_client_tool;
                            let headers = f_headers.clone();
                            let failed_panels = deferred_failures.clone();
                            async move {
                                let response = match dispatch_fusion_leg(
                                    &state,
                                    &body,
                                    &panel_body,
                                    &model,
                                    api_key.as_deref(),
                                    endpoint,
                                    client_tool,
                                    &headers,
                                    Some(false),
                                )
                                .await
                                {
                                    Ok(response) => response,
                                    Err(e) => {
                                        failed_panels.lock().push(model.clone());
                                        return Err(anyhow::anyhow!(
                                            "Fusion panel failed: {}",
                                            e.message
                                        ));
                                    }
                                };
                                let body_bytes =
                                    axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
                                        .await
                                        .map_err(|e| {
                                            anyhow::anyhow!("Failed to read panel body: {}", e)
                                        })?;
                                serde_json::from_slice(&body_bytes)
                                    .map_err(|e| anyhow::anyhow!("Failed to parse panel body: {e}"))
                            }
                        },
                    )
                    .await
                } else {
                    handle_fusion_chat(
                        &mut body.clone(),
                        &fusion_panels,
                        &fusion_cfg,
                        None,
                        move |model: String, panel_body: Value| {
                            let state = f_state.clone();
                            let body = f_body.clone();
                            let api_key = f_api_key.clone();
                            let client_tool = f_client_tool;
                            let headers = f_headers.clone();
                            let failed_panels = direct_failures.clone();
                            async move {
                                let response = match dispatch_fusion_leg(
                                    &state,
                                    &body,
                                    &panel_body,
                                    &model,
                                    api_key.as_deref(),
                                    endpoint,
                                    client_tool,
                                    &headers,
                                    Some(false),
                                )
                                .await
                                {
                                    Ok(response) => response,
                                    Err(e) => {
                                        failed_panels.lock().push(model.clone());
                                        return Err(anyhow::anyhow!(
                                            "Fusion panel failed: {}",
                                            e.message
                                        ));
                                    }
                                };
                                let body_bytes =
                                    axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
                                        .await
                                        .map_err(|e| {
                                            anyhow::anyhow!("Failed to read panel body: {}", e)
                                        })?;
                                serde_json::from_slice(&body_bytes)
                                    .map_err(|e| anyhow::anyhow!("Failed to parse panel body: {e}"))
                            }
                        },
                    )
                    .await
                };

                match fusion_result {
                    Ok(value) => {
                        // Deferred dispatch: the fusion pipeline decided which
                        // model runs the final leg (judge or single survivor) —
                        // run it with full stream semantics so SSE clients see
                        // a live stream (COMBO-1 / 9router combo.js parity).
                        if let Some(dispatch) = value.get("__openproxy_fusion_dispatch") {
                            let model = dispatch
                                .get("model")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let empty = Value::Object(Default::default());
                            let dispatch_body = dispatch
                                .get("body")
                                .cloned()
                                .unwrap_or_else(|| empty.clone());
                            let dispatched = dispatch_fusion_leg(
                                &state,
                                &body,
                                &dispatch_body,
                                &model,
                                presented_api_key.as_deref(),
                                endpoint,
                                client_tool,
                                &headers_map,
                                None,
                            )
                            .await;
                            return match dispatched {
                                Ok(response) => response,
                                Err(error) => combo_error_response(ComboExecutionError {
                                    status: error.status,
                                    message: error.message,
                                    earliest_retry_after: None,
                                    upstream_body: None,
                                }),
                            };
                        }
                        let json_str = serde_json::to_string(&value).unwrap_or_default();
                        Ok(axum::response::Response::new(axum::body::Body::from(
                            json_str,
                        )))
                    }
                    Err(e) => {
                        // openproxy-s2oc: park the panels that actually
                        // failed so the next request does not immediately
                        // re-attempt the same broken member.
                        let cooldown = check_fallback_error(e.status, &e.message, 0).cooldown;
                        for member in failed_panels.lock().iter() {
                            mark_combo_member_quarantined(
                                &combo_name_for_quarantine,
                                member,
                                cooldown,
                            );
                        }
                        Err(ComboExecutionError {
                            status: e.status,
                            message: e.message,
                            earliest_retry_after: None,
                            upstream_body: None,
                        })
                    }
                }
            } else {
                let attempted_members = attempted_members.clone();
                let combo_headers = headers_map.clone();
                execute_combo_strategy_full(
                    &augmented_models,
                    Some(&combo_name),
                    strategy,
                    &disabled_members,
                    sticky_limit,
                    Some(&required_caps),
                    &snapshot.pricing,
                    capacity_check,
                    move |combo_model| {
                        let state = combo_state.clone();
                        let mut body = combo_body.clone();
                        let combo_model = combo_model.to_string();
                        let api_key = combo_api_key.clone();
                        let headers = combo_headers.clone();
                        // 9router parity: history stripping applies ONLY to
                        // models the capacity adapter added — never to the
                        // original combo members.
                        if adapter_added.contains(&combo_model) {
                            let context_window = crate::core::model::catalog::provider_catalog()
                                .find_model(
                                    combo_model.split('/').next().unwrap_or(""),
                                    combo_model.split('/').nth(1).unwrap_or(""),
                                )
                                .and_then(|m| m.context_window.map(u64::from));
                            strip_history_for_context(&mut body, context_window);
                        }
                        attempted_members.lock().push(combo_model.clone());
                        // Re-resolve provider/model for this combo entry so each
                        // iteration dispatches against the correct provider node
                        // (e.g. "custom/gpt-fail" -> provider "node-openai", model "gpt-fail").
                        let inner_snapshot = state.db.snapshot();
                        let combo_resolved = get_model_info(&combo_model, &inner_snapshot);
                        tracing::warn!(
                            "COMBO model={} provider={:?} model_resolved={:?}",
                            combo_model,
                            combo_resolved.provider,
                            combo_resolved.model,
                        );
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
                        // Accept header not available inside combo closure — use body only
                        apply_stream_plan(
                            &mut combo_plan,
                            &body,
                            None,
                            client_tool_for_combo,
                            None,
                        );
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
                                Some(&headers),
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
            // 9router parity (chat.js:143-158): a SOLO provider registered on
            // a capacity adapter gets a fallback chain too. Without this a
            // plain provider whose capacity is exhausted has nowhere to go and
            // the request fails outright, while the same provider sitting
            // inside a combo would have fallen back to the pool.
            let required_caps = detect_required_capabilities(&body);
            let solo_chain = augment_models_with_capacity_adapter(
                std::slice::from_ref(&model_str.to_string()),
                &required_caps,
                &snapshot.settings.capacity_adapter,
            );
            // History stripping applies only to models the adapter added,
            // never to the one the client actually asked for.
            let solo_adapter_added: HashSet<String> = solo_chain
                .iter()
                .filter(|m| *m != model_str)
                .cloned()
                .collect();

            let mut last_error = None;
            let mut solo_response = None;
            for candidate in &solo_chain {
                let mut plan = RequestPlan::new(
                    endpoint,
                    &body,
                    resolved.provider.as_deref().unwrap_or(model_str),
                    &resolved.model,
                );
                plan.passthrough = is_native_passthrough(client_tool, &plan.provider);
                apply_stream_plan(
                    &mut plan,
                    &body,
                    accept_header.as_deref(),
                    client_tool,
                    None,
                );

                let mut attempt_body = body.clone();
                if solo_adapter_added.contains(candidate) {
                    let context_window = crate::core::model::catalog::provider_catalog()
                        .find_model(
                            candidate.split('/').next().unwrap_or(""),
                            candidate.split('/').nth(1).unwrap_or(""),
                        )
                        .and_then(|m| m.context_window.map(u64::from));
                    strip_history_for_context(&mut attempt_body, context_window);
                }

                match execute_single_model(
                    &state,
                    &attempt_body,
                    candidate,
                    presented_api_key.as_deref(),
                    endpoint,
                    &plan,
                    client_tool,
                    Some(&headers_map),
                )
                .await
                {
                    Ok(response) => {
                        solo_response = Some(response);
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(
                            "SOLO-CAPACITY model={} adapter={} error={:?}",
                            candidate,
                            solo_adapter_added.contains(candidate),
                            error
                        );
                        last_error = Some(error);
                    }
                }
            }

            match solo_response {
                Some(response) => response,
                None => attempt_error_response(last_error.unwrap_or(
                    crate::core::combo::ComboAttemptError {
                        status: 502,
                        message: "no capacity adapter candidate succeeded".to_string(),
                        retry_after: None,
                        upstream_body: None,
                    },
                )),
            }
        }
    };

    // Feature4: populate the cache on a successful non-streaming miss.
    // Gated on the same resolved decision as the lookup above so a live SSE
    // body is never stored under a JSON cache key. Skipped for
    // simulation-controlled requests (see lookup bypass above), and for combo
    // routes for the same reason — the guard and the combo leg resolve the
    // stream decision from different inputs, so neither side can be trusted
    // to agree with the other.
    if is_cacheable_route && !is_sse_response && !sim_controlled {
        return cache_miss_response(&state, &body, cache_provider, response).await;
    }
    response
}

/// Inject provider-level thinking override onto the **source** body
/// (before translation). 9router chatCore.js:68-80.
fn inject_provider_thinking(body: &mut Value, settings: &crate::types::Settings, provider: &str) {
    let Some(provider_thinking) = settings
        .extra
        .get("providerThinking")
        .and_then(|v| v.as_object())
    else {
        return;
    };
    let Some(mode_val) = provider_thinking.get(provider) else {
        return;
    };
    let mode = mode_val.as_str().unwrap_or("auto");
    if mode == "auto" {
        return;
    }
    // JS: !body.thinking (any truthy) / !body.reasoning_effort
    let has_thinking = body
        .get("thinking")
        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false));
    let has_effort = body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());

    if mode == "on" && !has_thinking {
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "thinking".to_string(),
                json!({"type": "enabled", "budget_tokens": 10000}),
            );
        }
    } else if mode == "off" && !has_thinking {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("thinking".to_string(), json!({"type": "disabled"}));
        }
    } else if mode != "on" && mode != "off" && !has_effort {
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "reasoning_effort".to_string(),
                Value::String(mode.to_string()),
            );
        }
    }
}

/// Prefetch remote images in OpenAI/Claude message content arrays.
async fn prefetch_images_in_messages(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    let client = reqwest::Client::new();
    for msg in messages.iter_mut() {
        let content_array = match msg.get_mut("content") {
            Some(Value::Array(arr)) => arr,
            _ => continue,
        };
        for part in content_array.iter_mut() {
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
            if let Some(source) = part.get("image").and_then(|im| im.get("source")) {
                if source.get("type").and_then(|t| t.as_str()) == Some("url") {
                    if let Some(url) = source.get("url").and_then(|u| u.as_str()) {
                        if url.starts_with("http://") || url.starts_with("https://") {
                            if let Some(fetched) = fetch_image_as_base64(&client, url).await {
                                if let Some(src) = part
                                    .get_mut("image")
                                    .and_then(|im| im.get_mut("source"))
                                    .and_then(|s| s.as_object_mut())
                                {
                                    src.insert("data".into(), Value::String(fetched.data_url));
                                    src.insert("type".into(), Value::String("base64".into()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Feature4: cache a successful response and tag it `X-Cache: MISS`.
async fn cache_miss_response(
    state: &AppState,
    body: &Value,
    provider: &str,
    response: Response,
) -> Response {
    if !response.status().is_success() {
        // Don't cache errors; just mark the miss.
        let mut response = response;
        response
            .headers_mut()
            .insert("x-cache", HeaderValue::from_static("MISS"));
        return response;
    }

    let headers = response.headers().clone();
    let bytes = match axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            // Body unreadable (should not happen for non-streaming JSON).
            let mut err = Response::new(Body::from(""));
            *err.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            return err;
        }
    };

    state
        .response_cache
        .set(body, bytes.to_vec(), provider, None);

    let mut resp = Response::new(Body::from(bytes));
    *resp.headers_mut() = headers;
    resp.headers_mut()
        .insert("x-cache", HeaderValue::from_static("MISS"));
    resp
}

/// GET /api/cache/stats — response-cache hit-rate counters for the dashboard.
///
/// Returns the live `hits` / `misses` / `sets` / `entries` counts and the
/// derived `hit_rate` (`hits / (hits + misses)`). Cheap: counters are atomic
/// and the entry count is a single DashMap len.
pub async fn cache_stats(State(state): State<AppState>) -> Response {
    let stats = state.response_cache.stats();
    Json(json!({
        "hits": stats.hits,
        "misses": stats.misses,
        "sets": stats.sets,
        "entries": stats.entries,
        "hit_rate": stats.hit_rate,
    }))
    .into_response()
}

/// Apply 9router stream decision to a RequestPlan (mutates stream + sse_to_json).
fn apply_stream_plan(
    plan: &mut RequestPlan,
    body: &Value,
    accept: Option<&str>,
    client_tool: Option<ClientTool>,
    model_type: Option<&str>,
) {
    let body_stream = body.get("stream").and_then(Value::as_bool);
    let sp = resolve_stream_flags(
        body_stream,
        accept,
        &plan.provider,
        &plan.model,
        plan.source_format,
        client_tool,
        model_type,
    );
    plan.stream = sp.stream;
    plan.sse_to_json = sp.sse_to_json;
    tracing::debug!(
        target: "openproxy::chat",
        "STREAM provider={} stream={} client_requested={} force={} sse_to_json={}",
        plan.provider,
        sp.stream,
        sp.client_requested_streaming,
        sp.provider_forced,
        sp.sse_to_json,
    );
}

/// Dispatch one fusion leg (panel, judge, or deferred final leg).
///
/// - Panel legs pass `force_stream = Some(false)` (createPanelBody parity).
/// - The deferred judge/survivor leg passes `force_stream = None`, so the
///   ORIGINAL client stream flag drives the plan — SSE flows untouched.
async fn dispatch_fusion_leg(
    state: &AppState,
    original_body: &Value,
    leg_body: &Value,
    model: &str,
    api_key: Option<&str>,
    endpoint: Option<&'static str>,
    client_tool: Option<ClientTool>,
    headers: &std::collections::HashMap<String, String>,
    force_stream: Option<bool>,
) -> Result<Response, ComboAttemptError> {
    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model, &snapshot);
    let provider = resolved
        .provider
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    let resolved_model = resolved.model.clone();
    let mut plan = RequestPlan::new(endpoint, original_body, &provider, &resolved_model);
    plan.passthrough = is_native_passthrough(client_tool, &provider);
    plan.stream = force_stream.unwrap_or_else(|| {
        original_body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    });
    plan.sse_to_json = false;
    execute_single_model(
        state,
        leg_body,
        &resolved_model,
        api_key,
        endpoint,
        &plan,
        client_tool,
        Some(headers),
    )
    .await
}

async fn execute_single_model(
    state: &AppState,
    request_body: &Value,
    model_str: &str,
    api_key: Option<&str>,
    endpoint: Option<&'static str>,
    plan: &RequestPlan,
    client_tool: Option<ClientTool>,
    client_headers: Option<&std::collections::HashMap<String, String>>,
) -> Result<Response, ComboAttemptError> {
    let snapshot = state.db.snapshot();

    // Bead openproxy-umtq: simulation mock must engage for EVERY provider,
    // not just the OpenAI-shaped else-arm of the dispatch ladder. When the
    // effective mode is `mock` we neutralize the plan's `target_format` to
    // the client's `source_format` so the request is NOT translated into a
    // provider-native dialect and the simulated envelope is NOT re-translated
    // on the way back. The mock executor renders in the source dialect
    // (`sim_format_for_source`) and the envelope round-trips verbatim.
    let sim_header_value = client_headers
        .and_then(|headers| headers.get("x-openproxy-sim"))
        .map(String::as_str);
    let mock_active = effective_mock_for(state, &plan.provider, sim_header_value);
    let neutralized_plan: Option<RequestPlan> = if mock_active {
        let mut neutral = plan.clone();
        neutral.target_format = neutral.source_format;
        Some(neutral)
    } else {
        None
    };
    let plan: &RequestPlan = neutralized_plan.as_ref().unwrap_or(plan);

    // 9router chatCore.js:229 — the `x-9router-token-saver` request header
    // opts a single request out of RTK/headroom/caveman/ponytail when its
    // value is the literal "off" (case-insensitive). Absent header (or any
    // other value incl. "") keeps savers ON.
    let token_saver_enabled = client_headers
        .map(|h| {
            h.get("x-9router-token-saver")
                .map(|v| !v.eq_ignore_ascii_case("off"))
                .unwrap_or(true)
        })
        .unwrap_or(true);

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

    // Strip control characters before the body is translated/forwarded: some
    // providers reject raw C0 bytes, and clients emit them from copied terminal
    // output (9router parity: sanitizeInput).
    crate::server::api::sanitization::sanitize_request_body(&mut body);

    // 0. providerThinking on SOURCE body BEFORE translate (9router chatCore.js:68-80)
    inject_provider_thinking(&mut body, &snapshot.settings, &plan.provider);

    // Catalog stripList (image/audio) before modality strip — 9router translateRequest stripList
    if !plan.strip_list.is_empty() {
        let refs: Vec<&str> = plan.strip_list.iter().map(String::as_str).collect();
        registry::strip_content_types(&mut body, &refs);
    }

    // 1–2. Modality strip + image prefetch only when NOT passthrough (9router)
    if !plan.passthrough {
        let caps = capabilities_for_format(plan.source_format);
        strip_unsupported_modalities(&mut body, plan.source_format, &caps);

        if plan.target_format.needs_image_prefetch() {
            prefetch_images_in_messages(&mut body).await;
        }
    }

    // Dispatch uses catalog upstreamModelId when set
    let dispatch_model = plan.dispatch_model().to_string();
    if let Some(fields) = body.as_object_mut() {
        fields.insert("model".into(), Value::String(dispatch_model.clone()));
    }

    // 3. Translate or native passthrough normalize
    if plan.passthrough {
        tracing::debug!(
            target: "openproxy::chat",
            "PASSTHROUGH client={:?} provider={}",
            client_tool,
            plan.provider
        );
        if client_tool == Some(ClientTool::Claude) {
            crate::core::translator::request::claude_format::normalize_claude_passthrough(
                &mut body,
                &dispatch_model,
            );
        }
    } else if plan.needs_translation() {
        // Include rawHeaders so Kiro session-replay can resolve a stable
        // conversationId from client session headers (x-session-id, etc.).
        let mut creds = json!({
            "provider": plan.provider,
        });
        if let Some(headers) = client_headers {
            if let Some(obj) = creds.as_object_mut() {
                let raw: serde_json::Map<String, Value> = headers
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect();
                obj.insert("rawHeaders".into(), Value::Object(raw));
            }
        }
        let strip_refs: Vec<&str> = plan.strip_list.iter().map(String::as_str).collect();
        // Snapshot _customToolNames BEFORE translate_request_with_strip
        // strips it (translator-only metadata for the response path).
        let custom_tool_names = body
            .get("_customToolNames")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        registry::global_registry().translate_request_with_strip(
            plan.source_format,
            plan.target_format,
            &dispatch_model,
            &mut body,
            plan.stream,
            Some(&creds),
            if strip_refs.is_empty() {
                None
            } else {
                Some(&strip_refs)
            },
        );
        // Thread custom-tool names through to the response path (9router
        // chatCore.js:198 + streamingHandler customToolNames).
        if !custom_tool_names.is_empty() {
            let joined = custom_tool_names.join(",");
            if let Some(obj) = body.as_object_mut() {
                obj.insert("_customToolNames".into(), Value::String(joined));
            }
        }
    }

    // 3b. Re-apply model(level) thinking onto provider-native fields
    // (9router applyThinking after translate). Suffix overrides; without a
    // suffix, leave providerThinking / client fields untouched.
    crate::core::utils::thinking_suffix::reapply_thinking_after_translate(
        plan.target_format,
        &plan.provider,
        &dispatch_model,
        &mut body,
        plan.thinking_level.as_deref(),
        plan.stream,
    );

    // 4. RTK tool-result compression (after translate — 9router parity)
    let compression_stats: Option<CompressionStats> = compress_messages(
        &mut body,
        token_saver_enabled && snapshot.settings.rtk_enabled,
    )
    .map(|rtk_stats| CompressionStats {
        bytes_before: rtk_stats.bytes_before as u64,
        bytes_after: rtk_stats.bytes_after as u64,
        bytes_saved: rtk_stats.hits.iter().map(|h| h.saved as u64).sum(),
        image_prompts: rtk_stats.image_prompts as u64,
    });

    // 5. Headroom (after translate — 9router parity; format = final body shape)
    {
        let headroom_cfg = HeadroomConfig {
            enabled: token_saver_enabled && snapshot.settings.headroom_enabled,
            url: snapshot.settings.headroom_url.clone(),
            timeout_ms: snapshot.settings.headroom_timeout_ms,
            compress_user_messages: snapshot.settings.headroom_compress_user_messages,
        };
        let final_is_claude = (plan.passthrough && plan.source_format == Format::Claude)
            || (!plan.passthrough && plan.target_format == Format::Claude);
        // 9router parity: dispatch the headroom pass on the final body format.
        // Kiro stays a Kiro-shaped body; Responses-API gets its own path.
        let headroom_format = if final_is_claude {
            "claude"
        } else if plan.target_format == Format::Kiro || plan.source_format == Format::Kiro {
            "kiro"
        } else if plan.target_format == Format::OpenAiResponses
            || plan.source_format == Format::OpenAiResponses
        {
            "openai-responses"
        } else {
            "openai"
        };
        if let Ok(body_str) = serde_json::to_string(&body) {
            let est_tokens = body_str.len().div_ceil(4);
            if est_tokens > 0 {
                tracing::debug!(
                    "headroom input ~{} tokens (estimated from body size)",
                    est_tokens
                );
            }
        }
        let mut headroom_diag = crate::core::rtk::headroom::HeadroomDiagnostics::default();
        if let Some(stats) = compress_with_headroom_diag(
            &mut body,
            &headroom_cfg,
            &plan.model,
            headroom_format,
            None,
            Some(&mut headroom_diag),
        )
        .await
        {
            tracing::debug!("{}", stats.format_headroom_log().unwrap_or_default());
        }
        let size_log = crate::core::rtk::headroom::format_headroom_size_log(&headroom_diag);
        if !size_log.is_empty() {
            tracing::debug!("headroom {size_log}");
        }
        if let Some(reason) = &headroom_diag.reason {
            tracing::debug!("headroom skip={reason}");
        }
    }

    // 6. Caveman + Ponytail (after translate — 9router parity; gated by the
    //    per-request token-saver header like JS chatCore.js:252,258)
    let _ = if token_saver_enabled {
        apply_request_preprocessing(&mut body, &snapshot.settings, &plan.model)
    } else {
        false
    };

    // 7. Tool dedupe for Claude clients (after translate, before dispatch)
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

    // 7b. Pin cache breakpoints LAST on Claude passthrough (9router
    // chatCore.js:306): every saver above can reshape system/tools/messages,
    // and a stale anchor costs a full prefix rewrite.
    if plan.passthrough && client_tool == Some(ClientTool::Claude) {
        crate::core::translator::request::claude_format::anchor_claude_cache(&mut body);
    }

    // 8. TTS models: strip tool messages + tools (9router chatCore.js:185-189).
    if is_tts_request(&plan.provider, &plan.model) {
        if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            msgs.retain(|m| m.get("role").and_then(|r| r.as_str()) != Some("tool"));
        }
        if let Some(obj) = body.as_object_mut() {
            obj.remove("tools");
        }
    }

    // Sync stream flag onto body for executors that read body.stream
    if let Some(obj) = body.as_object_mut() {
        obj.insert("stream".into(), Value::Bool(plan.stream));
    }

    tracing::debug!(
        target: "openproxy::chat",
        "PLAN provider={} model={} upstream={} source={:?} target={:?} stream={} translate={} transport={:?} strip={:?}",
        plan.provider,
        plan.model,
        dispatch_model,
        plan.source_format,
        plan.target_format,
        plan.stream,
        plan.needs_translation(),
        plan.transport_base_url,
        plan.strip_list,
    );

    // Simulation control headers (bead sim-04): extract x-openproxy-sim-* from
    // the incoming client headers. Never forwarded upstream (stripped at send).
    let mut sim_headers = HeaderMap::new();
    if let Some(ch) = client_headers {
        for (k, v) in ch {
            let kl = k.to_ascii_lowercase();
            if kl == "x-openproxy-sim" || kl.starts_with("x-openproxy-sim-") {
                if let (Ok(name), Ok(val)) = (
                    kl.parse::<reqwest::header::HeaderName>(),
                    v.parse::<reqwest::header::HeaderValue>(),
                ) {
                    sim_headers.insert(name, val);
                }
            }
        }
    }
    forward_with_provider_fallback(
        state,
        &plan.provider,
        &dispatch_model,
        body,
        sim_headers,
        api_key,
        endpoint,
        plan,
        client_tool,
        compression_stats,
    )
    .await
}

/// Whether the effective simulation mode for `provider` is `mock` on this
/// request (bead openproxy-umtq).
///
/// Mirrors the resolution the status endpoint uses (`status_for`, which folds
/// in `OPENPROXY_DEV_MOCK` / `settings.dev_mock_all` / the per-provider
/// configured mode) so `/api/mock/status` and the execution path can never
/// disagree: anything the status surface reports as `effective:mock` really
/// executes in mock mode here. `sim_header_value` is the per-request
/// `x-openproxy-sim` header (a per-request promotion the status surface
/// documents but cannot see); pass `None` when absent.
fn effective_mock_for(state: &AppState, provider: &str, sim_header_value: Option<&str>) -> bool {
    use crate::core::simulation::ProviderExecutionMode;
    let settings_force = state.db.snapshot().settings.dev_mock_all;
    let configured_mock = state
        .db
        .sqlite
        .with_conn(|conn| {
            let status = crate::core::simulation::status_for(conn, provider, settings_force);
            Ok::<_, rusqlite::Error>(status.effective == ProviderExecutionMode::Mock)
        })
        .unwrap_or(false);
    if configured_mock {
        return true;
    }
    sim_header_value.is_some_and(|v| v.trim().eq_ignore_ascii_case("mock"))
}

/// Map the client's source `Format` to the simulation `ProviderFormat` the
/// mock executor should render (bead openproxy-umtq).
///
/// The simulator only speaks OpenAI / Anthropic / Gemini dialects. Because the
/// short-circuit neutralizes `target_format` to `source_format` (see
/// `execute_single_model`), the envelope is rendered in the *client's* dialect
/// and returned verbatim. Exotic provider-native sources that have no
/// simulator (Kiro, Codex, Cursor, …) fall back to OpenAI — the universal
/// simulation dialect — rather than failing the whole mock.
fn sim_format_for_source(source: Format) -> crate::core::executor::ProviderFormat {
    use crate::core::executor::ProviderFormat;
    match source {
        Format::Claude => ProviderFormat::Anthropic,
        Format::Gemini | Format::GeminiCli => ProviderFormat::Gemini,
        _ => ProviderFormat::OpenAI,
    }
}

async fn forward_with_provider_fallback(
    state: &AppState,
    provider: &str,
    model: &str,
    mut request_body: Value,
    sim_headers: HeaderMap,
    api_key: Option<&str>,
    endpoint: Option<&'static str>,
    plan: &RequestPlan,
    client_tool: Option<ClientTool>,
    compression: Option<CompressionStats>,
) -> Result<Response, ComboAttemptError> {
    let mut excluded = HashSet::new();
    let mut last_error: Option<ComboAttemptError> = None;
    let mut reloaded = false;
    let registry = &state.account_registry;

    // Bead openproxy-i8fi: connections whose OAuth token was already refreshed
    // during THIS request. Guards the refresh arm from re-refreshing a
    // connection whose freshly-refreshed token is still rejected.
    let mut refreshed_this_request: HashSet<String> = HashSet::new();

    // Bead openproxy-i8fi (P0): hard ceiling on dispatch iterations.
    //
    // This loop has no `break` and no attempt counter — its only exits are
    // `return`. Every error arm is expected to `continue` AFTER inserting the
    // connection into `excluded`, so the candidate set shrinks and the loop
    // drains. The 401/403 OAuth-refresh arm did not: on a successful refresh
    // it persisted the new token and `continue`d WITHOUT excluding the
    // connection, leaving `select_connection` free to re-pick the very same
    // connection. An upstream that keeps answering 401 while its refresh
    // endpoint keeps succeeding (a revoked-but-refreshable account, a scope
    // the provider refuses, a token it accepts then rejects) therefore spun
    // forever, pinning a worker and an in-flight slot — a self-inflicted DoS.
    //
    // The budget is the backstop that makes termination structural rather
    // than dependent on every arm remembering to advance. Size it from the
    // connection count so a legitimate multi-account fallback is never
    // truncated: each account may be dispatched once, then retried once after
    // its refresh, plus a small constant for the stale-snapshot reload and the
    // final no-candidate check.
    let attempt_budget = {
        let snapshot = state.db.snapshot();
        let accounts = snapshot
            .provider_connections
            .iter()
            .filter(|connection| connection.provider == provider)
            .count();
        accounts.saturating_mul(2).saturating_add(2)
    };
    let mut attempts = 0usize;

    // Bead openproxy-umtq: resolve the effective simulation mode ONCE, before
    // the dispatch loop. The per-request `x-openproxy-sim` header is the one
    // signal `status_for` cannot see, so the effective mode here = (status
    // effective == mock) OR (header promotes real→mock). The dispatch ladder
    // below short-circuits to the simulator when this is true, so mock mode is
    // honored for EVERY provider — not just the OpenAI-shaped else-arm.
    let sim_header_value = sim_headers
        .get(crate::core::simulation::SIM_HEADER)
        .and_then(|value| value.to_str().ok());
    let mock_active = effective_mock_for(state, provider, sim_header_value);
    // The simulator renders in the client's dialect; the plan passed in was
    // already neutralized (target := source) by `execute_single_model`.
    let mock_sim_format = sim_format_for_source(plan.source_format);

    // Per-key monthly budget kill-switch (free-tier Feature 3): block the
    // request with 429 before any provider dispatch when the cap is reached.
    let budget_remaining = match crate::server::api::budget_guard::enforce_budget(state, api_key) {
        Ok(remaining) => remaining,
        Err(response) => return Ok(response),
    };

    // Extract tool name map from body (set by Claude cloaking).
    // Remove from body before dispatch to avoid serializing it upstream.
    let tool_name_map: Option<std::collections::BTreeMap<String, String>> = request_body
        .as_object_mut()
        .and_then(|obj| obj.remove("_toolNameMap"))
        .and_then(|v| serde_json::from_value(v).ok());

    // Extract custom-tool names (OpenAI Responses translator metadata).
    // Kept for the streaming response path; stripped from the body below.
    let custom_tool_names: Option<String> = request_body
        .as_object_mut()
        .and_then(|obj| obj.remove("_customToolNames"))
        .and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s),
            Value::Array(a) => {
                let names: Vec<String> = a
                    .iter()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect();
                if names.is_empty() {
                    None
                } else {
                    Some(names.join(","))
                }
            }
            _ => None,
        });

    loop {
        // Bead openproxy-i8fi: the iteration ceiling. Reaching it means some
        // arm re-entered the loop without advancing `excluded`; surface the
        // last upstream error instead of spinning forever.
        if attempts >= attempt_budget {
            let retry_after = last_error
                .as_ref()
                .and_then(|error| error.retry_after)
                .or_else(|| earliest_retry_after(&state.db.snapshot(), provider, model, &excluded));
            return Err(last_error.take().unwrap_or(ComboAttemptError {
                status: 503,
                message: format!(
                    "Provider dispatch exceeded the attempt budget for {provider}/{model}"
                ),
                retry_after,
                upstream_body: None,
            }));
        }
        attempts += 1;

        let snapshot = state.db.snapshot();
        // Simulation credentialless path (sim-16 dispatch completion): mock
        // mode needs NO credentials by design. When selection finds nothing
        // but the provider's effective mode is mock, a stub connection keeps
        // the loop on the normal dispatch tail — the executor mock branch
        // never reads credentials (audit-locked by test).
        // Single-shot: once the stub id is excluded (a stub attempt failed),
        // never recreate it, or error-loop iterations would spin forever.
        let stub: Option<ProviderConnection> = if !excluded
            .iter()
            .any(|id| id == &format!("sim-stub-{provider}"))
            && select_connection(&snapshot, provider, model, &excluded, Some(registry)).is_none()
        {
            let settings_force = snapshot.settings.dev_mock_all;
            let is_mock = state.db.sqlite.with_conn(|conn| {
                Ok::<_, rusqlite::Error>(crate::core::simulation::status_for(
                    conn,
                    provider,
                    settings_force,
                ))
            });
            match is_mock {
                Ok(s) if s.effective == crate::core::simulation::ProviderExecutionMode::Mock => {
                    let mut stub = ProviderConnection::default();
                    stub.id = format!("sim-stub-{provider}");
                    stub.provider = provider.to_string();
                    stub.auth_type = "apiKey".to_string();
                    stub.is_active = Some(true);
                    Some(stub)
                }
                _ => None,
            }
        } else {
            None
        };
        let Some(mut connection) = stub
            .or_else(|| select_connection(&snapshot, provider, model, &excluded, Some(registry)))
        else {
            let retry_after = earliest_retry_after(&snapshot, provider, model, &excluded);
            if let Some(mut error) = last_error {
                if retry_after.is_some() {
                    error.retry_after = retry_after;
                }
                return Err(error);
            }

            // Stale-snapshot recovery: if the CLI added a provider
            // connection while the server was running, the in-memory
            // snapshot won't have it. Reload from SQLite once and retry.
            if !reloaded && retry_after.is_none() {
                reloaded = true;
                if state.db.reload_snapshot().await.is_ok() {
                    continue;
                }
            }

            // 9router chat.js:237-249 splits these on what the credential
            // lookup actually reported, not on `retry_after` alone:
            //   * every account rate-limited → 503 + Retry-After
            //   * nothing excluded → no account was ever attempted, so the
            //     provider has no usable credential at all → 404
            //   * otherwise the accounts were tried and ran out → 503
            // The 503 body is deliberately not 9router's `[provider/model]
            // <lastError> (reset after 4m 12s)` (chat.js:238-242):
            // friendly_error_message replaces that whole message with canned
            // English and strips the `[…]` prefix before the client sees it.
            if retry_after.is_some() {
                return Err(ComboAttemptError {
                    status: 503,
                    message: format!("All accounts for {provider}/{model} are cooling down"),
                    retry_after,
                    upstream_body: None,
                });
            }

            if excluded.is_empty() {
                return Err(ComboAttemptError {
                    status: 404,
                    message: format!("No active credentials for provider: {provider}"),
                    retry_after: None,
                    upstream_body: None,
                });
            }

            return Err(ComboAttemptError {
                status: 503,
                message: format!("All accounts for {provider}/{model} are unavailable"),
                retry_after: None,
                upstream_body: None,
            });
        };

        // 9router resolveTransport: pin multi-endpoint base URL for this request
        if let Some(ref base) = plan.transport_base_url {
            connection.runtime_transport = Some(crate::types::RuntimeTransport {
                base_url: Some(base.clone()),
            });
        }

        // get_model_info resolves openai-compatible/anthropic-compatible
        // nodes to their node NAME as the provider — match on name OR prefix
        // so the node-aware DefaultExecutor path is taken for both.
        let provider_node = snapshot
            .provider_nodes
            .iter()
            .find(|node| {
                node.id == provider
                    || node.prefix.as_deref() == Some(provider)
                    || (node.r#type.ends_with("-compatible") && node.name == provider)
            })
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

        // Stream flag already resolved on plan via resolve_stream_flags
        // (DeepSeek-TUI, forceStream, Accept, imageGen — 9router parity).
        let stream = plan.stream;
        if let Some(obj) = request_body.as_object_mut() {
            obj.insert("stream".into(), Value::Bool(stream));
        }

        state
            .usage_live
            .start_request(model, provider, Some(connection.id.as_str()))
            .await;

        use crate::core::executor::{
            AntigravityExecutionRequest, AntigravityExecutor, AzureExecutionRequest, AzureExecutor,
            CodexExecutionRequest, CodexExecutor, CommandCodeExecutionRequest, CommandCodeExecutor,
            CursorExecutionRequest, CursorExecutor, DefaultExecutor, DevinCliExecutor,
            DevinExecutionRequest, ExecutionRequest, ExecutorError, GeminiCliExecutionRequest,
            GeminiCliExecutor, GithubExecutionRequest, GithubExecutor, GrokWebExecutionRequest,
            GrokWebExecutor, IFlowExecutionRequest, IFlowExecutor, KimchiExecutor,
            KiroExecutionRequest, KiroExecutor, KiroExecutorResponse, OpenCodeExecutionRequest,
            OpenCodeExecutor, OpenCodeGoExecutionRequest, OpenCodeGoExecutor,
            PerplexityWebExecutionRequest, PerplexityWebExecutor, ProviderExecutionRequest,
            ProviderExecutor, QoderExecutionRequest, QoderExecutor, QwenExecutionRequest,
            QwenExecutor, TraeExecutionRequest, TraeExecutor, VertexExecutionRequest,
            VertexExecutor, WindsurfExecutionRequest, WindsurfExecutor, XaiExecutionRequest,
            XaiExecutor,
        };

        let is_codex_model = model.starts_with("codex/") || provider == "codex";
        let is_cursor_model =
            model.starts_with("cursor/") || provider == "cu" || provider == "cursor";
        let executor_result: Result<KiroExecutorResponse, ComboAttemptError> =
            if mock_active {
                // Bead openproxy-umtq: simulation short-circuit. When the
                // effective mode is `mock`, route EVERY provider through
                // `DefaultExecutor` — the only executor that implements the
                // simulator — instead of entering the per-provider match. The
                // mock branch never reads credentials, base_url, or the pool,
                // so this works for providers with a dedicated arm (kiro,
                // codex, cursor, …) that have no `PROVIDER_CONFIGS` entry.
                let executor = crate::core::executor::DefaultExecutor::new_for_mock(
                    provider.to_string(),
                    mock_sim_format,
                    state.client_pool.clone(),
                );
                let result = executor
                    .execute(ExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                        sim_headers: sim_headers.clone(),
                        force_mock: true,
                    })
                    .await
                    .map_err(|err| err.into_combo_attempt_error())?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "kiro" {
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
            } else if provider == "vertex" || provider == "vertex-partner" || provider == "vxp" {
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
            } else if provider == "xai" {
                // Dedicated XaiExecutor (was falling through to DefaultExecutor).
                // Registry extras (xai.js): responsesUrl + image/video/search
                // configs live in provider_catalog/media layers; the chat path
                // only needs the wired executor with its grok-cli UA + Bearer.
                let executor =
                    XaiExecutor::new(state.client_pool.clone(), provider_node).map_err(|e| {
                        ComboAttemptError {
                            status: 500,
                            message: format!("Xai executor creation failed: {:?}", e),
                            retry_after: None,
                            upstream_body: None,
                        }
                    })?;
                let result = executor
                    .execute_request(XaiExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Xai execution failed: {:?}", e),
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
                        raw_headers: std::collections::BTreeMap::new(),
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
                        raw_headers: std::collections::BTreeMap::new(),
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
            } else if provider == "deepseek-web" || provider == "ds-web" {
                use crate::core::executor::{DeepSeekWebExecutionRequest, DeepSeekWebExecutor};
                let executor = DeepSeekWebExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(DeepSeekWebExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("DeepSeekWeb execution failed: {:?}", e),
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
            } else if provider == "windsurf" || provider == "ws" {
                let executor = WindsurfExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(WindsurfExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Windsurf execution failed: {:?}", e),
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
            } else if provider == "zed" {
                use crate::core::executor::{ZedExecutionRequest, ZedExecutor};
                let executor = ZedExecutor::new(state.client_pool.clone())
                    .unwrap_or_else(|e: std::convert::Infallible| match e {});
                let result = executor
                    .execute_request(ZedExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Zed execution failed: {}", e),
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
            } else if provider == "trae" {
                let executor = TraeExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(TraeExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Trae execution failed: {:?}", e),
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
            } else if provider == "devin-cli" || provider == "dv" {
                // ACP stdio executor — spawns `devin acp` (noAuth; the CLI
                // carries its own credentials) and bridges session/update
                // notifications to OpenAI SSE.
                let executor = DevinCliExecutor::new(state.client_pool.clone()).map_err(|e| {
                    ComboAttemptError {
                        status: 500,
                        message: format!("Devin executor init failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    }
                })?;
                let result = executor
                    .execute_request(DevinExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Devin execution failed: {}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url.clone(),
                    headers: HeaderMap::new(),
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "kimchi" {
                let executor = KimchiExecutor::new(state.client_pool.clone(), provider_node);
                let result = executor
                    .execute(ProviderExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                        signal: None,
                        log: None,
                        proxy_options: None,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Kimchi execution failed: {:?}", e),
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
            } else if provider == "codebuddy-cn" || provider == "cbcn" {
                use crate::core::executor::CodeBuddyCNExecutor;
                let executor =
                    CodeBuddyCNExecutor::new(state.client_pool.clone(), provider_node.clone());
                let result = executor
                    .execute(ProviderExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream: true, // force stream (9router)
                        credentials: connection.clone(),
                        proxy,
                        signal: None,
                        log: None,
                        proxy_options: None,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("CodeBuddy CN execution failed: {:?}", e),
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
            } else if provider == "codebuddy-intl" || provider == "cbai" {
                use crate::core::executor::CodeBuddyIntlExecutor;
                let executor =
                    CodeBuddyIntlExecutor::new(state.client_pool.clone(), provider_node.clone());
                let result = executor
                    .execute(ProviderExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream: true, // registry forceStream (JS #11101 fix)
                        credentials: connection.clone(),
                        proxy,
                        signal: None,
                        log: None,
                        proxy_options: None,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("CodeBuddy intl execution failed: {:?}", e),
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
            } else if provider == "ollama-local" || provider == "ollama" {
                use crate::core::executor::{OllamaExecutionRequest, OllamaExecutor};
                let executor = OllamaExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(OllamaExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("Ollama execution failed: {:?}", e),
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
            } else if provider == "mimo-free" || provider == "mmf" {
                use crate::core::executor::{MimoFreeExecutionRequest, MimoFreeExecutor};
                let executor = MimoFreeExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(MimoFreeExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("MimoFree execution failed: {:?}", e),
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
            } else if provider == "grok-cli"
                || provider == "gcli"
                || provider == "gb"
                || provider == "grok-build"
            {
                use crate::core::executor::{GrokCliExecutionRequest, GrokCliExecutor};
                let executor = GrokCliExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("GrokCli executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(GrokCliExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream: true, // forceStream (9router)
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ComboAttemptError {
                        status: 500,
                        message: format!("GrokCli execution failed: {:?}", e),
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
                .map_err(|error| match error {
                    // 9router executors/index.js:67-71 never fails to hand back
                    // an executor: a provider it has no config for still builds
                    // a DefaultExecutor and fails later at the fetch, which is
                    // the 502 path (chatCore.js:398-402).
                    ExecutorError::UnsupportedProvider(name) => ComboAttemptError {
                        status: 502,
                        message: format!("[502]: no upstream configured for provider {name}"),
                        retry_after: None,
                        upstream_body: None,
                    },
                    // Unreachable today — `DefaultExecutor::new` returns nothing
                    // but the variant above. A 500 still reads as a server
                    // fault, so it keeps that status.
                    other => ComboAttemptError {
                        status: 500,
                        message: format!("Default executor creation failed: {other:?}"),
                        retry_after: None,
                        upstream_body: None,
                    },
                })?;
                let result = executor
                    .execute(ExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                        sim_headers: sim_headers.clone(),
                        // Resolver-wiring (follow-up openproxy-1ycq): the stub
                        // gate above already did the DB lookup, so a stubbed
                        // connection means effective mock — activate the branch
                        // even without the per-request header.
                        force_mock: connection.id.starts_with("sim-stub-"),
                    })
                    .await
                    .map_err(|err| err.into_combo_attempt_error())?;
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
                    clear_connection_error_for_model(state, &connection.id, Some(model)).await;
                    if dashboard_stream {
                        let response = proxy_dashboard_sse_with_usage_tracking(
                            result.response,
                            state,
                            provider,
                            model,
                            Some(connection.id.as_str()),
                            api_key,
                            endpoint,
                            compression.clone(),
                        )
                        .await;
                        return Ok(crate::server::api::budget_guard::with_budget_header(
                            response,
                            budget_remaining,
                        ));
                    }
                    // forceStream + client non-stream → collect SSE → JSON (9router)
                    if plan.sse_to_json {
                        tracing::debug!(
                            target: "openproxy::chat",
                            "FORCE_STREAM sse_to_json provider={} model={}",
                            provider,
                            model
                        );
                        let response = proxy_sse_to_json_response(
                            result.response,
                            state,
                            provider,
                            model,
                            Some(connection.id.as_str()),
                            api_key,
                            endpoint,
                            plan,
                            compression.clone(),
                        )
                        .await;
                        return Ok(crate::server::api::budget_guard::with_budget_header(
                            response,
                            budget_remaining,
                        ));
                    }
                    if !stream {
                        let response = proxy_response_with_usage_tracking(
                            result.response,
                            state,
                            provider,
                            model,
                            Some(connection.id.as_str()),
                            api_key,
                            endpoint,
                            plan,
                            tool_name_map.as_ref(),
                            compression.clone(),
                        )
                        .await;
                        return Ok(crate::server::api::budget_guard::with_budget_header(
                            response,
                            budget_remaining,
                        ));
                    }
                    let normalize_for_dashboard =
                        endpoint == Some("/api/dashboard/chat/completions");
                    let response = proxy_response_with_pending_tracking(
                        result.response,
                        state.clone(),
                        provider.to_string(),
                        model.to_string(),
                        Some(connection.id.clone()),
                        api_key,
                        endpoint,
                        normalize_for_dashboard,
                        plan,
                        tool_name_map.as_ref(),
                        compression.clone(),
                        custom_tool_names.clone(),
                    )
                    .await;
                    return Ok(crate::server::api::budget_guard::with_budget_header(
                        response,
                        budget_remaining,
                    ));
                }

                // 9router parity: retryAfter may come from the Retry-After header
                // OR the error JSON body (errorBody.retryAfter). Header wins; the
                // body is the fallback when a provider returns it only in JSON.
                let header_retry_after = retry_after_from_headers(result.response.headers());
                let (message, raw_body, body_retry_after) =
                    extract_error_message_and_retry_after_with_body(result.response).await;
                let retry_after = header_retry_after.or(body_retry_after);
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
                    // H23 (bead openproxy-i7yt): preserve the raw upstream
                    // error body so attempt_error_response can return it
                    // verbatim instead of a generic 500. Without this the
                    // FreeTierError/insufficient_quota text is lost and the
                    // client sees only "Internal server error".
                    upstream_body: raw_body,
                });

                // Token refresh: on 401/403, try to refresh the access token
                // before giving up on this connection (9router parity).
                // On success, merge credentials (expires_at, refresh, PSD) and
                // continue the loop so the fresh snapshot picks up the token.
                //
                // Bead openproxy-i8fi: refresh each connection at most ONCE per
                // request. A second 401/403 after a refresh that already
                // succeeded means the freshly-minted credential is being
                // rejected too, so refreshing again cannot help — and because
                // this arm does not exclude the connection, `select_connection`
                // would re-pick it and spin forever. `refreshed_this_request`
                // makes the repeat fall through to the normal
                // `decision.should_fallback` branch below, which excludes the
                // connection and lets the loop advance to the next account.
                if (status.as_u16() == 401 || status.as_u16() == 403)
                    && connection.refresh_token.is_some()
                    && !refreshed_this_request.contains(&connection.id)
                {
                    if let Some(ref rt) = connection.refresh_token.clone() {
                        let refresh_provider = plan.provider.as_str();
                        if let Ok(result) = crate::oauth::token_refresh::dispatch_oauth_refresh(
                            refresh_provider,
                            rt,
                            &connection.provider_specific_data,
                        )
                        .await
                        {
                            let conn_id = connection.id.clone();
                            let new_access = result.access_token.clone();
                            let new_refresh = result.refresh_token.clone();
                            let expires_at = result.expires_in.map(|secs| {
                                (Utc::now() + ChronoDuration::seconds(secs)).to_rfc3339()
                            });
                            let last_refresh_at = Utc::now().to_rfc3339();
                            let _ = state
                                .db
                                .update(move |db| {
                                    if let Some(conn) =
                                        db.provider_connections.iter_mut().find(|c| c.id == conn_id)
                                    {
                                        conn.access_token = Some(new_access);
                                        // Preserve old refresh_token when response omits it
                                        if let Some(rt) = new_refresh {
                                            conn.refresh_token = Some(rt);
                                        }
                                        if let Some(exp) = expires_at {
                                            conn.expires_at = Some(exp);
                                        }
                                        conn.provider_specific_data.insert(
                                            "lastRefreshAt".into(),
                                            Value::String(last_refresh_at),
                                        );
                                        conn.last_error = None;
                                        conn.last_error_at = None;
                                        // A refresh that produced a working
                                        // credential is a re-enable: 9router
                                        // expands any write landing on "active"
                                        // into a full health reset
                                        // (connectionsRepo.js:15-33), so the
                                        // model locks and the rate-limit window
                                        // go too.
                                        conn.test_status = Some("active".to_string());
                                        crate::core::account_fallback::reset_health_state_on_activation(conn);
                                    }
                                })
                                .await;
                            refreshed_this_request.insert(connection.id.clone());
                            continue;
                        }
                    }
                }

                if decision.should_fallback {
                    // 9router githubMonthlyResetMs: a GitHub 402 with the
                    // monthly-usage-limit message locks the ACCOUNT (model="")
                    // until the first of next month, and resets backoff to 0.
                    let github_reset = crate::core::account_fallback::github_monthly_reset_ms(
                        status.as_u16(),
                        &message,
                        &plan.provider,
                    );
                    if let Some(reset_at) = github_reset {
                        let cooldown_ms = (reset_at - Utc::now()).to_std().unwrap_or_default();
                        mark_connection_unavailable(
                            state,
                            &connection.id,
                            "",
                            status.as_u16(),
                            &message,
                            cooldown_ms,
                            0,
                        )
                        .await;
                        excluded.insert(connection.id.clone());
                        continue;
                    }
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        model,
                        status.as_u16(),
                        &message,
                        cooldown,
                        next_backoff_level(&decision, current_backoff),
                    )
                    .await;
                    excluded.insert(connection.id.clone());
                    continue;
                }

                return Err(last_error.unwrap_or_else(|| {
                    ComboAttemptError::new(502, "provider error after exhausting all connections")
                }));
            }
            Err(error) => {
                let message = format!("{:?}", error);
                state
                    .usage_live
                    .finish_request(model, provider, Some(connection.id.as_str()), true)
                    .await;
                let current_backoff = connection.backoff_level.unwrap_or(0);
                let decision = check_fallback_error(502, &message, current_backoff);
                let error_for_return = ComboAttemptError::new(502, message.clone());
                last_error = Some(error);

                if decision.should_fallback {
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        model,
                        502,
                        &message,
                        decision.cooldown,
                        next_backoff_level(&decision, current_backoff),
                    )
                    .await;
                    excluded.insert(connection.id.clone());
                    continue;
                }

                return Err(last_error.unwrap_or(error_for_return));
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
    compression: Option<CompressionStats>,
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
                compression,
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
    registry: Option<&crate::core::account_fallback::AccountRegistry>,
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

    if candidates.is_empty() {
        // No stored connection. Inject a virtual one for noAuth free providers
        // (matches 9router's getProviderCredentials behavior). Lets OpenCode Free,
        // edge-tts, google-tts, etc. route requests without manual setup.
        //
        // 9router parity guard (bead openproxy-sewn): `opencode` (Free) is
        // `noAuth: true` in the registry, but `opencode-go` (paid Go
        // subscription, `category: "apikey"`, NO noAuth flag) is NOT — a
        // stored API-key connection is mandatory. The virtual "public"
        // connection must never shadow this: without it, a valid stored key
        // is ignored and upstream answers `AuthError Missing API key.`
        // (note the trailing period — upstream's text, not ours).
        if is_no_auth_provider(provider) && !excluded.contains("noauth") {
            return Some(virtual_no_auth_connection(provider));
        }
        return None;
    }

    // Determine strategy for this provider.
    // Uses provider_strategies map, then the account-level fallbackStrategy,
    // finally FillFirst.
    let provider_override = snapshot.settings.provider_strategies.get(provider).cloned();
    let strategy = provider_override
        .as_ref()
        .and_then(|entry| entry.fallback_strategy())
        .and_then(|s| s.parse::<StrategyType>().ok())
        .or_else(|| {
            snapshot
                .settings
                .fallback_strategy
                .parse::<StrategyType>()
                .ok()
        })
        .unwrap_or(StrategyType::FillFirst);
    // 9router stickyRoundRobinLimit: per-provider override → settings default (3).
    let sticky_limit = provider_override
        .as_ref()
        .and_then(|e| e.sticky_round_robin_limit())
        .unwrap_or(snapshot.settings.sticky_round_robin_limit);

    match strategy {
        StrategyType::FillFirst | StrategyType::LeastLoaded => {
            if let Some(reg) = registry {
                let refs: Vec<&ProviderConnection> = candidates.iter().collect();
                if let Some(idx) = reg.select_account_by_strategy(&refs, strategy, None, 300) {
                    if let Some(conn) = candidates.get(idx).cloned() {
                        return Some(conn);
                    }
                }
            }
            // Fallback: sort by priority
            candidates.sort_by_key(|connection| connection.priority.unwrap_or(999));
            candidates.into_iter().next()
        }
        StrategyType::RoundRobin => {
            if let Some(reg) = registry {
                let refs: Vec<&ProviderConnection> = candidates.iter().collect();
                let combo_id = format!("provider_{}", provider);
                if let Some(idx) = reg.select_with_sticky_limit(
                    &refs,
                    StrategyType::RoundRobin,
                    Some(&combo_id),
                    300,
                    sticky_limit.max(1),
                ) {
                    if let Some(conn) = candidates.get(idx).cloned() {
                        return Some(conn);
                    }
                }
            }
            candidates.sort_by_key(|connection| connection.priority.unwrap_or(999));
            candidates.into_iter().next()
        }
        StrategyType::Sticky => {
            if let Some(reg) = registry {
                let refs: Vec<&ProviderConnection> = candidates.iter().collect();
                let combo_id = format!("provider_{}", provider);
                if let Some(idx) = reg.select_account_by_strategy(
                    &refs,
                    StrategyType::Sticky,
                    Some(&combo_id),
                    300,
                ) {
                    if let Some(conn) = candidates.get(idx).cloned() {
                        return Some(conn);
                    }
                }
            }
            candidates.sort_by_key(|connection| connection.priority.unwrap_or(999));
            candidates.into_iter().next()
        }
    }
}

fn is_no_auth_provider(provider: &str) -> bool {
    // 9router parity (bead openproxy-sewn): only `opencode` (Free) carries
    // `noAuth: true` in the registry. `opencode-go` is a paid apikey
    // provider with NO noAuth flag — it must never get the virtual "public"
    // connection, or a valid stored key is bypassed and upstream returns
    // `AuthError Missing API key.` for every model on the provider.
    matches!(
        provider,
        "opencode"
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

/// TTS gate (9router chatCore.js:185-189): strip tool messages + tools for
/// TTS models. Catalog-first — model `kind == "tts"` in
/// `provider_catalog.json` wins when the model is known (e.g. `kokoro`,
/// `gpt-4o-mini-tts`); fall back to name-substring for unknown models.
fn is_tts_request(provider: &str, model: &str) -> bool {
    let base = model.rsplit('/').next().unwrap_or(model);
    if crate::core::model::catalog::provider_catalog()
        .find_model(provider, base)
        .is_some_and(|m| m.kind == "tts")
    {
        return true;
    }
    let lower = model.to_lowercase();
    lower.contains("tts") || lower.contains("speech") || lower.starts_with("tts-")
}

/// Merge 9router nested comboStrategies[name] (judgeModel / fusionTuning) into FusionConfig.
fn fusion_config_for(snapshot: &AppDb, combo_name: &str, panel_count: usize) -> FusionConfig {
    let mut extra: serde_json::Map<String, Value> = snapshot
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

    if let Some(entry) = snapshot.settings.combo_strategies.get(combo_name) {
        if let Some(judge) = entry.judge_model() {
            extra.insert("judgeModel".into(), Value::String(judge.to_string()));
            // Also nest under fusionConfig for from_extra
            let mut fc = extra
                .get("fusionConfig")
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            fc.insert("judgeModel".into(), Value::String(judge.to_string()));
            extra.insert("fusionConfig".into(), Value::Object(fc));
        }
        if let Some(tuning) = entry.fusion_tuning() {
            if let Some(obj) = tuning.as_object() {
                let mut fc = extra
                    .get("fusionConfig")
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                for (k, v) in obj {
                    fc.insert(k.clone(), v.clone());
                }
                extra.insert("fusionConfig".into(), Value::Object(fc));
            }
        }
    }

    FusionConfig::from_extra(&extra, panel_count)
}

/// The backoff level to persist alongside a failed request.
///
/// 9router writes `backoffLevel: newBackoffLevel ?? backoffLevel` (auth.js:275,
/// accountFallback.js:211) — only a rule that carries `backoff: true` returns a
/// `newBackoffLevel`, so every other failure leaves the level where it was. A
/// `+ 1` on the fallback arm made a 400, 404 or 502 escalate the level just like
/// a 429, so a client-side model error ratcheted a connection towards the
/// 5-minute cap without ever hitting the rate-limit path that the level exists
/// to model.
fn next_backoff_level(decision: &FallbackDecision, current_backoff: u32) -> u32 {
    decision.new_backoff_level.unwrap_or(current_backoff)
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
    clear_connection_error_for_model(state, connection_id, None).await;
}

/// Selective lock clear (9router src/sse/services/auth.js:306-312):
/// succeeded model lock + account-level `modelLock___all` + expired locks.
/// Other active model locks survive (different-model failures stay locked).
fn retain_lock_after_success(
    extra: &mut std::collections::BTreeMap<String, Value>,
    succeeded_model: Option<&str>,
    now: DateTime<Utc>,
) {
    let model_key = succeeded_model.map(|m| format!("modelLock_{m}"));
    extra.retain(|k, v| {
        if !k.starts_with("modelLock_") {
            return true;
        }
        // Drop expired
        if let Some(exp) = v.as_str() {
            if let Ok(t) = DateTime::parse_from_rfc3339(exp) {
                if t.with_timezone(&Utc) <= now {
                    return false;
                }
            }
        }
        // Drop succeeded model lock + account-level lock
        if let Some(ref mk) = model_key {
            if k == mk {
                return false;
            }
        }
        if k == "modelLock___all" {
            return false;
        }
        true
    });
}

/// Clear error state; only remove expired model locks and optionally the
/// succeeded model lock (9router clearAccountError selective clear).
async fn clear_connection_error_for_model(
    state: &AppState,
    connection_id: &str,
    succeeded_model: Option<&str>,
) {
    let connection_id = connection_id.to_string();
    let succeeded_model = succeeded_model.map(|s| s.to_string());
    let now = Utc::now();
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
                retain_lock_after_success(&mut connection.extra, succeeded_model.as_deref(), now);
            }
        })
        .await;
}

/// forceStream SSE→JSON: collect upstream SSE and collapse to chat.completion JSON.
async fn proxy_sse_to_json_response(
    response: UpstreamResponse,
    state: &AppState,
    provider: &str,
    model: &str,
    connection_id: Option<&str>,
    api_key: Option<&str>,
    endpoint: Option<&str>,
    plan: &RequestPlan,
    compression: Option<CompressionStats>,
) -> Response {
    let status = response.status();
    let (body_bytes, body_complete) = collect_upstream_response_bytes(response).await;

    let mut json_body =
        crate::core::media::responses::stream_to_json::sse_stream_to_json(&body_bytes, Some(model))
            .unwrap_or_else(|| {
                // Fallback: try parse as JSON already, else wrap error
                serde_json::from_slice(&body_bytes).unwrap_or_else(|_| {
                    json!({
                        "error": {
                            "message": "Failed to convert forced SSE stream to JSON",
                            "type": "server_error",
                            "code": "sse_to_json_failed"
                        }
                    })
                })
            });
    crate::server::api::sanitization::sanitize_response_object(&mut json_body);

    let out = Bytes::from(serde_json::to_vec(&json_body).unwrap_or_default());

    if body_complete {
        let usage = extract_token_usage_from_bytes(&out);
        state
            .usage_tracker()
            .track_request(
                provider,
                model,
                usage.as_ref(),
                connection_id,
                api_key,
                endpoint,
                compression,
            )
            .await;
    }
    state
        .usage_live
        .finish_request(model, provider, connection_id, false)
        .await;

    let resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(out))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to build response",
            )
                .into_response()
        });
    let _ = plan; // reserved for future format-specific collapse
    with_cors_response(resp)
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
    compression: Option<CompressionStats>,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let (body_bytes, body_complete) = collect_upstream_response_bytes(response).await;

    // 9router parity (open-sse/handlers/chatCore/nonStreamingHandler.js +
    // open-sse/shared/clineEnvelope.js unwrapClineEnvelope): unwrap before any
    // consumer reads choices/usage so non-stream clients get a bare OpenAI
    // body and usage tracking sees data.usage. No-op unless the provider opts
    // in via transport.quirks.clineEnvelope (cline/clinepass).
    let unenveloped_body = unwrap_cline_envelope(&body_bytes, provider);

    // 9router parity: decloak tool names when Claude cloaking was applied.
    let decloaked_body = if let Some(map) = tool_name_map {
        if !map.is_empty() {
            let body_val: serde_json::Value =
                serde_json::from_slice(&unenveloped_body).unwrap_or(serde_json::Value::Null);
            if !body_val.is_null() {
                let decloaked =
                    crate::core::utils::claude_cloaking::decloak_tool_names(&body_val, map);
                serde_json::to_vec(&decloaked)
                    .map(Bytes::from)
                    .unwrap_or_else(|_| unenveloped_body.clone())
            } else {
                unenveloped_body.clone()
            }
        } else {
            unenveloped_body.clone()
        }
    } else {
        unenveloped_body.clone()
    };

    let final_body = if body_complete {
        let token_usage = extract_token_usage_from_bytes(decloaked_body.as_ref());
        state
            .usage_tracker()
            .track_request(
                provider,
                model,
                token_usage.as_ref(),
                connection_id,
                api_key,
                endpoint,
                compression,
            )
            .await;
        state.usage_live.notify_update();

        // 9router parity: translate non-streaming response body when source
        // and target formats differ (handleNonStreamingResponse).
        // For Responses API format (Codex), the raw body is a response.completed JSON,
        // not SSE chunks. We parse it directly instead of using the streaming SSE transform.
        let translated_body = if plan.needs_translation() {
            if plan.target_format == registry::Format::OpenAiResponses
                || plan.target_format == registry::Format::Codex
            {
                // The Codex/Responses API returns a response.completed JSON body for non-streaming.
                // Parse out the text content and build a proper chat.completion response.
                translate_codex_non_streaming(decloaked_body.as_ref())
                    .unwrap_or_else(|| decloaked_body.clone())
            } else if plan.target_format == registry::Format::Claude
                && plan.source_format == registry::Format::OpenAi
            {
                // GitHub Copilot Claude /v1/messages (and other Claude-upstream
                // non-stream paths): full Messages JSON → chat.completion.
                match serde_json::from_slice::<Value>(decloaked_body.as_ref()) {
                    Ok(mut val) => {
                        crate::core::translator::response::non_streaming::claude_to_openai_non_streaming(
                            &mut val,
                        );
                        Bytes::from(
                            serde_json::to_vec(&val).unwrap_or_else(|_| decloaked_body.to_vec()),
                        )
                    }
                    Err(_) => decloaked_body.clone(),
                }
            } else {
                use crate::core::translator::registry::ResponseTransformState;
                let mut state = ResponseTransformState::default();
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

/// Unwrap Cline's non-stream envelope: {"success":true,"data":{...choices...}}.
///
/// Port of 9router `open-sse/shared/clineEnvelope.js` `unwrapClineEnvelope`
/// (v0.5.75): scoped to providers opting in via `transport.quirks.clineEnvelope`
/// (cline/clinepass) so no other provider's body is ever rewritten. The error
/// envelope ({"success":false,...}) never matches and passes through untouched.
fn unwrap_cline_envelope(body: &[u8], provider: &str) -> Bytes {
    if !matches!(provider, "cline" | "clinepass") {
        return Bytes::copy_from_slice(body);
    }
    let Ok(val) = serde_json::from_slice::<Value>(body) else {
        return Bytes::copy_from_slice(body);
    };
    let Some(success) = val.get("success") else {
        return Bytes::copy_from_slice(body);
    };
    if success != &Value::Bool(true) {
        return Bytes::copy_from_slice(body);
    }
    let data = match val.get("data") {
        Some(Value::Object(_)) => val.get("data").unwrap().clone(),
        _ => return Bytes::copy_from_slice(body),
    };
    serde_json::to_vec(&data).map_or_else(|_| Bytes::copy_from_slice(body), Bytes::from)
}

/// Translate a non-streaming Codex/Responses API response into standard Chat Completions format.
///
/// The Codex backend returns response.completed JSON for non-streaming requests:
/// ```json
/// {"type":"response.completed","response":{"output":[{"type":"message","content":[{"type":"output_text","text":"Hello"}]}],"usage":{"input_tokens":10,"output_tokens":5}}}
/// ```
///
/// This extracts the text content and builds a proper chat.completion response.
fn translate_codex_non_streaming(body: &[u8]) -> Option<Bytes> {
    let val: serde_json::Value = serde_json::from_slice(body).ok()?;

    // Navigate: response > output > [0] > content > [{output_text}]
    let output = val.pointer("/response/output").and_then(|v| v.as_array())?;

    let mut text_parts: Vec<String> = Vec::new();
    for item in output {
        let content = item.get("content").and_then(|v| v.as_array())?;
        for part in content {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                text_parts.push(text.to_string());
            }
        }
    }

    let content_text = text_parts.join("");

    // Extract usage
    let usage = val.pointer("/response/usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let response = serde_json::json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000")),
        "object": "chat.completion",
        "created": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        "model": "codex",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content_text,
            },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    });

    serde_json::to_string(&response).ok().map(Bytes::from)
}

async fn proxy_response_with_pending_tracking(
    response: UpstreamResponse,
    state: AppState,
    provider: String,
    model: String,
    connection_id: Option<String>,
    api_key: Option<&str>,
    endpoint: Option<&'static str>,
    normalize_for_dashboard: bool,
    plan: &RequestPlan,
    tool_name_map: Option<&std::collections::BTreeMap<String, String>>,
    compression: Option<CompressionStats>,
    custom_tool_names: Option<String>,
) -> Response {
    // Capture an owned copy of api_key for usage recording inside the stream
    // (the SSE stream requires 'static lifetimes; &str borrows can't escape).
    let api_key = api_key.map(|s| s.to_string());
    // Extract formats before stream closure to avoid lifetime issues
    let needs_stream_translation = plan.needs_translation();
    let stream_source_format = plan.source_format;
    let stream_target_format = plan.target_format;
    let status = response.status();
    let headers = response.headers().clone();

    // 9router streamingHandler: reject non-SSE content-types when client expects stream
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    // 9router rule (streamingHandler.js:60-77), ported as-is:
    //   block when the type is NEITHER text/event-stream NOR application/json.
    // That is an ALLOW-list of what may be piped into the SSE transform, not the
    // previous deny-list of three types. The deny-list let every other type
    // through into the transform, where it produced garbage frames with no
    // terminal [DONE] — the hang this guard exists to prevent.
    if should_block_non_sse(&ct) {
        // Read the body so an HTML error page does not go through the SSE pipe
        // (9router's stated reason: it crashes the chat router downstream).
        let (body_bytes, _) = collect_upstream_response_bytes(response).await;
        let body_text = String::from_utf8_lossy(&body_bytes);

        let short_msg = upstream_error_message(&body_text, &ct);

        tracing::warn!(
            target: "openproxy::chat",
            "STREAM_GUARD blocked non-SSE content-type={} status={} msg_len={}",
            ct,
            status.as_u16(),
            short_msg.chars().count()
        );

        // Preserve the UPSTREAM status (9router: `providerResponse.status || 502`).
        // Returning a hardcoded 502 for a 200-with-error-body is what made this
        // path a retry storm: 502 is retryable, so one flaky provider that
        // ignored stream:true triggered retries across the whole combo.
        let err = json!({
            "error": {
                "message": format!("[{}]: {}", status.as_u16(), short_msg),
                "type": "upstream_non_sse",
                "code": "upstream_non_sse"
            }
        });
        return with_cors_response(
            (
                status,
                [(header::CONTENT_TYPE, "application/json")],
                err.to_string(),
            )
                .into_response(),
        );
    }

    let transformer = normalize_for_dashboard
        .then(|| transformer_for_provider(&provider))
        .flatten();
    // Qoder wraps every SSE chunk in a {statusCodeValue, body} envelope that
    // must be unwrapped before downstream consumers see it (9router wrapQoderSSE).
    // Usage arrives on a later `choices: []` frame, so a coalescer merges the
    // held finish + usage frames into one terminal chunk (9router sse.js).
    // Billing blocks arrive pre-detected as a real 403 by the executor's
    // first-frame peek — but the flag is still re-checked here so the
    // non-peeked dashboard path also short-circuits.
    let qoder_sse_unwrap = provider == "qoder";
    // Billing block detection state (9router v0.5.55 peekFirstQoderFrame).
    let mut qoder_seen_first_frame = false;
    let mut qoder_billing_block = false;
    let mut qoder_coalescer: Option<crate::core::executor::qoder::QoderSseCoalescer> =
        if qoder_sse_unwrap {
            Some(crate::core::executor::qoder::QoderSseCoalescer::new(&model))
        } else {
            None
        };
    let body = match response {
        UpstreamResponse::Reqwest(response) => {
            let state = state.clone();
            let provider = provider.clone();
            let model = model.clone();
            let connection_id = connection_id.clone();
            let api_key = api_key.clone();
            let compression = compression.clone();
            let mut transformer = transformer;
            let mut pending_text = String::new();
            // Byte framing buffers for the translation and raw-passthrough
            // branches (bead openproxy-0ph4). Bytes, not String, so a
            // multi-byte character split across two reads is not corrupted.
            // Distinct from `pending_text`, which the dashboard transformer
            // branch uses.
            let mut translate_pending: Vec<u8> = Vec::new();
            // Captured BEFORE the closure, because `transformer` is moved into
            // it and cannot be inspected here. The [DONE] sentinel terminates a
            // PASSTHROUGH stream; emitting it when the request took the qoder /
            // dashboard / translation branch injects a terminator into a stream
            // that already has its own, and the client stops reading at it.
            let took_passthrough =
                !qoder_sse_unwrap && transformer.is_none() && !needs_stream_translation;
            let mut saw_done = false;
            let mut passthrough_pending: Vec<u8> = Vec::new();
            let custom_tool_names = custom_tool_names.clone();
            let stream = async_stream::stream! {
                            let mut upstream = response.bytes_stream();
                            // Persistent state for streaming format translation (e.g. Responses API -> Chat Completions).
                            let mut t_state = if needs_stream_translation {
                                let mut s = crate::core::translator::registry::ResponseTransformState::default();
                                // Thread custom-tool names into streaming state so
                                // function_call vs custom_tool_call branching survives
                                // (9router chatCore customToolNames → stream handler).
                                if let Some(ref names) = custom_tool_names {
                                    if !names.is_empty() {
                                        s.responses.state.insert(
                                            "customToolNames".to_string(),
                                            Value::String(names.clone()),
                                        );
                                    }
                                }
                                Some(s)
                            } else {
                                None
                            };
                            // Accumulate usage across EVERY frame, not just the last one.
                            // Anthropic splits usage across events (message_start carries
                            // input + cache counters, message_delta carries the cumulative
                            // output, message_stop carries none), so reading only the final
                            // frame structurally cannot see the prompt tokens. Merged with
                            // field-wise max, matching 9router mergeUsage.
                            let mut stream_usage: Option<TokenUsage> = None;
                            loop {
                                let next = tokio::time::timeout(sse_stall_timeout(), upstream.try_next()).await;
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
                                        record_streaming_usage(&state, &provider, &model,
                                            connection_id.as_deref(), api_key.as_deref(), endpoint, &stream_usage, compression.clone()).await;
                                        state
                                            .usage_live
                                            .finish_request(&model, &provider, connection_id.as_deref(), true)
                                            .await;
                                        yield Ok::<Bytes, std::io::Error>(Bytes::from(write_streaming_error(
                                            "Upstream SSE stream stalled",
                                            "server_error",
                                        )));
                                        return;
                                    }
                                    Ok(Ok(Some(chunk))) => {
                                        stream_usage = merge_token_usage(
                                            stream_usage,
                                            extract_token_usage_from_bytes(&chunk),
                                        );
                                        if qoder_sse_unwrap {
                                            for line in qoder_unwrap_sse_chunk(
                                                &chunk,
                                                &mut pending_text,
                                                &mut qoder_seen_first_frame,
                                                &mut qoder_billing_block,
                                                qoder_coalescer.as_mut(),
                                            ) {
                                                yield Ok::<Bytes, std::io::Error>(Bytes::from(line));
                                            }
                                            if qoder_billing_block {
                                                // Billing block: close the stream now so the
                                                // client sees the 403-shaped error frame.
                                                // (Combo fallback itself happens in the
                                                // executor's pre-stream peek; this flag is
                                                // the backstop for already-open streams.)
            record_streaming_usage(&state, &provider, &model,
                                                    connection_id.as_deref(), api_key.as_deref(), endpoint, &stream_usage, compression.clone()).await;
                                                state
                                                    .usage_live
                                                    .finish_request(&model, &provider, connection_id.as_deref(), true)
                                                    .await;
                                                return;
                                            }
                                        } else if let Some(transformer) = transformer.as_mut() {
                                            for line in transform_dashboard_sse_chunk(&chunk, transformer.as_mut(), &mut pending_text) {
                                                if let Some(frame) = sse_frame_for_dashboard(&line) {
                                                    yield Ok::<Bytes, std::io::Error>(frame);
                                                }
                                            }
                                        } else if needs_stream_translation {
                                            // Framing parity (bead openproxy-0ph4): a
                                            // transport chunk is NOT one SSE event. One read
                                            // can carry several frames, and one frame can
                                            // straddle two reads. 9router buffers per line
                                            // and only acts on COMPLETE lines
                                            // (.tmp/9router/open-sse/utils/stream.js:110-119:
                                            // "const lines = buffer.split(chr(10));
                                            //  buffer = lines.pop() || ''").
                                            //
                                            // Passing the raw chunk straight to
                                            // translate_response dropped every frame that
                                            // was not a whole chunk, and double-handled
                                            // any that was. The buffer below is the same
                                            // one the dashboard transformer path already
                                            // used, generalised so all three stream
                                            // branches share it.
                                            translate_pending.extend_from_slice(&chunk);
                                            for line in drain_complete_sse_lines(&mut translate_pending) {
                                                // t_state is Some whenever
                                                // needs_stream_translation is true, so the
                                                // None arm is unreachable; it is kept as a
                                                // compile-time total match, not a fallback.
                                                if let Some(ref mut ts) = t_state {
                                                    let chunks = registry::global_registry()
                                                        .translate_response(
                                                            stream_target_format,
                                                            stream_source_format,
                                                            &Bytes::from(line),
                                                            ts,
                                                        );
                                                    for out in chunks {
                                                        if let Some(frame) = sse_frame_for_dashboard(&out) {
                                                            yield Ok::<Bytes, std::io::Error>(frame);
                                                        }
                                                    }
                                                }
                                            }
                                        } else {
                                            passthrough_pending.extend_from_slice(&chunk);
                                            for line in drain_complete_sse_lines(&mut passthrough_pending) {
                                                // An upstream terminator means we must not append a
                                                // second one at EOF. This flag was declared and read but
                                                // never assigned in the first version of this commit, while
                                                // the commit message described it as tracked.
                                                if line.trim() == "data: [DONE]" {
                                                    saw_done = true;
                                                }
                                                yield Ok::<Bytes, std::io::Error>(
                                                    sanitize_sse_chunk(
                                                        &passthrough_frame_bytes(
                                                            &apply_passthrough_transforms(&line, &provider),
                                                        ),
                                                    ),
                                                );
                                            }
                                        }
                                    }
                                    Ok(Ok(None)) => break,
                                    Ok(Err(_)) => {
                                        record_streaming_usage(&state, &provider, &model,
                                            connection_id.as_deref(), api_key.as_deref(), endpoint, &stream_usage, compression.clone()).await;
                                        state
                                            .usage_live
                                            .finish_request(&model, &provider, connection_id.as_deref(), true)
                                            .await;
                                        yield Ok::<Bytes, std::io::Error>(Bytes::from(write_streaming_error(
                                            "Upstream stream error",
                                            "server_error",
                                        )));
                                        return;
                                    }
                                }
                            }
                            {
                    // Everything the stream still owes the client, computed in ONE
                    // place so the ORDER is a unit-testable contract instead of
                    // something only a live stream can reveal (bead openproxy-jkit).
                    // A client stops reading at [DONE], so getting this order
                    // wrong silently discards the chunk we were holding.
                    let dashboard_lines = match transformer.as_deref_mut() {
                        Some(t) => flush_dashboard_sse_chunk(t, &mut pending_text),
                        None => Vec::new(),
                    };
                    let translate_lines =
                        if needs_stream_translation && !translate_pending.is_empty() {
                            let last = std::mem::take(&mut translate_pending);
                            match t_state.as_mut() {
                                Some(ts) => registry::global_registry().translate_response(
                                    stream_target_format,
                                    stream_source_format,
                                    &Bytes::from(last),
                                    ts,
                                ),
                                None => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                    let passthrough_terminal =
                        take_terminal_passthrough_frame(&mut passthrough_pending).map(|final_frame| {
                            // The terminal frame runs the SAME pipeline as every
                            // other frame — fixInvalidId, object/created
                            // injection and empty-tool_calls deletion all apply,
                            // or the last frame is the one frame nobody
                            // normalised.
                            let text = String::from_utf8_lossy(&final_frame).into_owned();
                            sanitize_sse_chunk(&passthrough_frame_bytes(
                                &apply_passthrough_transforms(&text, &provider),
                            ))
                        });
                    let coalescer_lines = if qoder_sse_unwrap {
                        qoder_coalescer_flush(qoder_coalescer.as_mut())
                    } else {
                        Vec::new()
                    };
                    let finish_lines = match t_state.as_mut() {
                        Some(ts) => registry::global_registry().finish_stream(
                            stream_source_format,
                            stream_target_format,
                            ts,
                        ),
                        None => Vec::new(),
                    };
                    for emit in plan_eof_emits(
                        took_passthrough,
                        &provider,
                        saw_done,
                        dashboard_lines,
                        translate_lines,
                        passthrough_terminal,
                        coalescer_lines,
                        finish_lines,
                    ) {
                        match emit {
                            EofEmit::Frame(bytes) => {
                                yield Ok::<Bytes, std::io::Error>(bytes);
                            }
                            EofEmit::Done => {
                                yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                    b"data: [DONE]\n\n",
                                ));
                            }
                        }
                    }
                }
                record_streaming_usage(&state, &provider, &model,
                                connection_id.as_deref(), api_key.as_deref(), endpoint, &stream_usage, compression.clone()).await;
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
            let api_key = api_key.clone();
            let compression = compression.clone();
            let mut transformer = transformer;
            let mut pending_text = String::new();
            // Byte framing buffers for the SECOND (Hyper) stream arm — the
            // DEFAULT arm: use_hyper_transport (executor/default.rs:2409) is
            // true whenever there is no proxy and the URL ends in
            // /chat/completions, which is most non-proxy traffic. It carried
            // the same raw-chunk defect the Reqwest arm had.
            let mut translate_pending2: Vec<u8> = Vec::new();
            let mut passthrough_pending2: Vec<u8> = Vec::new();
            let took_passthrough2 =
                !qoder_sse_unwrap && transformer.is_none() && !needs_stream_translation;
            let mut saw_done2 = false;
            let custom_tool_names2 = custom_tool_names.clone();
            let stream = async_stream::stream! {
                // Persistent state for streaming format translation (e.g. Responses API -> Chat Completions).
                let mut t_state = if needs_stream_translation {
                    let mut s = crate::core::translator::registry::ResponseTransformState::default();
                    if let Some(ref names) = custom_tool_names2 {
                        if !names.is_empty() {
                            s.responses.state.insert(
                                "customToolNames".to_string(),
                                Value::String(names.clone()),
                            );
                        }
                    }
                    Some(s)
                } else {
                    None
                };
                // Accumulate the last data frame for best-effort `usage` extraction
                // at stream end (streaming SSE responses usually lack a usage field).
                // Same per-frame accumulation as the first stream arm: usage is
                // split across Anthropic events, so the final frame alone is
                // structurally insufficient.
                let mut stream_usage: Option<TokenUsage> = None;
                loop {
                    let next = tokio::time::timeout(sse_stall_timeout(), body.frame()).await;
                    let frame_result = match next {
                        Err(_elapsed) => {
                            tracing::warn!(
                                target: "openproxy::chat::stream",
                                provider = %provider,
                                model = %model,
                                "SSE stalled, closing stream"
                            );
                            record_streaming_usage(&state, &provider, &model,
                                connection_id.as_deref(), api_key.as_deref(), endpoint, &stream_usage, compression.clone()).await;
                            state
                                .usage_live
                                .finish_request(&model, &provider, connection_id.as_deref(), true)
                                .await;
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(write_streaming_error(
                                "Upstream SSE stream stalled",
                                "server_error",
                            )));
                            return;
                        }
                        Ok(Some(result)) => result,
                        Ok(None) => break,
                    };
                    match frame_result {
                        Ok(frame) => {
                            if let Ok(data) = frame.into_data() {
                                stream_usage = merge_token_usage(
                                    stream_usage,
                                    extract_token_usage_from_bytes(&data),
                                );
                                if let Some(transformer) = transformer.as_mut() {
                                    for line in transform_dashboard_sse_chunk(&data, transformer.as_mut(), &mut pending_text) {
                                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                                            yield Ok::<Bytes, std::io::Error>(frame);
                                        }
                                    }
                                } else if needs_stream_translation {
                                    translate_pending2.extend_from_slice(&data);
                                    for line in drain_complete_sse_lines(&mut translate_pending2) {
                                        if let Some(ref mut ts) = t_state {
                                            let chunks = registry::global_registry()
                                                .translate_response(
                                                    stream_target_format,
                                                    stream_source_format,
                                                    &Bytes::from(line),
                                                    ts,
                                                );
                                            for out in chunks {
                                                if let Some(frame) = sse_frame_for_dashboard(&out) {
                                                    yield Ok::<Bytes, std::io::Error>(frame);
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    passthrough_pending2.extend_from_slice(&data);
                                    for line in drain_complete_sse_lines(&mut passthrough_pending2) {
                                        if is_done_sentinel(&line) {
                                            saw_done2 = true;
                                        }
                                        yield Ok::<Bytes, std::io::Error>(
                                            sanitize_sse_chunk(
                                                &passthrough_frame_bytes(
                                                    &apply_passthrough_transforms(&line, &provider),
                                                ),
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            record_streaming_usage(&state, &provider, &model,
                                connection_id.as_deref(), api_key.as_deref(), endpoint, &stream_usage, compression.clone()).await;
                            state
                                .usage_live
                                .finish_request(&model, &provider, connection_id.as_deref(), true)
                                .await;
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(write_streaming_error(
                                "Upstream stream error",
                                "server_error",
                            )));
                            return;
                        }
                    }
                }
                {
                    // Same single-source EOF plan as the Reqwest arm, so the
                    // two cannot drift. See plan_eof_emits for why the ORDER is
                    // the contract, not an implementation detail.
                    let dashboard_lines = match transformer.as_deref_mut() {
                        Some(t) => flush_dashboard_sse_chunk(t, &mut pending_text),
                        None => Vec::new(),
                    };
                    let translate_lines =
                        if needs_stream_translation && !translate_pending2.is_empty() {
                            let last = std::mem::take(&mut translate_pending2);
                            match t_state.as_mut() {
                                Some(ts) => registry::global_registry().translate_response(
                                    stream_target_format,
                                    stream_source_format,
                                    &Bytes::from(last),
                                    ts,
                                ),
                                None => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                    let passthrough_terminal =
                        take_terminal_passthrough_frame(&mut passthrough_pending2).map(|final_frame| {
                            let text = String::from_utf8_lossy(&final_frame).into_owned();
                            sanitize_sse_chunk(&passthrough_frame_bytes(
                                &apply_passthrough_transforms(&text, &provider),
                            ))
                        });
                    // NOT a pure refactor on this arm. `qoder_sse_unwrap` and
                    // the coalescer are shared by both arms, and the Hyper main
                    // loop already fed the coalescer — but the Hyper arm had NO
                    // EOF flush, so a qoder request over Hyper silently dropped
                    // the finish+usage chunk the coalescer was holding. Routing
                    // this arm through plan_eof_emits added the flush and
                    // fixed that. Do not "simplify" it away.
                    let coalescer_lines = if qoder_sse_unwrap {
                        qoder_coalescer_flush(qoder_coalescer.as_mut())
                    } else {
                        Vec::new()
                    };
                    let finish_lines = match t_state.as_mut() {
                        Some(ts) => registry::global_registry().finish_stream(
                            stream_source_format,
                            stream_target_format,
                            ts,
                        ),
                        None => Vec::new(),
                    };
                    for emit in plan_eof_emits(
                        took_passthrough2,
                        &provider,
                        saw_done2,
                        dashboard_lines,
                        translate_lines,
                        passthrough_terminal,
                        coalescer_lines,
                        finish_lines,
                    ) {
                        match emit {
                            EofEmit::Frame(bytes) => {
                                yield Ok::<Bytes, std::io::Error>(bytes);
                            }
                            EofEmit::Done => {
                                yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                    b"data: [DONE]\n\n",
                                ));
                            }
                        }
                    }
                }
                record_streaming_usage(&state, &provider, &model,
                    connection_id.as_deref(), api_key.as_deref(), endpoint, &stream_usage, compression.clone()).await;
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

/// Record usage for a streaming SSE request at stream end.
///
/// Streaming SSE responses from most providers do not contain a `usage` field,
/// so we record the request with `tokens = None` (which still increments the
/// request count and captures provider/model/endpoint). If the provider emits a
/// final SSE data frame containing a Chat Completions `usage` block, extract it.
async fn record_streaming_usage(
    state: &AppState,
    provider: &str,
    model: &str,
    connection_id: Option<&str>,
    api_key: Option<&str>,
    endpoint: Option<&'static str>,
    usage: &Option<TokenUsage>,
    compression: Option<CompressionStats>,
) {
    let usage = usage.clone();
    state
        .usage_tracker()
        .track_request(
            provider,
            model,
            usage.as_ref(),
            connection_id,
            api_key,
            endpoint,
            compression,
        )
        .await;
}

/// Whether an SSE line is the OpenAI terminator sentinel.
///
/// Shared by the passthrough transform, the `saw_done` tracker and the EOF gate.
/// These were parsing the same line by two different rules —
/// `line.trim() == "data: [DONE]"` versus a `strip_prefix("data:")` + trim
/// comparison — and SSE permits `data:[DONE]` with no space, which only the
/// second rule accepted. Two definitions of one thing is how they drift.
pub(crate) fn is_done_sentinel(line: &str) -> bool {
    line.trim()
        .strip_prefix("data:")
        .is_some_and(|payload| payload.trim() == "[DONE]")
}

/// One thing the stream does at end-of-stream, in the order it must happen.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EofEmit {
    /// A frame already framed for the client (sanitised, delimited).
    Frame(Bytes),
    /// The OpenAI terminator, appended only for a passthrough stream.
    Done,
}

/// The order of the end-of-stream sequence, as ONE function.
///
/// Six review rounds in this epic found the same shape of defect: the fix was
/// right at the helper and wrong at the call site. A [DONE] sentinel gated on
/// the provider but not the branch, emitted before the terminal flushes, cut
/// qoder and kiro streams; a flag declared, read and never assigned; a
/// terminal frame that skipped every other transform. Every unit test stayed
/// GREEN through all of them, because they called the helper in isolation and
/// nothing reached the stream generator.
///
/// This makes the ORDER the unit under test:
///   1. dashboard transformer flush
///   2. translation flush        (last frame with no trailing newline)
///   3. passthrough terminal frame, through the SAME transform as every frame
///   4. qoder usage coalescer flush   (holds a finish+usage chunk)
///   5. finish_stream terminal chunk  (kiro EventStream -> SSE)
///   6. the [DONE] sentinel, LAST, and only for a passthrough request
///
/// A client stops reading at [DONE]. Emitting it before step 4 or 5 makes the
/// client discard exactly the chunk those steps exist to deliver.
pub(crate) fn plan_eof_emits(
    took_passthrough: bool,
    provider: &str,
    saw_done: bool,
    dashboard_lines: Vec<String>,
    translate_lines: Vec<String>,
    passthrough_terminal: Option<Bytes>,
    coalescer_lines: Vec<String>,
    finish_lines: Vec<String>,
) -> Vec<EofEmit> {
    let mut out = Vec::new();

    for line in dashboard_lines {
        if let Some(frame) = sse_frame_for_dashboard(&line) {
            out.push(EofEmit::Frame(frame));
        }
    }
    for out_line in translate_lines {
        if let Some(frame) = sse_frame_for_dashboard(&out_line) {
            out.push(EofEmit::Frame(frame));
        }
    }
    if let Some(frame) = passthrough_terminal {
        out.push(EofEmit::Frame(frame));
    }
    for line in coalescer_lines {
        out.push(EofEmit::Frame(Bytes::from(line)));
    }
    for line in finish_lines {
        if let Some(frame) = sse_frame_for_dashboard(&line) {
            out.push(EofEmit::Frame(frame));
        }
    }
    if should_emit_done_sentinel(took_passthrough, provider, saw_done) {
        out.push(EofEmit::Done);
    }
    out
}

/// Whether an upstream content-type must be blocked before its body is piped
/// into the SSE transform.
///
/// 9router `streamingHandler.js:60` is an ALLOW-list:
///   if (upstreamContentType && !upstreamContentType.includes('text/event-stream')
///       && !upstreamContentType.includes('application/json')) { block }
/// so exactly two families pass. The previous OpenProxy code was a DENY-list of
/// three types (text/html, application/json, text/plain), which let every other
/// type through into the transform, where it produced garbage frames with no
/// terminal [DONE] — the hang the guard exists to prevent — and it blocked
/// application/json, so a provider that ignored stream:true and returned a JSON
/// body was turned into a hardcoded 502, which is RETRYABLE and therefore a
/// retry-storm amplifier across a combo.
///
/// Extracted as a predicate so the rule is testable; an inline condition in the
/// stream handler cannot be.
pub(crate) fn should_block_non_sse(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    if ct.is_empty() {
        // 9router: `if (upstreamContentType && ...)` — an absent header is
        // not blocked.
        return false;
    }
    // 9router's allow-list is EXACTLY {text/event-stream, application/json},
    // and it is right for 9router — which has no binary-stream provider.
    // OpenProxy HAS one, and copying the two-entry list verbatim broke it:
    //
    //   content_type                          deny-list  9router-list  verdict
    //   application/vnd.amazon.eventstream    allowed    BLOCKED       kiro streams DEAD
    //   application/x-ndjson                  allowed    BLOCKED       transformer dead
    //   application/octet-stream              allowed    BLOCKED       transformer dead
    //   application/json                      blocked    allowed       the intended fix
    //   text/html                             blocked    blocked
    //
    // src/core/executor/kiro.rs:765 sends `accept: application/vnd.amazon.eventstream`
    // and kiro answers with it; response_transform.rs:1041 matches ndjson and
    // octet-stream. Blocking those would make the binary EventStream → SSE
    // transformer — which runs AFTER this guard — unreachable, so the file
    // would contradict itself.
    //
    // "eventstream" (no hyphen) is matched as well as "text/event-stream",
    // because the Amazon variant is exactly that word.
    !(ct.contains("text/event-stream")
        || ct.contains("eventstream")
        || ct.contains("application/json")
        || ct.contains("application/x-ndjson")
        || ct.contains("application/octet-stream"))
}

/// Apply the 9router passthrough transforms to one SSE line.
///
/// A line that is not a `data:` frame, or whose payload is not JSON, is
/// returned unchanged — 9router skips non-JSON data lines silently
/// ("Upstream providers sometimes return plain-text errors in the SSE stream
/// that would break downstream JSON decoders", stream.js:225-231).
pub(crate) fn apply_passthrough_transforms(line: &str, _provider: &str) -> String {
    let trimmed = line.trim();
    let Some(payload) = trimmed.strip_prefix("data:") else {
        return line.to_string();
    };
    let payload = payload.trim();
    if payload.is_empty() || is_done_sentinel(line) {
        return line.to_string();
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return line.to_string();
    };
    if normalise_passthrough_chunk(&mut value) {
        format!(
            "data: {}\n",
            serde_json::to_string(&value).unwrap_or_default()
        )
    } else {
        line.to_string()
    }
}

/// Normalise one OpenAI-shaped passthrough chunk, per 9router
/// `utils/stream.js:135-181` (STREAM_MODE.PASSTHROUGH).
///
/// Ported transforms, each for the reason 9router gives:
///
///  * `fixInvalidId` (streamHelpers.js:65) — an id of "chat", "completion", or
///    fewer than 8 chars is replaced with a `chatcmpl-` prefixed fallback,
///    because strict clients reject it.
///  * `object` / `created` injection when `choices` is present (stream.js:141-144)
///    — "Ensure OpenAI-required fields are present on streaming chunks (Letta
///    compat)".
///  * Azure field deletion (stream.js:147-157) — `prompt_filter_results` and
///    per-choice `content_filter_results` are Azure-specific and not standard.
///  * empty `delta.tool_calls` deletion (stream.js:160-180) — "Some providers
///    (e.g. CodeBuddy CN) include tool_calls: [] in every streaming delta. The
///    AI SDK checks `delta.tool_calls != null`; an EMPTY array passes that check,
///    causing premature reasoning-end on every chunk."
///
/// Returns the transformed payload plus whether anything changed, which is how
/// 9router decides between re-serialising and relaying the line verbatim
/// (`else if (idFixed || fieldsInjected)` at :221).
pub(crate) fn normalise_passthrough_chunk(payload: &mut Value) -> bool {
    use serde_json::Value;
    let mut changed = false;

    // fixInvalidId
    if let Some(id) = payload.get("id").and_then(Value::as_str) {
        let invalid = id == "chat" || id == "completion" || id.chars().count() < 8;
        if invalid {
            let fallback = payload
                .pointer("/extend_fields/requestId")
                .and_then(Value::as_str)
                .or_else(|| {
                    payload
                        .pointer("/extend_fields/traceId")
                        .and_then(Value::as_str)
                })
                .map(str::to_string)
                .unwrap_or_else(|| "0".to_string());
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("id".into(), Value::String(format!("chatcmpl-{fallback}")));
            }
            changed = true;
        }
    }

    let has_choices = payload
        .get("choices")
        .map(|c| !c.is_null())
        .unwrap_or(false);

    if has_choices {
        // object / created injection (Letta compat)
        if let Some(obj) = payload.as_object_mut() {
            if !obj.contains_key("object") {
                obj.insert(
                    "object".into(),
                    Value::String("chat.completion.chunk".into()),
                );
                changed = true;
            }
            if !obj.contains_key("created") {
                // 9router uses Date.now()/1000. Determinism matters more here
                // (mock mode and the sim suites depend on stable output), so a
                // fixed epoch is used and the value is still a valid integer.
                obj.insert("created".into(), Value::from(1_700_000_000i64));
                changed = true;
            }
        }
    }

    // Azure-only fields
    if payload
        .as_object_mut()
        .is_some_and(|o| o.remove("prompt_filter_results").is_some())
    {
        changed = true;
    }
    if let Some(choices) = payload.get_mut("choices").and_then(Value::as_array_mut) {
        for choice in choices.iter_mut() {
            if choice
                .as_object_mut()
                .is_some_and(|o| o.remove("content_filter_results").is_some())
            {
                changed = true;
            }
            // empty tool_calls arrays break AI SDK reasoning tracking
            let drop_empty = choice
                .get("delta")
                .and_then(|d| d.get("tool_calls"))
                .and_then(Value::as_array)
                .is_some_and(|a| a.is_empty());
            if drop_empty {
                if let Some(delta) = choice.get_mut("delta").and_then(Value::as_object_mut) {
                    delta.remove("tool_calls");
                }
                changed = true;
            }
        }
    }

    let _ = Value::Null; // keep the import used across cfg paths
    changed
}

/// Whether the OpenAI `data: [DONE]` sentinel must be appended at EOF.
///
/// Three conditions, all required (openproxy-24's review of 3ac819b3):
///
///  * `took_passthrough` — the sentinel terminates a PASSTHROUGH stream. Emitting
///    it when the request actually took the qoder, dashboard-transformer or
///    translation branch injects a terminator into a stream that already has its
///    own, and a client stops reading at it.
///  * `!saw_done` — the upstream may have sent its own `[DONE]`; appending a
///    second one is usually harmless but is a duplicate terminator.
///  * the provider is not in the Gemini family, which rejects the sentinel with
///    a 400 syntax error (9router stream.js:400-401).
///
/// It must also be the LAST yield of the EOF sequence — after
/// qoder_coalescer_flush and after finish_stream. Emitting it first makes the
/// client stop reading and discard the finish+usage chunk coalescer holds, and
/// discard the terminal chunk finish_stream emits.
pub(crate) fn should_emit_done_sentinel(
    took_passthrough: bool,
    provider: &str,
    saw_done: bool,
) -> bool {
    took_passthrough && !saw_done && passthrough_needs_done_sentinel(provider)
}

/// Whether a passthrough stream must be terminated with the OpenAI
/// `data: [DONE]` sentinel.
///
/// 9router `stream.js:398-401`: "In passthrough mode we still must terminate
/// the SSE stream. Some clients (e.g. OpenClaw) expect the OpenAI-style
/// sentinel. Without it they can hang until timeout and trigger failover."
/// Gemini-family clients reject it with a 400 syntax error, so it is withheld
/// for them.
pub(crate) fn passthrough_needs_done_sentinel(provider: &str) -> bool {
    !matches!(provider, "antigravity" | "gemini" | "vertex")
}

/// Build the short, sanitized message for a blocked non-SSE upstream response.
///
/// Ported from 9router `streamingHandler.js:61-67`:
///   const titleMatch = bodyText.match(/<title>([^<]+)<\/title>/i);
///   const sanitizedTitle = (titleMatch?.[1] || '').replace(/<[^>]*>/g, '')
///                               .replace(/[\r\n]+/g, ' ').trim().slice(0, 160);
///   const shortMsg = sanitizedTitle
///     || (bodyText.length < 200 ? bodyText.replace(/<[^>]*>/g, '').trim().slice(0, 160)
///                               : `Upstream returned non-SSE response (${ct})`);
///
/// The point of the extraction is that UNTRUSTED upstream bytes never reach the
/// client verbatim — the dashboard may render error.message as HTML, so echoing
/// a raw error page is an XSS sink. Extracted as a pure function so the rule is
/// testable; the previous inline version embedded the body directly.
pub(crate) fn upstream_error_message(body: &str, content_type: &str) -> String {
    let strip_tags = |s: &str| -> String {
        let mut out = String::with_capacity(s.len());
        let mut in_tag = false;
        for ch in s.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(ch),
                _ => {}
            }
        }
        out
    };
    let flatten_ws = |s: &str| -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") };
    let clamp = |s: &str| -> String { s.chars().take(160).collect() };

    // 9router's regex is /<title>([^<]+)<\/title>/i — `[^<]+` means a title
    // containing ANY further '<' does NOT match, and the code falls through to
    // the body branch. Mirrored here: a title carrying markup is rejected
    // rather than unwrapped, so `<title><script>alert(1)</script>x</title>`
    // degrades to a plain message instead of handing the script's text a
    // place to sit.
    let raw_title = body
        .split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>").map(|(t, _)| t))
        .filter(|t| !t.contains('<'))
        .unwrap_or_default();
    let from_title = clamp(&flatten_ws(&strip_tags(raw_title)));
    if !from_title.is_empty() {
        return from_title;
    }
    if body.chars().count() < 200 {
        let stripped = clamp(&flatten_ws(&strip_tags(body)));
        if !stripped.is_empty() {
            return stripped;
        }
    }
    format!("Upstream returned non-SSE response ({content_type})")
}

/// Strip provider-specific fields from one raw upstream SSE chunk before it
/// reaches the client (9router parity: sanitizeResponse). Chunks can split a
/// frame across reads, so each `data:` line is sanitized in isolation and
/// non-JSON payloads pass through untouched.
fn sanitize_sse_chunk(chunk: &Bytes) -> Bytes {
    let text = String::from_utf8_lossy(chunk);
    if !text.contains("data:") {
        return chunk.clone();
    }
    Bytes::from(crate::server::api::sanitization::sanitize_sse_body(&text).into_bytes())
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

/// Split raw upstream bytes into complete SSE lines and unwrap Qoder's
/// `{statusCodeValue, body}` envelope on each `data:` line (9router
/// wrapQoderSSE). Non-`data:` lines (keepalives) are dropped; the terminal
/// `[DONE]` frame passes through.
///
/// On the first `data:` line, checks for billing/quota blocks (9router v0.5.55
/// peekFirstQoderFrame). If detected, emits a synthetic 403 error frame and
/// sets `billing_block` to `true` so the caller can trigger combo fallback.
fn qoder_unwrap_sse_chunk(
    chunk: &Bytes,
    pending_text: &mut String,
    seen_first_frame: &mut bool,
    billing_block: &mut bool,
    mut coalescer: Option<&mut crate::core::executor::qoder::QoderSseCoalescer>,
) -> Vec<String> {
    pending_text.push_str(&String::from_utf8_lossy(chunk));
    let mut out = Vec::new();
    while let Some(newline_index) = pending_text.find('\n') {
        let mut line = pending_text[..newline_index].to_string();
        if line.ends_with('\r') {
            line.pop();
        }
        pending_text.drain(..=newline_index);
        if line.is_empty() {
            continue;
        }
        // First-frame billing block detection (9router peekFirstQoderFrame).
        if !*seen_first_frame && line.starts_with("data:") {
            *seen_first_frame = true;
            if let Some(billing_err) =
                crate::core::executor::qoder::check_billing_in_sse_line(&line)
            {
                *billing_block = true;
                // Emit the billing error as a JSON error frame so the chat
                // handler sees status 403 and triggers combo fallback.
                out.push(format!("data: {billing_err}\n\n"));
                out.push("data: [DONE]\n\n".to_string());
                return out;
            }
        }
        // Unwrap the {statusCodeValue, body} envelope, then run the inner
        // body through the usage coalescer (9router sse.js).
        let Some(unwrapped) =
            crate::core::executor::qoder::QoderExecutor::unwrap_qoder_envelope(&line)
        else {
            continue;
        };
        if let Some(coal) = coalescer.as_deref_mut() {
            let (frames, _terminal) = coal.handle_inner(&unwrapped);
            out.extend(frames);
            if coal.done_emitted() {
                out.push("data: [DONE]\n\n".to_string());
                return out;
            }
        } else if let Some(frame) =
            crate::core::executor::qoder::QoderExecutor::wrap_qoder_sse_line(&line)
        {
            out.push(frame);
        }
    }
    out
}

/// Flush a Qoder coalescer at end-of-stream: emit any held terminal
/// finish+usage chunk, then `[DONE]` (9router `coalescer.flush`).
fn qoder_coalescer_flush(
    coalescer: Option<&mut crate::core::executor::qoder::QoderSseCoalescer>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(coal) = coalescer {
        if let Some(t) = coal.flush() {
            out.push(t);
        }
    }
    out
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

/// One complete SSE line in, one sanitized line out. The streaming branches
/// feed this rather than the raw transport chunk, so a frame split across two
/// reads is acted on exactly once and a chunk carrying several frames yields
/// several calls (bead openproxy-0ph4).
///
/// Exposed for tests: the buffer itself is a local in the stream generator, so
/// the line-splitting contract is pinned here instead.
pub(crate) fn split_complete_sse_lines(buffer: &mut String) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(nl) = buffer.find('\n') {
        let mut line = buffer[..nl].to_string();
        if line.ends_with('\r') {
            line.pop();
        }
        buffer.drain(..=nl);
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

/// Drain complete SSE lines out of a raw byte buffer.
///
/// A transport chunk is NOT one SSE event: a single read can carry several
/// frames, and one frame can straddle two reads. 9router buffers per line and
/// only acts on COMPLETE lines
/// (.tmp/9router/open-sse/utils/stream.js:110-119: "const lines =
/// buffer.split(chr(10)); buffer = lines.pop() || ''").
///
/// The buffer is BYTES, not a decoded String, and that is the point rather than
/// an implementation detail. Decoding each chunk independently with
/// `String::from_utf8_lossy` corrupts any multi-byte character that straddles a
/// read boundary — a 2-byte Vietnamese diacritic or a 4-byte emoji yields U+FFFD
/// at the end of one chunk AND again at the start of the next, turning ONE
/// character into TWO replacement characters, permanently, on the main
/// OpenAI-compatible path. A line terminated by `\n` is a safe decode boundary
/// for valid UTF-8, so decoding each COMPLETE line — and only complete lines —
/// removes that whole failure class.
///
/// A trailing fragment with no terminator is always retained for the caller's
/// EOF flush; it is never acted on early.
pub(crate) fn drain_complete_sse_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let Some(nl) = buffer.iter().position(|b| *b == b'\n') else {
            break;
        };
        let mut raw: Vec<u8> = buffer.drain(..=nl).collect();
        raw.pop(); // the newline
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        if raw.is_empty() {
            continue;
        }
        // An incomplete multi-byte sequence cannot be silently mangled here:
        // the line is complete, so String::from_utf8_lossy only replaces bytes
        // that were never valid UTF-8 to begin with.
        out.push(String::from_utf8_lossy(&raw).into_owned());
    }
    out
}

/// Re-frame one drained line for the raw-passthrough path.
///
/// `drain_complete_sse_lines` strips the terminator, which is correct for the
/// translation path (the transformer re-emits its own framing) but WRONG here:
/// the passthrough path is supposed to relay the upstream framing verbatim, and
/// `sanitize_sse_body` only re-appends a newline when the line it was given
/// already ends in one. Emitting drained lines bare therefore concatenates
/// consecutive frames into a single SSE line — `data: {"a":1}data: {"a":2}` —
/// which the client parses as one malformed JSON frame and the stream dies.
/// Re-attach the `\n\n` frame separator so passthrough keeps upstream framing.
pub(crate) fn passthrough_frame_bytes(line: &str) -> Bytes {
    let mut framed = line.as_bytes().to_vec();
    framed.extend_from_slice(b"\n\n");
    Bytes::from(framed)
}

/// Take the final newline-free leftover out of a passthrough buffer, if it holds
/// a complete frame.
///
/// This exists as a function so it can be TESTED. The bug it replaces passed the
/// leftover back through `drain_complete_sse_lines`, which by construction
/// returns [] for input with no terminator — so the final frame was silently
/// dropped on both stream arms, and no test caught it because the tests only
/// exercised the drain helper in isolation. A regression here now fails a test
/// that runs the real extraction.
pub(crate) fn take_terminal_passthrough_frame(buffer: &mut Vec<u8>) -> Option<Bytes> {
    if buffer.is_empty() {
        return None;
    }
    let mut last = std::mem::take(buffer);
    if last.last() == Some(&b'\r') {
        last.pop();
    }
    if last.is_empty() {
        return None;
    }
    // No separator here: the call site runs this through passthrough_frame_bytes,
    // which attaches the \n\n. Appending it in both places gave every terminal
    // frame a four-newline tail. Harmless in practice — SSE ignores blank lines
    // between events — but it contradicted the symmetry the call site claims.
    Some(Bytes::from(last))
}

/// Strip the SSE `data:` prefix from a chunk, returning the JSON payload.
/// SSE data lines look like `data: {...}` or `data: {...}\n\nbuffer`.
/// If the body is valid JSON already (non-streaming path), return as-is.
///
/// The scan takes the first line that STARTS WITH `data:`, not line 0.
/// Anthropic names every event, so a frame arrives as
///   `event: message_start\ndata: {...}`
/// and taking line 0 meant the `data:` branch never ran for a single Claude
/// frame — the whole usage path returned None for Anthropic streams. That
/// silently reduced the usage merge to a no-op: the accumulator stayed None
/// and every Claude streaming request still recorded tokens:null / cost 0.00.
/// Found by the openproxy-kh7f bead review, which also showed the merge's own
/// tests had been written against pre-built TokenUsage values and so could not
/// have caught it.
fn strip_sse_data_prefix(body: &[u8]) -> &[u8] {
    for line in body.split(|&b| b == b'\n') {
        if line.starts_with(b"data:") {
            let after = &line[b"data:".len()..];
            let after = after
                .strip_prefix(b" ")
                .or_else(|| after.strip_prefix(b"\t"))
                .unwrap_or(after);
            if serde_json::from_slice::<serde_json::Value>(after).is_ok() {
                return after;
            }
        }
    }
    // Fall back: try parsing the whole body as JSON (non-streaming / already-stripped).
    if serde_json::from_slice::<serde_json::Value>(body).is_ok() {
        return body;
    }
    body
}

/// Fold a per-frame usage reading into a running accumulator, matching
/// 9router's `mergeUsage`
/// (.tmp/9router/open-sse/utils/usageTracking.js:321-335).
///
/// Why this exists: usage is SPLIT across Anthropic stream events. The comment
/// in 9router says it outright — "message_start has real input+cache,
/// message_delta has the real cumulative output". Reading only the final frame
/// therefore misses the prompt tokens and both cache counters entirely, because
/// the final frame is `message_stop` (no usage) or `message_delta` (output
/// only). The ledger recorded tokens:null / cost 0.00 for every Claude
/// streaming request, and that $0 flowed into the per-key monthly budget
/// guard, so streaming spend never counted against a limit.
///
/// Semantics copied from 9router:
///  - numeric fields take `max(prev, next)`, because the later frame carries
///    the CUMULATIVE value, not a delta;
///  - a non-finite value is skipped so one malformed chunk cannot poison the
///    whole accumulation (`Math.max(x, NaN)` is NaN — 9router guards this);
///  - nested detail objects take the latest.
pub(super) fn merge_token_usage(
    prev: Option<TokenUsage>,
    next: Option<TokenUsage>,
) -> Option<TokenUsage> {
    let Some(next) = next else { return prev };
    let Some(mut acc) = prev else {
        return Some(next);
    };

    macro_rules! max_field {
        ($($f:ident),+ $(,)?) => {$(
            if let Some(n) = next.$f {
                acc.$f = Some(acc.$f.unwrap_or(0).max(n));
            }
        )+};
    }
    max_field!(
        prompt_tokens,
        input_tokens,
        completion_tokens,
        output_tokens,
        total_tokens,
        reasoning_tokens,
        cached_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
    );
    if !next.extra.is_empty() {
        acc.extra.extend(next.extra);
    }
    Some(acc)
}

fn extract_token_usage_from_bytes(body: &[u8]) -> Option<TokenUsage> {
    let body = strip_sse_data_prefix(body);
    let value = serde_json::from_slice::<Value>(body).ok()?;

    // Claude/Anthropic names every stream event, and splits usage across two
    // shapes (9router extractUsage, usageTracking.js:239-263):
    //   message_start  -> usage nested under `message.usage`  (input + cache)
    //   message_delta  -> usage at the top level             (output)
    // The Anthropic branch was missing entirely, so neither the input tokens
    // nor either cache counter could ever be read off a Claude stream. The
    // generic top-level `usage` lookup below still serves message_delta and
    // every OpenAI-shaped provider; the nested lookup is added, not moved.
    let anthropic_nested = value
        .get("type")
        .and_then(Value::as_str)
        .filter(|t| *t == "message_start")
        .and_then(|_| value.get("message"))
        .and_then(|m| m.get("usage"))
        .and_then(Value::as_object);

    let usage_obj = anthropic_nested
        .or_else(|| value.get("usage").and_then(Value::as_object))
        .or_else(|| {
            value
                .get("data")
                .and_then(|d| d.get("usage"))
                .and_then(Value::as_object)
        })
        .or_else(|| {
            value
                .get("result")
                .and_then(|d| d.get("usage"))
                .and_then(Value::as_object)
        });

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

    if let Some(usage) = usage_obj {
        return Some(TokenUsage {
            prompt_tokens: extract_u64(usage, "prompt_tokens"),
            input_tokens: extract_u64(usage, "input_tokens"),
            completion_tokens: extract_u64(usage, "completion_tokens"),
            output_tokens: extract_u64(usage, "output_tokens"),
            total_tokens: extract_u64(usage, "total_tokens"),
            reasoning_tokens: extract_u64(usage, "reasoning_tokens"),
            cached_tokens: extract_u64(usage, "cached_tokens"),
            cache_read_input_tokens: extract_u64(usage, "cache_read_input_tokens"),
            cache_creation_input_tokens: extract_u64(usage, "cache_creation_input_tokens"),
            extra: usage
                .iter()
                .filter(|(key, _)| !known_fields.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        });
    }

    // Fallback: some providers put input_tokens/output_tokens directly at the
    // top level (e.g. Anthropic, some proxies). Only use this when at least
    // one token field is present to avoid creating a zero-filled entry for
    // responses that have no usage data at all.
    let input = extract_u64_from_value(&value, "input_tokens");
    let prompt = extract_u64_from_value(&value, "prompt_tokens");
    let output = extract_u64_from_value(&value, "output_tokens");
    let completion = extract_u64_from_value(&value, "completion_tokens");
    let total = extract_u64_from_value(&value, "total_tokens");
    if input + prompt + output + completion + total > 0 {
        return Some(TokenUsage {
            prompt_tokens: opt(prompt).or(opt(input)),
            input_tokens: opt(input).filter(|_| prompt == 0),
            completion_tokens: opt(completion).or(opt(output)),
            output_tokens: opt(output).filter(|_| completion == 0),
            total_tokens: opt(total),
            reasoning_tokens: opt(extract_u64_from_value(&value, "reasoning_tokens")),
            cached_tokens: opt(extract_u64_from_value(&value, "cached_tokens")),
            cache_read_input_tokens: opt(extract_u64_from_value(&value, "cache_read_input_tokens")),
            cache_creation_input_tokens: opt(extract_u64_from_value(
                &value,
                "cache_creation_input_tokens",
            )),
            extra: BTreeMap::new(),
        });
    }

    None
}

fn extract_u64(obj: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    obj.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    })
}

fn extract_u64_from_value(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(|v| match v {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .unwrap_or(0)
}

fn opt(v: u64) -> Option<u64> {
    if v > 0 {
        Some(v)
    } else {
        None
    }
}

/// Extract the error message AND raw body bytes from an upstream error response.
/// This preserves the upstream body for verbatim passthrough (H23).
async fn extract_upstream_error_with_body(response: UpstreamResponse) -> (String, Option<Vec<u8>>) {
    let status = response.status();
    let (body_bytes, _) = collect_upstream_response_bytes(response).await;
    let text = String::from_utf8_lossy(&body_bytes).to_string();
    let message = if let Ok(value) = serde_json::from_str::<Value>(&text) {
        if let Some(msg) = value
            .get("error")
            .and_then(|error| error.get("message").or(Some(error)))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            msg.to_string()
        } else if let Some(msg) = value
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            msg.to_string()
        } else {
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
    } else {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            status
                .canonical_reason()
                .unwrap_or("Upstream request failed")
                .to_string()
        } else {
            trimmed.to_string()
        }
    };
    let raw_body = if body_bytes.is_empty() {
        None
    } else {
        Some(body_bytes.to_vec())
    };
    (message, raw_body)
}

/// Read the error response body once and return both the extracted message and
/// a body-based `retryAfter` (9router `handleComboChat` reads
/// `errorBody.retryAfter`; `new Date(retryAfter)` accepts ISO date or seconds).
async fn extract_error_message_and_retry_after(
    response: UpstreamResponse,
) -> (String, Option<DateTime<Utc>>) {
    let (message, _, retry_after) = extract_error_message_and_retry_after_with_body(response).await;
    (message, retry_after)
}

/// Same as [`extract_error_message_and_retry_after`] but additionally
/// returns the raw upstream body bytes so the caller can preserve them on
/// `ComboAttemptError::upstream_body` for verbatim passthrough (H23, bead
/// openproxy-i7yt). Split out rather than changing the existing signature
/// because the other call sites only need message + retryAfter.
async fn extract_error_message_and_retry_after_with_body(
    response: UpstreamResponse,
) -> (String, Option<Vec<u8>>, Option<DateTime<Utc>>) {
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
    let raw_body = if text.is_empty() {
        None
    } else {
        Some(text.clone().into_bytes())
    };
    let retry_after = crate::core::combo::parse_retry_after_from_body(text.as_bytes());
    let message = {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(message) = value
                .get("error")
                .and_then(|error| error.get("message").or(Some(error)))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                message.to_string()
            } else if let Some(message) = value
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                message.to_string()
            } else {
                fallback_error_text(status, &text)
            }
        } else {
            fallback_error_text(status, &text)
        }
    };
    (message, raw_body, retry_after)
}

fn fallback_error_text(status: StatusCode, text: &str) -> String {
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
        upstream_body: error.upstream_body,
    }))
}

fn attempt_error_response(error: ComboAttemptError) -> Response {
    // H23: When upstream_body is available, return it verbatim instead
    // of constructing a new error body.
    if let Some(body_bytes) = error.upstream_body {
        let status_code = StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_GATEWAY);
        let mut response = (status_code, Body::from(body_bytes)).into_response();
        if let Some(retry_after) = error.retry_after {
            let seconds = (retry_after - Utc::now()).num_seconds().max(1).to_string();
            if let Ok(value) = seconds.parse() {
                response.headers_mut().insert("retry-after", value);
            }
        }
        return response;
    }

    // 9router hands the status it was given straight to errorResponse
    // (open-sse/utils/error.js:27-35) — it never re-derives one from the
    // message text, and neither do we. Re-deriving is what turned a
    // handler-written 400 into 406/403 over its own prose.
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let friendly =
        crate::core::utils::error::friendly_error_message(status.as_u16(), &error.message);
    let body = crate::core::utils::error::build_error_body(status.as_u16(), Some(&friendly));
    let mut response = (status, Json(body)).into_response();

    if let Some(retry_after) = error.retry_after {
        let seconds = (retry_after - Utc::now()).num_seconds().max(1).to_string();
        if let Ok(value) = seconds.parse() {
            response.headers_mut().insert("retry-after", value);
        }
    }

    response
}

fn json_error_response(status: StatusCode, message: &str) -> Response {
    let friendly = crate::core::utils::error::friendly_error_message(status.as_u16(), message);
    let body = crate::core::utils::error::build_error_body(status.as_u16(), Some(&friendly));
    with_cors_response((status, Json(body)).into_response())
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

/// Produce an OpenAI-compatible SSE error chunk for mid-stream errors.
/// Clients (Claude Code, Gemini CLI, etc.) parse error chunks and surface
/// the message, so writing one before closing the stream lets them show
/// a useful error instead of a generic "connection closed" message.
fn write_streaming_error(error_msg: &str, error_type: &str) -> String {
    let friendly = crate::core::utils::error::friendly_error_message(502, error_msg);
    let msg = serde_json::json!({
        "error": {
            "message": friendly,
            "type": error_type,
            "code": null
        }
    });
    format!(
        "data: {}\n\n",
        serde_json::to_string(&msg).unwrap_or_default()
    )
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
    use std::sync::Arc;

    use axum::http::StatusCode;
    use bytes::Bytes;
    use chrono::{Duration as ChronoDuration, Utc};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};

    use super::{
        build_dashboard_sse_response, build_proxied_response, check_fallback_error,
        earliest_retry_after, is_no_auth_provider, is_tts_request, select_connection,
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
            runtime_transport: None,
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
        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &excluded, None)
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

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new(), None)
            .expect("should select an account");

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

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new(), None)
            .expect("should select an account");

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

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new(), None)
            .expect("should select an account");

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

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new(), None)
            .expect("should select an account");

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

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new(), None)
            .expect("should select an account");

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

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new(), None)
            .expect("should select an account");

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

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new(), None)
            .expect("should select an account");

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

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &excluded, None);
        assert!(
            selected.is_none(),
            "should return None when all accounts excluded"
        );
    }

    #[test]
    fn select_connection_returns_none_when_no_connections_match() {
        let snapshot = AppDb::default();

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new(), None);
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
    fn success_clear_drops_model_and_all_locks_but_keeps_others() {
        // 9router src/sse/services/auth.js:306-312 parity: succeeded model
        // lock + modelLock___all clear; other active model locks survive.
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let past = (Utc::now() - ChronoDuration::seconds(10)).to_rfc3339();
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("modelLock_gpt-4.1".into(), Value::String(future.clone()));
        extra.insert("modelLock___all".into(), Value::String(future));
        extra.insert(
            "modelLock_other-model".into(),
            Value::String((Utc::now() + ChronoDuration::seconds(60)).to_rfc3339()),
        );
        extra.insert("modelLock_stale".into(), Value::String(past));
        extra.insert("unrelated".into(), Value::String("keep".into()));

        super::retain_lock_after_success(&mut extra, Some("gpt-4.1"), Utc::now());

        assert!(
            !extra.contains_key("modelLock_gpt-4.1"),
            "succeeded lock cleared"
        );
        assert!(!extra.contains_key("modelLock___all"), "___all cleared");
        assert!(!extra.contains_key("modelLock_stale"), "expired cleared");
        assert!(
            extra.contains_key("modelLock_other-model"),
            "other lock survives"
        );
        assert!(extra.contains_key("unrelated"), "non-lock keys untouched");
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

    // 9router auth.js:275 / accountFallback.js:211 write
    // `backoffLevel: newBackoffLevel ?? backoffLevel`. The dispatcher used
    // `unwrap_or(current_backoff + 1)` instead, so every non-backoff failure —
    // a 400 from a malformed tool schema, the 404 model-not-found, a 502 from a
    // dead upstream — ratcheted the level towards the 5-minute cap without ever
    // taking the rate-limit path the level exists to model.
    #[test]
    fn only_a_backoff_decision_advances_the_persisted_level() {
        let decision = check_fallback_error(404, "The model `gpt-9` does not exist", 3);
        assert_eq!(
            super::next_backoff_level(&decision, 3),
            3,
            "a client-side model error must not escalate backoff"
        );
        assert_eq!(decision.cooldown, std::time::Duration::from_secs(120));
        assert!(decision.should_fallback);

        for (status, text) in [
            (400u16, "unsupported parameter: max_tokens"),
            (401, "bad token"),
            (402, "payment required"),
            (403, "denied"),
            (404, "no such model"),
            (502, "connection refused"),
        ] {
            let decision = check_fallback_error(status, text, 7);
            assert_eq!(
                super::next_backoff_level(&decision, 7),
                7,
                "status {status} must leave the level alone"
            );
        }

        let rate_limited = check_fallback_error(429, "boom", 2);
        assert_eq!(super::next_backoff_level(&rate_limited, 2), 3);
    }

    async fn dispatch_without_credentials(
        state: &crate::server::state::AppState,
    ) -> super::ComboAttemptError {
        let body = json!({"model": "gpt-4.1", "messages": []});
        let plan = crate::core::chat::RequestPlan::new(
            Some("/v1/chat/completions"),
            &body,
            "openai",
            "gpt-4.1",
        );
        super::forward_with_provider_fallback(
            state,
            "openai",
            "gpt-4.1",
            body,
            axum::http::HeaderMap::new(),
            None,
            None,
            &plan,
            None,
            None,
        )
        .await
        .expect_err("a provider with no connection cannot dispatch")
    }

    // 9router chat.js:244-247: a provider with no usable credential and
    // nothing excluded is a 404 `No active credentials for provider: X`. The
    // dispatcher branched on `retry_after` instead, collapsing "never tried"
    // into the same 400 as "exhausted" and reporting the wrong text.
    #[tokio::test]
    async fn no_usable_credentials_returns_404_not_400() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::load_from(dir.path()).await.unwrap();
        let state = crate::server::state::AppState::new(Arc::new(db));

        let error = dispatch_without_credentials(&state).await;

        assert_eq!(error.status, 404);
        assert_eq!(error.message, "No active credentials for provider: openai");
        assert!(error.retry_after.is_none());
    }

    /// A provider whose only account is locked out is *not* the 404 case: the
    /// account was attempted, so the caller gets a 503 plus the retry window.
    #[tokio::test]
    async fn exhausted_accounts_still_return_503() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::load_from(dir.path()).await.unwrap();
        let state = crate::server::state::AppState::new(Arc::new(db));
        state
            .db
            .update(|app| {
                let mut locked = connection("locked", 1);
                locked.rate_limited_until =
                    Some((Utc::now() + ChronoDuration::seconds(120)).to_rfc3339());
                app.provider_connections.push(locked);
            })
            .await
            .unwrap();

        let error = dispatch_without_credentials(&state).await;

        assert_eq!(error.status, 503, "an exhausted account is not a 404");
        assert!(
            error.retry_after.is_some(),
            "the cooling-down window must reach the caller"
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

    /// 9router chatCore.js:229 — the x-9router-token-saver header opts out of
    /// savers when its value is the literal "off" (case-insensitive); absent
    /// header or any other value keeps savers ON.
    fn token_saver_gate(headers: &std::collections::HashMap<String, String>) -> bool {
        headers
            .get("x-9router-token-saver")
            .map(|v| !v.eq_ignore_ascii_case("off"))
            .unwrap_or(true)
    }

    /// 9router parity (open-sse/shared/clineEnvelope.js unwrapClineEnvelope +
    /// tests/unit/cline-free-models-envelope.test.js): non-stream Cline/ClinePass
    /// responses wrapped in {"success":true,"data":...} unwrap to data before
    /// usage extraction/translation; the error envelope passes through untouched.
    #[test]
    fn unwrap_cline_envelope_success_unwraps_to_data() {
        let body = br#"{"success":true,"data":{"choices":[{"message":{"content":"Hi"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}}}"#;
        for provider in ["cline", "clinepass"] {
            let out = super::unwrap_cline_envelope(body, provider);
            let val: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(val["choices"][0]["message"]["content"], "Hi");
            assert!(val.get("success").is_none());
        }
    }

    #[test]
    fn unwrap_cline_envelope_error_passes_through() {
        let body = br#"{"success":false,"error":"empty response content"}"#;
        for provider in ["cline", "clinepass"] {
            let out = super::unwrap_cline_envelope(body, provider);
            let val: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(val["success"], false);
            assert_eq!(val["error"], "empty response content");
        }
    }

    #[test]
    fn unwrap_cline_envelope_non_opt_in_provider_untouched() {
        // The unwrap is opt-in via transport.quirks.clineEnvelope so it can
        // never rewrite another provider's body — including one that happens
        // to return {"success":true,"data":...} for its own reasons.
        let body = br#"{"success":true,"data":{"choices":[{"message":{"content":"Hi"}}]}}"#;
        let out = super::unwrap_cline_envelope(body, "openai");
        let val: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(val["success"], true);
        assert_eq!(val["data"]["choices"][0]["message"]["content"], "Hi");
        assert!(val.get("choices").is_none());
    }

    #[test]
    fn unwrap_cline_envelope_bare_body_unchanged() {
        let body = br#"{"choices":[{"message":{"content":"Hi"}}]}"#;
        for provider in ["cline", "clinepass"] {
            let out = super::unwrap_cline_envelope(body, provider);
            let val: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(val["choices"][0]["message"]["content"], "Hi");
        }
    }

    #[test]
    fn token_saver_header_disables_rtk_and_caveman() {
        use std::collections::HashMap;
        // "off" → savers disabled.
        let off = HashMap::from([("x-9router-token-saver".to_string(), "off".to_string())]);
        assert!(!token_saver_gate(&off));
        // Case-insensitive: "OFF"/"Off".
        let off_upper = HashMap::from([("x-9router-token-saver".to_string(), "OFF".to_string())]);
        assert!(!token_saver_gate(&off_upper));
        // Absent header → enabled.
        assert!(token_saver_gate(&HashMap::new()));
        // Empty value / other value → enabled (JS `!== "off"`).
        let empty = HashMap::from([("x-9router-token-saver".to_string(), String::new())]);
        assert!(token_saver_gate(&empty));
        let yes = HashMap::from([("x-9router-token-saver".to_string(), "yes".to_string())]);
        assert!(token_saver_gate(&yes));
    }

    // Bead openproxy-sewn: opencode-go must never resolve to the virtual
    // no-auth connection.
    #[test]
    fn select_connection_never_returns_virtual_noauth_for_opencode_go() {
        // No stored connections at all: opencode-go → None (NOT virtual).
        let snapshot = AppDb {
            provider_connections: vec![],
            ..AppDb::default()
        };
        assert!(
            select_connection(&snapshot, "opencode-go", "glm-5.1", &HashSet::new(), None).is_none(),
            "opencode-go without stored key must yield None, never the virtual public connection"
        );
        // Meanwhile opencode (Free, noAuth) still gets the virtual fallback.
        let fallback =
            select_connection(&snapshot, "opencode", "big-pickle", &HashSet::new(), None)
                .expect("opencode free must keep the virtual no-auth fallback");
        assert_eq!(fallback.id, "noauth");
    }

    #[test]
    fn select_connection_prefers_stored_key_over_anything_for_opencode_go() {
        let mut stored = connection("ocg-stored", 1);
        stored.provider = "opencode-go".to_string();
        stored.default_model = None;
        let snapshot = AppDb {
            provider_connections: vec![stored.clone()],
            ..AppDb::default()
        };
        let selected =
            select_connection(&snapshot, "opencode-go", "glm-5.1", &HashSet::new(), None)
                .expect("stored opencode-go key must be selected");
        assert_eq!(selected.id, "ocg-stored");
        assert_eq!(selected.api_key.as_deref(), Some("sk-ocg-stored"));
    }

    #[test]
    fn is_no_auth_provider_matches_registry_flags() {
        // 9router registry: opencode has noAuth:true; opencode-go does not.
        assert!(is_no_auth_provider("opencode"));
        assert!(!is_no_auth_provider("opencode-go"));
        assert!(!is_no_auth_provider("ocg"));
    }

    #[test]
    fn is_tts_request_consults_catalog_then_substring() {
        // Catalog kind == "tts" wins even without a tts/speech substring
        // (kokoro on selfhosted-tts).
        assert!(is_tts_request("selfhosted-tts", "kokoro"));
        // Catalog-known TTS model with substring also matches.
        assert!(is_tts_request("openai", "tts-1"));
        // Unknown models fall back to name-substring.
        assert!(is_tts_request("custom", "my-tts-voice"));
        assert!(is_tts_request("custom", "speech-synth"));
        assert!(!is_tts_request("openai", "gpt-4.1"));
    }
    // Bead openproxy-i7yt: upstream error bodies must survive to the
    // client verbatim (H23) instead of collapsing to generic 500.
    #[test]
    fn upstream_body_preserved_verbatim_in_error_response() {
        use super::attempt_error_response;
        use crate::core::combo::ComboAttemptError;
        // JSON upstream body (e.g. FreeTierError/insufficient_quota).
        let raw = br#"{"error":{"message":"OpenCode's free tier can only be used from within OpenCode","type":"permission_error"}}"#;
        let err = ComboAttemptError {
            status: 403,
            message: "OpenCode's free tier can only be used from within OpenCode".to_string(),
            retry_after: None,
            upstream_body: Some(raw.to_vec()),
        };
        let resp = attempt_error_response(err);
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
        // Non-JSON upstream body (e.g. Cloudflare "error code: 1010").
        let raw2 = b"error code: 1010";
        let err2 = ComboAttemptError {
            status: 403,
            message: "error code: 1010".to_string(),
            retry_after: None,
            upstream_body: Some(raw2.to_vec()),
        };
        let resp2 = attempt_error_response(err2);
        assert_eq!(resp2.status(), axum::http::StatusCode::FORBIDDEN);
    }

    // 9router error.js:27-35 hands the status it was given to the response
    // untouched. Re-deriving one from the message text turned a handler's own
    // 400 into a 406 because the prose mentioned an unsupported model.
    #[test]
    fn attempt_error_response_does_not_reinfer_the_status_from_the_message() {
        use super::attempt_error_response;
        use crate::core::combo::ComboAttemptError;

        let resp = attempt_error_response(ComboAttemptError {
            status: 400,
            message: "That adapter is not supported by this endpoint".to_string(),
            retry_after: None,
            upstream_body: None,
        });
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn json_error_response_does_not_reinfer_the_status_from_the_message() {
        use super::json_error_response;

        assert_eq!(
            json_error_response(
                axum::http::StatusCode::BAD_REQUEST,
                "Request body failed validation"
            )
            .status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        assert_eq!(
            json_error_response(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Every account is rate limited right now"
            )
            .status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "a 503 must not be demoted to a 429 by its own prose"
        );
    }

    #[tokio::test]
    async fn extractor_returns_raw_body_bytes() {
        use super::extract_error_message_and_retry_after_with_body;
        use crate::core::executor::UpstreamResponse;
        fn upstream(status: u16, body: &'static str) -> UpstreamResponse {
            let http_resp = axum::http::Response::builder()
                .status(status)
                .body(body)
                .unwrap();
            UpstreamResponse::Reqwest(reqwest::Response::from(http_resp))
        }
        // JSON body round-trips as raw bytes.
        let raw = r#"{"error":{"message":"quota hit"}}"#;
        let (msg, body, _) =
            extract_error_message_and_retry_after_with_body(upstream(403, raw)).await;
        assert_eq!(msg, "quota hit");
        assert_eq!(body, Some(raw.as_bytes().to_vec()));
        // Plain-text body (Cloudflare-style) also preserved.
        let (msg2, body2, _) =
            extract_error_message_and_retry_after_with_body(upstream(403, "error code: 1010"))
                .await;
        assert_eq!(msg2, "error code: 1010");
        assert_eq!(body2, Some(b"error code: 1010".to_vec()));
        // Empty body → None (no verbatim passthrough to preserve).
        let (_, body3, _) =
            extract_error_message_and_retry_after_with_body(upstream(500, "")).await;
        assert_eq!(body3, None);
    }
}

#[cfg(test)]
mod usage_merge_tests {
    use super::merge_token_usage;
    use crate::types::TokenUsage;

    fn empty() -> TokenUsage {
        TokenUsage {
            prompt_tokens: None,
            input_tokens: None,
            completion_tokens: None,
            output_tokens: None,
            total_tokens: None,
            reasoning_tokens: None,
            cached_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            extra: Default::default(),
        }
    }

    /// Regression (parity finding P0-4xx, 9router stream.js:319-321 +
    /// usageTracking.js:321-335): usage is split across Anthropic stream
    /// events. The final frame is message_stop (no usage) or message_delta
    /// (output only), so reading only the last frame left prompt tokens and
    /// both cache counters structurally unreachable — the ledger recorded
    /// tokens:null / cost 0.00 for every Claude streaming request, and that $0
    /// flowed into the per-key monthly budget guard.
    fn anthropic_start() -> TokenUsage {
        TokenUsage {
            input_tokens: Some(1200),
            output_tokens: Some(0),
            cache_read_input_tokens: Some(8000),
            cache_creation_input_tokens: Some(400),
            ..empty()
        }
    }

    fn anthropic_delta() -> TokenUsage {
        TokenUsage {
            output_tokens: Some(180),
            ..empty()
        }
    }

    #[test]
    fn usage_is_merged_across_the_frames_that_carry_it() {
        let merged = merge_token_usage(Some(anthropic_start()), Some(anthropic_delta()));
        let m = merged.expect("merged");
        assert_eq!(m.input_tokens, Some(1200), "input from message_start");
        assert_eq!(m.output_tokens, Some(180), "output from message_delta");
        assert_eq!(
            m.cache_read_input_tokens,
            Some(8000),
            "cache read preserved"
        );
        assert_eq!(
            m.cache_creation_input_tokens,
            Some(400),
            "cache create preserved"
        );
    }

    /// The exact regression: the LAST frame carries no usage at all. The
    /// accumulator must not be clobbered by it.
    #[test]
    fn a_final_frame_with_no_usage_does_not_erase_the_total() {
        let merged = merge_token_usage(
            merge_token_usage(Some(anthropic_start()), Some(anthropic_delta())),
            None,
        );
        let m = merged.expect("merged");
        assert_eq!(m.input_tokens, Some(1200));
        assert_eq!(m.output_tokens, Some(180));
    }

    /// 9router uses max, not sum, because the later frame is CUMULATIVE.
    /// A provider that re-sends the same usage on every event must not inflate.
    #[test]
    fn repeated_cumulative_frames_take_max_not_sum() {
        let a = TokenUsage {
            output_tokens: Some(100),
            total_tokens: Some(900),
            ..empty()
        };
        let b = TokenUsage {
            output_tokens: Some(100),
            total_tokens: Some(900),
            ..empty()
        };
        let m = merge_token_usage(Some(a), Some(b)).expect("merged");
        assert_eq!(m.output_tokens, Some(100), "not 200");
        assert_eq!(m.total_tokens, Some(900), "not 1800");
    }

    /// A later frame that legitimately grows the count must still win.
    #[test]
    fn a_growing_counter_still_advances() {
        let a = TokenUsage {
            output_tokens: Some(100),
            ..empty()
        };
        let b = TokenUsage {
            output_tokens: Some(250),
            ..empty()
        };
        let m = merge_token_usage(Some(a), Some(b)).expect("merged");
        assert_eq!(m.output_tokens, Some(250));
    }

    #[test]
    fn first_frame_seeds_the_accumulator_and_none_passes_it_through() {
        let seeded = merge_token_usage(None, Some(anthropic_start()));
        assert_eq!(seeded.expect("seeded").input_tokens, Some(1200));
        let carried = merge_token_usage(Some(anthropic_start()), None);
        assert_eq!(carried.expect("carried").input_tokens, Some(1200));
        assert!(merge_token_usage(None, None).is_none());
    }
}

#[cfg(test)]
mod extractor_framing_tests {
    /// message_start is the ONLY Anthropic event carrying input tokens and the
    /// two cache counters. Losing it loses the cache accounting entirely.
    #[test]
    fn extractor_reads_cache_counters_off_message_start() {
        let frame = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1200,\"output_tokens\":0,\"cache_read_input_tokens\":8000,\"cache_creation_input_tokens\":400}}}\n\n";
        let u = super::extract_token_usage_from_bytes(frame).expect("usage");
        assert_eq!(u.input_tokens.or(u.prompt_tokens), Some(1200));
        assert_eq!(u.cache_read_input_tokens, Some(8000));
        assert_eq!(u.cache_creation_input_tokens, Some(400));
    }

    /// message_delta carries the cumulative output at the TOP level.
    #[test]
    fn extractor_reads_output_off_message_delta() {
        let frame = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":180}}\n\n";
        let u = super::extract_token_usage_from_bytes(frame).expect("usage");
        assert_eq!(u.output_tokens.or(u.completion_tokens), Some(180));
    }

    /// The end-to-end shape of the original defect: run BOTH real Anthropic
    /// frames through extract-then-merge, exactly as the stream arms do, and
    /// assert the total that reaches the ledger. Before the fix this was None.
    #[test]
    fn a_real_anthropic_stream_yields_a_complete_total() {
        let start = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1200,\"output_tokens\":0,\"cache_read_input_tokens\":8000,\"cache_creation_input_tokens\":400}}}\n\n";
        let delta = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":180}}\n\n";
        let stop = b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

        let mut acc: Option<crate::types::TokenUsage> = None;
        for frame in [&start[..], &delta[..], &stop[..]] {
            acc = super::merge_token_usage(acc, super::extract_token_usage_from_bytes(frame));
        }
        let total = acc.expect("a Claude stream must record usage, not None");
        assert_eq!(
            total.input_tokens.or(total.prompt_tokens),
            Some(1200),
            "prompt tokens"
        );
        assert_eq!(
            total.output_tokens.or(total.completion_tokens),
            Some(180),
            "output tokens"
        );
        assert_eq!(total.cache_read_input_tokens, Some(8000), "cache read");
        assert_eq!(
            total.cache_creation_input_tokens,
            Some(400),
            "cache creation"
        );
    }

    /// The OpenAI shape must keep working — the data: scan and the new nested
    /// branch must not regress a provider that was already fine.
    #[test]
    fn openai_data_only_frames_still_extract() {
        let frame = b"data: {\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7}}\n\n";
        let u = super::extract_token_usage_from_bytes(frame).expect("usage");
        assert_eq!(u.prompt_tokens, Some(42));
        assert_eq!(u.completion_tokens, Some(7));
    }

    /// The reviewer of bead openproxy-kh7f caught this: the usage MERGE was
    /// implemented and unit-tested, but the tests fed it pre-built TokenUsage,
    /// so they never checked that the extractor can actually read a real
    /// Anthropic frame. It cannot: strip_sse_data_prefix takes the FIRST line,
    /// which on an Anthropic frame is `event: ...`, never `data: ...`.
    #[test]
    fn extractor_reads_a_named_event_anthropic_frame() {
        let frame = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1200,\"cache_read_input_tokens\":8000}}}\n\n";
        let got = super::extract_token_usage_from_bytes(frame);
        assert!(
            got.is_some(),
            "extractor must find the data: line in a named-event frame, got {got:?}"
        );
    }
}

#[cfg(test)]
#[cfg(test)]
mod sse_framing_tests {
    use super::drain_complete_sse_lines;

    fn buf_of(bytes: &[u8]) -> Vec<u8> {
        bytes.to_vec()
    }

    /// Regression (bead openproxy-0ph4): one transport chunk is NOT one SSE
    /// event. One read can carry several frames; one frame can straddle two
    /// reads. 9router acts only on COMPLETE lines
    /// (.tmp/9router/open-sse/utils/stream.js:110-119).
    #[test]
    fn one_chunk_carrying_two_frames_yields_two_lines() {
        let mut buf = buf_of(b"data: {\"a\":1}\ndata: {\"a\":2}\n");
        assert_eq!(
            drain_complete_sse_lines(&mut buf),
            vec!["data: {\"a\":1}", "data: {\"a\":2}"]
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn a_frame_split_across_two_reads_is_emitted_exactly_once() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"data: {\"a\"");
        assert!(
            drain_complete_sse_lines(&mut buf).is_empty(),
            "an incomplete frame must not be acted on"
        );
        buf.extend_from_slice(b":1}\n");
        assert_eq!(drain_complete_sse_lines(&mut buf), vec!["data: {\"a\":1}"]);
        assert!(buf.is_empty());
    }

    /// THE FINDING review surfaced: the buffer is bytes, and decoding each
    /// CHUNK with String::from_utf8_lossy turns ONE multi-byte character into
    /// TWO U+FFFD when it straddles a read boundary. That is permanent
    /// corruption on the main OpenAI-compatible path. A line terminated by
    /// \n is a safe decode boundary, so draining whole lines removes it.
    #[test]
    fn a_multibyte_char_split_across_reads_survives_intact() {
        let full = "data: {\"text\":\"xin chào\"}"; // 2-byte diacritic
        let bytes = full.as_bytes();
        // find a byte offset that lands INSIDE the multi-byte char
        let split = full.find("ào").unwrap() + 1; // between the two bytes of 'à'
        let mut buf = Vec::new();
        buf.extend_from_slice(&bytes[..split]);
        assert!(drain_complete_sse_lines(&mut buf).is_empty());
        buf.extend_from_slice(&bytes[split..]);
        buf.push(b'\n');
        let lines = drain_complete_sse_lines(&mut buf);
        assert_eq!(lines, vec![full], "the character must survive intact");
        assert!(
            !lines[0].contains('\u{FFFD}'),
            "no replacement char emitted"
        );
    }

    #[test]
    fn a_four_byte_emoji_split_across_reads_survives_intact() {
        let full = "data: {\"e\":\"\u{1F600}\"}";
        let bytes = full.as_bytes();
        let pos = full.find('\u{1F600}').unwrap();
        let split = pos + 2; // mid emoji
        let mut buf = Vec::new();
        buf.extend_from_slice(&bytes[..split]);
        assert!(drain_complete_sse_lines(&mut buf).is_empty());
        buf.extend_from_slice(&bytes[split..]);
        buf.push(b'\n');
        let lines = drain_complete_sse_lines(&mut buf);
        assert_eq!(lines, vec![full]);
        assert!(!lines[0].contains('\u{FFFD}'));
    }

    /// A frame whose JSON string value contains braces must not be split.
    #[test]
    fn braces_inside_a_string_value_do_not_split_a_frame() {
        let mut buf = buf_of(b"data: {\"text\":\"a}b{c}\"}\n");
        let lines = drain_complete_sse_lines(&mut buf);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("a}b{c}"));
    }

    /// A named-event frame is TWO physical lines and both must survive.
    #[test]
    fn a_named_event_frame_keeps_both_lines() {
        let mut buf = buf_of(b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n");
        assert_eq!(
            drain_complete_sse_lines(&mut buf),
            vec!["event: message_start", "data: {\"type\":\"message_start\"}"]
        );
    }

    /// No trailing newline: held for the caller's EOF flush, never dropped.
    #[test]
    fn a_trailing_frame_without_a_newline_stays_in_the_buffer() {
        let mut buf = buf_of(b"data: {\"a\":1}");
        assert!(drain_complete_sse_lines(&mut buf).is_empty());
        assert_eq!(buf, b"data: {\"a\":1}");
    }

    #[test]
    fn crlf_terminators_are_normalised() {
        let mut buf = buf_of(b"data: {\"a\":1}\r\n");
        assert_eq!(drain_complete_sse_lines(&mut buf), vec!["data: {\"a\":1}"]);
    }

    /// Blank keep-alive lines are separators, not frames.
    #[test]
    fn blank_separator_lines_produce_no_output() {
        let mut buf = buf_of(b"data: {\"a\":1}\n\ndata: {\"a\":2}\n");
        assert_eq!(
            drain_complete_sse_lines(&mut buf),
            vec!["data: {\"a\":1}", "data: {\"a\":2}"]
        );
    }
}

#[cfg(test)]
mod passthrough_framing_tests {
    use super::{drain_complete_sse_lines, passthrough_frame_bytes};
    use crate::server::api::sanitization::sanitize_sse_body;

    /// Regression (openproxy-24's review of 764469cd): the passthrough path is
    /// supposed to relay upstream framing VERBATIM. drain_complete_sse_lines
    /// strips the terminator, which is right for the translation path but broke
    /// this one: sanitize_sse_body only re-appends a newline when the line it is
    /// given already ends in one, so two consecutive frames were emitted as
    ///     data: {"a":1}data: {"a":2}
    /// which a client parses as ONE malformed frame and the stream dies.
    #[test]
    fn two_passthrough_frames_stay_two_frames() {
        let mut buf: Vec<u8> = b"data: {\"a\":1}\n\ndata: {\"a\":2}\n\n".to_vec();
        let mut emitted = String::new();
        for line in drain_complete_sse_lines(&mut buf) {
            let bytes = passthrough_frame_bytes(&line);
            emitted.push_str(&sanitize_sse_body(&String::from_utf8_lossy(&bytes)));
        }
        // Count SSE data lines the way a client would: split on blank line.
        let frames: Vec<&str> = emitted
            .split("\n\n")
            .filter(|f| !f.trim().is_empty())
            .collect();
        assert_eq!(
            frames.len(),
            2,
            "two frames in, two frames out; got {emitted:?}"
        );
        for f in &frames {
            assert!(
                f.trim_start().starts_with("data: "),
                "each frame keeps its data: prefix: {f:?}"
            );
        }
        assert!(
            !emitted.contains("data: {\"a\":1}data:"),
            "frames must not be concatenated: {emitted:?}"
        );
    }

    /// The second defect, pinned against the REAL extraction this time: the
    /// flush used to pass a newline-free leftover back through
    /// drain_complete_sse_lines, which returns [] for terminator-free input, so
    /// the final frame was dropped on BOTH arms. The earlier version of this
    /// test asserted the property of the drain helper instead and stayed green
    /// against the bug — exactly the "test that cannot fail" trap. This one
    /// runs the function the flush actually calls.
    #[test]
    fn a_newline_free_leftover_is_still_a_frame() {
        let mut leftover: Vec<u8> = b"data: {\"a\":2}".to_vec();
        let frame = super::take_terminal_passthrough_frame(&mut leftover)
            .expect("a terminator-free leftover holding a frame must still be emitted");
        let text = String::from_utf8_lossy(&frame).into_owned();
        assert!(
            text.contains("data:"),
            "final frame must reach the client: {text:?}"
        );
        // The extractor itself does NOT append a separator: the call site
        // routes this through passthrough_frame_bytes, which is where the
        // delimiter is attached. Appending it in both places gave every terminal
        // frame a four-newline tail. What matters here is that the frame is
        // recovered at all, and that it is not pre-terminated. The
        // passthrough_frame_bytes test covers the delimiter.
        assert!(
            !text.ends_with("\n\n"),
            "the extractor must not pre-terminate — passthrough_frame_bytes owns \
             the delimiter, so adding it here would double it: {text:?}"
        );
        assert!(leftover.is_empty(), "buffer is drained by the extraction");
    }

    #[test]
    fn an_empty_or_whitespace_leftover_emits_nothing() {
        assert!(super::take_terminal_passthrough_frame(&mut Vec::new()).is_none());
        let mut crlf_only: Vec<u8> = b"\r".to_vec();
        assert!(super::take_terminal_passthrough_frame(&mut crlf_only).is_none());
    }

    /// A passthrough frame must keep SSE framing, never be re-wrapped as
    /// `data: <whole line>` — that would change the semantics of a line that is
    /// not itself a data line.
    #[test]
    fn passthrough_framing_does_not_rewrap_non_data_lines() {
        let framed = passthrough_frame_bytes("event: ping");
        let text = String::from_utf8_lossy(&framed).into_owned();
        assert_eq!(text, "event: ping\n\n", "event lines pass through as-is");
        assert!(!text.starts_with("data: event:"));
    }
}

#[cfg(test)]
mod non_sse_guard_tests {
    use super::upstream_error_message;

    /// Regression (bead openproxy-n12c). 9router
    /// streamingHandler.js:61-67 pulls a short message out of <title>, strips
    /// tags and clamps, precisely because "untrusted upstream text never
    /// reaches the client verbatim (the UI may render error.message as HTML)".
    /// The previous OpenProxy guard pasted the first 500 raw bytes of the
    /// upstream body into error.message, so an HTML error page became an XSS
    /// sink.
    #[test]
    fn an_html_error_page_never_reaches_the_client_verbatim() {
        let hostile = concat!(
            "<html><head><title>",
            "<script>alert('xss')</script>Gateway Timeout",
            "</title></head><body>nginx</body></html>"
        );
        let msg = upstream_error_message(hostile, "text/html");
        // No tag markup may survive — this is the actual XSS boundary, since
        // the dashboard may render error.message as HTML.
        assert!(!msg.contains('<'), "tags must be stripped: {msg:?}");
        assert!(!msg.contains('>'), "tags must be stripped: {msg:?}");
        // A title containing markup is not unwrapped at all (9router's [^<]+
        // rule) — the message degrades to the body branch.
        //
        // Note on the threat model, checked here so it is not assumed: 9router's
        // tag regex removes the MARKUP and keeps the text between tags, so a
        // script BODY still appears as plain text. That is not a hole — the
        // message carries no tags, so a UI rendering error.message as HTML shows
        // inert characters. The property that actually matters, and that is
        // asserted above, is that no '<' or '>' survives. Asserting the script
        // text is gone would demand a behaviour 9router does not have.
        assert!(
            msg.contains("Gateway Timeout"),
            "the page is still described: {msg:?}"
        );
    }

    /// A plain title IS extracted — the happy path, so the guard does not
    /// degrade every real upstream HTML page to a generic message.
    #[test]
    fn a_plain_html_title_becomes_the_message() {
        let msg = upstream_error_message(
            "<html><head><title>Gateway Timeout</title></head><body>nginx</body></html>",
            "text/html",
        );
        assert_eq!(msg, "Gateway Timeout");
    }

    /// Tag stripping still applies to the BODY branch (no title present).
    #[test]
    fn the_body_branch_also_strips_markup() {
        let msg = upstream_error_message("error: <b>bad</b> gateway", "text/plain");
        assert!(
            !msg.contains('<'),
            "tags stripped in the body branch too: {msg:?}"
        );
        assert_eq!(msg, "error: bad gateway");
    }

    /// 9router clamps to 160 characters; a long title must not be relayed whole.
    #[test]
    fn the_message_is_clamped_to_160_chars() {
        let long = format!("<title>{}</title>", "A".repeat(500));
        let msg = upstream_error_message(&long, "text/html");
        assert_eq!(msg.chars().count(), 160, "clamped: {}", msg.len());
    }

    /// Newlines in a title must collapse — they would otherwise split the
    /// JSON error body across lines.
    #[test]
    fn newlines_in_a_title_are_flattened() {
        let msg = upstream_error_message(
            "<title>line one\nline two\r\nline three</title>",
            "text/html",
        );
        assert!(!msg.contains('\n'), "newlines flattened: {msg:?}");
        assert!(!msg.contains('\r'), "CR flattened: {msg:?}");
        assert_eq!(msg, "line one line two line three");
    }

    /// No title, small body: 9router uses the body itself (tags stripped).
    #[test]
    fn a_small_bodiless_title_body_is_used() {
        let msg = upstream_error_message("upstream said no", "text/plain");
        assert_eq!(msg, "upstream said no");
    }

    /// No title, LARGE body: 9router substitutes a generic message rather than
    /// dumping an arbitrary amount of untrusted text.
    #[test]
    fn a_large_bodiless_title_body_becomes_a_generic_message() {
        let big = "X".repeat(500);
        let msg = upstream_error_message(&big, "text/plain");
        assert_eq!(msg, "Upstream returned non-SSE response (text/plain)");
    }

    /// An empty body must not produce an empty error message.
    #[test]
    fn an_empty_body_still_yields_a_usable_message() {
        let msg = upstream_error_message("", "application/xml");
        assert_eq!(msg, "Upstream returned non-SSE response (application/xml)");
        assert!(!msg.is_empty());
    }
}

#[cfg(test)]
mod non_sse_predicate_tests {
    use super::should_block_non_sse;

    /// Regression (bead openproxy-n12c): 9router's rule is an ALLOW-list of
    /// text/event-stream and application/json. OpenProxy shipped a DENY-list of
    /// three types, which had two opposite failures — it blocked
    /// application/json (turning a 200-with-JSON-body into a retryable 502,
    /// i.e. a retry-storm amplifier) and let every OTHER type through into the
    /// SSE transform, producing garbage frames with no terminal [DONE].
    #[test]
    fn only_non_sse_non_json_types_are_blocked() {
        // allowed: these two families are exactly what 9router lets through
        assert!(!should_block_non_sse("text/event-stream"));
        assert!(!should_block_non_sse("text/event-stream; charset=utf-8"));
        assert!(!should_block_non_sse("application/json"));
        assert!(!should_block_non_sse("application/json; charset=utf-8"));
        // an EMPTY content-type is not blocked, matching 9router's
        // `upstreamContentType &&` guard
        assert!(!should_block_non_sse(""));
        // Types OpenProxy genuinely streams and transforms. kiro answers with
        // AWS EventStream; response_transform.rs:1041 also consumes ndjson and
        // octet-stream. Blocking any of these kills a live provider or makes
        // the binary transformer unreachable. A first cut of this test listed
        // them as BLOCKED, because it was written straight from 9router's
        // two-entry list — which is how a P0 shipped in 08e746aa.
        for allowed in [
            "application/vnd.amazon.eventstream", // kiro (kiro.rs:765)
            "application/x-ndjson",               // response_transform.rs:1041
            "application/octet-stream",           // response_transform.rs:1041
        ] {
            assert!(
                !should_block_non_sse(allowed),
                "{allowed} must reach the transformer"
            );
        }
        // everything else is blocked
        for blocked in [
            "text/html",
            "text/plain",
            "application/xml",
            "application/pdf",
            "image/png",
            "application/javascript",
        ] {
            assert!(should_block_non_sse(blocked), "{blocked} must be blocked");
        }
    }

    /// Case-insensitive, like 9router's `.toLowerCase()` on the header.
    #[test]
    fn the_content_type_check_is_case_insensitive() {
        assert!(!should_block_non_sse("TEXT/EVENT-STREAM"));
        assert!(!should_block_non_sse("Application/JSON"));
        assert!(should_block_non_sse("TEXT/HTML"));
    }

    /// The exact failure the deny-list caused: a provider that ignored
    /// stream:true and returned a JSON body must NOT be blocked, or the client
    /// gets a retryable 502 instead of the body.
    #[test]
    fn a_json_body_from_a_streaming_request_is_not_blocked() {
        assert!(
            !should_block_non_sse("application/json"),
            "blocking this is what produced the 502 retry storm"
        );
    }
}

#[cfg(test)]
mod passthrough_transform_tests {
    use super::{
        apply_passthrough_transforms, normalise_passthrough_chunk, passthrough_needs_done_sentinel,
    };
    use serde_json::{json, Value};

    fn transformed(line: &str) -> Value {
        let out = apply_passthrough_transforms(line, "openai");
        serde_json::from_str(out.trim().trim_start_matches("data:").trim()).expect("json out")
    }

    /// 9router streamHelpers.js:65 — an id of "chat", "completion", or fewer
    /// than 8 chars is replaced, because strict clients reject it.
    #[test]
    fn a_degenerate_id_is_replaced() {
        for bad in ["chat", "completion", "short", "x"] {
            let v = transformed(&format!("data: {{\"id\":\"{bad}\",\"choices\":[]}}"));
            let id = v["id"].as_str().unwrap_or("");
            assert!(id.starts_with("chatcmpl-"), "{bad} -> {id}");
        }
    }

    #[test]
    fn a_valid_id_is_left_alone() {
        let v = transformed("data: {\"id\":\"chatcmpl-abcdefgh123\",\"choices\":[]}");
        assert_eq!(v["id"], "chatcmpl-abcdefgh123");
    }

    /// stream.js:141-144 — "Ensure OpenAI-required fields are present on
    /// streaming chunks (Letta compat)".
    #[test]
    fn object_and_created_are_injected_when_choices_present() {
        let v = transformed("data: {\"id\":\"chatcmpl-abcdefgh\",\"choices\":[{}]}");
        assert_eq!(v["object"], "chat.completion.chunk");
        assert!(v["created"].is_i64(), "created must be an integer: {v}");
    }

    #[test]
    fn object_and_created_are_not_injected_without_choices() {
        let v = transformed("data: {\"id\":\"chatcmpl-abcdefgh\"}");
        assert!(v.get("object").is_none(), "no choices -> no injection: {v}");
        assert!(
            v.get("created").is_none(),
            "no choices -> no injection: {v}"
        );
    }

    /// stream.js:147-157 — Azure-only fields are not standard OpenAI.
    #[test]
    fn azure_filter_fields_are_removed() {
        let v = transformed(
            "data: {\"id\":\"chatcmpl-abcdefgh\",\"prompt_filter_results\":{\"x\":1},\"choices\":[{\"content_filter_results\":{\"y\":2}}]}",
        );
        assert!(v.get("prompt_filter_results").is_none(), "{v}");
        assert!(
            v["choices"][0].get("content_filter_results").is_none(),
            "{v}"
        );
    }

    /// stream.js:160-180 — "Some providers (e.g. CodeBuddy CN) include
    /// tool_calls: [] in every streaming delta. The AI SDK checks
    /// delta.tool_calls != null; an EMPTY array passes that check, causing
    /// premature reasoning-end on every chunk."
    #[test]
    fn an_empty_tool_calls_array_is_removed() {
        let v = transformed(
            "data: {\"id\":\"chatcmpl-abcdefgh\",\"choices\":[{\"delta\":{\"tool_calls\":[]}}]}",
        );
        assert!(
            v["choices"][0]["delta"].get("tool_calls").is_none(),
            "empty array must be deleted so `!= null` is false: {v}"
        );
    }

    #[test]
    fn a_non_empty_tool_calls_array_is_preserved() {
        let v = transformed(
            "data: {\"id\":\"chatcmpl-abcdefgh\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"c1\"}]}}]}",
        );
        assert_eq!(v["choices"][0]["delta"]["tool_calls"][0]["id"], "c1");
    }

    /// stream.js:225-231 — non-JSON data lines are skipped, not forwarded as
    /// broken frames.
    #[test]
    fn a_non_json_data_line_is_left_untouched() {
        let raw = "data: upstream rate limit reached";
        assert_eq!(apply_passthrough_transforms(raw, "openai"), raw);
    }

    #[test]
    fn the_done_sentinel_and_event_lines_pass_through() {
        assert_eq!(
            apply_passthrough_transforms("data: [DONE]", "openai"),
            "data: [DONE]"
        );
        assert_eq!(
            apply_passthrough_transforms("event: ping", "openai"),
            "event: ping"
        );
    }

    /// A chunk needing no change must be relayed byte-identical, matching
    /// 9router's `else if (idFixed || fieldsInjected)` gate — otherwise every
    /// chunk would be needlessly re-serialised.
    #[test]
    fn an_unchanged_chunk_is_relayed_verbatim() {
        // A chunk that already carries every field the transforms would add,
        // so nothing fires and the line must be relayed byte-identical.
        let raw = concat!(
            "data: {\"id\":\"chatcmpl-abcdefgh\",",
            "\"object\":\"chat.completion.chunk\",",
            "\"created\":1700000000,",
            "\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}"
        );
        assert_eq!(apply_passthrough_transforms(raw, "openai"), raw);

        // And a chunk MISSING object/created must come back re-serialised with
        // them injected — the complement of the case above.
        let bare =
            "data: {\"id\":\"chatcmpl-abcdefgh\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}";
        let out = apply_passthrough_transforms(bare, "openai");
        assert_ne!(out, bare, "a chunk needing injection must be re-serialised");
        let v: serde_json::Value =
            serde_json::from_str(out.trim().trim_start_matches("data:").trim()).expect("json");
        assert_eq!(v["object"], "chat.completion.chunk");
    }

    /// stream.js:398-401 — the sentinel is required for most providers and
    /// rejected by the Gemini family.
    #[test]
    fn the_done_sentinel_is_withheld_only_for_the_gemini_family() {
        for p in [
            "openai",
            "openrouter",
            "anthropic",
            "kiro",
            "qoder",
            "grok-cli",
        ] {
            assert!(passthrough_needs_done_sentinel(p), "{p} needs the sentinel");
        }
        for p in ["antigravity", "gemini", "vertex"] {
            assert!(
                !passthrough_needs_done_sentinel(p),
                "{p} rejects the sentinel"
            );
        }
    }

    /// The transform must never panic on odd shapes.
    #[test]
    fn odd_payload_shapes_do_not_panic() {
        for raw in [
            "data: null",
            "data: []",
            "data: {\"choices\":\"notanarray\"}",
            "data: {\"choices\":[null]}",
            "data: {\"id\":123}",
            "data: {\"choices\":[{\"delta\":null}]}",
            "data: {}",
        ] {
            let _ = normalise_passthrough_chunk(
                &mut serde_json::from_str::<Value>(raw.trim_start_matches("data:").trim())
                    .unwrap_or(Value::Null),
            );
        }
    }
}

#[cfg(test)]
mod done_sentinel_tests {
    use super::should_emit_done_sentinel;

    /// Regression (openproxy-24's review of 3ac819b3): the sentinel was gated
    /// only on the PROVIDER name, with no knowledge of which stream branch the
    /// request actually took, and it was emitted BEFORE qoder_coalescer_flush
    /// and finish_stream. A client stops reading at [DONE], so that ordering
    /// made it discard the finish+usage chunk the coalescer holds and the
    /// terminal chunk finish_stream emits — silently truncating qoder and kiro
    /// streams. The first version of this test asserted only the provider
    /// predicate, which stayed green through both defects.
    #[test]
    fn no_sentinel_when_the_request_did_not_take_passthrough() {
        for (took, provider) in [
            (false, "qoder"),  // coalescer holds a finish+usage chunk
            (false, "kiro"),   // finish_stream emits a terminal chunk
            (false, "openai"), // translation branch
            (false, "claude"), // translation branch
        ] {
            assert!(
                !should_emit_done_sentinel(took, provider, false),
                "{provider} on a non-passthrough branch must not receive a sentinel"
            );
        }
    }

    #[test]
    fn a_real_passthrough_stream_gets_the_sentinel() {
        assert!(should_emit_done_sentinel(true, "openai", false));
        assert!(should_emit_done_sentinel(true, "openrouter", false));
    }

    #[test]
    fn no_duplicate_sentinel_when_upstream_sent_one() {
        assert!(
            !should_emit_done_sentinel(true, "openai", true),
            "upstream already terminated the stream"
        );
    }

    #[test]
    fn the_gemini_family_still_rejects_the_sentinel() {
        // Even on passthrough: these reject it with a 400 syntax error.
        for p in ["antigravity", "gemini", "vertex"] {
            assert!(!should_emit_done_sentinel(true, p, false), "{p}");
        }
    }
}

#[cfg(test)]
mod done_sentinel_detection_tests {
    use super::is_done_sentinel;

    /// SSE permits `data:[DONE]` with no space. The `saw_done` tracker compared
    /// against the exact string "data: [DONE]" while the transform used a
    /// prefix+trim comparison, so a no-space terminator set the transform's
    /// early return but NOT the tracker's flag, and the stream still got a
    /// duplicate terminator. One definition now, used by all three sites.
    #[test]
    fn both_spellings_of_the_terminator_are_detected() {
        for line in [
            "data: [DONE]",
            "data:[DONE]",
            "  data: [DONE]  ",
            "\tdata:[DONE]",
        ] {
            assert!(is_done_sentinel(line), "{line:?} must be detected");
        }
    }

    #[test]
    fn ordinary_data_lines_are_not_mistaken_for_the_terminator() {
        for line in [
            "data: {\"a\":1}",
            "data: [DONE ]",
            "data: [DONE",
            "event: message_stop",
            "[DONE]",
            "",
        ] {
            assert!(!is_done_sentinel(line), "{line:?} must not match");
        }
    }
}

#[cfg(test)]
mod sse_stall_clock_tests {
    use super::sse_stall_timeout;

    /// Bead openproxy-qzj8. 9router sets 360s
    /// (config/runtimeConfig.js:53) "so slow reasoning models aren't aborted
    /// mid-stream". OpenProxy hard-coded 180s in chat.rs while
    /// runtime_config::STREAM_STALL_TIMEOUT_MS already carried 360s labelled
    /// "matching the 9router default" — and nothing read it. A reasoning turn
    /// that pauses three to six minutes was cut here and not there, and there
    /// were two constants claiming to be the same policy.
    #[test]
    fn the_stall_clock_is_9routers_and_not_the_old_180s() {
        let d = sse_stall_timeout();
        assert_eq!(d, std::time::Duration::from_secs(360));
        assert_ne!(d, std::time::Duration::from_secs(180));
    }

    /// The value must be the SHARED one, not a second literal that happens to
    /// agree. If someone changes the runtime config, the stream must follow.
    #[test]
    fn the_stall_clock_tracks_the_shared_runtime_config() {
        assert_eq!(
            sse_stall_timeout(),
            std::time::Duration::from_millis(
                crate::core::config::runtime_config::STREAM_STALL_TIMEOUT_MS
            )
        );
    }
}

#[cfg(test)]
mod eof_order_tests {
    use super::{plan_eof_emits, EofEmit};

    fn f(s: &str) -> bytes::Bytes {
        bytes::Bytes::copy_from_slice(s.as_bytes())
    }

    /// Bead openproxy-jkit. A client stops reading at [DONE], so everything the
    /// stream still owes the client must be emitted BEFORE it. This is the
    /// order a unit test could never check while the sequence lived inline in
    /// the async_stream closure — which is how a [DONE] emitted too early went
    /// unnoticed through six review rounds while every helper test was green.
    #[test]
    fn the_done_sentinel_is_always_last() {
        let plan = plan_eof_emits(
            true,
            "openai",
            false,
            vec!["data: {\"d\":1}".into()], // dashboard
            vec!["data: {\"t\":1}".into()], // translate
            Some(f("data: {\"p\":1}")),
            vec!["data: {\"c\":1}".into()], // qoder coalescer
            vec!["data: {\"f\":1}".into()], // finish_stream
        );
        assert_eq!(plan.len(), 6);
        assert_eq!(plan.last(), Some(&EofEmit::Done), "sentinel must be last");
        assert_eq!(
            plan.iter().filter(|e| **e == EofEmit::Done).count(),
            1,
            "exactly one sentinel"
        );
    }

    /// The regression that actually shipped: a kiro request's finish_stream
    /// terminal chunk arrived AFTER the sentinel, so the client stopped reading
    /// and lost it.
    #[test]
    fn a_kiro_terminal_chunk_precedes_the_sentinel() {
        let plan = plan_eof_emits(
            false,
            "kiro",
            false,
            vec![],
            vec![],
            None,
            vec![],
            vec!["data: {\"type\":\"message_stop\"}".into()],
        );
        assert!(
            plan.iter().all(|e| *e != EofEmit::Done),
            "a translated stream has no passthrough sentinel at all: {plan:?}"
        );
        assert_eq!(plan.len(), 1, "the finish_stream chunk is delivered");
    }

    /// Same for qoder: the coalescer holds a finish+usage chunk back, and a
    /// sentinel emitted before it would swallow the usage accounting.
    #[test]
    fn a_qoder_usage_chunk_precedes_any_sentinel() {
        let plan = plan_eof_emits(
            false,
            "qoder",
            false,
            vec![],
            vec![],
            None,
            vec!["data: {\"usage\":{\"total_tokens\":5}}".into()],
            vec![],
        );
        assert!(plan.iter().all(|e| *e != EofEmit::Done));
        assert_eq!(plan.len(), 1, "the coalescer chunk is delivered");
    }

    /// A real passthrough stream gets its frames and then exactly one sentinel.
    #[test]
    fn a_passthrough_stream_ends_with_one_sentinel() {
        let plan = plan_eof_emits(
            true,
            "openai",
            false,
            vec![],
            vec![],
            Some(f("data: {\"p\":1}")),
            vec![],
            vec![],
        );
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], EofEmit::Frame(f("data: {\"p\":1}")));
        assert_eq!(plan[1], EofEmit::Done);
    }

    /// Upstream already terminated: no second sentinel.
    #[test]
    fn an_upstream_terminator_is_not_duplicated() {
        let plan = plan_eof_emits(
            true,
            "openai",
            true,
            vec![],
            vec![],
            Some(f("data: [DONE]")),
            vec![],
            vec![],
        );
        assert!(plan.iter().all(|e| *e != EofEmit::Done));
    }

    /// The gemini family still rejects the sentinel even on passthrough.
    #[test]
    fn the_gemini_family_gets_no_sentinel() {
        for p in ["antigravity", "gemini", "vertex"] {
            let plan = plan_eof_emits(
                true,
                p,
                false,
                vec![],
                vec![],
                Some(f("data: {\"p\":1}")),
                vec![],
                vec![],
            );
            assert!(
                plan.iter().all(|e| *e != EofEmit::Done),
                "{p} must not receive it"
            );
        }
    }

    /// Everything is empty -> nothing is emitted. A no-op EOF must not invent
    /// a terminator on a stream that produced nothing.
    #[test]
    fn an_empty_eof_emits_nothing() {
        let plan = plan_eof_emits(false, "openai", false, vec![], vec![], None, vec![], vec![]);
        assert!(plan.is_empty(), "{plan:?}");
    }
}
