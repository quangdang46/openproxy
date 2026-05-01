use std::collections::BTreeMap;
use std::sync::Arc;

use openproxy::core::executor::ClientPool;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;

fn connection(provider: &str) -> ProviderConnection {
    ProviderConnection {
        id: format!("{provider}-conn"),
        provider: provider.to_string(),
        auth_type: "apikey".into(),
        name: Some(provider.into()),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: None,
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: Some("sk-test".into()),
        test_status: None,
        last_tested: None,
        last_error: None,
        last_error_at: None,
        rate_limited_until: None,
        expires_in: None,
        error_code: None,
        consecutive_use_count: None,
        backoff_level: None,
        consecutive_errors: None,
        proxy_url: None,
        proxy_label: None,
        use_connection_proxy: None,
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

#[test]
fn vertex_executor_new_succeeds_with_valid_pool() {
    let pool = Arc::new(ClientPool::new());
    let executor = openproxy::core::executor::VertexExecutor::new(pool.clone(), None);
    assert!(executor.is_ok());
    assert!(Arc::ptr_eq(executor.as_ref().unwrap().pool(), &pool));
}

#[test]
fn vertex_executor_new_succeeds_with_provider_node() {
    let pool = Arc::new(ClientPool::new());
    let provider_node = ProviderNode {
        id: "vertex-node".into(),
        r#type: "vertex".into(),
        name: "Vertex Node".into(),
        prefix: Some("vertex".into()),
        api_type: Some("chat".into()),
        base_url: Some("https://aiplatform.googleapis.com".into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };
    let executor = openproxy::core::executor::VertexExecutor::new(pool, Some(provider_node));
    assert!(executor.is_ok());
}

#[tokio::test]
async fn vertex_executor_execute_request_missing_credentials() {
    let executor = openproxy::core::executor::VertexExecutor::new(
        Arc::new(ClientPool::new()),
        None,
    ).expect("vertex executor");

    let mut conn = connection("vertex");
    conn.access_token = None;

    let body = json!({"contents": []});

    let result = executor.execute_request(openproxy::core::executor::VertexExecutionRequest {
        model: "vertex/gemini-2.5-flash".to_string(),
        body,
        stream: false,
        credentials: conn,
        proxy: None,
    }).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, openproxy::core::executor::VertexExecutorError::MissingCredentials(_)));
}

#[tokio::test]
async fn vertex_executor_execute_request_invalid_service_account_json() {
    let executor = openproxy::core::executor::VertexExecutor::new(
        Arc::new(ClientPool::new()),
        None,
    ).expect("vertex executor");

    let mut conn = connection("vertex");
    conn.access_token = Some("not valid json".to_string());

    let body = json!({"contents": []});

    let result = executor.execute_request(openproxy::core::executor::VertexExecutionRequest {
        model: "vertex/gemini-2.5-flash".to_string(),
        body,
        stream: false,
        credentials: conn,
        proxy: None,
    }).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, openproxy::core::executor::VertexExecutorError::MissingServiceAccountJson(_)));
}

#[tokio::test]
async fn vertex_executor_execute_request_wrong_service_account_type() {
    let executor = openproxy::core::executor::VertexExecutor::new(
        Arc::new(ClientPool::new()),
        None,
    ).expect("vertex executor");

    let mut conn = connection("vertex");
    conn.access_token = Some(r#"{"type":"wrong","client_email":"test@test.com","private_key":"key","token_uri":"https://oauth2.googleapis.com/token"}"#.to_string());

    let body = json!({"contents": []});

    let result = executor.execute_request(openproxy::core::executor::VertexExecutionRequest {
        model: "vertex/gemini-2.5-flash".to_string(),
        body,
        stream: false,
        credentials: conn,
        proxy: None,
    }).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, openproxy::core::executor::VertexExecutorError::MissingServiceAccountJson(_)));
}

#[tokio::test]
async fn vertex_executor_execute_request_empty_private_key() {
    let executor = openproxy::core::executor::VertexExecutor::new(
        Arc::new(ClientPool::new()),
        None,
    ).expect("vertex executor");

    let mut conn = connection("vertex");
    conn.access_token = Some(r#"{"type":"service_account","client_email":"test@test.com","private_key":"","token_uri":"https://oauth2.googleapis.com/token"}"#.to_string());

    let body = json!({"contents": []});

    let result = executor.execute_request(openproxy::core::executor::VertexExecutionRequest {
        model: "vertex/gemini-2.5-flash".to_string(),
        body,
        stream: false,
        credentials: conn,
        proxy: None,
    }).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, openproxy::core::executor::VertexExecutorError::MissingServiceAccountJson(_)));
}

#[tokio::test]
async fn vertex_executor_execute_request_empty_client_email() {
    let executor = openproxy::core::executor::VertexExecutor::new(
        Arc::new(ClientPool::new()),
        None,
    ).expect("vertex executor");

    let mut conn = connection("vertex");
    conn.access_token = Some(r#"{"type":"service_account","client_email":"","private_key":"key","token_uri":"https://oauth2.googleapis.com/token"}"#.to_string());

    let body = json!({"contents": []});

    let result = executor.execute_request(openproxy::core::executor::VertexExecutionRequest {
        model: "vertex/gemini-2.5-flash".to_string(),
        body,
        stream: false,
        credentials: conn,
        proxy: None,
    }).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, openproxy::core::executor::VertexExecutorError::MissingServiceAccountJson(_)));
}

#[tokio::test]
async fn vertex_executor_execute_request_empty_token_uri() {
    let executor = openproxy::core::executor::VertexExecutor::new(
        Arc::new(ClientPool::new()),
        None,
    ).expect("vertex executor");

    let mut conn = connection("vertex");
    conn.access_token = Some(r#"{"type":"service_account","client_email":"test@test.com","private_key":"key","token_uri":""}"#.to_string());

    let body = json!({"contents": []});

    let result = executor.execute_request(openproxy::core::executor::VertexExecutionRequest {
        model: "vertex/gemini-2.5-flash".to_string(),
        body,
        stream: false,
        credentials: conn,
        proxy: None,
    }).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, openproxy::core::executor::VertexExecutorError::MissingServiceAccountJson(_)));
}

#[tokio::test]
async fn vertex_executor_execute_request_network_error() {
    let executor = openproxy::core::executor::VertexExecutor::new(
        Arc::new(ClientPool::new()),
        None,
    ).expect("vertex executor");

    let mut conn = connection("vertex");
    conn.access_token = Some(r#"{"type":"service_account","client_email":"test@project.iam.gserviceaccounts.com","private_key":"-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA0Z3VS5JJcds3xfn/ygWyF8PbnGy0FFDcEPQ8gNLzmRszKzX9f\ndkKNOHKFxkC3hY8m6xM6FGBvA9wWVRqSvh7x8nGJhPxwLlpYD9wQ/E8qOj8dL0\nj6pW1N5zT3mJ8w5qZkJpF4e3pJPhNPLkZP5m8vPjx4c7F6b4L8pVS8mN7xZhJ6\nqF1yF2kP5p0T3mJ8w5qZkJpF4e3pJPhNPLkZP5m8vPjx4c7F6b4L8pVS8mN7x\n-----END RSA PRIVATE KEY-----","token_uri":"http://localhost:99999/nonexistent"}"#.to_string());

    let body = json!({"contents": []});

    let result = executor.execute_request(openproxy::core::executor::VertexExecutionRequest {
        model: "vertex/gemini-2.5-flash".to_string(),
        body,
        stream: false,
        credentials: conn,
        proxy: None,
    }).await;

    assert!(result.is_err());
}

#[test]
fn vertex_executor_error_display() {
    let err = openproxy::core::executor::VertexExecutorError::MissingCredentials("test message".to_string());
    let debug_fmt = format!("{:?}", err);
    assert!(debug_fmt.contains("MissingCredentials"));

    let err2 = openproxy::core::executor::VertexExecutorError::JwtGenerationFailed("jwt failed".to_string());
    let debug_fmt2 = format!("{:?}", err2);
    assert!(debug_fmt2.contains("JwtGenerationFailed"));

    let err3 = openproxy::core::executor::VertexExecutorError::RequestFailed("failed".to_string());
    let debug_fmt3 = format!("{:?}", err3);
    assert!(debug_fmt3.contains("RequestFailed"));

    let err4 = openproxy::core::executor::VertexExecutorError::RsaPemParse("parse error".to_string());
    let debug_fmt4 = format!("{:?}", err4);
    assert!(debug_fmt4.contains("RsaPemParse"));

    let err5 = openproxy::core::executor::VertexExecutorError::InvalidToken("invalid".to_string());
    let debug_fmt5 = format!("{:?}", err5);
    assert!(debug_fmt5.contains("InvalidToken"));
}
