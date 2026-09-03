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
    check_fallback_error, detect_required_capabilities, execute_combo_strategy_full,
    get_combo_models_from_data, get_disabled_members_for_combo, mark_combo_member_quarantined,
    strategy_for_combo, ComboAttemptError, ComboExecutionError, ComboStrategy, FusionConfig,
    ModelCapacity,
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
use crate::server::auth::{extract_api_key, require_api_key};
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
    if require_api_key_auth && state.db.snapshot().settings.require_login {
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
    // Only non-streaming requests are cached: the cache stores a single JSON
    // body, and a streaming client would misinterpret a cached non-SSE body.
    let is_streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    if !is_streaming {
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
                let f_state = state.clone();
                let f_body = body.clone();
                let f_api_key = presented_api_key.clone();
                let f_client_tool = client_tool;
                let f_headers = headers_map.clone();

                let panel_count = combo_models.len();
                let fusion_cfg = fusion_config_for(&snapshot, &combo_name, panel_count);
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
                        &combo_models,
                        &fusion_cfg,
                        None,
                        move |model: String, panel_body: Value| {
                            let state = f_state.clone();
                            let body = f_body.clone();
                            let api_key = f_api_key.clone();
                            let client_tool = f_client_tool;
                            let headers = f_headers.clone();
                            async move {
                                let response = dispatch_fusion_leg(
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
                                .map_err(|e| {
                                    anyhow::anyhow!("Fusion panel failed: {}", e.message)
                                })?;
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
                        &combo_models,
                        &fusion_cfg,
                        None,
                        move |model: String, panel_body: Value| {
                            let state = f_state.clone();
                            let body = f_body.clone();
                            let api_key = f_api_key.clone();
                            let client_tool = f_client_tool;
                            let headers = f_headers.clone();
                            async move {
                                let response = dispatch_fusion_leg(
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
                                .map_err(|e| {
                                    anyhow::anyhow!("Fusion panel failed: {}", e.message)
                                })?;
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
                    Err(e) => Err(ComboExecutionError {
                        status: e.status,
                        message: e.message,
                        earliest_retry_after: None,
                        upstream_body: None,
                    }),
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
            match execute_single_model(
                &state,
                &body,
                model_str,
                presented_api_key.as_deref(),
                endpoint,
                &plan,
                client_tool,
                Some(&headers_map),
            )
            .await
            {
                Ok(response) => response,
                Err(error) => attempt_error_response(error),
            }
        }
    };

    // Feature4: populate the cache on a successful non-streaming miss.
    if !is_streaming {
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

    // 8. TTS models: strip tool messages + tools (9router chatCore.js:185-189)
    let model_lower = plan.model.to_lowercase();
    if model_lower.contains("tts")
        || model_lower.contains("speech")
        || model_lower.starts_with("tts-")
    {
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

    forward_with_provider_fallback(
        state,
        &plan.provider,
        &dispatch_model,
        body,
        api_key,
        endpoint,
        plan,
        client_tool,
        compression_stats,
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
    compression: Option<CompressionStats>,
) -> Result<Response, ComboAttemptError> {
    let mut excluded = HashSet::new();
    let mut last_error: Option<ComboAttemptError> = None;
    let registry = &state.account_registry;

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

    loop {
        let snapshot = state.db.snapshot();
        let Some(mut connection) =
            select_connection(&snapshot, provider, model, &excluded, Some(registry))
        else {
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
            DevinExecutionRequest, ExecutionRequest, GeminiCliExecutionRequest, GeminiCliExecutor,
            GithubExecutionRequest, GithubExecutor, GrokWebExecutionRequest, GrokWebExecutor,
            IFlowExecutionRequest, IFlowExecutor, KimchiExecutor, KiroExecutionRequest,
            KiroExecutor, KiroExecutorResponse, OpenCodeExecutionRequest, OpenCodeExecutor,
            OpenCodeGoExecutionRequest, OpenCodeGoExecutor, PerplexityWebExecutionRequest,
            PerplexityWebExecutor, ProviderExecutionRequest, ProviderExecutor,
            QoderExecutionRequest, QoderExecutor, QwenExecutionRequest, QwenExecutor,
            TraeExecutionRequest, TraeExecutor, VertexExecutionRequest, VertexExecutor,
            WindsurfExecutionRequest, WindsurfExecutor,
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
                let (message, body_retry_after) =
                    extract_error_message_and_retry_after(result.response).await;
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
                // On success, merge credentials (expires_at, refresh, PSD) and
                // continue the loop so the fresh snapshot picks up the token.
                if (status.as_u16() == 401 || status.as_u16() == 403)
                    && connection.refresh_token.is_some()
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
                                        conn.error_code = None;
                                        conn.backoff_level = Some(0);
                                    }
                                })
                                .await;
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
                        decision.new_backoff_level.unwrap_or(current_backoff + 1),
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
                        decision.new_backoff_level.unwrap_or(current_backoff + 1),
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
                None, // latency_ms
                None, // ttft_ms
                None, // status
                None, // error_class
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
                // Selective clear: remove expired locks + lock for succeeded model only
                let model_key = succeeded_model.as_ref().map(|m| format!("modelLock_{m}"));
                connection.extra.retain(|k, v| {
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
                    // Drop succeeded model lock
                    if let Some(ref mk) = model_key {
                        if k == mk {
                            return false;
                        }
                    }
                    true
                });
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

    let json_body =
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
                None, // latency_ms
                None, // ttft_ms
                None, // status
                None, // error_class
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
                compression,
                None, // latency_ms
                None, // ttft_ms
                None, // status
                None, // error_class
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
    if !ct.is_empty()
        && !ct.contains("text/event-stream")
        && !ct.contains("application/octet-stream")
        && !ct.contains("application/x-ndjson")
        && (ct.contains("text/html")
            || ct.contains("application/json")
            || ct.contains("text/plain"))
    {
        // Collect body and return structured error instead of piping garbage as SSE
        let (body_bytes, _) = collect_upstream_response_bytes(response).await;
        let msg = String::from_utf8_lossy(&body_bytes);
        let msg = if msg.len() > 500 {
            format!("{}…", &msg[..500])
        } else {
            msg.to_string()
        };
        tracing::warn!(
            target: "openproxy::chat",
            "STREAM_GUARD non-SSE content-type={} status={} body_snip={}",
            ct,
            status.as_u16(),
            msg.chars().take(120).collect::<String>()
        );
        let err = json!({
            "error": {
                "message": format!("Upstream returned non-SSE content-type '{ct}': {msg}"),
                "type": "server_error",
                "code": "upstream_non_sse"
            }
        });
        return with_cors_response(
            (
                StatusCode::BAD_GATEWAY,
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
    let qoder_sse_unwrap = provider == "qoder";
    // Billing block detection state (9router v0.5.55 peekFirstQoderFrame).
    let mut qoder_seen_first_frame = false;
    let mut qoder_billing_block = false;
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
            let stream = async_stream::stream! {
                let mut upstream = response.bytes_stream();
                // Persistent state for streaming format translation (e.g. Responses API -> Chat Completions).
                let mut t_state = if needs_stream_translation {
                    Some(crate::core::translator::registry::ResponseTransformState::default())
                } else {
                    None
                };
                // Accumulate the last data frame for best-effort `usage` extraction
                // at stream end. Streaming SSE responses usually lack a usage field,
                // so most requests record with tokens=None (request count only).
                let mut last_data: Option<Bytes> = None;
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
                            record_streaming_usage(&state, &provider, &model,
                                connection_id.as_deref(), api_key.as_deref(), endpoint, &last_data, compression.clone()).await;
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
                            last_data = Some(chunk.clone());
                            if qoder_sse_unwrap {
                                for line in qoder_unwrap_sse_chunk(
                                    &chunk,
                                    &mut pending_text,
                                    &mut qoder_seen_first_frame,
                                    &mut qoder_billing_block,
                                ) {
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(line));
                                }
                            } else if let Some(transformer) = transformer.as_mut() {
                                for line in transform_dashboard_sse_chunk(&chunk, transformer.as_mut(), &mut pending_text) {
                                    if let Some(frame) = sse_frame_for_dashboard(&line) {
                                        yield Ok::<Bytes, std::io::Error>(frame);
                                    }
                                }
                            } else if needs_stream_translation {
                                if let Some(ref mut t_state) = t_state {
                                    let chunks = registry::global_registry()
                                        .translate_response(
                                            stream_target_format,
                                            stream_source_format,
                                            &chunk,
                                            t_state,
                                        );
                                    for line in chunks {
                                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                                            yield Ok::<Bytes, std::io::Error>(frame);
                                        }
                                    }
                                } else {
                                    yield Ok::<Bytes, std::io::Error>(chunk);
                                }
                            } else {
                                yield Ok::<Bytes, std::io::Error>(chunk);
                            }
                        }
                        Ok(Ok(None)) => break,
                        Ok(Err(_)) => {
                            record_streaming_usage(&state, &provider, &model,
                                connection_id.as_deref(), api_key.as_deref(), endpoint, &last_data, compression.clone()).await;
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
                if let Some(transformer) = transformer.as_mut() {
                    for line in flush_dashboard_sse_chunk(transformer.as_mut(), &mut pending_text) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                // End-of-stream flush: emit the terminal chunk + [DONE] for
                // buffered binary transforms (kiro EventStream → SSE).
                if let Some(ref mut t_state) = t_state {
                    for line in registry::global_registry().finish_stream(
                        stream_source_format,
                        stream_target_format,
                        t_state,
                    ) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                record_streaming_usage(&state, &provider, &model,
                    connection_id.as_deref(), api_key.as_deref(), endpoint, &last_data, compression.clone()).await;
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
            let stream = async_stream::stream! {
                // Persistent state for streaming format translation (e.g. Responses API -> Chat Completions).
                let mut t_state = if needs_stream_translation {
                    Some(crate::core::translator::registry::ResponseTransformState::default())
                } else {
                    None
                };
                // Accumulate the last data frame for best-effort `usage` extraction
                // at stream end (streaming SSE responses usually lack a usage field).
                let mut last_data: Option<Bytes> = None;
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
                            record_streaming_usage(&state, &provider, &model,
                                connection_id.as_deref(), api_key.as_deref(), endpoint, &last_data, compression.clone()).await;
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
                                last_data = Some(data.clone());
                                if let Some(transformer) = transformer.as_mut() {
                                    for line in transform_dashboard_sse_chunk(&data, transformer.as_mut(), &mut pending_text) {
                                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                                            yield Ok::<Bytes, std::io::Error>(frame);
                                        }
                                    }
                                } else if needs_stream_translation {
                                    if let Some(ref mut t_state) = t_state {
                                        let chunks = registry::global_registry()
                                            .translate_response(
                                                stream_target_format,
                                                stream_source_format,
                                                &data,
                                                t_state,
                                            );
                                        for line in chunks {
                                            if let Some(frame) = sse_frame_for_dashboard(&line) {
                                                yield Ok::<Bytes, std::io::Error>(frame);
                                            }
                                        }
                                    } else {
                                        yield Ok::<Bytes, std::io::Error>(data);
                                    }
                                } else {
                                    yield Ok::<Bytes, std::io::Error>(data);
                                }
                            }
                        }
                        Err(_) => {
                            record_streaming_usage(&state, &provider, &model,
                                connection_id.as_deref(), api_key.as_deref(), endpoint, &last_data, compression.clone()).await;
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
                if let Some(transformer) = transformer.as_mut() {
                    for line in flush_dashboard_sse_chunk(transformer.as_mut(), &mut pending_text) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                // End-of-stream flush: emit the terminal chunk + [DONE] for
                // buffered binary transforms (kiro EventStream → SSE).
                if let Some(ref mut t_state) = t_state {
                    for line in registry::global_registry().finish_stream(
                        stream_source_format,
                        stream_target_format,
                        t_state,
                    ) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                record_streaming_usage(&state, &provider, &model,
                    connection_id.as_deref(), api_key.as_deref(), endpoint, &last_data, compression.clone()).await;
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
    last_data: &Option<Bytes>,
    compression: Option<CompressionStats>,
) {
    let usage = last_data
        .as_ref()
        .and_then(|b| extract_token_usage_from_bytes(b));
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
            None, // latency_ms
            None, // ttft_ms
            None, // status
            None, // error_class
        )
        .await;
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
        if let Some(frame) = crate::core::executor::qoder::QoderExecutor::wrap_qoder_sse_line(&line)
        {
            out.push(frame);
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

/// Strip the SSE `data:` prefix from a chunk, returning the JSON payload.
/// SSE data lines look like `data: {...}` or `data: {...}\n\nbuffer`.
/// If the body is valid JSON already (non-streaming path), return as-is.
fn strip_sse_data_prefix(body: &[u8]) -> &[u8] {
    let trimmed = body.split(|&b| b == b'\n').next().unwrap_or(body);
    if trimmed.starts_with(b"data:") {
        let after = &trimmed[b"data:".len()..];
        let after = after
            .strip_prefix(b" ")
            .or_else(|| after.strip_prefix(b"\t"))
            .unwrap_or(after);
        if serde_json::from_slice::<serde_json::Value>(after).is_ok() {
            return after;
        }
    }
    // Fall back: try parsing the whole body as JSON (non-streaming / already-stripped).
    if serde_json::from_slice::<serde_json::Value>(body).is_ok() {
        return body;
    }
    body
}

fn extract_token_usage_from_bytes(body: &[u8]) -> Option<TokenUsage> {
    let body = strip_sse_data_prefix(body);
    let value = serde_json::from_slice::<Value>(body).ok()?;

    let usage_obj = value
        .get("usage")
        .and_then(Value::as_object)
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
    (message, retry_after)
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

    // Prefer a status that matches the error text when upstream lied about the code
    // (e.g. free-console proxies returning 401 for "model not supported").
    let status_code =
        crate::core::utils::error::infer_status_from_message(error.status, &error.message);
    let status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY);
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
    let status_code =
        crate::core::utils::error::infer_status_from_message(status.as_u16(), message);
    let status = StatusCode::from_u16(status_code).unwrap_or(status);
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

    /// 9router chatCore.js:229 — the x-9router-token-saver header opts out of
    /// savers when its value is the literal "off" (case-insensitive); absent
    /// header or any other value keeps savers ON.
    fn token_saver_gate(headers: &std::collections::HashMap<String, String>) -> bool {
        headers
            .get("x-9router-token-saver")
            .map(|v| !v.eq_ignore_ascii_case("off"))
            .unwrap_or(true)
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
}
