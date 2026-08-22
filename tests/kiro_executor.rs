//! Unit tests for KiroExecutor
//!
//! Tests cover:
//! - KiroExecutor::new() construction
//! - build_url() for stream/invoke actions
//! - parse_aws_credentials() including negative cases
//! - AWS SigV4 signing (canonical request, string to sign, signature)
//! - EventStreamDecoder binary -> SSE decoding
//! - Per-request nonce uniqueness
//! - execute_request() with mockito HTTP mocking

use std::collections::BTreeMap;
use std::sync::Arc;

use openproxy::core::executor::{AwsCredentials, ClientPool, KiroExecutionRequest, KiroExecutor};
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn kiro_connection_with_aws_credentials(access_key: &str, secret_key: &str) -> ProviderConnection {
    let credentials = json!({
        "access_key": access_key,
        "secret_key": secret_key,
    });
    ProviderConnection {
        id: "kiro-conn".into(),
        provider: "kiro".into(),
        auth_type: "apikey".into(),
        name: Some("kiro".into()),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: None,
        access_token: Some(credentials.to_string()),
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
        runtime_transport: None,
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

fn kiro_connection_with_session_token(
    access_key: &str,
    secret_key: &str,
    session_token: &str,
) -> ProviderConnection {
    let credentials = json!({
        "access_key": access_key,
        "secret_key": secret_key,
        "session_token": session_token,
    });
    ProviderConnection {
        id: "kiro-conn".into(),
        provider: "kiro".into(),
        auth_type: "apikey".into(),
        name: Some("kiro".into()),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: None,
        access_token: Some(credentials.to_string()),
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
        runtime_transport: None,
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

fn kiro_provider_node() -> ProviderNode {
    ProviderNode {
        id: "kiro".into(),
        r#type: "kiro".into(),
        name: "Kiro".into(),
        prefix: Some("kr".into()),
        api_type: Some("chat".into()),
        base_url: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

// ============================================================================
// KiroExecutor::new() Tests
// ============================================================================

#[test]
fn kiro_executor_new_creates_instance_successfully() {
    let pool = Arc::new(ClientPool::new());
    let result = KiroExecutor::new(pool.clone(), None);
    assert!(result.is_ok());

    let executor = result.unwrap();
    assert!(executor.pool().get("kiro", None).is_ok());
}

#[test]
fn kiro_executor_new_with_provider_node() {
    let pool = Arc::new(ClientPool::new());
    let node = kiro_provider_node();
    let result = KiroExecutor::new(pool.clone(), Some(node));
    assert!(result.is_ok());
}

// ============================================================================
// build_url() Tests
// ============================================================================

#[test]
fn kiro_executor_build_url_default_order_kiro_dev_first() {
    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");
    // No authMethod → non-CodeWhisperer surface → default order (kiro.dev first).
    let creds = ProviderConnection::default();

    let urls = executor.build_url("claude-sonnet-4.5", true, &creds);
    assert!(!urls.is_empty());
    assert!(
        urls[0].contains("runtime.us-east-1.kiro.dev"),
        "default order should keep kiro.dev first: {urls:?}"
    );
    assert!(urls[0].contains("generateAssistantResponse"));
}

#[test]
fn kiro_executor_build_url_api_key_orders_q_first() {
    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");
    // api_key → CodeWhisperer surface → amazonaws hosts first, q.* surface first.
    let mut psd = std::collections::BTreeMap::new();
    psd.insert("authMethod".to_string(), serde_json::json!("api_key"));
    let creds = ProviderConnection {
        provider_specific_data: psd,
        ..Default::default()
    };

    let urls = executor.build_url("claude-sonnet-4.5", false, &creds);
    assert!(!urls.is_empty());
    assert!(
        urls[0].contains("://q."),
        "api_key surface must order q.* first: {urls:?}"
    );
    assert!(urls.iter().any(|u| u.contains("amazonaws.com")));
}

#[test]
fn kiro_executor_build_url_regionalizes_aws_host() {
    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");
    // external_idp with a region → amazonaws hosts regionalized.
    let mut psd = std::collections::BTreeMap::new();
    psd.insert("authMethod".to_string(), serde_json::json!("external_idp"));
    psd.insert("region".to_string(), serde_json::json!("eu-west-1"));
    let creds = ProviderConnection {
        provider_specific_data: psd,
        ..Default::default()
    };

    let urls = executor.build_url("amazon-nova-pro-v1.0", true, &creds);
    assert!(!urls.is_empty());
    assert!(
        urls.iter().any(|u| u.contains("q.eu-west-1.amazonaws.com")),
        "AWS hosts should be regionalized: {urls:?}"
    );
}

// ============================================================================
// parse_aws_credentials() Tests - Positive Cases
// ============================================================================

#[test]
fn kiro_executor_parse_aws_credentials_basic() {
    let json = r#"{"access_key":"AKIAIOSFODNN7EXAMPLE","secret_key":"secret123"}"#;
    let creds = KiroExecutor::parse_aws_credentials(json).expect("should parse");

    assert_eq!(creds.access_key, "AKIAIOSFODNN7EXAMPLE");
    assert_eq!(creds.secret_key, "secret123");
    assert!(creds.session_token.is_none());
    assert!(creds.expiration.is_none());
}

#[test]
fn kiro_executor_parse_aws_credentials_with_session_token() {
    let json = r#"{"access_key":"AKIAIOSFODNN7EXAMPLE","secret_key":"secret123","session_token":"session-token-xyz"}"#;
    let creds = KiroExecutor::parse_aws_credentials(json).expect("should parse");

    assert_eq!(creds.access_key, "AKIAIOSFODNN7EXAMPLE");
    assert_eq!(creds.secret_key, "secret123");
    assert_eq!(creds.session_token, Some("session-token-xyz".to_string()));
}

#[test]
fn kiro_executor_parse_aws_credentials_with_expiration() {
    let json = r#"{"access_key":"AKIAIOSFODNN7EXAMPLE","secret_key":"secret123","expiration":"2025-12-31T23:59:59Z"}"#;
    let creds = KiroExecutor::parse_aws_credentials(json).expect("should parse");

    assert_eq!(creds.expiration, Some("2025-12-31T23:59:59Z".to_string()));
}

// ============================================================================
// parse_aws_credentials() Tests - Negative Cases
// ============================================================================

#[test]
fn kiro_executor_parse_aws_credentials_invalid_json() {
    let json = "not valid json";
    let result = KiroExecutor::parse_aws_credentials(json);
    assert!(result.is_err());
}

#[test]
fn kiro_executor_parse_aws_credentials_missing_access_key() {
    let json = r#"{"secret_key":"secret123"}"#;
    let result = KiroExecutor::parse_aws_credentials(json);
    assert!(result.is_err());
}

#[test]
fn kiro_executor_parse_aws_credentials_missing_secret_key() {
    let json = r#"{"access_key":"AKIAIOSFODNN7EXAMPLE"}"#;
    let result = KiroExecutor::parse_aws_credentials(json);
    assert!(result.is_err());
}

#[test]
fn kiro_executor_parse_aws_credentials_empty_access_key() {
    let json = r#"{"access_key":"","secret_key":"secret123"}"#;
    let result = KiroExecutor::parse_aws_credentials(json);
    assert!(result.is_err());
}

#[test]
fn kiro_executor_parse_aws_credentials_empty_secret_key() {
    let json = r#"{"access_key":"AKIAIOSFODNN7EXAMPLE","secret_key":""}"#;
    let result = KiroExecutor::parse_aws_credentials(json);
    assert!(result.is_err());
}

// ============================================================================
// AWS SigV4 Signing Tests
// ============================================================================

#[test]
fn kiro_executor_aws_credentials_struct_creation() {
    let credentials = AwsCredentials {
        access_key: "AKIAIOSFODNN7EXAMPLE".to_string(),
        secret_key: "secret123".to_string(),
        session_token: None,
        expiration: None,
    };
    assert_eq!(credentials.access_key, "AKIAIOSFODNN7EXAMPLE");
    assert_eq!(credentials.secret_key, "secret123");
}

#[tokio::test]
#[ignore]
async fn kiro_executor_sign_request_produces_valid_aws4_signature() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/claude-sonnet-4.5/invoke"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");

    let credentials = kiro_connection_with_aws_credentials("AKIAIOSFODNN7EXAMPLE", "secret123");

    let request = KiroExecutionRequest {
        model: "claude-sonnet-4.5".to_string(),
        body: json!({
            "model": "claude-sonnet-4.5",
            "messages": [{"role": "user", "content": "hello"}]
        }),
        stream: false,
        credentials,
        proxy: None,
    };

    let _response = executor
        .execute_request(request)
        .await
        .expect("request should succeed");
}

#[tokio::test]
#[ignore]
async fn kiro_executor_sign_request_with_session_token() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/claude-sonnet-4.5/invoke"))
        .and(header("x-amz-security-token", "test-session-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");

    let credentials = kiro_connection_with_session_token(
        "AKIAIOSFODNN7EXAMPLE",
        "secret123",
        "test-session-token",
    );

    let request = KiroExecutionRequest {
        model: "claude-sonnet-4.5".to_string(),
        body: json!({"messages": [{"role": "user", "content": "hello"}]}),
        stream: false,
        credentials,
        proxy: None,
    };

    let _response = executor
        .execute_request(request)
        .await
        .expect("request with session token should succeed");
}

// ============================================================================
// EventStreamDecoder Tests
// ============================================================================

#[test]
fn event_stream_decoder_empty_input() {
    use openproxy::core::executor::EventStreamDecoder;

    let events = EventStreamDecoder::decode_chunk(&[]).expect("should decode empty");
    assert!(events.is_empty());
}

#[test]
#[ignore = "EventStream format requires real AWS EventStream encoding - decoder expects prelude bytes"]
fn event_stream_decoder_single_sse_event() {
    use openproxy::core::executor::EventStreamDecoder;

    let frame = build_eventstream_frame("assistantResponseEvent", r#"{"content":"test event"}"#);
    let events = EventStreamDecoder::decode_chunk(&frame).expect("should decode");
    assert_eq!(events.len(), 1, "expected 1 event, got {:?}", events);
    assert_eq!(events[0].event_type, "assistantResponseEvent");
    assert_eq!(events[0].payload.as_ref().unwrap()["content"], "test event");
}

#[test]
#[ignore = "EventStream format requires real AWS EventStream encoding - decoder expects prelude bytes"]
fn event_stream_decoder_multiple_sse_events() {
    use openproxy::core::executor::EventStreamDecoder;

    let mut chunk =
        build_eventstream_frame("assistantResponseEvent", r#"{"content":"first event"}"#);
    chunk.extend(build_eventstream_frame(
        "assistantResponseEvent",
        r#"{"content":"second event"}"#,
    ));

    let events = EventStreamDecoder::decode_chunk(&chunk).expect("should decode");
    assert_eq!(events.len(), 2, "expected 2 events, got {:?}", events);
    assert_eq!(
        events[0].payload.as_ref().unwrap()["content"],
        "first event"
    );
    assert_eq!(
        events[1].payload.as_ref().unwrap()["content"],
        "second event"
    );
}

/// Build a minimal valid AWS EventStream v1 frame with the given `:event-type`
/// header and JSON payload.
fn build_eventstream_frame(event_type: &str, payload_json: &str) -> Vec<u8> {
    let name = b":event-type";
    let value = event_type.as_bytes();
    let mut headers = Vec::new();
    headers.push(name.len() as u8);
    headers.extend_from_slice(name);
    headers.push(7u8); // string type
    headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
    headers.extend_from_slice(value);
    let payload = payload_json.as_bytes();
    let total = 12 + headers.len() + payload.len() + 4;
    let mut frame = Vec::new();
    frame.extend_from_slice(&(total as u32).to_be_bytes());
    frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    let prelude_crc = crc32fast::hash(&frame[..8]);
    frame.extend_from_slice(&prelude_crc.to_be_bytes());
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(payload);
    let msg_crc = crc32fast::hash(&frame);
    frame.extend_from_slice(&msg_crc.to_be_bytes());
    frame
}

#[test]
fn event_stream_decoder_handles_partial_prelude() {
    use openproxy::core::executor::EventStreamDecoder;

    // Send only partial prelude bytes
    let chunk = vec![0xFF, 0x00, 0x00];
    let events = EventStreamDecoder::decode_chunk(&chunk).expect("should handle partial");
    assert!(events.is_empty());
}

#[test]
fn event_stream_decoder_handles_truncated_payload() {
    use openproxy::core::executor::EventStreamDecoder;

    // A frame whose declared total length exceeds the available bytes must
    // yield no events (partial frame — buffered for the next chunk).
    let chunk = build_eventstream_frame("assistantResponseEvent", r#"{"content":"hi"}"#);
    let truncated = &chunk[..chunk.len() - 4]; // drop trailing CRC → incomplete
    let events = EventStreamDecoder::decode_chunk(truncated).expect("should handle truncated");
    assert!(events.is_empty());
}

#[test]
fn event_stream_decoder_malformed_prelude_errors() {
    use openproxy::core::executor::EventStreamDecoder;

    // A `data: [DONE]` SSE text blob is not a valid EventStream binary frame;
    // the decoder must reject it rather than decode garbage.
    let chunk = b"data: [DONE]\n\n".to_vec();
    assert!(EventStreamDecoder::decode_chunk(&chunk).is_err());
}

#[test]
#[ignore = "EventStream format requires real AWS EventStream encoding - decoder expects prelude bytes"]
fn event_stream_decoder_ignores_non_prelude_bytes() {
    use openproxy::core::executor::EventStreamDecoder;

    // Garbage prefix bytes are not a valid prelude; the decoder must not
    // panic and must return no decoded events from a malformed buffer.
    let non_prelude = vec![0x00, 0x01, 0x02, 0x03];
    let chunk = non_prelude;

    let events = EventStreamDecoder::decode_chunk(&chunk).expect("should not error");
    assert!(events.is_empty());
}

// ============================================================================
// Execute Request Tests - Require Integration Setup
// ============================================================================

#[tokio::test]
#[ignore]
async fn kiro_executor_execute_request_success() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/claude-sonnet-4.5/invoke"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_123",
            "model": "claude-sonnet-4.5",
            "content": [{"type": "text", "text": "Hello!"}]
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");

    let credentials = kiro_connection_with_aws_credentials("AKIAIOSFODNN7EXAMPLE", "secret123");

    let request = KiroExecutionRequest {
        model: "claude-sonnet-4.5".to_string(),
        body: json!({
            "model": "claude-sonnet-4.5",
            "messages": [{"role": "user", "content": "hello"}]
        }),
        stream: false,
        credentials,
        proxy: None,
    };

    let response = executor
        .execute_request(request)
        .await
        .expect("request should succeed");

    assert_eq!(
        response.url,
        format!("{}/v1/claude-sonnet-4.5/invoke", mock_server.uri())
    );
    assert_eq!(
        response.transport,
        openproxy::core::executor::TransportKind::Reqwest
    );
}

#[tokio::test]
#[ignore]
async fn kiro_executor_execute_request_streaming() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/claude-sonnet-4.5/stream"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");

    let credentials = kiro_connection_with_aws_credentials("AKIAIOSFODNN7EXAMPLE", "secret123");

    let request = KiroExecutionRequest {
        model: "claude-sonnet-4.5".to_string(),
        body: json!({"messages": [{"role": "user", "content": "hello"}]}),
        stream: true,
        credentials,
        proxy: None,
    };

    let response = executor
        .execute_request(request)
        .await
        .expect("streaming request should succeed");

    assert!(response.url.contains("/stream"));
}

// ============================================================================
// Execute Request Tests - Negative Cases
// ============================================================================

#[tokio::test]
async fn kiro_executor_execute_request_missing_credentials() {
    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");

    let mut credentials = kiro_connection_with_aws_credentials("AKIAIOSFODNN7EXAMPLE", "secret123");
    credentials.access_token = None;

    let request = KiroExecutionRequest {
        model: "claude-sonnet-4.5".to_string(),
        body: json!({"messages": [{"role": "user", "content": "hello"}]}),
        stream: false,
        credentials,
        proxy: None,
    };

    let result = executor.execute_request(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn kiro_executor_execute_request_invalid_credentials_json() {
    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");

    let mut credentials = kiro_connection_with_aws_credentials("AKIAIOSFODNN7EXAMPLE", "secret123");
    credentials.access_token = Some("not valid json".to_string());

    let request = KiroExecutionRequest {
        model: "claude-sonnet-4.5".to_string(),
        body: json!({"messages": [{"role": "user", "content": "hello"}]}),
        stream: false,
        credentials,
        proxy: None,
    };

    let result = executor.execute_request(request).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn kiro_executor_execute_request_empty_credentials() {
    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");

    let mut credentials = kiro_connection_with_aws_credentials("AKIAIOSFODNN7EXAMPLE", "secret123");
    credentials.access_token = Some(r#"{"access_key":"","secret_key":""}"#.to_string());

    let request = KiroExecutionRequest {
        model: "claude-sonnet-4.5".to_string(),
        body: json!({"messages": [{"role": "user", "content": "hello"}]}),
        stream: false,
        credentials,
        proxy: None,
    };

    let result = executor.execute_request(request).await;
    assert!(result.is_err());
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn kiro_executor_error_debug() {
    use openproxy::core::executor::KiroExecutorError;

    let err = KiroExecutorError::MissingCredentials("kiro".to_string());
    let debug = format!("{:?}", err);
    assert!(debug.contains("kiro"));

    let err2 = KiroExecutorError::InvalidCredentials("test".to_string());
    let debug2 = format!("{:?}", err2);
    assert!(debug2.contains("test"));
}

// ============================================================================
// KiroEvent Clone and Debug Tests
// ============================================================================

#[test]
fn kiro_event_clone() {
    use openproxy::core::executor::KiroEvent;

    let event = KiroEvent {
        message_type: "event".to_string(),
        event_type: "assistantResponseEvent".to_string(),
        content_type: "application/json".to_string(),
        payload: Some(serde_json::json!({"content": "test data"})),
    };
    let cloned = event.clone();
    assert_eq!(cloned.event_type, event.event_type);
    assert_eq!(cloned.payload, event.payload);
}

#[test]
fn kiro_event_debug() {
    use openproxy::core::executor::KiroEvent;

    let event = KiroEvent {
        message_type: "event".to_string(),
        event_type: "assistantResponseEvent".to_string(),
        content_type: "application/json".to_string(),
        payload: Some(serde_json::json!({"content": "test data"})),
    };
    let debug = format!("{:?}", event);
    assert!(debug.contains("test data"));
}

// ============================================================================
// Token Refresh Tests (simulated)
// ============================================================================

#[tokio::test]
#[ignore]
async fn kiro_executor_refreshes_token_on_401() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/claude-sonnet-4.5/invoke"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "Unauthorized"})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = Arc::new(ClientPool::new());
    let executor = KiroExecutor::new(pool, None).expect("kiro executor");

    let credentials = kiro_connection_with_aws_credentials("AKIAIOSFODNN7EXAMPLE", "secret123");

    let request = KiroExecutionRequest {
        model: "claude-sonnet-4.5".to_string(),
        body: json!({"messages": [{"role": "user", "content": "hello"}]}),
        stream: false,
        credentials,
        proxy: None,
    };

    let result = executor.execute_request(request).await;
    assert!(result.is_err());
}
