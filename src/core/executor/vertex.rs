use std::sync::Arc;

use hyper::http::{self as hyper_http, uri::InvalidUri};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::Duration;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const VERTEX_AI_BASE_URL: &str = "https://aiplatform.googleapis.com/v2beta";
const VERTEX_DEFAULT_LOCATION: &str = "us-central1";

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
        _project_id: &str,
        location: &str,
        is_partner: bool,
    ) -> String {
        let model_stripped = model.strip_prefix("vertex/").unwrap_or(model);

        if is_partner {
            format!(
                "https://{}-{}/publishers/{}",
                location, VERTEX_AI_BASE_URL, model_stripped
            )
        } else {
            format!(
                "https://{}-{}/publishers/google/{}",
                location, VERTEX_AI_BASE_URL, model_stripped
            )
        }
    }

    pub async fn execute_request(
        &self,
        request: VertexExecutionRequest,
    ) -> Result<VertexExecutorResponse, VertexExecutorError> {
        let credentials_json = request.credentials.access_token.as_deref().ok_or_else(|| {
            VertexExecutorError::MissingCredentials(
                "access_token (service account JSON) is required for Vertex AI".to_string(),
            )
        })?;

        let service_account = Self::parse_service_account_json(credentials_json)?;
        let jwt = Self::create_rs256_jwt(&service_account)?;
        let access_token = Self::exchange_jwt_for_token(&jwt, &service_account.token_uri).await?;

        let (location, project_id, _, is_partner) = Self::parse_vertex_model(&request.model);
        let url = Self::build_vertex_url(&request.model, &project_id, &location, is_partner);

        let transformed_body =
            Self::build_vertex_request_body(&request.body, &request.model, request.stream)?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", access_token.token))
                .map_err(VertexExecutorError::InvalidHeader)?,
        );

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_vertex_model_standard() {
        let (location, project_id, actual_model, is_partner) =
            VertexExecutor::parse_vertex_model("vertex/gemini-2.5-flash");
        assert_eq!(location, "us-central1");
        assert_eq!(actual_model, "models/gemini-2.5-flash");
        assert!(!is_partner);
    }

    #[test]
    fn test_parse_vertex_model_partner() {
        let (location, project_id, actual_model, is_partner) =
            VertexExecutor::parse_vertex_model("vertex-partner/glm-5-maas");
        assert_eq!(location, "us-central1");
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
