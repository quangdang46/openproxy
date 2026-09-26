//! Embeddings handler — orchestrates one upstream call.

use std::time::Duration;

use reqwest::Client;
use serde_json::Value;
use thiserror::Error;

use super::base::{EmbeddingAdapter, EmbeddingRequest};

#[derive(Debug, Error)]
pub enum EmbeddingsHandlerError {
    // 9router `open-sse/utils/error.js createErrorResult` prefixes the status
    // as `[n]:`, not `HTTP n:`.
    #[error("[{0}]: {1}")]
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

/// Upstream fetch deadline for one embeddings call.
///
/// 9router `open-sse/config/runtimeConfig.js:59` reads
/// `FETCH_CONNECT_TIMEOUT_MS` per request (default 60 s) and arms
/// `AbortSignal.timeout` with it (embeddingsCore.js:70-73). Read per call
/// rather than cached in a `Lazy` so a deployment can retune without a
/// restart, matching `envMs`.
pub fn fetch_timeout() -> Duration {
    Duration::from_millis(
        std::env::var("FETCH_CONNECT_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(60_000),
    )
}

/// Backoff schedule for `refreshWithRetry(…, 3, …)`
/// (9router `services/tokenRefresh.js:257-275`).
const REFRESH_BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];

/// Refresh once, retrying the same way `refreshWithRetry` does. Returns the
/// first refresh that produced an access token, or `None` if every attempt
/// failed or came back empty.
async fn refresh_with_retry(
    request: &EmbeddingRequest<'_>,
) -> Option<crate::oauth::token_refresh::RefreshResult> {
    let refresh_token = request
        .credentials
        .refresh_token
        .clone()
        .unwrap_or_default();
    for attempt in 0..3 {
        match crate::oauth::token_refresh::dispatch_oauth_refresh(
            &request.credentials.provider,
            &refresh_token,
            &request.credentials.provider_specific_data,
        )
        .await
        {
            Ok(refresh) if !refresh.access_token.is_empty() => return Some(refresh),
            Ok(_) | Err(_) => {
                if let Some(delay) = REFRESH_BACKOFF.get(attempt) {
                    tokio::time::sleep(*delay).await;
                }
            }
        }
    }
    None
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
    // 9router embeddingsCore.js:23-26 — `if (!input)` is a JS falsy test, so
    // "", null, 0 and false are all "missing". An empty ARRAY is truthy in JS
    // and must still reach the provider.
    if is_falsy(input) {
        return Err(EmbeddingsHandlerError::Validation(
            "Missing required field: input".into(),
        ));
    }
    if !input.is_string() && !input.is_array() {
        return Err(EmbeddingsHandlerError::Validation(
            "input must be a string or array of strings".into(),
        ));
    }

    let mut url = adapter
        .build_url(&request)
        .map_err(EmbeddingsHandlerError::Validation)?;
    let mut headers = adapter
        .build_headers(&request)
        .map_err(EmbeddingsHandlerError::Validation)?;
    let body = adapter
        .build_body(&request)
        .map_err(EmbeddingsHandlerError::Validation)?;

    let mut res = send(client, &url, &headers, &body).await?;

    // 9router embeddingsCore.js:82-107 — on 401/403 refresh, and re-fire ONLY
    // when the refresh actually produced new credentials. When there is no
    // refresh token (every API-key provider) or the refresh fails, the original
    // response is used as-is: exactly one upstream call, never a duplicate.
    let status = res.status().as_u16();
    if (status == 401 || status == 403) && !adapter.no_auth() {
        let Some(refresh) = refresh_with_retry(&request).await else {
            // No new credentials → fall through on the original response.
            return finish(adapter, &request, res).await;
        };
        let mut refreshed = request.credentials.clone();
        refreshed.access_token = Some(refresh.access_token);
        if let Some(refresh_token) = refresh.refresh_token {
            refreshed.refresh_token = Some(refresh_token);
        }
        let retry_req = EmbeddingRequest {
            body: request.body,
            model: request.model,
            credentials: &refreshed,
        };
        // Rebuild url/headers against the refreshed credentials; a
        // misconfigured connection falls back to the originals rather than
        // retrying with stale material. The BODY is deliberately the one built
        // before the refresh — embeddingsCore.js:100 re-sends the same
        // `requestBody`, and rebuilding it could change `model`/`dimensions`.
        url = adapter.build_url(&retry_req).unwrap_or(url);
        headers = adapter.build_headers(&retry_req).unwrap_or(headers);
        res = send(client, &url, &headers, &body).await?;
    }

    finish(adapter, &request, res).await
}

/// JS truthiness for the `!body.input` gate 9router applies.
pub fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f == 0.0),
        Value::String(s) => s.is_empty(),
        _ => false,
    }
}

async fn send(
    client: &Client,
    url: &str,
    headers: &reqwest::header::HeaderMap,
    body: &Value,
) -> Result<reqwest::Response, EmbeddingsHandlerError> {
    let timeout = fetch_timeout();
    tokio::time::timeout(
        timeout,
        client.post(url).headers(headers.clone()).json(body).send(),
    )
    .await
    .map_err(|_| {
        EmbeddingsHandlerError::Upstream(format!(
            "embeddings upstream timeout after {}ms",
            timeout.as_millis()
        ))
    })?
    .map_err(|e| EmbeddingsHandlerError::Upstream(e.to_string()))
}

async fn finish(
    adapter: &dyn EmbeddingAdapter,
    request: &EmbeddingRequest<'_>,
    res: reqwest::Response,
) -> Result<Value, EmbeddingsHandlerError> {
    if !res.status().is_success() {
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        return Err(EmbeddingsHandlerError::Http(
            status,
            crate::core::utils::error::parse_upstream_message(&text),
        ));
    }

    let parsed: Value = res
        .json()
        .await
        .map_err(|e| EmbeddingsHandlerError::Upstream(format!("parse json: {e}")))?;

    Ok(adapter.normalize(&parsed, request.model))
}

#[cfg(test)]
mod tests {
    use super::super::base;
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

    /// 9router `!body.input` (embeddingsCore.js:23-26) is a JS falsy test, so
    /// `""`, `null`, `0` and `false` are all "missing" — but `[]` is truthy and
    /// must survive the gate.
    #[test]
    fn falsy_input_matches_js_truthiness() {
        for missing in [json!(null), json!(""), json!(0), json!(false)] {
            assert!(is_falsy(&missing), "expected falsy: {missing}");
        }
        for present in [json!([]), json!("hi"), json!(["a"]), json!(1), json!({})] {
            assert!(!is_falsy(&present), "expected truthy: {present}");
        }
    }

    #[test]
    fn default_fetch_timeout_is_sixty_seconds() {
        // Guard the env override is unset for the rest of the suite.
        if std::env::var("FETCH_CONNECT_TIMEOUT_MS").is_err() {
            assert_eq!(fetch_timeout(), Duration::from_secs(60));
        }
    }

    /// A hung upstream must abort at the deadline instead of holding the caller
    /// for the client's own (much longer) timeout. 9router arms
    /// `AbortSignal.timeout(FETCH_CONNECT_TIMEOUT_MS)` at embeddingsCore.js:70-73.
    #[tokio::test]
    async fn hung_upstream_aborts_at_the_fetch_deadline() {
        // One connection, one accept: read the request, then sit on it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 1024];
            use tokio::io::AsyncReadExt;
            let _ = socket.read(&mut buf).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let mut creds = ProviderConnection::default();
        creds.api_key = Some("sk-test".into());
        creds
            .provider_specific_data
            .insert("baseUrl".into(), json!(format!("http://{addr}/v1")));
        let body = json!({"input": "hi"});
        let request = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };

        // 150ms deadline, server answers in 30s.
        std::env::set_var("FETCH_CONNECT_TIMEOUT_MS", "150");
        let result = handle_embeddings(&Client::new(), &base::OPENAI_COMPAT_NODE, request).await;
        std::env::remove_var("FETCH_CONNECT_TIMEOUT_MS");
        server.abort();

        match result {
            Err(EmbeddingsHandlerError::Upstream(message)) => {
                assert!(
                    message.contains("timeout"),
                    "expected a timeout, got: {message}"
                );
                assert_eq!(
                    EmbeddingsHandlerError::Upstream(message).status(),
                    502,
                    "an aborted fetch is 9router's BAD_GATEWAY arm"
                );
            }
            other => panic!("expected an upstream timeout, got {other:?}"),
        }
    }

    /// An API-key provider has no refresh token, so a 401 must cost exactly ONE
    /// upstream call — 9router only re-fires when the refresh produced new
    /// material (embeddingsCore.js:88).
    #[tokio::test]
    async fn a_401_without_a_refresh_token_costs_one_upstream_call() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_hits = hits.clone();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            for _ in 0..4 {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                server_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let body = r#"{"error":{"message":"Incorrect API key provided"}}"#;
                let response = format!(
                    "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let mut creds = ProviderConnection::default();
        creds.api_key = Some("sk-bad".into());
        creds
            .provider_specific_data
            .insert("baseUrl".into(), json!(format!("http://{addr}/v1")));
        let body = json!({"input": "hi"});
        let request = EmbeddingRequest {
            body: &body,
            model: "x",
            credentials: &creds,
        };

        let result = handle_embeddings(&Client::new(), &base::OPENAI_COMPAT_NODE, request).await;
        server.abort();

        match result {
            Err(EmbeddingsHandlerError::Http(401, message)) => {
                assert_eq!(message, "Incorrect API key provided", "message: {message}");
            }
            other => panic!("expected the upstream 401 verbatim, got {other:?}"),
        }
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a 401 with no refresh token must not be re-sent upstream"
        );
    }
}
