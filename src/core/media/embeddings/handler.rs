//! Embeddings handler — orchestrates one upstream call.

use reqwest::Client;
use serde_json::Value;
use thiserror::Error;

use super::base::{EmbeddingAdapter, EmbeddingRequest};

#[derive(Debug, Error)]
pub enum EmbeddingsHandlerError {
    #[error("HTTP {0}: {1}")]
    Http(u16, String),
    #[error("validation: {0}")]
    Validation(String),
    #[error("provider {0} not supported for embeddings")]
    UnsupportedProvider(String),
    #[error("upstream: {0}")]
    Upstream(String),
}

impl EmbeddingsHandlerError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Http(c, _) => *c,
            Self::Validation(_) => 400,
            Self::UnsupportedProvider(_) => 400,
            Self::Upstream(_) => 502,
        }
    }
}

/// Run the embeddings pipeline. Returns the OpenAI-shaped response body.
pub async fn handle_embeddings(
    client: &Client,
    adapter: &dyn EmbeddingAdapter,
    request: EmbeddingRequest<'_>,
) -> Result<Value, EmbeddingsHandlerError> {
    let input = request.input().ok_or_else(|| {
        EmbeddingsHandlerError::Validation("Missing required field: input".into())
    })?;
    if !input.is_string() && !input.is_array() {
        return Err(EmbeddingsHandlerError::Validation(
            "input must be a string or array of strings".into(),
        ));
    }

    let url = adapter
        .build_url(&request)
        .map_err(EmbeddingsHandlerError::Validation)?;
    let headers = adapter
        .build_headers(&request)
        .map_err(EmbeddingsHandlerError::Validation)?;
    let body = adapter
        .build_body(&request)
        .map_err(EmbeddingsHandlerError::Validation)?;

    let res = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|e| EmbeddingsHandlerError::Upstream(e.to_string()))?;

    if !res.status().is_success() {
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        return Err(EmbeddingsHandlerError::Http(status, text));
    }

    let parsed: Value = res
        .json()
        .await
        .map_err(|e| EmbeddingsHandlerError::Upstream(format!("parse json: {e}")))?;

    Ok(adapter.normalize(&parsed, request.model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::media::embeddings::get_embedding_adapter;
    use crate::types::ProviderConnection;
    use serde_json::json;

    #[test]
    fn registry_returns_known_providers() {
        for p in [
            "openai",
            "openrouter",
            "mistral",
            "voyage-ai",
            "fireworks",
            "together",
            "nebius",
            "github",
            "nvidia",
            "jina-ai",
            "gemini",
            "google_ai_studio",
        ] {
            assert!(get_embedding_adapter(p).is_some(), "missing adapter: {p}");
        }
        assert!(get_embedding_adapter("nope").is_none());
    }

    #[test]
    fn registry_falls_back_to_node_adapter() {
        assert!(get_embedding_adapter("openai-compatible-foo").is_some());
        assert!(get_embedding_adapter("custom-embedding-xyz").is_some());
    }

    #[test]
    fn validation_rejects_missing_input() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = Client::new();
        let body = json!({});
        let creds = ProviderConnection::default();
        let req = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };
        let res = runtime.block_on(handle_embeddings(
            &client,
            get_embedding_adapter("openai").unwrap(),
            req,
        ));
        assert!(matches!(res, Err(EmbeddingsHandlerError::Validation(_))));
    }
}
