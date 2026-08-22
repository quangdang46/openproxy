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

    let mut url = adapter
        .build_url(&request)
        .map_err(EmbeddingsHandlerError::Validation)?;
    let mut headers = adapter
        .build_headers(&request)
        .map_err(EmbeddingsHandlerError::Validation)?;
    let body = adapter
        .build_body(&request)
        .map_err(EmbeddingsHandlerError::Validation)?;

    let mut res = client
        .post(&url)
        .headers(headers.clone())
        .json(&body)
        .send()
        .await
        .map_err(|e| EmbeddingsHandlerError::Upstream(e.to_string()))?;

    // 9router embeddingsCore.js:66-101 — one-shot refresh retry on 401/403:
    // never for no_auth adapters, never retried twice, other statuses fail.
    let status = res.status().as_u16();
    if (status == 401 || status == 403) && !adapter.no_auth() {
        drop(res);
        let refresh_token = request
            .credentials
            .refresh_token
            .clone()
            .unwrap_or_default();
        if !refresh_token.is_empty() {
            if let Ok(refresh) = crate::oauth::token_refresh::dispatch_oauth_refresh(
                &request.credentials.provider,
                &refresh_token,
                &request.credentials.provider_specific_data,
            )
            .await
            {
                let mut refreshed = request.credentials.clone();
                refreshed.access_token = Some(refresh.access_token);
                if let Some(rt) = refresh.refresh_token {
                    refreshed.refresh_token = Some(rt);
                }
                // Rebuild url/headers/body against the refreshed credentials.
                let retry_req = EmbeddingRequest {
                    body: request.body,
                    model: request.model,
                    credentials: &refreshed,
                };
                if let Ok(new_url) = adapter.build_url(&retry_req) {
                    url = new_url;
                }
                if let Ok(new_headers) = adapter.build_headers(&retry_req) {
                    headers = new_headers;
                }
                if let Ok(new_body) = adapter.build_body(&retry_req) {
                    res = client.post(&url).headers(headers).json(&new_body).send().await
                        .map_err(|e| EmbeddingsHandlerError::Upstream(e.to_string()))?;

                    if !res.status().is_success() {
                        let status = res.status().as_u16();
                        let text = res.text().await.unwrap_or_default();
                        return Err(EmbeddingsHandlerError::Http(status, text));
                    }
                    let parsed: Value = res.json().await.map_err(|e| {
                        EmbeddingsHandlerError::Upstream(format!("parse json: {e}"))
                    })?;
                    return Ok(adapter.normalize(&parsed, request.model));
                }
            }
        }
        // Refresh failed or rebuild failed → re-fire the original once so the
        // caller still gets a proper upstream status (matches JS re-request).
        res = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| EmbeddingsHandlerError::Upstream(e.to_string()))?;
    }

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
