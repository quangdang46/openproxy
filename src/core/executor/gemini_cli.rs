use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::config::app_constants::{
    gemini_cli_client_metadata, gemini_cli_user_agent, GEMINI_CLI_API_CLIENT,
    INTERNAL_REQUEST_HEADER_NAME, INTERNAL_REQUEST_HEADER_VALUE,
};
use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::project_id_cache::lookup_project_id;
use super::{ClientPool, TransportKind, UpstreamResponse};

const GEMINI_CLI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[derive(Clone)]
pub struct GeminiCliExecutor {
    pool: Arc<ClientPool>,
    #[allow(dead_code)]
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum GeminiCliExecutorError {
    MissingCredentials(String),
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for GeminiCliExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for GeminiCliExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for GeminiCliExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for GeminiCliExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for GeminiCliExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct GeminiCliExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct GeminiCliExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct GeminiCliTokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

impl GeminiCliExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, GeminiCliExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// Build the standard Gemini CLI API URL (no API key in query string).
    fn build_url(&self, model: &str, stream: bool) -> String {
        let action = if stream {
            "streamGenerateContent?alt=sse"
        } else {
            "generateContent"
        };
        format!("{}/{}:{}", GEMINI_CLI_BASE_URL, model, action)
    }

    /// Build the Gemini CLI API URL with the API key appended as a query
    /// parameter (`?key=...` for unary, `&key=...` for SSE).
    fn build_url_with_api_key(&self, model: &str, stream: bool, api_key: &str) -> String {
        let base = self.build_url(model, stream);
        if stream {
            format!("{}&key={}", base, api_key)
        } else {
            format!("{}?key={}", base, api_key)
        }
    }

    /// Detect whether this connection uses OAuth (Bearer) or API-key auth.
    fn is_oauth(credentials: &ProviderConnection) -> bool {
        credentials
            .access_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_some()
    }

    /// Build headers for OAuth (Bearer) mode, as emitted by the real
    /// Gemini CLI SDK.
    ///
    /// | Header             | Value                                              |
    /// |--------------------|----------------------------------------------------|
    /// | Authorization      | Bearer {access_token}                              |
    /// | User-Agent         | gemini-cli/0.34.0/{model} ({os}; {arch}; terminal) |
    /// | X-Goog-Api-Client  | google-genai-sdk/1.41.0 gl-node/v22.19.0           |
    /// | Client-Metadata    | {"ideType":9,"platform":<enum>,"pluginType":2}     |
    fn build_gemini_cli_headers(
        access_token: &str,
        stream: bool,
        model: &str,
    ) -> Result<HeaderMap, GeminiCliExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let auth = format!("Bearer {}", access_token);
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&auth)?);

        let ua = gemini_cli_user_agent(model);
        headers.insert(reqwest::header::USER_AGENT, HeaderValue::from_str(&ua)?);

        headers.insert(
            "X-Goog-Api-Client",
            HeaderValue::from_static(GEMINI_CLI_API_CLIENT),
        );

        // Client-Metadata: serialised JSON object.
        let metadata = gemini_cli_client_metadata();
        let metadata_str = serde_json::to_string(&metadata).unwrap_or_default();
        if !metadata_str.is_empty() {
            headers.insert("Client-Metadata", HeaderValue::from_str(&metadata_str)?);
        }

        headers.insert(
            INTERNAL_REQUEST_HEADER_NAME,
            HeaderValue::from_static(INTERNAL_REQUEST_HEADER_VALUE),
        );

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        Ok(headers)
    }

    /// Legacy header builder used for API-key-only connections.
    /// Kept for backward compatibility with existing callers.
    fn build_headers(
        &self,
        credentials: &ProviderConnection,
        stream: bool,
        model: &str,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let Some(token) = credentials.access_token.as_deref() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", token))
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }

        let ua = gemini_cli_user_agent(model);
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_str(&ua).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(
            "X-Goog-Api-Client",
            HeaderValue::from_static(GEMINI_CLI_API_CLIENT),
        );
        headers.insert(
            INTERNAL_REQUEST_HEADER_NAME,
            HeaderValue::from_static(INTERNAL_REQUEST_HEADER_VALUE),
        );

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        headers
    }

    /// Transform the request body:
    /// - OAuth mode: inject `project` from the ProjectIdCache.
    /// - API-key mode (existing behaviour): inject `project` from
    ///   `provider_specific_data["projectId"]` if present.
    fn transform_request(&self, body: &Value, credentials: &ProviderConnection) -> Value {
        let is_oauth = Self::is_oauth(credentials);
        self.transform_request_inner(body, credentials, is_oauth)
    }

    fn transform_request_inner(
        &self,
        body: &Value,
        credentials: &ProviderConnection,
        is_oauth: bool,
    ) -> Value {
        let mut transformed = body.clone();

        if transformed.get("project").is_some() {
            return transformed;
        }

        if is_oauth {
            // OAuth mode: resolve projectId via the cache (which checks
            // provider_specific_data, ProviderConnection.project_id, etc.)
            if let Some(project_id) = lookup_project_id(credentials) {
                transformed["project"] = Value::String(project_id);
            }
        } else {
            // API-key mode: original behaviour — read from provider_specific_data.
            if let Some(project_id) = credentials.provider_specific_data.get("projectId") {
                transformed["project"] = project_id.clone();
            }
        }

        transformed
    }

    pub async fn execute_request(
        &self,
        request: GeminiCliExecutionRequest,
    ) -> Result<GeminiCliExecutorResponse, GeminiCliExecutorError> {
        let is_oauth = Self::is_oauth(&request.credentials);

        // Pick URL and headers according to auth mode.
        let (url, headers) = if is_oauth {
            let access_token = request.credentials.access_token.as_deref().unwrap_or("");
            let hdrs =
                Self::build_gemini_cli_headers(access_token, request.stream, &request.model)?;
            let url = self.build_url(&request.model, request.stream);
            (url, hdrs)
        } else {
            let api_key = request.credentials.api_key.as_deref().ok_or_else(|| {
                GeminiCliExecutorError::MissingCredentials(
                    "neither access_token nor api_key present".to_string(),
                )
            })?;
            let url = self.build_url_with_api_key(&request.model, request.stream, api_key);
            let hdrs = self.build_headers(&request.credentials, request.stream, &request.model);
            (url, hdrs)
        };

        let transformed_body = self.transform_request(&request.body, &request.credentials);

        let client = self.pool.get("gemini-cli", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(GeminiCliExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    pub async fn refresh_token(
        &self,
        refresh_token: &str,
        client_id: &str,
        client_secret: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Option<GeminiCliTokenResponse> {
        let client = reqwest::Client::builder().build().ok()?;

        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ];

        let response = client
            .post(GOOGLE_TOKEN_URL)
            .form(&params)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;

        if !response.status().is_success() {
            return None;
        }

        response.json::<GeminiCliTokenResponse>().await.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_url_picks_stream_or_unary_path() {
        let executor = GeminiCliExecutor {
            pool: Arc::new(ClientPool::new()),
            provider_node: None,
        };
        assert!(executor
            .build_url("gemini-2.0-flash", true)
            .contains("streamGenerateContent?alt=sse"));
        assert!(executor
            .build_url("gemini-2.0-flash", false)
            .contains("generateContent"));
    }

    #[test]
    fn build_url_with_api_key_appends_correctly() {
        let executor = GeminiCliExecutor {
            pool: Arc::new(ClientPool::new()),
            provider_node: None,
        };
        let stream_url = executor.build_url_with_api_key("gemini-2.0-flash", true, "sk-test");
        assert!(stream_url.ends_with("&key=sk-test"));

        let unary_url = executor.build_url_with_api_key("gemini-2.0-flash", false, "sk-test");
        assert!(unary_url.ends_with("?key=sk-test"));
    }

    #[test]
    fn is_oauth_detects_access_token() {
        let mut creds = ProviderConnection::default();
        assert!(!GeminiCliExecutor::is_oauth(&creds));

        creds.access_token = Some("".to_string());
        assert!(!GeminiCliExecutor::is_oauth(&creds));

        creds.access_token = Some("tok-valid".to_string());
        assert!(GeminiCliExecutor::is_oauth(&creds));
    }

    #[test]
    fn build_gemini_cli_headers_includes_all_expected_headers() {
        let hdrs =
            GeminiCliExecutor::build_gemini_cli_headers("tok-test", false, "gemini-2.0-flash")
                .unwrap();

        assert_eq!(
            hdrs.get("Authorization").unwrap().to_str().unwrap(),
            "Bearer tok-test"
        );
        assert!(hdrs.get("user-agent").is_some());
        assert!(hdrs
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("gemini-cli/0.34.0"));
        assert!(hdrs
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("terminal"));
        assert_eq!(
            hdrs.get("x-goog-api-client").unwrap().to_str().unwrap(),
            GEMINI_CLI_API_CLIENT
        );
        assert!(hdrs.get("client-metadata").is_some());
        let cm = hdrs.get("client-metadata").unwrap().to_str().unwrap();
        assert!(cm.contains(r#""ideType":9"#));
        assert!(cm.contains(r#""pluginType":2"#));
        assert_eq!(
            hdrs.get("content-type").unwrap().to_str().unwrap(),
            "application/json"
        );
        assert_eq!(
            hdrs.get("accept").unwrap().to_str().unwrap(),
            "application/json"
        );
        assert_eq!(
            hdrs.get("x-request-source").unwrap().to_str().unwrap(),
            "local"
        );
    }

    #[test]
    fn build_gemini_cli_headers_sets_stream_accept_for_stream() {
        let hdrs =
            GeminiCliExecutor::build_gemini_cli_headers("tok-test", true, "gemini-2.0-flash")
                .unwrap();
        assert_eq!(
            hdrs.get("accept").unwrap().to_str().unwrap(),
            "text/event-stream"
        );
    }

    #[test]
    fn transform_request_injects_project_id_from_cache_for_oauth() {
        let executor = GeminiCliExecutor {
            pool: Arc::new(ClientPool::new()),
            provider_node: None,
        };
        let mut creds = ProviderConnection::default();
        creds.access_token = Some("tok-transform".to_string());
        creds.project_id = Some("proj-from-cache".to_string());

        let body = json!({"contents": [{"parts": [{"text": "hello"}]}]});
        let transformed = executor.transform_request_inner(&body, &creds, true);
        assert_eq!(transformed["project"], "proj-from-cache");
    }

    #[test]
    fn transform_request_does_not_overwrite_existing_project() {
        let executor = GeminiCliExecutor {
            pool: Arc::new(ClientPool::new()),
            provider_node: None,
        };
        let mut creds = ProviderConnection::default();
        creds.access_token = Some("tok-exist".to_string());
        creds.project_id = Some("proj-from-cache".to_string());

        let body = json!({"project": "existing-proj", "contents": []});
        let transformed = executor.transform_request_inner(&body, &creds, true);
        assert_eq!(transformed["project"], "existing-proj");
    }

    #[test]
    fn transform_request_uses_api_key_mode_when_no_access_token() {
        let executor = GeminiCliExecutor {
            pool: Arc::new(ClientPool::new()),
            provider_node: None,
        };
        let mut creds = ProviderConnection::default();
        // No access_token -- API key mode.
        let mut data = std::collections::BTreeMap::new();
        data.insert(
            "projectId".to_string(),
            serde_json::Value::String("proj-psd".to_string()),
        );
        creds.provider_specific_data = data;

        let body = json!({"contents": [{"parts": [{"text": "hi"}]}]});
        // false = API key mode
        let transformed = executor.transform_request_inner(&body, &creds, false);
        assert_eq!(transformed["project"], "proj-psd");
    }

    #[test]
    fn transform_request_skips_project_when_cache_empty_for_oauth() {
        let executor = GeminiCliExecutor {
            pool: Arc::new(ClientPool::new()),
            provider_node: None,
        };
        let creds = ProviderConnection::default(); // no access token, no project_id
        let body = json!({"contents": [{"parts": [{"text": "hi"}]}]});
        let transformed = executor.transform_request_inner(&body, &creds, true);
        assert!(transformed.get("project").is_none());
    }
}
