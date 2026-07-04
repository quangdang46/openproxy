//! Antigravity executor.
//!
//! Port of `open-sse/executors/antigravity.js`. Forwards Gemini-shaped
//! requests to Google's Antigravity Cloud Code endpoint
//! (`/v1internal:streamGenerateContent` or `/v1internal:generateContent`).
//!
//! Notes:
//! - Antigravity uses Gemini's request shape (`request.contents/tools/...`),
//!   not OpenAI's. This executor expects the body to already be in that
//!   shape (the request translator pipeline does the conversion).
//! - A per-connection session id is derived via [`derive_session_id`] so
//!   prompt caching survives within a single OpenProxy run.
//! - Tool function names are sanitised to Gemini's regex
//!   `[a-zA-Z_][a-zA-Z0-9_.:\-]{0,63}`.
//! - The `cleanJSONSchemaForAntigravity` schema-cleaning step from 9router
//!   is **NOT** ported here yet — it lives in `geminiHelper.js` and
//!   should be added once the gemini translator helper is ported. For
//!   now we forward tool parameters verbatim.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use hyper::http;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Map, Value};

use crate::core::config::app_constants::{
    ag_chat_user_agent, cloud_code_api, load_code_assist_metadata, AG_DEFAULT_TOOLS,
    INTERNAL_REQUEST_HEADER_NAME, INTERNAL_REQUEST_HEADER_VALUE,
};
use crate::core::proxy::ProxyTarget;
use crate::core::utils::project_id_cache;
use crate::core::utils::session_manager::derive_session_id;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

/// Default base URL for Antigravity's Cloud Code endpoint.
pub const ANTIGRAVITY_BASE_URL: &str = "https://cloudcode-pa.googleapis.com";

/// Antigravity caps maxOutputTokens at 16k regardless of what the caller
/// asks for; matches the upstream JS implementation.
const MAX_ANTIGRAVITY_OUTPUT_TOKENS: u64 = 16_384;

/// Suffix appended to every client tool name when forwarding to Antigravity.
/// Mirrors 9router's `cloakTools()` behaviour.
const ANTIGRAVITY_TOOL_SUFFIX: &str = "_ide";

#[derive(Clone)]
pub struct AntigravityExecutor {
    pool: Arc<ClientPool>,
    #[allow(dead_code)]
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum AntigravityExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    MissingCredentials(String),
    RetryExhausted(String),
}

impl From<reqwest::Error> for AntigravityExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for AntigravityExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for AntigravityExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for AntigravityExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for AntigravityExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct AntigravityExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct AntigravityExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl AntigravityExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, AntigravityExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// Build the Antigravity URL.
    pub fn build_url(stream: bool) -> String {
        let action = if stream {
            "streamGenerateContent?alt=sse"
        } else {
            "generateContent"
        };
        format!("{ANTIGRAVITY_BASE_URL}/v1internal:{action}")
    }

    fn build_headers(
        access_token: &str,
        stream: bool,
        session_id: Option<&str>,
    ) -> Result<HeaderMap, AntigravityExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let auth = format!("Bearer {access_token}");
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&auth)?);

        let ua = ag_chat_user_agent();
        headers.insert("User-Agent", HeaderValue::from_str(&ua)?);
        headers.insert(
            INTERNAL_REQUEST_HEADER_NAME,
            HeaderValue::from_static(INTERNAL_REQUEST_HEADER_VALUE),
        );

        if let Some(sid) = session_id {
            if !sid.is_empty() {
                headers.insert("X-Machine-Session-Id", HeaderValue::from_str(sid)?);
            }
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        Ok(headers)
    }

    /// Build the Client-Metadata + api-client headers that Antigravity's
    /// Cloud Code endpoint expects. These are sent alongside the standard
    /// auth headers produced by [`build_headers`].
    ///
    /// - `User-Agent: google-api-nodejs-client/9.15.1`
    /// - `X-Goog-Api-Client: google-cloud-sdk vscode_cloudshelleditor/0.1`
    /// - `Client-Metadata: {"ideType":9,"platform":<enum>,"pluginType":2}`
    pub fn build_antigravity_headers() -> Result<HeaderMap, AntigravityExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "User-Agent",
            HeaderValue::from_static("google-api-nodejs-client/9.15.1"),
        );
        headers.insert(
            "X-Goog-Api-Client",
            HeaderValue::from_static("google-cloud-sdk vscode_cloudshelleditor/0.1"),
        );
        let metadata = json!({
            "ideType": 9,
            "platform": crate::core::config::app_constants::current_platform() as u8,
            "pluginType": 2,
        });
        headers.insert(
            "Client-Metadata",
            HeaderValue::from_str(&serde_json::to_string(&metadata)?)?,
        );
        Ok(headers)
    }

    /// Resolve the project ID for the given Antigravity connection.
    ///
    /// 1. Check the [`ProjectIdCache`] first (5-minute TTL).
    /// 2. On cache miss: POST `loadCodeAssist`, extract the project id
    ///    from the response, cache it, and return.
    /// 3. Returns an empty string if the lookup fails (caller should
    ///    proceed without a project id in that case).
    pub async fn get_project_id(connection_id: &str, access_token: &str) -> String {
        // Check cache.
        if let Some(pid) = project_id_cache::get_cached_project_id(connection_id) {
            return pid;
        }

        // Cache miss: call loadCodeAssist.
        let client = reqwest::Client::new();
        let metadata = load_code_assist_metadata();
        let metadata_json = serde_json::to_string(&metadata).unwrap_or_default();

        let response = client
            .post(cloud_code_api::LOAD_CODE_ASSIST)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Content-Type", "application/json")
            .header("User-Agent", "google-api-nodejs-client/9.15.1")
            .header(
                "X-Goog-Api-Client",
                "google-cloud-sdk vscode_cloudshelleditor/0.1",
            )
            .header("Client-Metadata", &metadata_json)
            .json(&json!({ "metadata": metadata }))
            .send()
            .await;

        match response {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(payload) = resp.json::<Value>().await {
                    if let Some(pid) =
                        crate::core::utils::project_id_cache::extract_google_project_id(&payload)
                    {
                        project_id_cache::set_cached_project_id(connection_id, pid.clone());
                        return pid;
                    }
                }
            }
            _ => {}
        }

        String::new()
    }

    /// POST `onboardUser` and poll (5 s interval, up to 10 attempts) until
    /// the server responds with `{"done": true}`.
    ///
    /// This is a best-effort fire-and-forget call: network errors silently
    /// return `Ok(())` so the caller is never blocked by an unreachable
    /// onboard endpoint.
    pub async fn on_user_onboard(
        access_token: &str,
        project_id: &str,
    ) -> Result<(), AntigravityExecutorError> {
        let client = reqwest::Client::new();
        let metadata = load_code_assist_metadata();
        let metadata_json = serde_json::to_string(&metadata).unwrap_or_default();
        let onboard_url = cloud_code_api::ONBOARD_USER;

        for attempt in 0..10 {
            let response = client
                .post(onboard_url)
                .header("Authorization", format!("Bearer {access_token}"))
                .header("Content-Type", "application/json")
                .header("User-Agent", "google-api-nodejs-client/9.15.1")
                .header(
                    "X-Goog-Api-Client",
                    "google-cloud-sdk vscode_cloudshelleditor/0.1",
                )
                .header("Client-Metadata", &metadata_json)
                .json(&json!({
                    "tierId": "legacy-tier",
                    "projectId": project_id,
                    "metadata": metadata,
                }))
                .send()
                .await;

            match response {
                Ok(resp) if resp.status().is_success() => {
                    let result = resp.json::<Value>().await.unwrap_or(Value::Null);
                    if result.get("done").and_then(Value::as_bool) == Some(true) {
                        tracing::info!(
                            "antigravity onboardUser succeeded after {} poll(s)",
                            attempt + 1
                        );
                        return Ok(());
                    }
                }
                Err(_) => {
                    // Network error during onboard is non-fatal — the API
                    // will onboard on first request anyway.
                    return Ok(());
                }
                _ => {}
            }

            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }

        tracing::warn!("antigravity onboardUser did not complete after 10 polls");
        Ok(())
    }

    /// Sanitize a tool function name so it matches Gemini's allowed
    /// pattern: `[a-zA-Z_][a-zA-Z0-9_.:\-]{0,63}`. Returns `_unknown`
    /// for empty input.
    fn sanitize_function_name(name: &str) -> String {
        if name.is_empty() {
            return "_unknown".to_string();
        }
        let mut s: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == ':' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if !s
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        {
            s.insert(0, '_');
        }
        s.chars().take(64).collect()
    }

    /// Port of 9router's `cloakTools()`:
    /// 1. Append `ANTIGRAVITY_TOOL_SUFFIX` to every existing function name
    ///    *before* sanitization runs.
    /// 2. Inject every name in [`app_constants::AG_DEFAULT_TOOLS`] as a
    ///    decoy function declaration (with `_ide` suffix applied and the
    ///    "This tool is currently unavailable" description).
    /// 3. Defensive dedup: ensure every name in
    ///    [`app_constants::AG_DEFAULT_TOOLS`] is present (skip if step 2
    ///    already injected it).
    ///
    /// Operates on the raw `tools` array (shape: `[{functionDeclarations: ...}]`)
    /// as it appears in the inbound request body. Caller is expected to run
    /// this **before** the sanitize+merge loop so the `_ide` suffix is treated
    /// as part of the name.
    fn cloak_tools(tools: &mut Value) {
        let Some(groups) = tools.as_array_mut() else {
            return;
        };

        // Make sure at least one group exists so we have somewhere to inject.
        if groups.is_empty() {
            groups.push(json!({"functionDeclarations": []}));
        }

        // Collect every existing function name (lower-cased) so we can dedupe
        // AG_DEFAULT_TOOLS entries without scanning twice.
        let mut existing_names: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();

        // Step 1 + collect existing names.
        for group in groups.iter_mut() {
            let Some(decls) = group
                .get_mut("functionDeclarations")
                .and_then(|v| v.as_array_mut())
            else {
                continue;
            };
            for decl in decls.iter_mut() {
                let Some(obj) = decl.as_object_mut() else {
                    continue;
                };
                let raw_name = obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !raw_name.is_empty() {
                    let new_name = format!("{raw_name}{ANTIGRAVITY_TOOL_SUFFIX}");
                    existing_names.insert(new_name.to_ascii_lowercase());
                    obj.insert("name".into(), Value::String(new_name));
                }
            }
        }

        // Make sure the first group has a `functionDeclarations` array.
        let first_group_has_decls = groups[0]
            .get("functionDeclarations")
            .map(|v| v.is_array())
            .unwrap_or(false);
        if !first_group_has_decls {
            if let Some(obj) = groups[0].as_object_mut() {
                obj.insert("functionDeclarations".into(), Value::Array(Vec::new()));
            }
        }
        let first_decls = groups[0]
            .get_mut("functionDeclarations")
            .and_then(|v| v.as_array_mut())
            .expect("functionDeclarations array just ensured");

        // Step 2 + 3: inject every AG_DEFAULT_TOOLS entry as a decoy
        // declaration (with the `_ide` suffix applied). The `BTreeSet`
        // iteration is deterministic.
        for base_name in AG_DEFAULT_TOOLS.iter() {
            let cloaked = format!("{base_name}{ANTIGRAVITY_TOOL_SUFFIX}");
            if existing_names.contains(&cloaked.to_ascii_lowercase()) {
                continue;
            }
            first_decls.push(json!({
                "name": cloaked,
                "description": "This tool is currently unavailable",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "reason": {"type": "string", "description": "Brief explanation"}
                    },
                    "required": ["reason"]
                }
            }));
            existing_names.insert(cloaked.to_ascii_lowercase());
        }
    }

    /// Perform the inbound-request transformation matching `transformRequest`
    /// in 9router's antigravity.js. Mutates `body` in place and returns the
    /// derived session id so caller can pass it through to headers.
    fn transform_request(
        body: &mut Value,
        credentials: &ProviderConnection,
    ) -> Result<String, AntigravityExecutorError> {
        // Pull out request.contents and rewrite role/parts as needed.
        if let Some(request_obj) = body.get_mut("request").and_then(|v| v.as_object_mut()) {
            // Rewrite contents.
            if let Some(contents) = request_obj
                .get_mut("contents")
                .and_then(|v| v.as_array_mut())
            {
                for content in contents.iter_mut() {
                    let Some(co) = content.as_object_mut() else {
                        continue;
                    };
                    let parts_owned = co.get("parts").cloned();
                    let Some(parts_array) = parts_owned.as_ref().and_then(|p| p.as_array()) else {
                        continue;
                    };

                    let has_function_response = parts_array.iter().any(|p| {
                        p.as_object()
                            .map(|o| o.contains_key("functionResponse"))
                            .unwrap_or(false)
                    });
                    if has_function_response {
                        co.insert("role".into(), Value::String("user".to_string()));
                    }

                    // Strip thought-only parts.
                    let cleaned_parts: Vec<Value> = parts_array
                        .iter()
                        .filter(|p| {
                            let Some(o) = p.as_object() else {
                                return true;
                            };
                            let has_thought =
                                o.get("thought").map(|v| !v.is_null()).unwrap_or(false);
                            let has_function_call = o.contains_key("functionCall");
                            let has_thought_signature = o.contains_key("thoughtSignature");
                            let has_text = o.contains_key("text");
                            // Drop pure thought parts but keep thoughtSignature
                            // when paired with functionCall (Gemini 3+ requires it).
                            if has_thought && !has_function_call {
                                return false;
                            }
                            if has_thought_signature && !has_function_call && !has_text {
                                return false;
                            }
                            true
                        })
                        .cloned()
                        .collect();
                    // Ported from 9router's antigravity.js transformRequest:
                    // Gemini 3+ rejects functionCall parts without thoughtSignature.
                    // Clients (Claude Code, IDE) don't persist thoughtSignature in
                    // their history, so backfill the default signature on any
                    // functionCall part that arrives without one.
                    let needs_backfill = cleaned_parts.iter().any(|p| {
                        p.as_object()
                            .map(|o| {
                                o.contains_key("functionCall")
                                    && !o.contains_key("thoughtSignature")
                            })
                            .unwrap_or(false)
                    });
                    let final_parts: Vec<Value> = if needs_backfill {
                        cleaned_parts
                            .into_iter()
                            .map(|p| {
                                let Some(o) = p.as_object() else {
                                    return p;
                                };
                                if o.contains_key("functionCall") && !o.contains_key("thoughtSignature") {
                                    let mut backfilled = p.clone();
                                    if let Some(obj) = backfilled.as_object_mut() {
                                        obj.insert(
                                            "thoughtSignature".into(),
                                            Value::String(
                                                crate::core::translator::request::openai_to_gemini::DEFAULT_THINKING_AG_SIGNATURE.to_string(),
                                            ),
                                        );
                                    }
                                    backfilled
                                } else {
                                    p
                                }
                            })
                            .collect()
                    } else {
                        cleaned_parts
                    };
                    co.insert("parts".into(), Value::Array(final_parts));
                }
            }

            // Sanitize and merge tool function declarations into a single group.
            // Cloak first so the `_ide` suffix is part of the name when
            // `sanitize_function_name` runs below.
            if let Some(tools) = request_obj.get_mut("tools") {
                Self::cloak_tools(tools);
            }

            // Also rename functionCall/functionResponse names in contents turn
            // history. 9router's `cloakTools` walks the parts array and
            // suffixes every function name with `_ide` so the model sees a
            // consistent namespace between the tool declarations and the
            // history it has to reason about.
            if let Some(contents) = request_obj
                .get_mut("contents")
                .and_then(|v| v.as_array_mut())
            {
                for content in contents.iter_mut() {
                    let Some(co) = content.as_object_mut() else {
                        continue;
                    };
                    let Some(parts) = co.get_mut("parts").and_then(|v| v.as_array_mut()) else {
                        continue;
                    };
                    for part in parts.iter_mut() {
                        let Some(po) = part.as_object_mut() else {
                            continue;
                        };
                        // Rename functionCall.name
                        if let Some(fc) = po.get_mut("functionCall") {
                            if let Some(fc_obj) = fc.as_object_mut() {
                                if let Some(name) = fc_obj
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                {
                                    if !name.ends_with(ANTIGRAVITY_TOOL_SUFFIX) {
                                        fc_obj.insert(
                                            "name".into(),
                                            Value::String(format!(
                                                "{name}{ANTIGRAVITY_TOOL_SUFFIX}"
                                            )),
                                        );
                                    }
                                }
                            }
                        }
                        // Rename functionResponse.name
                        if let Some(fr) = po.get_mut("functionResponse") {
                            if let Some(fr_obj) = fr.as_object_mut() {
                                if let Some(name) = fr_obj
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                {
                                    if !name.ends_with(ANTIGRAVITY_TOOL_SUFFIX) {
                                        fr_obj.insert(
                                            "name".into(),
                                            Value::String(format!(
                                                "{name}{ANTIGRAVITY_TOOL_SUFFIX}"
                                            )),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let merged_tools: Option<Vec<Value>> = if let Some(tools) =
                request_obj.get("tools").and_then(|v| v.as_array()).cloned()
            {
                let mut all_decls: Vec<Value> = Vec::new();
                for group in tools {
                    let Some(decls) = group.get("functionDeclarations").and_then(|v| v.as_array())
                    else {
                        continue;
                    };
                    for decl in decls {
                        let mut new_decl = decl.clone();
                        if let Some(obj) = new_decl.as_object_mut() {
                            let raw_name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            obj.insert(
                                "name".into(),
                                Value::String(Self::sanitize_function_name(raw_name)),
                            );
                            // Clean JSON schema for Antigravity API compatibility.
                            // Ported from 9router's antigravity.js transformRequest:
                            // `fn.parameters ? cleanJSONSchemaForAntigravity(structuredClone(fn.parameters)) : ...`
                            if let Some(params) = obj.get_mut("parameters") {
                                let cleaned = crate::core::translator::request::openai_to_gemini::clean_json_schema(params);
                                obj.insert("parameters".into(), cleaned);
                            } else {
                                // Provide an empty-but-valid schema if missing.
                                obj.insert(
                                    "parameters".into(),
                                    json!({
                                        "type": "object",
                                        "properties": {
                                            "reason": {"type": "string", "description": "Brief explanation"}
                                        },
                                        "required": ["reason"]
                                    }),
                                );
                            }
                        }
                        all_decls.push(new_decl);
                    }
                }
                if all_decls.is_empty() {
                    Some(Vec::new())
                } else {
                    Some(vec![json!({"functionDeclarations": all_decls})])
                }
            } else {
                None
            };

            if let Some(tools) = merged_tools {
                if tools.is_empty() {
                    request_obj.remove("tools");
                    request_obj.remove("toolConfig");
                } else {
                    request_obj.insert("tools".into(), Value::Array(tools));
                    request_obj.insert(
                        "toolConfig".into(),
                        json!({"functionCallingConfig": {"mode": "VALIDATED"}}),
                    );
                }
            }

            // Cap maxOutputTokens.
            let gen = request_obj
                .entry("generationConfig".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(gen_obj) = gen.as_object_mut() {
                let cap = MAX_ANTIGRAVITY_OUTPUT_TOKENS;
                if let Some(max_out) = gen_obj.get("maxOutputTokens").and_then(|v| v.as_u64()) {
                    if max_out > cap {
                        gen_obj.insert("maxOutputTokens".into(), Value::from(cap));
                    }
                }
            }

            // Drop safetySettings (Antigravity ignores them anyway and
            // some values cause 400s).
            request_obj.remove("safetySettings");

            // Resolve session id.
            let connection_id = credentials
                .email
                .as_deref()
                .or_else(|| credentials.id.as_str().into())
                .unwrap_or("");
            let session_id = match request_obj.get("sessionId").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => derive_session_id(connection_id),
            };
            request_obj.insert("sessionId".into(), Value::String(session_id.clone()));
            return Ok(session_id);
        }

        // No `request` envelope → fall back to a fresh session id.
        let connection_id = credentials
            .email
            .as_deref()
            .or_else(|| credentials.id.as_str().into())
            .unwrap_or("");
        Ok(derive_session_id(connection_id))
    }

    /// Parse the Retry-After header value into a Duration.
    /// Handles both HTTP-date and integer-seconds formats.
    fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
        let value = headers.get("Retry-After")?;
        let value_str = value.to_str().ok()?.trim();

        // Try integer seconds first.
        if let Ok(seconds) = value_str.parse::<u64>() {
            let capped = seconds.min(120); // cap at 2 minutes
            return Some(Duration::from_secs(capped));
        }

        // Try HTTP-date format (not commonly used, but handle it).
        if let Ok(expires) = chrono::DateTime::parse_from_rfc2822(value_str) {
            let now = chrono::Utc::now();
            let duration = expires.signed_duration_since(now);
            let secs = duration.num_seconds().max(1).min(120) as u64;
            return Some(Duration::from_secs(secs));
        }

        None
    }

    /// Check whether a model ID refers to an image-generation model (Imagen).
    fn is_image_model(model: &str) -> bool {
        let base = model.rsplit('/').next().unwrap_or(model);
        base.starts_with("imagen")
    }

    /// Parse a dash-separated aspect-ratio/resolution suffix from the end of a
    /// model name.  The suffix must match `DIGITSxDIGITS` (one `x` separator).
    ///
    /// Returns `(&model[..last_hyphen], Some(suffix))` on a match, or
    /// `(model, None)` when no suffix is found.
    ///
    /// Examples:
    ///   `imagen-3.0-generate-002-16x9`     -> (`imagen-3.0-generate-002`, Some("16x9"))
    ///   `imagen-3.0-generate-002-1024x768` -> (`imagen-3.0-generate-002`, Some("1024x768"))
    ///   `imagen-3.0-generate-002`           -> (`imagen-3.0-generate-002`, None)
    fn parse_image_model_suffix(model: &str) -> (&str, Option<&str>) {
        if let Some(pos) = model.rfind('-') {
            let suffix = &model[pos + 1..];
            let parts: Vec<&str> = suffix.splitn(2, 'x').collect();
            if parts.len() == 2
                && !parts[0].is_empty()
                && !parts[1].is_empty()
                && parts[0].chars().all(|c| c.is_ascii_digit())
                && parts[1].chars().all(|c| c.is_ascii_digit())
                && suffix.chars().filter(|&c| c == 'x').count() == 1
            {
                return (&model[..pos], Some(suffix));
            }
        }
        (model, None)
    }

    /// Check if an error response body suggests a transient error that should
    /// be automatically retried.  Looks for common rate-limit and quota-exhausted
    /// signals in the response text.
    fn is_transient_antigravity_error(body_text: &str) -> bool {
        let lower = body_text.to_ascii_lowercase();
        lower.contains("rate_limit")
            || lower.contains("quota_exceeded")
            || lower.contains("resource_exhausted")
            || body_text.contains("429")
    }

    pub async fn execute_request(
        &self,
        mut request: AntigravityExecutionRequest,
    ) -> Result<AntigravityExecutorResponse, AntigravityExecutorError> {
        let access_token = request
            .credentials
            .access_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AntigravityExecutorError::MissingCredentials("access_token required".to_string())
            })?
            .to_string();

        // The translator pipeline (OpenAi -> openai_to_gemini_cli_request) produces a flat
        // body {contents, tools, ...}.  Antigravity's Cloud Code endpoint requires the
        // Gemini-like body wrapped in a {"request": body} envelope.  If the body doesn't
        // already have a "request" key, wrap it here.
        if request.body.get("request").is_none() {
            let inner = std::mem::replace(&mut request.body, Value::Null);
            request.body = json!({"request": inner});
        }

        // Resolve the project ID (cached or via loadCodeAssist) and inject
        // it into the request body so the upstream sees a valid GCP project.
        let connection_id = request
            .credentials
            .email
            .as_deref()
            .or_else(|| request.credentials.id.as_str().into())
            .unwrap_or("");
        let project_id = Self::get_project_id(connection_id, &access_token).await;
        if !project_id.is_empty() {
            if let Some(req) = request
                .body
                .get_mut("request")
                .and_then(|v| v.as_object_mut())
            {
                req.insert("projectId".into(), Value::String(project_id.clone()));
            }
        }

        // Spawn best-effort onboardUser notification in the background so
        // it does not block the critical path.
        if !project_id.is_empty() {
            let at = access_token.clone();
            let pid = project_id.clone();
            tokio::spawn(async move {
                let _ = Self::on_user_onboard(&at, &pid).await;
            });
        }

        let session_id = Self::transform_request(&mut request.body, &request.credentials)?;

        // --- Image model support ---
        // Detect image-generation models (Imagen) and, for those, use a
        // separate non-streaming generateContent path with additional fields
        // that the Antigravity endpoint requires (candidateCount, safetySettings).
        let is_image = Self::is_image_model(&request.model);
        let (clean_model, _suffix) = if is_image {
            Self::parse_image_model_suffix(&request.model)
        } else {
            (request.model.as_str(), None)
        };

        let url = if is_image {
            // Image models always use the non-streaming path.
            Self::build_url(false)
        } else {
            Self::build_url(request.stream)
        };

        // For image models, inject the model name (after stripping suffix)
        // and add candidateCount + safetySettings so the upstream returns
        // images rather than text-only candidates.
        if is_image {
            if let Some(req) = request
                .body
                .get_mut("request")
                .and_then(|v| v.as_object_mut())
            {
                req.insert("model".into(), Value::String(clean_model.to_string()));
                req.insert("candidateCount".into(), json!(1));
                // Minimal safety settings: block nothing.  Antigravity will
                // apply its own server-side filtering regardless.
                if req.get("safetySettings").is_none() {
                    req.insert(
                        "safetySettings".into(),
                        json!([
                            {"category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_ONLY_HIGH"},
                            {"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_ONLY_HIGH"},
                            {"category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "BLOCK_ONLY_HIGH"},
                            {"category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "BLOCK_ONLY_HIGH"},
                        ]),
                    );
                }
            }
        }

        // Retry up to 3 times with exponential backoff, Retry-After header,
        // and error-body-text transient detection.
        const MAX_RETRIES: usize = 3;
        for attempt in 0..MAX_RETRIES {
            let mut headers =
                Self::build_headers(&access_token, request.stream, Some(&session_id))?;

            // Add Antigravity-specific headers (Client-Metadata, etc.).
            let ag_headers = Self::build_antigravity_headers()?;
            for (key, value) in ag_headers.iter() {
                headers.insert(key, value.clone());
            }

            let client = self.pool.get("antigravity", request.proxy.as_ref())?;
            let response = client
                .post(&url)
                .headers(headers.clone())
                .json(&request.body)
                .send()
                .await?;

            let status = response.status();

            // Success -- return immediately with unconsumed response.
            if status.is_success() {
                return Ok(AntigravityExecutorResponse {
                    response: UpstreamResponse::Reqwest(response),
                    url,
                    headers,
                    transformed_body: request.body,
                    transport: TransportKind::Reqwest,
                });
            }

            // Save response headers before body consumption, then check
            // for transient error signals in the body text.
            let response_headers = response.headers().clone();
            let body_bytes = response.bytes().await.unwrap_or_default();
            let body_text = String::from_utf8_lossy(&body_bytes);

            // Determine retryability: status code check + body text check.
            let is_retryable = status.as_u16() == 429
                || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                || status == reqwest::StatusCode::BAD_GATEWAY
                || status == reqwest::StatusCode::GATEWAY_TIMEOUT
                || Self::is_transient_antigravity_error(&body_text);

            if !is_retryable || attempt + 1 >= MAX_RETRIES {
                // Not retryable or no retries left -- reconstruct a
                // reqwest::Response from status, saved headers, and body bytes.
                let mut builder = http::Response::builder().status(status);
                for (key, value) in response_headers.iter() {
                    builder = builder.header(key.clone(), value.clone());
                }
                let reconstructed_http = builder
                    .body(body_bytes)
                    .map_err(|e| {
                        AntigravityExecutorError::RequestFailed(format!(
                            "response reconstruction: {e}"
                        ))
                    })?;
                let reconstructed = reqwest::Response::from(reconstructed_http);
                return Ok(AntigravityExecutorResponse {
                    response: UpstreamResponse::Reqwest(reconstructed),
                    url,
                    headers,
                    transformed_body: request.body,
                    transport: TransportKind::Reqwest,
                });
            }

            // Determine wait duration: Retry-After header wins, otherwise
            // exponential backoff: 500ms * 2^attempt (jittered).
            let delay = Self::parse_retry_after(&response_headers).unwrap_or_else(|| {
                let base_ms = 500u64 * (1u64 << attempt);
                let jitter = rand::random::<u64>() % (base_ms / 2 + 1);
                Duration::from_millis(base_ms + jitter)
            });

            tracing::info!(
                "antigravity request got HTTP {}, retrying in {:?} (attempt {}/{})",
                status.as_u16(),
                delay,
                attempt + 1,
                MAX_RETRIES,
            );

            tokio::time::sleep(delay).await;
        }

        Err(AntigravityExecutorError::RetryExhausted(
            "antigravity request failed after max retries".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_picks_stream_or_unary_path() {
        assert!(AntigravityExecutor::build_url(true).ends_with("streamGenerateContent?alt=sse"));
        assert!(AntigravityExecutor::build_url(false).ends_with("generateContent"));
    }

    #[test]
    fn sanitize_function_name_replaces_invalid_chars() {
        assert_eq!(
            AntigravityExecutor::sanitize_function_name("my:tool/name with space"),
            "my:tool_name_with_space"
        );
    }

    #[test]
    fn sanitize_function_name_prepends_underscore_if_starts_with_digit() {
        assert_eq!(AntigravityExecutor::sanitize_function_name("3foo"), "_3foo");
    }

    #[test]
    fn sanitize_function_name_truncates_to_64() {
        let long = "a".repeat(100);
        assert_eq!(AntigravityExecutor::sanitize_function_name(&long).len(), 64);
    }

    #[test]
    fn sanitize_function_name_handles_empty() {
        assert_eq!(AntigravityExecutor::sanitize_function_name(""), "_unknown");
    }

    #[test]
    fn transform_request_caps_max_output_tokens() {
        let mut body = json!({
            "request": {
                "contents": [],
                "generationConfig": {"maxOutputTokens": 1_000_000}
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert_eq!(
            body["request"]["generationConfig"]["maxOutputTokens"],
            16_384
        );
    }

    #[test]
    fn transform_request_strips_thought_only_parts() {
        let mut body = json!({
            "request": {
                "contents": [{
                    "role": "model",
                    "parts": [
                        {"thought": true, "text": ""},
                        {"text": "real content"}
                    ]
                }]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        let parts = body["request"]["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "real content");
    }

    #[test]
    fn transform_request_keeps_thought_signature_when_paired_with_function_call() {
        let mut body = json!({
            "request": {
                "contents": [{
                    "role": "model",
                    "parts": [
                        {"thoughtSignature": "abc", "functionCall": {"name": "x", "args": {}}}
                    ]
                }]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert_eq!(
            body["request"]["contents"][0]["parts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn transform_request_rewrites_role_for_function_response() {
        let mut body = json!({
            "request": {
                "contents": [{
                    "role": "tool",
                    "parts": [
                        {"functionResponse": {"name": "x", "response": {"result": "ok"}}}
                    ]
                }]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert_eq!(body["request"]["contents"][0]["role"], "user");
    }

    #[test]
    fn transform_request_merges_tools_into_single_group() {
        let mut body = json!({
            "request": {
                "contents": [],
                "tools": [
                    {"functionDeclarations": [{"name": "a", "parameters": {"type": "object"}}]},
                    {"functionDeclarations": [{"name": "b!?", "parameters": {"type": "object"}}]}
                ]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        let groups = body["request"]["tools"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        let decls = groups[0]["functionDeclarations"].as_array().unwrap();
        // 2 original tools + AG_DEFAULT_TOOLS decoys (every entry, with
        // `_ide` suffix appended). The test does not assume a magic number
        // -- it computes the expected count from the same source of truth
        // the executor uses.
        let expected_count = 2 + crate::core::config::app_constants::AG_DEFAULT_TOOLS.len();
        assert_eq!(decls.len(), expected_count);

        let names: Vec<&str> = decls
            .as_slice()
            .iter()
            .filter_map(|d| d.get("name").and_then(|v| v.as_str()))
            .collect();
        assert!(
            names.contains(&"a_ide"),
            "expected a_ide to be in merged decls"
        );
        assert!(
            names.contains(&"b___ide"),
            "expected b___ide to be in merged decls"
        );
        for base in crate::core::config::app_constants::AG_DEFAULT_TOOLS.iter() {
            let required = format!("{base}_ide");
            assert!(
                names.contains(&required.as_str()),
                "missing default tool: {required}"
            );
        }
        // toolConfig set when tools are present.
        assert_eq!(
            body["request"]["toolConfig"]["functionCallingConfig"]["mode"],
            "VALIDATED"
        );
    }

    #[test]
    fn transform_request_drops_safety_settings() {
        let mut body = json!({
            "request": {
                "contents": [],
                "safetySettings": [{"category": "x", "threshold": "BLOCK_NONE"}]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert!(body["request"].get("safetySettings").is_none());
    }

    #[test]
    fn flat_body_gets_wrapped_in_request_envelope() {
        let mut body = json!({
            "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
            "tools": [{"functionDeclarations": [{"name": "my_tool", "parameters": {"type": "object"}}]}]
        });

        if body.get("request").is_none() {
            let inner = std::mem::replace(&mut body, Value::Null);
            body = json!({"request": inner});
        }

        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();

        assert!(
            body.get("request").is_some(),
            "body should have request envelope"
        );

        let tools = body["request"]["tools"].as_array().expect("tools array");
        let decls = tools[0]["functionDeclarations"]
            .as_array()
            .expect("functionDeclarations");
        let names: Vec<&str> = decls
            .iter()
            .filter_map(|d| d.get("name").and_then(|v| v.as_str()))
            .collect();

        assert!(
            names.contains(&"my_tool_ide"),
            "client tool should have _ide suffix"
        );

        for base in crate::core::config::app_constants::AG_DEFAULT_TOOLS.iter() {
            let required = format!("{base}_ide");
            assert!(
                names.contains(&required.as_str()),
                "missing default tool: {required}"
            );
        }

        assert_eq!(
            body["request"]["contents"][0]["role"], "user",
            "contents should be preserved after wrapping"
        );
    }

    #[test]
    fn build_antigravity_headers_sets_expected_statics() {
        let headers = AntigravityExecutor::build_antigravity_headers().unwrap();
        assert_eq!(
            headers.get("User-Agent").and_then(|v| v.to_str().ok()),
            Some("google-api-nodejs-client/9.15.1")
        );
        assert_eq!(
            headers
                .get("X-Goog-Api-Client")
                .and_then(|v| v.to_str().ok()),
            Some("google-cloud-sdk vscode_cloudshelleditor/0.1")
        );
        // Client-Metadata should be valid JSON.
        let cm = headers
            .get("Client-Metadata")
            .and_then(|v| v.to_str().ok())
            .expect("Client-Metadata header present");
        let parsed: serde_json::Value =
            serde_json::from_str(cm).expect("Client-Metadata is valid JSON");
        assert_eq!(parsed["ideType"], 9);
        assert!(parsed["platform"].is_number());
        assert_eq!(parsed["pluginType"], 2);
    }

    #[test]
    fn build_antigravity_headers_platform_detection() {
        let headers = AntigravityExecutor::build_antigravity_headers().unwrap();
        let cm = headers
            .get("Client-Metadata")
            .and_then(|v| v.to_str().ok())
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(cm).unwrap();
        let platform = parsed["platform"].as_u64().unwrap();
        // Platform should be a valid AgPlatform value (1-5 or 0 for unknown).
        assert!(
            platform <= 5,
            "platform value {platform} is in expected range"
        );
    }

    // ---- image model helper tests ----

    #[test]
    fn is_image_model_detects_imagen_prefix() {
        assert!(AntigravityExecutor::is_image_model("google/imagen-3.0-generate-002"));
        assert!(AntigravityExecutor::is_image_model("imagen-3.0-generate-002"));
        assert!(AntigravityExecutor::is_image_model("imagen"));
        assert!(!AntigravityExecutor::is_image_model("gemini-2.5-flash"));
        assert!(!AntigravityExecutor::is_image_model("gemini-2.5-pro"));
        assert!(!AntigravityExecutor::is_image_model(""));
    }

    #[test]
    fn parse_image_model_suffix_handles_aspect_ratio() {
        let (base, suffix) = AntigravityExecutor::parse_image_model_suffix("imagen-3.0-generate-002-16x9");
        assert_eq!(base, "imagen-3.0-generate-002");
        assert_eq!(suffix, Some("16x9"));
    }

    #[test]
    fn parse_image_model_suffix_handles_resolution() {
        let (base, suffix) = AntigravityExecutor::parse_image_model_suffix("imagen-3.0-generate-002-1024x768");
        assert_eq!(base, "imagen-3.0-generate-002");
        assert_eq!(suffix, Some("1024x768"));
    }

    #[test]
    fn parse_image_model_suffix_returns_none_when_no_suffix() {
        let (base, suffix) = AntigravityExecutor::parse_image_model_suffix("imagen-3.0-generate-002");
        assert_eq!(base, "imagen-3.0-generate-002");
        assert_eq!(suffix, None);
    }

    #[test]
    fn parse_image_model_suffix_returns_none_for_non_model_suffix() {
        let (base, suffix) = AntigravityExecutor::parse_image_model_suffix("gemini-2.5-flash");
        assert_eq!(base, "gemini-2.5-flash");
        assert_eq!(suffix, None);
    }

    #[test]
    fn parse_image_model_suffix_multiple_x_not_matched() {
        // Multiple 'x' chars should not be parsed as a valid WxH suffix.
        let (base, suffix) = AntigravityExecutor::parse_image_model_suffix("model-16x9x2");
        assert_eq!(base, "model-16x9x2");
        assert_eq!(suffix, None);
    }

    // ---- transient error body-text detection tests ----

    #[test]
    fn is_transient_antigravity_error_detects_rate_limit() {
        assert!(AntigravityExecutor::is_transient_antigravity_error(
            "{\"error\": {\"message\": \"rate_limit exceeded\"}}"
        ));
    }

    #[test]
    fn is_transient_antigravity_error_detects_quota_exceeded() {
        assert!(AntigravityExecutor::is_transient_antigravity_error(
            "quota_exceeded: daily limit reached"
        ));
    }

    #[test]
    fn is_transient_antigravity_error_detects_resource_exhausted() {
        assert!(AntigravityExecutor::is_transient_antigravity_error(
            "RESOURCE_EXHAUSTED: quota exhausted"
        ));
    }

    #[test]
    fn is_transient_antigravity_error_detects_429_string() {
        assert!(
            AntigravityExecutor::is_transient_antigravity_error(
                "Error code: 429 Too Many Requests"
            )
        );
    }

    #[test]
    fn is_transient_antigravity_error_returns_false_for_other_errors() {
        assert!(!AntigravityExecutor::is_transient_antigravity_error(
            "{\"error\": {\"message\": \"invalid argument\"}}"
        ));
        assert!(!AntigravityExecutor::is_transient_antigravity_error(
            "{\"error\": {\"message\": \"permission denied\"}}"
        ));
        assert!(!AntigravityExecutor::is_transient_antigravity_error(""));
    }

    #[test]
    fn is_transient_antigravity_error_case_insensitive() {
        assert!(AntigravityExecutor::is_transient_antigravity_error(
            "RATE_LIMIT REACHED"
        ));
        assert!(AntigravityExecutor::is_transient_antigravity_error(
            "Rate_Limit Exceeded"
        ));
    }
}
