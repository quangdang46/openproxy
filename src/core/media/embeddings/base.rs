//! Embedding-adapter trait + the three concrete impls.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::types::ProviderConnection;

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
        self.body.get("encoding_format").and_then(|v| v.as_str())
    }

    pub fn dimensions(&self) -> Option<u64> {
        self.body
            .get("dimensions")
            .and_then(|v| v.as_u64())
            .filter(|&n| n > 0)
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
pub static GITHUB: OpenAiCompatAdapter = OpenAiCompatAdapter {
    provider_id: "github",
    endpoint: "https://models.github.ai/inference/embeddings",
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

#[async_trait]
impl EmbeddingAdapter for OpenAiCompatAdapter {
    fn build_url(&self, _: &EmbeddingRequest<'_>) -> Result<String, String> {
        Ok(self.endpoint.to_string())
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
            headers.insert(
                "HTTP-Referer",
                HeaderValue::from_static("https://openproxy.local"),
            );
            headers.insert("X-Title", HeaderValue::from_static("OpenProxy"));
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
                obj.insert("dimensions".into(), json!(dim));
            }
        }
        Ok(body)
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
        let dim = request.dimensions().map(|d| json!(d));
        if let Some(arr) = input.as_array() {
            let requests: Vec<Value> = arr
                .iter()
                .map(|v| {
                    let text = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
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
            let text = match input {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
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
        json!({
            "object": "list",
            "data": items,
            "model": model,
            "usage": {"prompt_tokens": 0, "total_tokens": 0}
        })
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
}
