use std::path::PathBuf;
use std::sync::Arc;

use hyper::http::{self as hyper_http, uri::InvalidUri};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::Duration;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const VERTEX_AI_BASE_URL: &str = "https://aiplatform.googleapis.com/v2beta";
const VERTEX_DEFAULT_LOCATION: &str = "us-central1";

/// Environment variable pointing to the GCP ADC credential file.
const ADC_CREDENTIALS_ENV: &str = "GOOGLE_APPLICATION_CREDENTIALS";
/// Default path for `gcloud auth application-default login` credentials.
const ADC_DEFAULT_PATH: &str = ".config/gcloud/application_default_credentials.json";

#[derive(Clone)]
#[allow(dead_code)]
pub struct VertexExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum VertexExecutorError {
    UnsupportedProvider(String),
    MissingCredentials(String),
    MissingServiceAccountJson(String),
    JwtGenerationFailed(String),
    InvalidToken(String),
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidUri(InvalidUri),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    RsaPemParse(String),
    RsaSigning(String),
    Base64Decode(base64::DecodeError),
    StreamingResponseFailed(String),
    JsonWebToken(jsonwebtoken::errors::Error),
}

impl From<reqwest::Error> for VertexExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for VertexExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for VertexExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper_http::Error> for VertexExecutorError {
    fn from(_error: hyper_http::Error) -> Self {
        Self::RequestFailed("HTTP error".to_string())
    }
}

impl From<serde_json::Error> for VertexExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for VertexExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for VertexExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<base64::DecodeError> for VertexExecutorError {
    fn from(error: base64::DecodeError) -> Self {
        Self::Base64Decode(error)
    }
}

impl From<jsonwebtoken::errors::Error> for VertexExecutorError {
    fn from(error: jsonwebtoken::errors::Error) -> Self {
        Self::JsonWebToken(error)
    }
}

pub struct VertexExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct VertexExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Display for VertexExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProvider(msg) => write!(f, "unsupported provider: {msg}"),
            Self::MissingCredentials(msg) => write!(f, "missing credentials: {msg}"),
            Self::MissingServiceAccountJson(msg) => {
                write!(f, "missing service account JSON: {msg}")
            }
            Self::JwtGenerationFailed(msg) => write!(f, "JWT generation failed: {msg}"),
            Self::InvalidToken(msg) => write!(f, "invalid token: {msg}"),
            Self::RequestFailed(msg) => write!(f, "request failed: {msg}"),
            Self::Serialize(e) => write!(f, "serialization error: {e}"),
            Self::HyperClientInit(e) => write!(f, "hyper client init error: {e}"),
            Self::Hyper(e) => write!(f, "hyper error: {e}"),
            Self::Request(e) => write!(f, "request error: {e}"),
            Self::InvalidUri(e) => write!(f, "invalid URI: {e}"),
            Self::InvalidHeader(e) => write!(f, "invalid header: {e}"),
            Self::RsaPemParse(msg) => write!(f, "RSA PEM parse error: {msg}"),
            Self::RsaSigning(msg) => write!(f, "RSA signing error: {msg}"),
            Self::Base64Decode(e) => write!(f, "base64 decode error: {e}"),
            Self::StreamingResponseFailed(msg) => write!(f, "streaming response failed: {msg}"),
            Self::JsonWebToken(e) => write!(f, "JWT error: {e}"),
        }
    }
}

impl std::error::Error for VertexExecutorError {}

impl std::fmt::Debug for VertexExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ServiceAccountJson {
    #[serde(rename = "type")]
    account_type: String,
    #[serde(rename = "client_email")]
    client_email: String,
    #[serde(rename = "private_key")]
    private_key: String,
    #[serde(rename = "token_uri")]
    token_uri: String,
    #[serde(rename = "project_id")]
    project_id: Option<String>,
}

/// Represents `gcloud auth application-default login` credentials
/// (type = "authorized_user").
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct AuthorizedUserCredential {
    #[serde(rename = "type")]
    credential_type: String,
    #[serde(rename = "client_id")]
    client_id: String,
    #[serde(rename = "client_secret")]
    client_secret: String,
    #[serde(rename = "refresh_token")]
    refresh_token: String,
    /// Optional; the quota project.
    #[serde(rename = "quota_project_id")]
    quota_project_id: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CachedAccessToken {
    token: String,
    expires_at: time::OffsetDateTime,
}

#[derive(Debug, Serialize)]
struct JwtClaims {
    iss: String,
    scope: String,
    aud: String,
    iat: i64,
    exp: i64,
}

impl VertexExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, VertexExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn parse_service_account_json(
        json_str: &str,
    ) -> Result<ServiceAccountJson, VertexExecutorError> {
        let parsed: ServiceAccountJson = serde_json::from_str(json_str).map_err(|e| {
            VertexExecutorError::MissingServiceAccountJson(format!(
                "Failed to parse service account JSON: {}",
                e
            ))
        })?;

        if parsed.account_type != "service_account" {
            return Err(VertexExecutorError::MissingServiceAccountJson(format!(
                "Expected type 'service_account', got '{}'",
                parsed.account_type
            )));
        }

        if parsed.client_email.is_empty() {
            return Err(VertexExecutorError::MissingServiceAccountJson(
                "client_email is required".to_string(),
            ));
        }

        if parsed.private_key.is_empty() {
            return Err(VertexExecutorError::MissingServiceAccountJson(
                "private_key is required".to_string(),
            ));
        }

        if parsed.token_uri.is_empty() {
            return Err(VertexExecutorError::MissingServiceAccountJson(
                "token_uri is required".to_string(),
            ));
        }

        Ok(parsed)
    }

    fn create_rs256_jwt(
        service_account: &ServiceAccountJson,
    ) -> Result<String, VertexExecutorError> {
        let private_key_pem = service_account.private_key.replace("\\n", "\n");

        let now = time::OffsetDateTime::now_utc();
        let iat = now.unix_timestamp();
        let exp = iat + 3600;

        let claims = JwtClaims {
            iss: service_account.client_email.clone(),
            scope: "https://www.googleapis.com/auth/cloud-platform".to_string(),
            aud: service_account.token_uri.clone(),
            iat,
            exp,
        };

        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        let encoding_key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
            .map_err(|e| VertexExecutorError::RsaPemParse(e.to_string()))?;

        jsonwebtoken::encode(&header, &claims, &encoding_key)
            .map_err(|e| VertexExecutorError::JwtGenerationFailed(e.to_string()))
    }

    async fn exchange_jwt_for_token(
        jwt: &str,
        token_uri: &str,
    ) -> Result<CachedAccessToken, VertexExecutorError> {
        let client = reqwest::Client::new();
        let params = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", jwt),
        ];

        let response = client
            .post(token_uri)
            .form(&params)
            .send()
            .await
            .map_err(|e| {
                VertexExecutorError::RequestFailed(format!("Token exchange request failed: {}", e))
            })?;

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: u64,
        }

        let token_resp: TokenResponse = response.json().await.map_err(|e| {
            VertexExecutorError::InvalidToken(format!("Failed to parse token response: {}", e))
        })?;

        let expires_at =
            time::OffsetDateTime::now_utc() + Duration::seconds(token_resp.expires_in as i64);

        Ok(CachedAccessToken {
            token: token_resp.access_token,
            expires_at,
        })
    }

    fn parse_vertex_model(model: &str) -> (String, String, String, bool) {
        let (model_stripped, is_partner) = if model.starts_with("vertex-partner/") {
            (model.strip_prefix("vertex-partner/").unwrap_or(model), true)
        } else if model.starts_with("vertex/") {
            (model.strip_prefix("vertex/").unwrap_or(model), false)
        } else {
            (model, false)
        };

        let actual_model = if is_partner {
            model_stripped.to_string()
        } else {
            format!("models/{}", model_stripped)
        };

        (
            VERTEX_DEFAULT_LOCATION.to_string(),
            "".to_string(),
            actual_model,
            is_partner,
        )
    }

    fn build_vertex_request_body(
        body: &Value,
        model: &str,
        _stream: bool,
    ) -> Result<Value, VertexExecutorError> {
        let (_, _, actual_model, _) = Self::parse_vertex_model(model);

        let contents = body
            .get("contents")
            .or_else(|| body.get("messages"))
            .cloned()
            .unwrap_or(Value::Null);

        let mut generation_config = Value::Null;
        if let Some(temp) = body.get("temperature") {
            let max_tokens = body
                .get("maxOutputTokens")
                .or_else(|| body.get("max_tokens"));
            let top_p = body.get("topP").or_else(|| body.get("top_p"));

            generation_config = json!({
                "temperature": temp,
                "maxOutputTokens": max_tokens.unwrap_or(&json!(8192)),
                "topP": top_p.unwrap_or(&json!(0.9)),
            });
        }

        let system_instruction = body.get("systemInstruction").cloned();

        let mut request_body = json!({
            "model": actual_model,
            "contents": contents,
        });

        if generation_config != Value::Null {
            request_body["generationConfig"] = generation_config;
        }

        if let Some(system) = system_instruction {
            request_body["systemInstruction"] = system;
        }

        Ok(request_body)
    }

    fn build_vertex_url(
        model: &str,
        project_id: &str,
        location: &str,
        is_partner: bool,
        stream: bool,
    ) -> String {
        let model_stripped = model.strip_prefix("vertex/").unwrap_or(model);

        // 9router VertexExecutor v1 URL pattern:
        //   https://LOCATION-aiplatform.googleapis.com/v1/projects/PROJECT/locations/LOCATION/publishers/google/MODEL:streamGenerateContent
        //   ?alt=sse
        let base = format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}",
            location,
            if project_id.is_empty() {
                "unknown"
            } else {
                project_id
            },
            location,
        );

        let url = if is_partner {
            format!("{base}/publishers/{model_stripped}:streamGenerateContent",)
        } else {
            format!("{base}/publishers/google/{model_stripped}:streamGenerateContent",)
        };

        if stream {
            format!("{}?alt=sse", url)
        } else {
            url
        }
    }

    pub async fn execute_request(
        &self,
        request: VertexExecutionRequest,
    ) -> Result<VertexExecutorResponse, VertexExecutorError> {
        // Extract project_id and location from model or credentials
        let (location, _, _, is_partner) = Self::parse_vertex_model(&request.model);

        // Determine auth path:
        // 1. Raw API key (api_key field) -> ADC or x-goog-api-key
        // 2. Service account JSON (access_token field) -> JWT exchange
        // 3. No credentials -> try ADC (file-based then metadata server)
        let (project_id, auth_token) = if let Some(api_key) = &request.credentials.api_key {
            // Raw API key auth path
            let project_id = request
                .credentials
                .provider_specific_data
                .get("projectId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (project_id, api_key.to_string())
        } else if let Some(credentials_json) = &request.credentials.access_token {
            let service_account = Self::parse_service_account_json(credentials_json)?;
            let jwt = Self::create_rs256_jwt(&service_account)?;
            let cached_token =
                Self::exchange_jwt_for_token(&jwt, &service_account.token_uri).await?;
            let project_id = service_account.project_id.clone().unwrap_or_default();
            (project_id, cached_token.token)
        } else {
            // Try ADC (Application Default Credentials):
            // 1. GOOGLE_APPLICATION_CREDENTIALS env var (authorized_user or service_account)
            // 2. ~/.config/gcloud/application_default_credentials.json
            // 3. GCP metadata server (Compute Engine, Cloud Run, etc.)
            let (project_id, token) = Self::fetch_adc_token().await?;
            (project_id, token)
        };

        let url = Self::build_vertex_url(
            &request.model,
            &project_id,
            &location,
            is_partner,
            request.stream,
        );

        let transformed_body =
            Self::build_vertex_request_body(&request.body, &request.model, request.stream)?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        // Use x-goog-api-key for API key auth, Bearer token for OAuth/ADC
        if request.credentials.api_key.is_some() {
            headers.insert(
                HeaderName::from_bytes(b"x-goog-api-key").unwrap(),
                HeaderValue::from_str(&auth_token).map_err(VertexExecutorError::InvalidHeader)?,
            );
        } else {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", auth_token))
                    .map_err(VertexExecutorError::InvalidHeader)?,
            );
        }

        let client = self.pool.get("vertex", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        Ok(VertexExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body,
            transport: TransportKind::Reqwest,
        })
    }

    /// Resolve the default ADC credential path (Unix/macOS).
    fn adc_credential_path() -> Option<PathBuf> {
        // 1. Check GOOGLE_APPLICATION_CREDENTIALS env var.
        if let Ok(path) = std::env::var(ADC_CREDENTIALS_ENV) {
            let p = PathBuf::from(path);
            if p.exists() {
                return Some(p);
            }
        }
        // 2. Fall back to ~/.config/gcloud/application_default_credentials.json.
        if let Some(home) = Self::home_dir() {
            let p = home.join(ADC_DEFAULT_PATH);
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    /// Get the user's home directory.
    fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }

    /// Read an ADC credential file and resolve to a (project_id, access_token) pair.
    ///
    /// Supports:
    /// - `type: "authorized_user"` — uses `refresh_token` against Google's OAuth
    ///   token endpoint to mint a new access token.
    /// - `type: "service_account"` — uses the existing service account JWT flow.
    async fn resolve_adc_file(path: &PathBuf) -> Result<(String, String), VertexExecutorError> {
        let contents = tokio::fs::read_to_string(path).await.map_err(|e| {
            VertexExecutorError::MissingCredentials(format!(
                "Failed to read ADC credential file '{}': {}",
                path.display(),
                e
            ))
        })?;

        let raw: Value = serde_json::from_str(&contents).map_err(|e| {
            VertexExecutorError::MissingCredentials(format!(
                "Failed to parse ADC credential file: {}",
                e
            ))
        })?;

        let cred_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match cred_type {
            "authorized_user" => {
                let cred: AuthorizedUserCredential = serde_json::from_value(raw).map_err(|e| {
                    VertexExecutorError::MissingCredentials(format!(
                        "Invalid authorized_user credential: {}",
                        e
                    ))
                })?;
                Self::exchange_authorized_user_refresh(&cred).await
            }
            "service_account" => {
                let service_account = Self::parse_service_account_json(&contents)?;
                let jwt = Self::create_rs256_jwt(&service_account)?;
                let cached_token =
                    Self::exchange_jwt_for_token(&jwt, &service_account.token_uri).await?;
                let project_id = service_account.project_id.clone().unwrap_or_default();
                Ok((project_id, cached_token.token))
            }
            other => Err(VertexExecutorError::MissingCredentials(format!(
                "Unsupported ADC credential type: '{}'",
                other
            ))),
        }
    }

    /// Exchange an authorized_user refresh token for an access token.
    async fn exchange_authorized_user_refresh(
        cred: &AuthorizedUserCredential,
    ) -> Result<(String, String), VertexExecutorError> {
        let client = reqwest::Client::new();
        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", &cred.refresh_token),
            ("client_id", &cred.client_id),
            ("client_secret", &cred.client_secret),
        ];

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: Option<u64>,
            #[serde(default)]
            scope: Option<String>,
        }

        let response = client
            .post("https://oauth2.googleapis.com/token")
            .form(&params)
            .send()
            .await
            .map_err(|e| {
                VertexExecutorError::MissingCredentials(format!(
                    "Authorized user token refresh failed: {}",
                    e
                ))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(VertexExecutorError::InvalidToken(format!(
                "Authorized user token refresh returned {}: {}",
                status, body
            )));
        }

        let token_resp: TokenResponse = response.json().await.map_err(|e| {
            VertexExecutorError::InvalidToken(format!(
                "Failed to parse authorized user token response: {}",
                e
            ))
        })?;

        let project_id = cred.quota_project_id.clone().unwrap_or_default();
        Ok((project_id, token_resp.access_token))
    }

    /// Fetch credentials from ADC sources in priority order:
    /// 1. ADC credential file (authorized_user or service_account)
    /// 2. GCP metadata server (Compute Engine, Cloud Run, etc.)
    async fn fetch_adc_token() -> Result<(String, String), VertexExecutorError> {
        // Try file-based ADC first.
        if let Some(path) = Self::adc_credential_path() {
            match Self::resolve_adc_file(&path).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    tracing::warn!(
                        "ADC credential file '{}' failed: {}; falling back to metadata server",
                        path.display(),
                        e
                    );
                }
            }
        }

        // Fall back to GCP metadata server.
        let client = reqwest::Client::new();
        let resp = client
            .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/identity?audience=https://aiplatform.googleapis.com&format=full")
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .map_err(|e| {
                VertexExecutorError::MissingCredentials(format!(
                    "ADC metadata server token fetch failed: {}",
                    e
                ))
            })?;
        let token = resp.text().await.map_err(|e| {
            VertexExecutorError::InvalidToken(format!(
                "ADC metadata server token parse failed: {}",
                e
            ))
        })?;
        // Metadata server does not return a project id via this path.
        Ok((String::new(), token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_vertex_model_standard() {
        let (location, project_id, actual_model, is_partner) =
            VertexExecutor::parse_vertex_model("vertex/gemini-2.5-flash");
        assert_eq!(location, "us-central1");
        assert_eq!(project_id, "");
        assert_eq!(actual_model, "models/gemini-2.5-flash");
        assert!(!is_partner);
    }

    #[test]
    fn test_parse_vertex_model_partner() {
        let (location, project_id, actual_model, is_partner) =
            VertexExecutor::parse_vertex_model("vertex-partner/glm-5-maas");
        assert_eq!(location, "us-central1");
        assert_eq!(project_id, "");
        assert_eq!(actual_model, "glm-5-maas");
        assert!(is_partner);
    }

    #[test]
    fn test_parse_service_account_json_missing_type() {
        let json = r#"{"client_email":"test@test.com","private_key":"key","token_uri":"https://oauth2.googleapis.com/token"}"#;
        let result = VertexExecutor::parse_service_account_json(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_service_account_json_wrong_type() {
        let json = r#"{"type":"wrong","client_email":"test@test.com","private_key":"key","token_uri":"https://oauth2.googleapis.com/token"}"#;
        let result = VertexExecutor::parse_service_account_json(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_service_account_json_valid() {
        let json = r#"{
            "type": "service_account",
            "client_email": "test@test.com",
            "private_key": "-----BEGIN RSA PRIVATE KEY-----\ntest\n-----END RSA PRIVATE KEY-----",
            "token_uri": "https://oauth2.googleapis.com/token",
            "project_id": "my-project"
        }"#;
        let result = VertexExecutor::parse_service_account_json(json);
        assert!(result.is_ok());
        let sa = result.unwrap();
        assert_eq!(sa.client_email, "test@test.com");
        assert_eq!(sa.project_id, Some("my-project".to_string()));
    }
}
