use std::collections::BTreeMap;
use std::sync::Arc;

use openproxy::core::executor::{ClientPool, TransportKind};
use openproxy::core::proxy::ProxyTarget;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use openproxy::core::executor::{
    CodexExecutor, CodexExecutorError, CodexExecutionRequest, convert_openai_sse_to_standard,
};

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
        api_key: Some("sk-test-codex-key".into()),
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

fn connection_with_access_token(provider: &str, token: &str) -> ProviderConnection {
    let mut conn = connection(provider);
    conn.api_key = None;
    conn.access_token = Some(token.into());
    conn
}

fn provider_node() -> ProviderNode {
    ProviderNode {
        id: "codex-node".into(),
        r#type: "openai-compatible".into(),
        name: "Codex Node".into(),
        prefix: Some("codex".into()),
        api_type: Some("responses".into()),
        base_url: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

#[test]
fn codex_executor_new_succeeds_with_valid_pool() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool.clone(), None);
    assert!(executor.is_ok());
    assert!(Arc::ptr_eq(executor.as_ref().unwrap().pool(), &pool));
}

#[test]
fn codex_executor_new_succeeds_with_provider_node() {
    let pool = Arc::new(ClientPool::new());
    let node = provider_node();
    let executor = CodexExecutor::new(pool, Some(node));
    assert!(executor.is_ok());
}

#[test]
fn codex_executor_parse_codex_model_with_codex_prefix() {
    assert_eq!(CodexExecutor::parse_codex_model("codex/o4-mini"), "o4-mini");
    assert_eq!(CodexExecutor::parse_codex_model("codex/o4-mini-high"), "o4-mini-high");
    assert_eq!(CodexExecutor::parse_codex_model("codex/o3"), "o3");
    assert_eq!(CodexExecutor::parse_codex_model("codex/o3-mini"), "o3-mini");
    assert_eq!(CodexExecutor::parse_codex_model("codex/gpt-4-turbo"), "gpt-4-turbo");
}

#[test]
fn codex_executor_parse_codex_model_without_prefix() {
    assert_eq!(CodexExecutor::parse_codex_model("o4-mini"), "o4-mini");
    assert_eq!(CodexExecutor::parse_codex_model("o3"), "o3");
    assert_eq!(CodexExecutor::parse_codex_model("gpt-4"), "gpt-4");
    assert_eq!(CodexExecutor::parse_codex_model("gpt-4-turbo"), "gpt-4-turbo");
}

#[test]
fn codex_sse_conversion_standard_format() {
    let openai_sse = b"event: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\nevent: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}\n\nevent: response.done\ndata: {\"type\":\"response.done\"}\n";

    let result = convert_openai_sse_to_standard(openai_sse);
    let result_str = String::from_utf8(result).unwrap();

    assert!(result_str.contains("data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}"));
    assert!(result_str.contains("data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}"));
    assert!(result_str.contains("data: {\"type\":\"response.done\"}"));
    assert!(result_str.contains("\n\n"));
}

#[test]
fn codex_sse_conversion_empty_input() {
    let result = convert_openai_sse_to_standard(b"");
    assert!(result.is_empty());
}

#[test]
fn codex_sse_conversion_standard_format_unchanged() {
    let standard_sse = b"data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n";
    let result = convert_openai_sse_to_standard(standard_sse);
    let result_str = String::from_utf8(result).unwrap();

    assert!(result_str.contains("data: {\"type\":\"content.delta\""));
    assert!(result_str.contains("\n\n"));
}

#[test]
fn codex_sse_conversion_skips_event_lines() {
    let input = b"event: message\ndata: {\"type\":\"content.delta\"}\n\nevent: done\ndata: {\"type\":\"response.done\"}\n";

    let result = convert_openai_sse_to_standard(input);
    let result_str = String::from_utf8(result).unwrap();

    assert!(!result_str.contains("event:"));
    assert!(result_str.contains("data: {\"type\":\"content.delta\"}"));
    assert!(result_str.contains("data: {\"type\":\"response.done\"}"));
}

#[tokio::test]
async fn codex_executor_execute_missing_credentials_fails() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: ProviderConnection {
            id: "test".into(),
            provider: "codex".into(),
            auth_type: "apikey".into(),
            name: None,
            priority: None,
            is_active: None,
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
            api_key: None,
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
        },
        proxy: None,
    };

    let result = executor.execute(req).await;
    assert!(result.is_err());

    if let Err(e) = result {
        assert!(matches!(e, CodexExecutorError::MissingCredentials(_)));
    }
}

#[tokio::test]
async fn codex_executor_execute_returns_correct_url() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let response = executor.execute(req).await.expect("execute request");
    assert_eq!(response.url, "https://api.openai.com/v1/responses");
}

#[tokio::test]
async fn codex_executor_execute_returns_correct_headers() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: true,
        credentials: connection("codex"),
        proxy: None,
    };

    let response = executor.execute(req).await.expect("execute request");
    assert_eq!(response.headers.get("authorization").unwrap().to_str().unwrap(), "Bearer sk-test-codex-key");
    assert_eq!(response.headers.get("content-type").unwrap().to_str().unwrap(), "application/json");
    assert_eq!(response.headers.get("accept").unwrap().to_str().unwrap(), "text/event-stream");
}

#[tokio::test]
async fn codex_executor_execute_access_token_preferred() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: connection_with_access_token("codex", "oauth-token-preferred"),
        proxy: None,
    };

    let response = executor.execute(req).await.expect("execute request");
    assert_eq!(response.headers.get("authorization").unwrap().to_str().unwrap(), "Bearer oauth-token-preferred");
}

#[tokio::test]
async fn codex_executor_execute_non_streaming_no_accept_header() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let response = executor.execute(req).await.expect("execute request");
    assert!(response.headers.get("accept").is_none());
}

#[tokio::test]
async fn codex_executor_execute_multiple_messages_input() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi"},
                {"role": "user", "content": "There"}
            ]
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let response = executor.execute(req).await.expect("execute request");
    assert_eq!(response.transformed_body["input"], "Hello\nHi\nThere");
}

#[tokio::test]
async fn codex_executor_execute_transforms_model_name() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o3-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let response = executor.execute(req).await.expect("execute request");
    assert_eq!(response.transformed_body["model"], "o3-mini");
}

#[tokio::test]
async fn codex_executor_execute_without_codex_prefix() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let response = executor.execute(req).await.expect("execute request");
    assert_eq!(response.transformed_body["model"], "o4-mini");
}

#[tokio::test]
async fn codex_executor_execute_empty_messages_fails() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": []
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let result = executor.execute(req).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), CodexExecutorError::UnsupportedFormat(_)));
}

#[tokio::test]
async fn codex_executor_execute_all_params_copied() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false,
            "temperature": 0.7,
            "max_tokens": 1000,
            "top_p": 0.9,
            "stop": ["END"]
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let response = executor.execute(req).await.expect("execute request");
    assert_eq!(response.transformed_body["temperature"], 0.7);
    assert_eq!(response.transformed_body["max_tokens"], 1000);
    assert_eq!(response.transformed_body["top_p"], 0.9);
    assert_eq!(response.transformed_body["stop"], json!(["END"]));
}

#[tokio::test]
async fn codex_executor_execute_reasoning_param_stripped() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}],
            "reasoning": {"effort": "high"}
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let response = executor.execute(req).await.expect("execute request");
    assert!(response.transformed_body.get("reasoning").is_none());
}

#[test]
fn codex_executor_error_missing_credentials_is_debug() {
    let err = CodexExecutorError::MissingCredentials("API key required".to_string());
    let msg = format!("{:?}", err);
    assert!(msg.contains("MissingCredentials"));
}

#[test]
fn codex_executor_error_invalid_credentials_is_debug() {
    let err = CodexExecutorError::InvalidCredentials("Invalid token format".to_string());
    let msg = format!("{:?}", err);
    assert!(msg.contains("InvalidCredentials"));
}

#[test]
fn codex_executor_error_unsupported_format_is_debug() {
    let err = CodexExecutorError::UnsupportedFormat("Missing messages array".to_string());
    let msg = format!("{:?}", err);
    assert!(msg.contains("UnsupportedFormat"));
}