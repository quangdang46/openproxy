use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::core::utils::session_manager::resolve_session_identity;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const OPENCODE_BASE: &str = "https://opencode.ai";
const OPENCODE_PICKLE_PATH: &str = "/zen/v1/messages";
const OPENCODE_DEFAULT_PATH: &str = "/zen/v1/chat/completions";
const OPENCODE_RESPONSES_PATH: &str = "/zen/v1/responses";

/// Check if a model should be routed through the Responses API instead of chat.
/// Muse Spark models use `/zen/v1/responses`.
/// Mirrors `isResponsesModel` in `open-sse/executors/opencode.js:29-32`.
fn is_responses_model(model: &str) -> bool {
    let base = model.split([':', '@']).next().unwrap_or(model);
    base.contains("muse") && base.contains("spark")
}

/// Valid thinking levels per thinking format.
/// Mirrors `FORMAT_LEVELS` in `open-sse/providers/thinkingLevels.js`
/// (only the formats relevant to the opencode executor path are listed;
/// unknown formats fall back to the openai set, matching Muse Spark's
/// `thinkingFormat: "openai"` capabilities entry).
fn thinking_levels_for_format(format: Option<&str>) -> Option<&'static [&'static str]> {
    match format {
        Some("openai") | None => Some(&["none", "minimal", "low", "medium", "high", "xhigh"]),
        Some("claude-adaptive") | Some("kimi") => Some(&["none", "low", "medium", "high", "max"]),
        Some("claude-budget") => Some(&["none", "low", "medium", "high", "xhigh", "max"]),
        Some("gemini-level") => Some(&["minimal", "low", "medium", "high"]),
        Some("gemini-budget") | Some("qwen") | Some("hunyuan") | Some("step") => {
            Some(&["none", "low", "medium", "high"])
        }
        Some("zai") | Some("minimax") => Some(&["none", "thinking"]),
        Some("deepseek") => Some(&["none", "high", "max"]),
        _ => None,
    }
}

/// Normalize Chat thinking fields into the Responses `reasoning` object.
///
/// Mirrors `normalizeOpencodeReasoning` in `open-sse/executors/opencode.js`
/// (JS ab044e6d): `reasoning_effort` (or an existing `reasoning.effort`)
/// becomes `reasoning: { effort, summary: "auto" }`, and `max`/`ultra`
/// clamp down to the highest level the model accepts (`xhigh` for Muse
/// Spark, whose openai level set has no `max`). No string effort → no-op.
fn normalize_opencode_reasoning(model: &str, body: &mut Value) {
    let Some(body_obj) = body.as_object_mut() else {
        return;
    };
    let current_reasoning = body_obj.get("reasoning").filter(|v| v.is_object()).cloned();
    let requested_effort = body_obj
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            current_reasoning
                .as_ref()
                .and_then(|r| r.get("effort"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(requested) = requested_effort else {
        return;
    };
    // Strip a trailing thinking suffix so capability lookup hits the base id
    // (JS `baseModelId`).
    let (clean_model, _) = crate::core::utils::thinking_suffix::strip_thinking_suffix(model);
    let clean_model = if clean_model.is_empty() {
        model
    } else {
        clean_model
    };
    let format =
        crate::core::combo::capabilities::get_capabilities_for_model("opencode", clean_model)
            .thinking_format;
    let supported_levels = thinking_levels_for_format(format);
    let mut effort = requested.to_lowercase();
    effort = effort.trim().to_string();
    if (effort == "max" || effort == "ultra")
        && supported_levels
            .is_some_and(|levels| !levels.is_empty() && !levels.contains(&effort.as_str()))
    {
        let levels = supported_levels.unwrap_or(&[]);
        if effort == "ultra" && levels.contains(&"max") {
            effort = "max".to_string();
        } else if levels.contains(&"xhigh") {
            effort = "xhigh".to_string();
        }
    }
    let mut reasoning = current_reasoning
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    reasoning.insert("effort".to_string(), Value::String(effort));
    reasoning
        .entry("summary".to_string())
        .or_insert(Value::String("auto".to_string()));
    body_obj.insert("reasoning".to_string(), Value::Object(reasoning));
    body_obj.remove("reasoning_effort");
}

#[derive(Clone)]
pub struct OpenCodeExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum OpenCodeExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for OpenCodeExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for OpenCodeExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for OpenCodeExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for OpenCodeExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for OpenCodeExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct OpenCodeExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
    /// Downstream request headers for passthrough (9router rawHeaders).
    pub raw_headers: std::collections::BTreeMap<String, String>,
}

pub struct OpenCodeExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl OpenCodeExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, OpenCodeExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(&self, model: &str) -> String {
        let path = if is_responses_model(model) {
            OPENCODE_RESPONSES_PATH
        } else if model == "big-pickle" {
            OPENCODE_PICKLE_PATH
        } else {
            OPENCODE_DEFAULT_PATH
        };
        format!("{}{}", OPENCODE_BASE, path)
    }

    /// Build headers for the OpenCode request.
    ///
    /// Session management (9router v0.5.55): resolve a stable per-conversation
    /// session ID and pass through downstream OpenCode-specific headers when
    /// present. Forward the downstream User-Agent if it contains "opencode".
    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
        body: &Value,
        raw_headers: &std::collections::BTreeMap<String, String>,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer public"));

        // Session ID resolution (9router resolveOpencodeSession).
        let session_id = resolve_session_identity(
            Some(&std::collections::HashMap::from_iter(
                raw_headers.iter().map(|(k, v)| (k.clone(), v.clone())),
            )),
            Some(body),
            Some(&credentials.id),
            "opencode",
        )
        .session_id;

        // Pass through downstream OpenCode-specific headers when present,
        // falling back to generated/default values.
        let downstream_ua = raw_headers
            .get("user-agent")
            .or_else(|| raw_headers.get("User-Agent"))
            .map(String::as_str)
            .unwrap_or("");
        let is_opencode_downstream = downstream_ua.to_lowercase().contains("opencode");

        let client = raw_headers
            .get("x-opencode-client")
            .map(String::as_str)
            .unwrap_or("desktop");
        let session = raw_headers
            .get("x-opencode-session")
            .map(String::as_str)
            .unwrap_or(&session_id);
        let request_id = raw_headers
            .get("x-opencode-request")
            .map(String::as_str)
            .unwrap_or("global");

        // User-Agent: forward downstream if it's an OpenCode client, else use default.
        let ua = if is_opencode_downstream {
            downstream_ua
        } else {
            "opencode"
        };
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(ua).unwrap_or_else(|_| HeaderValue::from_static("opencode")),
        );
        headers.insert(
            "x-opencode-client",
            HeaderValue::from_str(client).unwrap_or_else(|_| HeaderValue::from_static("desktop")),
        );
        headers.insert(
            "x-opencode-session",
            HeaderValue::from_str(session)
                .unwrap_or_else(|_| HeaderValue::from_static("ses_unknown")),
        );
        headers.insert(
            "x-opencode-request",
            HeaderValue::from_str(request_id)
                .unwrap_or_else(|_| HeaderValue::from_static("global")),
        );
        headers.insert(
            "x-opencode-project",
            HeaderValue::from_str(
                raw_headers
                    .get("x-opencode-project")
                    .map(String::as_str)
                    .unwrap_or("global"),
            )
            .unwrap_or_else(|_| HeaderValue::from_static("global")),
        );

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        headers
    }

    pub async fn execute_request(
        &self,
        mut request: OpenCodeExecutionRequest,
    ) -> Result<OpenCodeExecutorResponse, OpenCodeExecutorError> {
        // Normalize developer→system role (many providers reject role:developer)
        normalize_developer_role(&mut request.body);

        // Responses API models need max_tokens → max_output_tokens normalization
        // plus reasoning_effort → reasoning{effort,summary} normalization.
        // Mirrors opencode.js:76-86 (transformRequest for isResponsesModel).
        let is_responses = is_responses_model(&request.model);
        if is_responses {
            if let Some(body_obj) = request.body.as_object_mut() {
                // Read the value first to avoid borrow conflicts
                let max_val = body_obj
                    .remove("max_tokens")
                    .or_else(|| body_obj.remove("max_completion_tokens"));
                if let Some(val) = max_val {
                    body_obj.insert("max_output_tokens".to_string(), val);
                }
            }
            normalize_opencode_reasoning(&request.model, &mut request.body);
        }

        let url = self.build_url(&request.model);
        let headers = self.build_headers(
            &request.credentials,
            request.stream,
            &request.body,
            &request.raw_headers,
        );

        let client = self.pool.get("opencode", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&request.body)
            .send()
            .await?;

        Ok(OpenCodeExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body: request.body,
            transport: TransportKind::Reqwest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_effort_becomes_reasoning_object_with_summary_auto() {
        let mut body = json!({"model": "muse-spark-1.2", "reasoning_effort": "high"});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
        assert_eq!(body["reasoning"]["effort"], json!("high"));
        assert_eq!(body["reasoning"]["summary"], json!("auto"));
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn max_and_ultra_clamp_to_xhigh_for_muse_spark() {
        // Muse Spark's openai level set has no max/ultra — clamp to xhigh.
        for level in ["max", "ultra", "MAX", " Ultra "] {
            let mut body = json!({"reasoning_effort": level});
            normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
            assert_eq!(body["reasoning"]["effort"], json!("xhigh"), "level={level}");
        }
    }

    #[test]
    fn existing_reasoning_effort_preserved_and_summary_kept() {
        // Effort falls back to reasoning.effort when reasoning_effort absent.
        let mut body = json!({"reasoning": {"effort": "medium", "summary": "detailed"}});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
        assert_eq!(body["reasoning"]["effort"], json!("medium"));
        assert_eq!(body["reasoning"]["summary"], json!("detailed"));
    }

    #[test]
    fn no_effort_string_is_noop() {
        let mut body = json!({"model": "muse-spark-1.2"});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
        assert!(body.get("reasoning").is_none());
        // Non-object reasoning + no effort string → no-op.
        let mut body = json!({"reasoning": "high"});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free", &mut body);
        assert_eq!(body["reasoning"], json!("high"));
    }

    #[test]
    fn thinking_suffix_stripped_for_capability_lookup() {
        let mut body = json!({"reasoning_effort": "ultra"});
        normalize_opencode_reasoning("muse-spark-1.2-contributor-free(xhigh)", &mut body);
        assert_eq!(body["reasoning"]["effort"], json!("xhigh"));
    }
}
