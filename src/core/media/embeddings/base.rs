//! Embedding-adapter trait + the three concrete impls.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::types::ProviderConnection;

/// `Number()` semantics meet `JSON.stringify`: an integral f64 must serialize
/// as `256`, not `256.0`, or every strict client sees a type change.
fn json_number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

/// `String(value)` the way JS coerces it — 9router's Gemini adapter runs both
/// the batch elements and the single input through `String(text)`
/// (`embeddingProviders/gemini.js:24, :31`), which is `Array.prototype.toString`
/// for an array, not `JSON.stringify`. So `[1,2]` is `"1,2"` and `[[1,2],[3,4]]`
/// flattens to `"1,2,3,4"`; an object is `"[object Object]"`; an integral double
/// loses the `.0` that `serde_json` would keep.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => match n.as_u64() {
                Some(u) => u.to_string(),
                None => match n.as_f64() {
                    Some(f) if f.is_finite() && f.fract() == 0.0 => format!("{}", f as i64),
                    Some(f) => f.to_string(),
                    None => n.to_string(),
                },
            },
        },
        Value::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// One inbound embeddings request.
#[derive(Debug, Clone)]
pub struct EmbeddingRequest<'a> {
    pub body: &'a Value,
    pub model: &'a str,
    pub credentials: &'a ProviderConnection,
}

impl<'a> EmbeddingRequest<'a> {
    pub fn input(&self) -> Option<&'a Value> {
        self.body.get("input")
    }

    pub fn encoding_format(&self) -> Option<&'a str> {
        // JS embeddingsCore.js:52 `encoding_format: body.encoding_format || "float"`
        // coerces an empty string to "float". Treat empty as None so the caller's
        // `unwrap_or("float")` yields "float" for both absent and empty.
        self.body
            .get("encoding_format")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }

    /// Coerced `dimensions`, reproducing 9router's
    /// `const dim = Number(dimensions); if (Number.isFinite(dim) && dim > 0)`
    /// (`embeddingProviders/openai.js buildBody`).
    ///
    /// JS `Number()` accepts a numeric string and any finite number, so
    /// `"256"` and `256.0` both become `256`. `null`, `""` and anything
    /// non-numeric are dropped — the same set the JS guard rejects.
    pub fn dimensions(&self) -> Option<f64> {
        let raw = self.body.get("dimensions")?;
        if raw.is_null() {
            return None;
        }
        let n = raw
            .as_f64()
            .or_else(|| raw.as_str().and_then(|s| s.trim().parse::<f64>().ok()))?;
        (n.is_finite() && n > 0.0).then_some(n)
    }
}

/// Adapter response — always delivered as `serde_json::Value`. Adapters
/// may pre-normalise to OpenAI's `{ object: "list", data: [...] }` shape.
pub type EmbeddingResponse = Value;

#[async_trait]
pub trait EmbeddingAdapter: Send + Sync {
    fn no_auth(&self) -> bool {
        false
    }
    fn build_url(&self, request: &EmbeddingRequest<'_>) -> Result<String, String>;
    fn build_headers(&self, request: &EmbeddingRequest<'_>) -> Result<HeaderMap, String>;
    fn build_body(&self, request: &EmbeddingRequest<'_>) -> Result<Value, String>;
    fn normalize(&self, body: &Value, model: &str) -> Value {
        let _ = model;
        body.clone()
    }
}

// ─── OpenAI-compatible adapter (10 providers) ────────────────────────────

pub struct OpenAiCompatAdapter {
    pub provider_id: &'static str,
    pub endpoint: &'static str,
    pub include_referer: bool,
}

pub static OPENAI: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "openai",
    endpoint: "https://api.openai.com/v1/embeddings",
    include_referer: false,
};
pub static OPENROUTER: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "openrouter",
    endpoint: "https://openrouter.ai/api/v1/embeddings",
    include_referer: true,
};
pub static MISTRAL: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "mistral",
    endpoint: "https://api.mistral.ai/v1/embeddings",
    include_referer: false,
};
pub static VOYAGE_AI: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "voyage-ai",
    endpoint: "https://api.voyageai.com/v1/embeddings",
    include_referer: false,
};
pub static FIREWORKS: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "fireworks",
    endpoint: "https://api.fireworks.ai/inference/v1/embeddings",
    include_referer: false,
};
pub static TOGETHER: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "together",
    endpoint: "https://api.together.xyz/v1/embeddings",
    include_referer: false,
};
pub static NEBIUS: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "nebius",
    endpoint: "https://api.tokenfactory.nebius.com/v1/embeddings",
    include_referer: false,
};
pub static NVIDIA: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "nvidia",
    endpoint: "https://integrate.api.nvidia.com/v1/embeddings",
    include_referer: false,
};
pub static JINA_AI: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "jina-ai",
    endpoint: "https://api.jina.ai/v1/embeddings",
    include_referer: false,
};
pub static VERCEL_AI_GATEWAY: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "vercel-ai-gateway",
    endpoint: "https://ai-gateway.vercel.sh/v1/embeddings",
    include_referer: false,
};

#[async_trait]
impl EmbeddingAdapter for OpenAiCompatAdapter {
    /// A connection-level `baseUrl` overrides the baked-in endpoint.
    ///
    /// `self.endpoint` is a compile-time constant, so a provider configured to
    /// point somewhere else — a self-hosted OpenAI-compatible gateway, a mock,
    /// a corporate proxy — was still dialled at `api.openai.com` with the
    /// user's key attached. That is both a broken configuration and a
    /// credential-disclosure path. `tts/openai.rs` has always honoured
    /// `baseUrl`; this brings the embedding adapters in line.
    ///
    /// The stored value is a base, not a full endpoint, so the adapter's own
    /// path suffix is reapplied. An explicit `/embeddings` on the configured
    /// value is tolerated rather than doubled.
    fn build_url(&self, request: &EmbeddingRequest<'_>) -> Result<String, String> {
        let Some(configured) = request
            .credentials
            .provider_specific_data
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(self.endpoint.to_string());
        };
        let base = configured.trim_end_matches('/');
        if base.ends_with("/embeddings") {
            return Ok(base.to_string());
        }
        // The baked-in endpoints carry the API prefix (`/v1/embeddings`), so
        // reattach the suffix the connection's base URL left off.
        let suffix = self
            .endpoint
            .strip_prefix("https://api.openai.com/v1")
            .map(|rest| rest.to_string())
            .unwrap_or_else(|| {
                // Non-OpenAI providers bake in a different origin; keep their
                // path and only swap the origin for the configured one.
                self.endpoint
                    .split_once("/v1")
                    .map(|(_, rest)| format!("/v1{rest}"))
                    .unwrap_or_else(|| "/embeddings".to_string())
            });
        Ok(format!("{base}{suffix}"))
    }

    fn build_headers(&self, request: &EmbeddingRequest<'_>) -> Result<HeaderMap, String> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        if !key.is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|e| format!("auth header: {e}"))?,
            );
        }
        if self.include_referer {
            // 9router parity (registry/openrouter.js:50-58): media endpoints
            // use the endpoint-proxy.local referer, matching the chat path.
            headers.insert(
                "HTTP-Referer",
                HeaderValue::from_static("https://endpoint-proxy.local"),
            );
            headers.insert("X-Title", HeaderValue::from_static("Endpoint Proxy"));
        }
        Ok(headers)
    }

    fn build_body(&self, request: &EmbeddingRequest<'_>) -> Result<Value, String> {
        let input = request
            .input()
            .ok_or_else(|| "Missing required field: input".to_string())?;
        let mut body = json!({"model": request.model, "input": input.clone()});
        // Default encoding_format to "float" if not specified by caller.
        let encoding_fmt = request.encoding_format().unwrap_or("float");
        if let Some(obj) = body.as_object_mut() {
            obj.insert("encoding_format".into(), json!(encoding_fmt));
        }
        if let Some(dim) = request.dimensions() {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("dimensions".into(), json_number(dim));
            }
        }
        Ok(body)
    }
}

// ─── GitHub Copilot embeddings (dedicated adapter) ───────────────────────
// 9router `open-sse/handlers/embeddingProviders/github.js`: the GitHub
// Models inference API (models.github.ai) was retired; Copilot embeddings
// live at `api.githubcopilot.com/embeddings` (registry
// `embeddingConfig.baseUrl`) and authenticate with the short-lived Copilot
// token plus the IDE-identifying headers the chat executor already sends —
// a subset: no `openai-intent`, `x-request-id`, `X-Initiator`, or
// `anthropic-version`. Upstream's `buildUrl` ignores per-connection
// `baseUrl` (the registry value is the source of truth), so this adapter
// returns the fixed endpoint too. `buildBody`/`normalize` are inherited
// from the OpenAI-compatible adapter unchanged.
//
// The Copilot header constants mirror `executor/github.rs` (itself a port
// of the registry `transport.copilot` block); they are duplicated here
// because that module is private to `core::executor`.

const GITHUB_EMBEDDINGS_URL: &str = "https://api.githubcopilot.com/embeddings";
const GITHUB_COPILOT_VSCODE_VERSION: &str = "1.110.0";
const GITHUB_COPILOT_CHAT_VERSION: &str = "0.38.0";
const GITHUB_COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.38.0";
const GITHUB_COPILOT_API_VERSION: &str = "2025-04-01";

pub struct GithubEmbeddingAdapter;
pub static GITHUB: GithubEmbeddingAdapter = GithubEmbeddingAdapter;

#[async_trait]
impl EmbeddingAdapter for GithubEmbeddingAdapter {
    fn build_url(&self, _request: &EmbeddingRequest<'_>) -> Result<String, String> {
        Ok(GITHUB_EMBEDDINGS_URL.to_string())
    }

    fn build_headers(&self, request: &EmbeddingRequest<'_>) -> Result<HeaderMap, String> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        // 9router github.js: `creds.copilotToken || psd.copilotToken ||
        // creds.accessToken` — no apiKey fallback (Copilot tokens are
        // short-lived OAuth exchanges, not user API keys).
        let token = request
            .credentials
            .provider_specific_data
            .get("copilotToken")
            .and_then(|v| v.as_str())
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        if !token.is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|e| format!("auth header: {e}"))?,
            );
        }
        headers.insert(
            "copilot-integration-id",
            HeaderValue::from_static("vscode-chat"),
        );
        headers.insert(
            "editor-version",
            HeaderValue::from_str(&format!("vscode/{GITHUB_COPILOT_VSCODE_VERSION}"))
                .map_err(|e| format!("editor-version header: {e}"))?,
        );
        headers.insert(
            "editor-plugin-version",
            HeaderValue::from_str(&format!("copilot-chat/{GITHUB_COPILOT_CHAT_VERSION}"))
                .map_err(|e| format!("editor-plugin-version header: {e}"))?,
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(GITHUB_COPILOT_USER_AGENT),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(GITHUB_COPILOT_API_VERSION),
        );
        Ok(headers)
    }

    fn build_body(&self, request: &EmbeddingRequest<'_>) -> Result<Value, String> {
        OPENAI.build_body(request)
    }
}

// ─── OpenAI-compatible Node (runtime baseUrl) ────────────────────────────

pub struct OpenAiCompatNodeAdapter;
pub static OPENAI_COMPAT_NODE: OpenAiCompatNodeAdapter = OpenAiCompatNodeAdapter;

#[async_trait]
impl EmbeddingAdapter for OpenAiCompatNodeAdapter {
    fn build_url(&self, request: &EmbeddingRequest<'_>) -> Result<String, String> {
        let raw = request
            .credentials
            .provider_specific_data
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.trim_end_matches('/'))
            .unwrap_or("https://api.openai.com/v1");
        let raw = raw.strip_suffix("/embeddings").unwrap_or(raw);
        Ok(format!("{raw}/embeddings"))
    }

    fn build_headers(&self, request: &EmbeddingRequest<'_>) -> Result<HeaderMap, String> {
        OPENAI.build_headers(request)
    }

    fn build_body(&self, request: &EmbeddingRequest<'_>) -> Result<Value, String> {
        OPENAI.build_body(request)
    }
}

// ─── Self-hosted Embedding (per-connection baseUrl, NO cloud fallback) ────
// 9router `open-sse/handlers/embeddingProviders/selfhostedEmbedding.js` — must
// refuse to fall back to api.openai.com (would leak input text + API key).

pub struct SelfhostedEmbeddingAdapter;
pub static SELFHOSTED_EMBEDDING: SelfhostedEmbeddingAdapter = SelfhostedEmbeddingAdapter;

#[async_trait]
impl EmbeddingAdapter for SelfhostedEmbeddingAdapter {
    fn build_url(&self, request: &EmbeddingRequest<'_>) -> Result<String, String> {
        let raw = request
            .credentials
            .provider_specific_data
            .get("baseUrl")
            .and_then(|v| v.as_str());
        let raw = raw
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "Self-hosted Embedding needs an endpoint: set this connection's baseUrl \
             to the OpenAI base URL of your server, e.g. http://host:8080/v1 (note \
             the /v1 — \"/embeddings\" is appended to it). Refusing to fall back to \
             api.openai.com, which would send your input and API key to OpenAI."
                    .to_string()
            })?;
        // JS order: strip a single trailing "/", then the "/embeddings" suffix.
        let trimmed = raw.trim_end_matches('/');
        let trimmed = trimmed.strip_suffix("/embeddings").unwrap_or(trimmed);
        Ok(format!("{trimmed}/embeddings"))
    }

    fn build_headers(&self, request: &EmbeddingRequest<'_>) -> Result<HeaderMap, String> {
        OPENAI.build_headers(request)
    }

    fn build_body(&self, request: &EmbeddingRequest<'_>) -> Result<Value, String> {
        OPENAI.build_body(request)
    }
}

// ─── Gemini embeddings (embedContent / batchEmbedContents) ───────────────

pub struct GeminiAdapter;
pub static GEMINI: GeminiAdapter = GeminiAdapter;

const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

fn model_path(model: &str) -> String {
    if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{model}")
    }
}

#[async_trait]
impl EmbeddingAdapter for GeminiAdapter {
    fn build_url(&self, request: &EmbeddingRequest<'_>) -> Result<String, String> {
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        let path = model_path(request.model);
        let op = if request.input().map(|v| v.is_array()).unwrap_or(false) {
            "batchEmbedContents"
        } else {
            "embedContent"
        };
        Ok(format!(
            "{GEMINI_BASE}/{path}:{op}?key={}",
            urlencoding::encode(key)
        ))
    }

    fn build_headers(&self, _: &EmbeddingRequest<'_>) -> Result<HeaderMap, String> {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(h)
    }

    fn build_body(&self, request: &EmbeddingRequest<'_>) -> Result<Value, String> {
        let input = request
            .input()
            .ok_or_else(|| "Missing required field: input".to_string())?;
        let m = model_path(request.model);
        // Forward dimensions as outputDimensionality for Gemini (Gemini API name).
        let dim = request.dimensions().map(json_number);
        if let Some(arr) = input.as_array() {
            let requests: Vec<Value> = arr
                .iter()
                .map(|v| {
                    let text = js_string(v);
                    let mut req = json!({
                        "model": m,
                        "content": {"parts": [{"text": text}]},
                    });
                    if let Some(ref d) = dim {
                        if let Some(obj) = req.as_object_mut() {
                            obj.insert("outputDimensionality".into(), d.clone());
                        }
                    }
                    req
                })
                .collect();
            Ok(json!({"requests": requests}))
        } else {
            let text = js_string(input);
            let mut body = json!({"model": m, "content": {"parts": [{"text": text}]}});
            if let Some(d) = dim {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("outputDimensionality".into(), d);
                }
            }
            Ok(body)
        }
    }

    fn normalize(&self, body: &Value, model: &str) -> Value {
        if body.get("object") == Some(&Value::String("list".into()))
            && body.get("data").and_then(|v| v.as_array()).is_some()
        {
            return body.clone();
        }
        let items: Vec<Value> = if let Some(arr) = body.get("embeddings").and_then(|v| v.as_array())
        {
            arr.iter()
                .enumerate()
                .map(|(idx, emb)| {
                    let values = emb
                        .get("values")
                        .cloned()
                        .unwrap_or_else(|| Value::Array(Vec::new()));
                    json!({
                        "object": "embedding",
                        "index": idx,
                        "embedding": values,
                    })
                })
                .collect()
        } else if let Some(values) = body.get("embedding").and_then(|e| e.get("values")).cloned() {
            vec![json!({
                "object": "embedding",
                "index": 0,
                "embedding": values,
            })]
        } else {
            Vec::new()
        };
        // Gemini's embedContent / batchEmbedContents responses do not include
        // usage (token) information, so we inject a 0-usage placeholder to
        // stay compatible with the OpenAI response shape downstream.
        json!({
            "object": "list",
            "data": items,
            "model": model,
            "usage": {"prompt_tokens": 0, "total_tokens": 0}
        })
    }
}

// ─── Ollama Local embeddings (/v1/embeddings on the local daemon) ────────
// 9router `open-sse/handlers/embeddingProviders/ollamaLocal.js` — the saved
// host (`providerSpecificData.baseUrl`, default http://localhost:11434)
// is normalised by stripping trailing slashes plus a `/v1/embeddings` or
// `/v1` suffix before `/v1/embeddings` is re-appended. Bearer auth is sent
// only when an API key is configured (`creds?.apiKey` — a local daemon
// needs none). `buildBody`/`normalize` are the OpenAI-compatible shape:
// `{model, input, encoding_format, dimensions?}` in, the response passes
// through untouched.
//
// The Phonon TTS side lives in `core::media::tts::ollama_local`; the host
// resolver is shared via `core::media::ollama_local::resolve_host`.

pub struct OllamaLocalAdapter;
pub static OLLAMA_LOCAL: OllamaLocalAdapter = OllamaLocalAdapter;

/// Strip a saved endpoint suffix so `{host}/v1/embeddings` never doubles
/// up — JS ollamaLocal.js buildUrl: trailing slashes, then
/// `/v1/embeddings`, then `/v1`.
fn ollama_local_embeddings_url(credentials: &ProviderConnection) -> String {
    let mut host = crate::core::media::ollama_local::resolve_host(credentials);
    if let Some(stripped) = host.strip_suffix("/v1/embeddings") {
        host = stripped.trim_end_matches('/').to_string();
    }
    if let Some(stripped) = host.strip_suffix("/v1") {
        host = stripped.trim_end_matches('/').to_string();
    }
    format!("{host}/v1/embeddings")
}

#[async_trait]
impl EmbeddingAdapter for OllamaLocalAdapter {
    fn build_url(&self, request: &EmbeddingRequest<'_>) -> Result<String, String> {
        Ok(ollama_local_embeddings_url(request.credentials))
    }

    fn build_headers(&self, request: &EmbeddingRequest<'_>) -> Result<HeaderMap, String> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        // `creds?.apiKey` only — upstream does not fall back to the OAuth
        // access token for this provider.
        if let Some(key) = request
            .credentials
            .api_key
            .as_deref()
            .filter(|k| !k.is_empty())
        {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|e| format!("auth header: {e}"))?,
            );
        }
        Ok(headers)
    }

    fn build_body(&self, request: &EmbeddingRequest<'_>) -> Result<Value, String> {
        OPENAI.build_body(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderConnection;
    use serde_json::json;

    #[test]
    fn openai_compat_endpoint() {
        let body = json!({"input": "hello"});
        let creds = ProviderConnection::default();
        let req = EmbeddingRequest {
            body: &body,
            model: "text-embedding-3-small",
            credentials: &creds,
        };
        let url = OPENAI.build_url(&req).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/embeddings");
    }

    /// The plain OpenAI adapter used to return its compile-time `endpoint`
    /// unconditionally, so a connection configured with a `baseUrl` — a
    /// self-hosted OpenAI-compatible gateway, a mock, a corporate proxy — was
    /// still dialled at `api.openai.com` with the user's key attached. That is
    /// both a broken configuration and a credential-disclosure path.
    #[test]
    fn openai_compat_honours_a_configured_base_url() {
        let body = json!({"input": "hello"});
        let mut creds = ProviderConnection::default();
        creds
            .provider_specific_data
            .insert("baseUrl".to_string(), json!("http://127.0.0.1:45998/v1"));
        let req = EmbeddingRequest {
            body: &body,
            model: "text-embedding-3-small",
            credentials: &creds,
        };
        let url = OPENAI.build_url(&req).unwrap();
        assert_eq!(url, "http://127.0.0.1:45998/v1/embeddings");
    }

    /// A configured base that already carries the path suffix must not get it
    /// doubled.
    #[test]
    fn openai_compat_tolerates_a_base_url_that_already_has_the_suffix() {
        let body = json!({"input": "hello"});
        let mut creds = ProviderConnection::default();
        creds.provider_specific_data.insert(
            "baseUrl".to_string(),
            json!("http://127.0.0.1:45998/v1/embeddings"),
        );
        let req = EmbeddingRequest {
            body: &body,
            model: "text-embedding-3-small",
            credentials: &creds,
        };
        let url = OPENAI.build_url(&req).unwrap();
        assert_eq!(url, "http://127.0.0.1:45998/v1/embeddings");
    }

    #[test]
    fn openai_compat_node_uses_credentials_base_url() {
        let body = json!({"input": "hello"});
        let mut creds = ProviderConnection::default();
        creds.provider_specific_data.insert(
            "baseUrl".to_string(),
            json!("https://example.com/v1/embeddings"),
        );
        let req = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };
        let url = OPENAI_COMPAT_NODE.build_url(&req).unwrap();
        assert_eq!(url, "https://example.com/v1/embeddings");
    }

    #[test]
    fn gemini_picks_batch_for_array_input() {
        let mut creds = ProviderConnection::default();
        creds.api_key = Some("k".into());
        let body = json!({"input": ["a", "b"]});
        let req = EmbeddingRequest {
            body: &body,
            model: "embedding-001",
            credentials: &creds,
        };
        let url = GEMINI.build_url(&req).unwrap();
        assert!(url.contains(":batchEmbedContents"));
    }

    #[test]
    fn gemini_normalizes_embeddings_to_openai_shape() {
        let body = json!({
            "embeddings": [
                {"values": [0.1, 0.2]},
                {"values": [0.3, 0.4]}
            ]
        });
        let normalized = GEMINI.normalize(&body, "embedding-001");
        assert_eq!(normalized["object"], "list");
        assert_eq!(normalized["data"].as_array().unwrap().len(), 2);
        assert_eq!(normalized["data"][0]["embedding"], json!([0.1, 0.2]));
    }

    /// 9router `gemini.js:24, :31` coerces every element with `String(text)`.
    /// `serde_json::Value::to_string` is `JSON.stringify`, so a numeric array
    /// reached Gemini as `"[1,2]"` instead of JS's `"1,2"`.
    #[test]
    fn gemini_batch_stringifies_elements_like_js() {
        let creds = ProviderConnection::default();
        let body = json!({"input": [[1, 2], [[3], [4, 5]], 1.0, true, null, {"a": 1}, "x"]});
        let req = EmbeddingRequest {
            body: &body,
            model: "embedding-001",
            credentials: &creds,
        };
        let built = GEMINI.build_body(&req).unwrap();
        let texts: Vec<String> = built["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                r["content"]["parts"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            texts,
            vec!["1,2", "3,4,5", "1", "true", "null", "[object Object]", "x"]
        );
    }

    /// The single-input `embedContent` arm applies the same `String()`, so an
    /// integral double reaches Gemini as `"1"`, not serde_json's `"1.0"`.
    #[test]
    fn gemini_single_input_stringifies_like_js() {
        let creds = ProviderConnection::default();
        let body = json!({"input": 1.0});
        let req = EmbeddingRequest {
            body: &body,
            model: "embedding-001",
            credentials: &creds,
        };
        let built = GEMINI.build_body(&req).unwrap();
        assert_eq!(built["content"]["parts"][0]["text"], "1");
    }

    #[test]
    fn build_body_validates_input() {
        let body = json!({});
        let creds = ProviderConnection::default();
        let req = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };
        assert!(OPENAI.build_body(&req).is_err());
    }

    #[test]
    fn build_body_includes_dimensions_when_set() {
        let body = json!({"input": "hi", "dimensions": 256});
        let creds = ProviderConnection::default();
        let req = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };
        let v = OPENAI.build_body(&req).unwrap();
        assert_eq!(v["dimensions"], 256);
    }

    /// 9router coerces with `Number(dimensions)` before the
    /// `isFinite && > 0` guard, so a numeric string reaches the provider as a
    /// number. `as_u64` returned None for it and the field was silently lost.
    #[test]
    fn dimensions_coerces_numeric_strings() {
        let creds = ProviderConnection::default();
        for raw in [json!("256"), json!(" 512 ")] {
            let body = json!({"input": "hi", "dimensions": raw});
            let req = EmbeddingRequest {
                body: &body,
                model: "x",
                credentials: &creds,
            };
            let v = OPENAI.build_body(&req).unwrap();
            assert!(
                v["dimensions"].is_number(),
                "{raw} must be forwarded as a number, got {}",
                v["dimensions"]
            );
        }
        let body = json!({"input": "hi", "dimensions": "256"});
        let req = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };
        assert_eq!(OPENAI.build_body(&req).unwrap()["dimensions"], 256);
    }

    /// JS `Number` has no integer restriction — a fractional request is a
    /// client error the provider should see, not one we swallow.
    #[test]
    fn dimensions_keeps_non_integers() {
        let creds = ProviderConnection::default();
        for (raw, expected) in [(json!(8.5), 8.5_f64), (json!(256.0), 256.0_f64)] {
            let body = json!({"input": "hi", "dimensions": raw});
            let req = EmbeddingRequest {
                body: &body,
                model: "x",
                credentials: &creds,
            };
            let v = OPENAI.build_body(&req).unwrap();
            assert_eq!(v["dimensions"].as_f64().unwrap(), expected, "for {raw}");
        }
    }

    /// Everything the JS guard rejects stays dropped: `null`, `""` (the
    /// `dimensions !== ""` early-out), non-numeric strings, and non-positive
    /// numbers.
    #[test]
    fn dimensions_drops_unusable_values() {
        let creds = ProviderConnection::default();
        for raw in [
            json!(null),
            json!(""),
            json!("abc"),
            json!(0),
            json!(-1),
            json!(true),
        ] {
            let body = json!({"input": "hi", "dimensions": raw});
            let req = EmbeddingRequest {
                body: &body,
                model: "x",
                credentials: &creds,
            };
            let v = OPENAI.build_body(&req).unwrap();
            assert!(
                v.get("dimensions").is_none(),
                "{raw} must be dropped, got {}",
                v["dimensions"]
            );
        }
    }

    /// Gemini takes the same coerced value under its own key.
    #[test]
    fn gemini_forwards_coerced_output_dimensionality() {
        let mut creds = ProviderConnection::default();
        creds.api_key = Some("k".into());
        let body = json!({"input": "hi", "dimensions": "768"});
        let req = EmbeddingRequest {
            body: &body,
            model: "embedding-001",
            credentials: &creds,
        };
        let v = GEMINI.build_body(&req).unwrap();
        assert_eq!(v["outputDimensionality"], 768);
    }

    #[test]
    fn openai_embedding_encoding_format_matches_js_coercion() {
        // JS embeddingsCore.js:52 `encoding_format: body.encoding_format || "float"`
        // always sends encoding_format upstream (client value or "float").
        // Rust mirrors that: absent → "float"; empty → "float"; value → value.
        let creds = ProviderConnection::default();

        // Absent → "float" (both JS and Rust).
        let body = json!({"input": "hi"});
        let req = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };
        let v = OPENAI.build_body(&req).unwrap();
        assert_eq!(v["encoding_format"], "float");

        // Empty string → coerced to "float" by JS `||`; Rust now treats it as None.
        let body = json!({"input": "hi", "encoding_format": ""});
        let req = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };
        let v = OPENAI.build_body(&req).unwrap();
        assert_eq!(v["encoding_format"], "float");

        // Explicit value passed through.
        let body = json!({"input": "hi", "encoding_format": "base64"});
        let req = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };
        let v = OPENAI.build_body(&req).unwrap();
        assert_eq!(v["encoding_format"], "base64");
    }

    #[test]
    fn vercel_gateway_embedding_registered() {
        let body = json!({"input": "hi"});
        let creds = ProviderConnection::default();
        let req = EmbeddingRequest {
            body: &body,
            model: "text-embedding-3-small",
            credentials: &creds,
        };
        let url = VERCEL_AI_GATEWAY.build_url(&req).unwrap();
        assert_eq!(url, "https://ai-gateway.vercel.sh/v1/embeddings");
        assert!(!VERCEL_AI_GATEWAY.include_referer);
    }

    #[test]
    fn selfhosted_embedding_requires_base_url() {
        let body = json!({"input": "hi"});
        let creds = ProviderConnection::default();
        let req = EmbeddingRequest {
            body: &body,
            model: "embedding",
            credentials: &creds,
        };
        let err = SELFHOSTED_EMBEDDING.build_url(&req).unwrap_err();
        assert!(
            err.contains("Self-hosted Embedding needs an endpoint"),
            "must refuse missing baseUrl, got: {err}"
        );
        // Refuses — it must NOT resolve to api.openai.com (the leak the
        // adapter is written to prevent). The message mentions the fallback
        // only to warn about it.
        assert!(!err.starts_with("https://api.openai.com"));
    }

    #[test]
    fn selfhosted_embedding_appends_embeddings() {
        let body = json!({"input": "hi"});
        for base in ["http://host:8080/v1", "http://host:8080/v1/embeddings"] {
            let mut creds = ProviderConnection::default();
            creds
                .provider_specific_data
                .insert("baseUrl".into(), json!(base));
            let req = EmbeddingRequest {
                body: &body,
                model: "embedding",
                credentials: &creds,
            };
            let url = SELFHOSTED_EMBEDDING.build_url(&req).unwrap();
            assert_eq!(url, "http://host:8080/v1/embeddings", "for base {base}");
        }
    }

    #[test]
    fn selfhosted_embedding_blank_base_url_refuses() {
        let body = json!({"input": "hi"});
        let mut creds = ProviderConnection::default();
        creds
            .provider_specific_data
            .insert("baseUrl".into(), json!("   "));
        let req = EmbeddingRequest {
            body: &body,
            model: "embedding",
            credentials: &creds,
        };
        let err = SELFHOSTED_EMBEDDING.build_url(&req).unwrap_err();
        assert!(err.contains("needs an endpoint"));
    }

    #[test]
    fn openrouter_embedding_referer_matches_registry() {
        // 9router registry/openrouter.js:50-55 — endpoint-proxy.local referer.
        let body = json!({"input": "hello"});
        let creds = ProviderConnection::default();
        let req = EmbeddingRequest {
            body: &body,
            model: "text-embedding-3-small",
            credentials: &creds,
        };
        let headers = OPENROUTER.build_headers(&req).unwrap();
        assert_eq!(
            headers.get("HTTP-Referer").and_then(|v| v.to_str().ok()),
            Some("https://endpoint-proxy.local")
        );
        assert_eq!(
            headers.get("X-Title").and_then(|v| v.to_str().ok()),
            Some("Endpoint Proxy")
        );
    }

    /// 9router commit 51a1827e (`fix(github): route Copilot embeddings to
    /// api.githubcopilot.com`): the GitHub Models inference API
    /// (models.github.ai) was retired; embeddings now live on the Copilot
    /// host with the Copilot token + IDE headers.
    #[test]
    fn github_embedding_resolves_to_copilot_endpoint() {
        let body = json!({"input": "hello"});
        let mut creds = ProviderConnection::default();
        creds
            .provider_specific_data
            .insert("copilotToken".to_string(), json!("tid=copilot-token"));
        creds.access_token = Some("gho-oauth".to_string());
        let req = EmbeddingRequest {
            body: &body,
            model: "text-embedding-ada-002",
            credentials: &creds,
        };
        assert_eq!(
            GITHUB.build_url(&req).unwrap(),
            "https://api.githubcopilot.com/embeddings"
        );
        // Copilot token wins over the OAuth access token.
        let headers = GITHUB.build_headers(&req).unwrap();
        assert_eq!(
            headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()),
            Some("Bearer tid=copilot-token")
        );
        assert_eq!(
            headers
                .get("copilot-integration-id")
                .and_then(|v| v.to_str().ok()),
            Some("vscode-chat")
        );
        assert_eq!(
            headers.get("editor-version").and_then(|v| v.to_str().ok()),
            Some("vscode/1.110.0")
        );
        assert_eq!(
            headers
                .get("editor-plugin-version")
                .and_then(|v| v.to_str().ok()),
            Some("copilot-chat/0.38.0")
        );
        assert_eq!(
            headers
                .get(reqwest::header::USER_AGENT)
                .and_then(|v| v.to_str().ok()),
            Some("GitHubCopilotChat/0.38.0")
        );
        assert_eq!(
            headers
                .get("x-github-api-version")
                .and_then(|v| v.to_str().ok()),
            Some("2025-04-01")
        );
        // The model id passes through into the OpenAI-shaped body.
        let built = GITHUB.build_body(&req).unwrap();
        assert_eq!(built["model"], "text-embedding-ada-002");
    }

    /// With no copilotToken the adapter falls back to access_token, matching
    /// upstream's `psd.copilotToken || creds.accessToken` tail.
    #[test]
    fn github_embedding_falls_back_to_access_token() {
        let body = json!({"input": "hi"});
        let mut creds = ProviderConnection::default();
        creds.access_token = Some("gho-oauth".to_string());
        let req = EmbeddingRequest {
            body: &body,
            model: "text-embedding-ada-002",
            credentials: &creds,
        };
        let headers = GITHUB.build_headers(&req).unwrap();
        assert_eq!(
            headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()),
            Some("Bearer gho-oauth")
        );
    }

    #[test]
    fn jina_embedding_endpoint_matches() {
        // 9router embeddingProviders/openai.js ENDPOINTS: jina-ai →
        // https://api.jina.ai/v1/embeddings (no override needed).
        assert_eq!(JINA_AI.endpoint, "https://api.jina.ai/v1/embeddings");
        // The adapter's build_url returns the endpoint verbatim.
        let body = serde_json::json!({"input": "hi"});
        let creds = ProviderConnection::default();
        let req = EmbeddingRequest {
            body: &body,
            model: "jina-embeddings-v3",
            credentials: &creds,
        };
        assert_eq!(
            JINA_AI.build_url(&req).unwrap(),
            "https://api.jina.ai/v1/embeddings"
        );
    }

    // ── ollama-local ────────────────────────────────────────────────────

    fn ollama_req<'a>(body: &'a Value, creds: &'a ProviderConnection) -> EmbeddingRequest<'a> {
        EmbeddingRequest {
            body,
            model: "nomic-embed-text",
            credentials: creds,
        }
    }

    #[test]
    fn ollama_local_url_normalizes_saved_host_suffixes() {
        // 9router tests/unit/ollama-local-embeddings.test.js — every saved
        // host shape must collapse to {host}/v1/embeddings.
        let body = json!({"input": "hi"});
        for (saved, expected) in [
            (None, "http://localhost:11434/v1/embeddings"),
            (
                Some("http://192.168.1.100:11434"),
                "http://192.168.1.100:11434/v1/embeddings",
            ),
            (
                Some("http://192.168.1.100:11434///"),
                "http://192.168.1.100:11434/v1/embeddings",
            ),
            (
                Some("http://192.168.1.100:11434/v1"),
                "http://192.168.1.100:11434/v1/embeddings",
            ),
            (
                Some("http://192.168.1.100:11434/v1/embeddings"),
                "http://192.168.1.100:11434/v1/embeddings",
            ),
        ] {
            let mut creds = ProviderConnection::default();
            if let Some(url) = saved {
                creds
                    .provider_specific_data
                    .insert("baseUrl".to_string(), json!(url));
            }
            let req = ollama_req(&body, &creds);
            assert_eq!(OLLAMA_LOCAL.build_url(&req).unwrap(), expected, "{saved:?}");
        }
    }

    #[test]
    fn ollama_local_sends_bearer_only_when_api_key_set() {
        let body = json!({"input": "hi"});

        let creds = ProviderConnection::default();
        let req = ollama_req(&body, &creds);
        let headers = OLLAMA_LOCAL.build_headers(&req).unwrap();
        assert!(headers.get(AUTHORIZATION).is_none());

        let mut keyed = ProviderConnection::default();
        keyed.api_key = Some("ollama-secret".into());
        let req = ollama_req(&body, &keyed);
        let headers = OLLAMA_LOCAL.build_headers(&req).unwrap();
        assert_eq!(
            headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()),
            Some("Bearer ollama-secret")
        );

        // Upstream reads `creds?.apiKey` only — an OAuth access token alone
        // must NOT produce an Authorization header.
        let mut token_only = ProviderConnection::default();
        token_only.access_token = Some("oauth-token".into());
        let req = ollama_req(&body, &token_only);
        let headers = OLLAMA_LOCAL.build_headers(&req).unwrap();
        assert!(headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn ollama_local_body_forwards_model_input_encoding_and_dimensions() {
        let body = json!({"input": "hi", "dimensions": 512});
        let creds = ProviderConnection::default();
        let req = ollama_req(&body, &creds);
        let built = OLLAMA_LOCAL.build_body(&req).unwrap();
        assert_eq!(built["model"], "nomic-embed-text");
        assert_eq!(built["input"], "hi");
        assert_eq!(built["encoding_format"], "float");
        assert_eq!(built["dimensions"], 512);
    }
}
