use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::Value;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

/// Default Azure OpenAI endpoint when neither provider-specific data nor
/// `AZURE_ENDPOINT` is configured.
const DEFAULT_AZURE_ENDPOINT: &str = "https://api.openai.com";
const DEFAULT_API_VERSION: &str = "2024-10-01-preview";
const DEFAULT_DEPLOYMENT: &str = "gpt-4";

#[derive(Clone)]
pub struct AzureExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum AzureExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for AzureExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for AzureExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for AzureExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for AzureExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for AzureExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct AzureExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct AzureExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl AzureExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, AzureExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(&self, credentials: &ProviderConnection, model: &str) -> String {
        // 9router precedence: provider-specific data → env → default.
        let endpoint = credentials
            .provider_specific_data
            .get("azureEndpoint")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                std::env::var("AZURE_ENDPOINT")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| DEFAULT_AZURE_ENDPOINT.to_string());

        let api_version = credentials
            .provider_specific_data
            .get("apiVersion")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                std::env::var("AZURE_API_VERSION")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());

        // 9router precedence: psd deployment → model → env → "gpt-4".
        let deployment = credentials
            .provider_specific_data
            .get("deployment")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                if model.is_empty() {
                    None
                } else {
                    Some(model.to_string())
                }
            })
            .or_else(|| {
                std::env::var("AZURE_DEPLOYMENT")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| DEFAULT_DEPLOYMENT.to_string());

        let endpoint = endpoint.trim_end_matches('/');
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            endpoint, deployment, api_version
        )
    }

    fn build_headers(&self, credentials: &ProviderConnection, stream: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        // 9router precedence: psd apiKey → accessToken → env OPENAI_API_KEY.
        let env_api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        let api_key = credentials
            .api_key
            .as_deref()
            .or(credentials.access_token.as_deref())
            .or(env_api_key.as_deref());

        if let Some(key) = api_key {
            if let Ok(header_val) = HeaderValue::from_str(key) {
                headers.insert("api-key", header_val);
            }
        }

        // 9router precedence: psd organization → env AZURE_ORGANIZATION.
        let env_org = std::env::var("AZURE_ORGANIZATION")
            .ok()
            .filter(|s| !s.is_empty());
        let org = credentials
            .provider_specific_data
            .get("organization")
            .and_then(|v| v.as_str())
            .or(env_org.as_deref());
        if let Some(org) = org {
            if let Ok(header_val) = HeaderValue::from_str(org) {
                headers.insert("openai-organization", header_val);
            }
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        headers
    }

    pub async fn execute_request(
        &self,
        request: AzureExecutionRequest,
    ) -> Result<AzureExecutorResponse, AzureExecutorError> {
        let url = self.build_url(&request.credentials, &request.model);
        let headers = self.build_headers(&request.credentials, request.stream);
        let transformed_body = request.body.clone();

        let client = self.pool.get("azure", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(AzureExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that set/remove env vars — cargo runs tests in
    /// parallel threads, and env vars are process-global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    const AZURE_URL_ENVS: [&str; 3] = ["AZURE_ENDPOINT", "AZURE_API_VERSION", "AZURE_DEPLOYMENT"];

    fn clear_azure_url_envs() {
        for name in AZURE_URL_ENVS {
            unsafe {
                std::env::remove_var(name);
            }
        }
    }

    fn executor() -> AzureExecutor {
        AzureExecutor::new(Arc::new(ClientPool::new()), None).unwrap()
    }

    fn creds_with_azure_endpoint(endpoint: &str) -> ProviderConnection {
        let mut creds = ProviderConnection::default();
        creds.provider_specific_data.insert(
            "azureEndpoint".to_string(),
            Value::String(endpoint.to_string()),
        );
        creds
    }

    #[test]
    fn test_build_url_prefers_psd_over_env() {
        let _guard = env_guard();
        clear_azure_url_envs();
        std::env::set_var("AZURE_ENDPOINT", "https://env.openai.azure.com");

        // psd wins over env.
        let url = executor().build_url(
            &creds_with_azure_endpoint("https://mine.openai.azure.com"),
            "gpt-4o",
        );
        assert!(
            url.starts_with("https://mine.openai.azure.com"),
            "psd endpoint should win over env, got: {url}"
        );

        // Without psd, env wins.
        let url = executor().build_url(&ProviderConnection::default(), "gpt-4o");
        assert!(
            url.starts_with("https://env.openai.azure.com"),
            "env endpoint should be used without psd, got: {url}"
        );

        // Without both, the JS default applies.
        unsafe {
            std::env::remove_var("AZURE_ENDPOINT");
        }
        let url = executor().build_url(&ProviderConnection::default(), "gpt-4o");
        assert!(
            url.starts_with("https://api.openai.com"),
            "default endpoint should apply without psd/env, got: {url}"
        );
    }

    #[test]
    fn test_build_url_deployment_precedence() {
        let _guard = env_guard();
        clear_azure_url_envs();

        // psd deployment wins over model.
        let mut creds = ProviderConnection::default();
        creds.provider_specific_data.insert(
            "deployment".to_string(),
            Value::String("my-deploy".to_string()),
        );
        let url = executor().build_url(&creds, "gpt-4o");
        assert!(url.contains("/deployments/my-deploy/"), "got: {url}");

        // Model is used when no psd deployment — JS: psd → model → env → default,
        // so a non-empty model always beats the env var.
        let url = executor().build_url(&ProviderConnection::default(), "gpt-4o");
        assert!(url.contains("/deployments/gpt-4o/"), "got: {url}");
        std::env::set_var("AZURE_DEPLOYMENT", "env-deploy");
        let url = executor().build_url(&ProviderConnection::default(), "gpt-4o");
        assert!(url.contains("/deployments/gpt-4o/"), "got: {url}");

        // Empty model + no psd + env set → env wins.
        let url = executor().build_url(&ProviderConnection::default(), "");
        assert!(url.contains("/deployments/env-deploy/"), "got: {url}");

        // Empty model + no psd + no env → default "gpt-4".
        unsafe {
            std::env::remove_var("AZURE_DEPLOYMENT");
        }
        let url = executor().build_url(&ProviderConnection::default(), "");
        assert!(url.contains("/deployments/gpt-4/"), "got: {url}");
    }

    #[test]
    fn test_build_url_trims_trailing_slash_and_defaults() {
        let _guard = env_guard();
        clear_azure_url_envs();
        let url = executor().build_url(
            &creds_with_azure_endpoint("https://mine.openai.azure.com/"),
            "gpt-4o",
        );
        assert_eq!(
            url,
            "https://mine.openai.azure.com/openai/deployments/gpt-4o/chat/completions?api-version=2024-10-01-preview"
        );
    }

    #[test]
    fn test_build_headers_env_fallbacks() {
        let _guard = env_guard();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("AZURE_ORGANIZATION");
        }

        // No creds and no env → no api-key / organization headers.
        let headers = executor().build_headers(&ProviderConnection::default(), true);
        assert!(headers.get("api-key").is_none());
        assert!(headers.get("openai-organization").is_none());
        assert_eq!(
            headers.get(ACCEPT).and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );

        // OPENAI_API_KEY env fallback.
        std::env::set_var("OPENAI_API_KEY", "env-key");
        let headers = executor().build_headers(&ProviderConnection::default(), false);
        assert_eq!(
            headers.get("api-key").and_then(|v| v.to_str().ok()),
            Some("env-key")
        );
        assert_eq!(
            headers.get(ACCEPT).and_then(|v| v.to_str().ok()),
            Some("application/json")
        );

        // Credential apiKey beats env.
        let mut creds = ProviderConnection::default();
        creds.api_key = Some("psd-key".to_string());
        let headers = executor().build_headers(&creds, false);
        assert_eq!(
            headers.get("api-key").and_then(|v| v.to_str().ok()),
            Some("psd-key")
        );

        // AZURE_ORGANIZATION env fallback.
        std::env::set_var("AZURE_ORGANIZATION", "env-org");
        let headers = executor().build_headers(&ProviderConnection::default(), false);
        assert_eq!(
            headers
                .get("openai-organization")
                .and_then(|v| v.to_str().ok()),
            Some("env-org")
        );

        // psd organization beats env.
        let mut creds = ProviderConnection::default();
        creds.provider_specific_data.insert(
            "organization".to_string(),
            Value::String("psd-org".to_string()),
        );
        let headers = executor().build_headers(&creds, false);
        assert_eq!(
            headers
                .get("openai-organization")
                .and_then(|v| v.to_str().ok()),
            Some("psd-org")
        );

        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("AZURE_ORGANIZATION");
        }
    }
}
