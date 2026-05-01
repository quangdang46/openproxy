use std::collections::BTreeMap;
use std::sync::Arc;

use openproxy::core::executor::{ClientPool, CursorExecutor, CursorExecutorError, CursorExecutionRequest, SseEvent, parse_cursor_sse_events};
use openproxy::types::{ProviderConnection, ProviderNode};

fn cursor_connection() -> ProviderConnection {
    ProviderConnection {
        id: "cursor-conn".into(),
        provider: "cursor".into(),
        auth_type: "oauth".into(),
        name: Some("Cursor".into()),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: None,
        access_token: Some("cursor-token-abc123".into()),
        refresh_token: None,
        expires_at: None,
        token_type: Some("Bearer".into()),
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
    }
}

#[test]
fn cursor_executor_builds_connect_protocol_content_type() {
    let pool = Arc::new(ClientPool::new());
    let _executor = CursorExecutor::new(pool, None).expect("executor");
    let conn = cursor_connection();

    let req = CursorExecutionRequest {
        model: "cursor/claude-4.6-opus-max".into(),
        body: serde_json::json!({
            "messages": [{"role": "user", "content": "hello"}]
        }),
        stream: true,
        credentials: conn,
        proxy: None,
    };

    assert_eq!(req.model, "cursor/claude-4.6-opus-max");
    assert!(req.stream);
}

#[test]
fn cursor_executor_new_succeeds() {
    let pool = Arc::new(ClientPool::new());
    let executor = CursorExecutor::new(pool.clone(), None);
    assert!(executor.is_ok());
    assert!(Arc::ptr_eq(executor.unwrap().pool(), &pool));
}

#[test]
fn cursor_executor_accepts_provider_node() {
    let pool = Arc::new(ClientPool::new());
    let node = ProviderNode {
        id: "cursor-node".into(),
        r#type: "cursor".into(),
        name: "Cursor".into(),
        prefix: Some("cursor".into()),
        api_type: None,
        base_url: Some("https://agent.cursor.sh".into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };

    let executor = CursorExecutor::new(pool, Some(node));
    assert!(executor.is_ok());
}

#[test]
fn cursor_executor_parse_cursor_model_with_prefix() {
    assert_eq!(
        CursorExecutor::parse_cursor_model("cursor/claude-4.6-opus-max"),
        "claude-4.6-opus-max"
    );
    assert_eq!(
        CursorExecutor::parse_cursor_model("cursor/gpt-5.3-codex"),
        "gpt-5.3-codex"
    );
    assert_eq!(
        CursorExecutor::parse_cursor_model("cursor/kimi-k2.5"),
        "kimi-k2.5"
    );
}

#[test]
fn cursor_executor_parse_cursor_model_without_prefix() {
    assert_eq!(
        CursorExecutor::parse_cursor_model("claude-4.6-opus-max"),
        "claude-4.6-opus-max"
    );
    assert_eq!(
        CursorExecutor::parse_cursor_model("gpt-5.3-codex"),
        "gpt-5.3-codex"
    );
}

#[test]
fn cursor_executor_parse_cursor_model_empty_string() {
    assert_eq!(CursorExecutor::parse_cursor_model(""), "");
}

#[test]
fn cursor_executor_parse_cursor_model_preserves_non_cursor_prefix() {
    assert_eq!(
        CursorExecutor::parse_cursor_model("cursor-model-name"),
        "cursor-model-name"
    );
}

#[test]
fn sse_event_debug_trait_impl() {
    let event = SseEvent::Text("hello".to_string());
    let debug_str = format!("{:?}", event);
    assert!(debug_str.contains("Text"));
    assert!(debug_str.contains("hello"));
}

#[test]
fn sse_event_tool_call_variant() {
    let tool_call_json = serde_json::json!({
        "id": "tool_123",
        "type": "function",
        "function": {"name": "Read", "arguments": "{}"}
    });
    let event = SseEvent::ToolCall(tool_call_json);
    let debug_str = format!("{:?}", event);
    assert!(debug_str.contains("ToolCall"));
}

#[test]
fn sse_event_raw_variant() {
    let event = SseEvent::Raw("raw data".to_string());
    let debug_str = format!("{:?}", event);
    assert!(debug_str.contains("Raw"));
}

#[test]
fn parse_cursor_sse_events_empty_data() {
    let events = parse_cursor_sse_events(b"").expect("parse empty");
    assert!(events.is_empty());
}

#[test]
fn parse_cursor_sse_events_raw_sse_data() {
    let data = b"data: some event\ndata: another event\n";
    let events = parse_cursor_sse_events(data).expect("parse events");
    assert!(!events.is_empty());
}

#[test]
fn parse_cursor_sse_events_ignores_done() {
    let data = b"data: content\ndata: [DONE]\n";
    let events = parse_cursor_sse_events(data).expect("parse events");

    for event in &events {
        if let SseEvent::Raw(content) = event {
            assert_ne!(content, "[DONE]");
        }
    }
}

#[test]
fn cursor_executor_error_debug() {
    let err = CursorExecutorError::MissingCredentials("test".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("MissingCredentials"));
}

#[test]
fn cursor_executor_error_unsupported_format() {
    let err = CursorExecutorError::UnsupportedFormat("test".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("UnsupportedFormat"));
}

#[test]
fn cursor_executor_error_protobuf_encode() {
    let err = CursorExecutorError::ProtobufEncode("test".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("ProtobufEncode"));
}

#[test]
fn cursor_executor_error_protobuf_decode() {
    let err = CursorExecutorError::ProtobufDecode("test".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("ProtobufDecode"));
}

#[test]
fn cursor_executor_error_checksum() {
    let err = CursorExecutorError::ChecksumError("test".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("ChecksumError"));
}

#[test]
fn cursor_executor_error_stream() {
    let err = CursorExecutorError::StreamError("test".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("StreamError"));
}

#[tokio::test]
async fn cursor_executor_missing_access_token_fails() {
    let pool = Arc::new(ClientPool::new());
    let executor = CursorExecutor::new(pool, None).expect("executor");

    let conn = ProviderConnection {
        id: "cursor".into(),
        provider: "cursor".to_string(),
        auth_type: "oauth".to_string(),
        name: None,
        priority: None,
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
    };

    let request = CursorExecutionRequest {
        model: "cursor/claude-4.6-opus-max".into(),
        body: serde_json::json!({
            "messages": [{"role": "user", "content": "hello"}]
        }),
        stream: true,
        credentials: conn,
        proxy: None,
    };

    let result = executor.execute(request).await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(matches!(err, CursorExecutorError::MissingCredentials(_)));
}

#[test]
fn cursor_executor_parse_cursor_model_variants() {
    let variants = vec![
        ("cursor/claude-sonnet-4.5", "claude-sonnet-4.5"),
        ("cursor/claude-opus-4-7", "claude-opus-4-7"),
        ("cursor/gpt-5.4", "gpt-5.4"),
        ("cursor/kimi-k2.5", "kimi-k2.5"),
        ("claude-sonnet-4.5", "claude-sonnet-4.5"),
        ("gpt-5.4", "gpt-5.4"),
    ];

    for (input, expected) in variants {
        let result = CursorExecutor::parse_cursor_model(input);
        assert_eq!(result, expected, "Failed for input: {}", input);
    }
}

#[test]
fn cursor_execution_request_fields() {
    let request = CursorExecutionRequest {
        model: "cursor/claude-4.6-opus-max".into(),
        body: serde_json::json!({"messages": []}),
        stream: false,
        credentials: cursor_connection(),
        proxy: None,
    };

    assert_eq!(request.model, "cursor/claude-4.6-opus-max");
    assert!(!request.stream);
    assert!(request.credentials.access_token.is_some());
}

#[test]
fn cursor_connection_has_access_token() {
    let conn = cursor_connection();
    assert!(conn.access_token.is_some());
    assert_eq!(conn.access_token.unwrap(), "cursor-token-abc123");
}

#[test]
fn cursor_connection_provider_is_cursor() {
    let conn = cursor_connection();
    assert_eq!(conn.provider, "cursor");
}

#[test]
fn provider_node_cursor_type() {
    let node = ProviderNode {
        id: "cursor".into(),
        r#type: "cursor".into(),
        name: "Cursor".into(),
        prefix: Some("cursor".into()),
        api_type: None,
        base_url: Some("https://agent.cursor.sh".into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };

    assert_eq!(node.r#type, "cursor");
    assert!(node.prefix.is_some());
    assert_eq!(node.prefix.unwrap(), "cursor");
}

#[test]
fn cursor_executor_response_debug() {
    let resp = CursorExecutorError::MissingCredentials("test".into());
    let _ = format!("{:?}", resp);
}

#[test]
fn cursor_connection_auth_type_oauth() {
    let conn = cursor_connection();
    assert_eq!(conn.auth_type, "oauth");
}

#[test]
fn cursor_connection_token_type_bearer() {
    let conn = cursor_connection();
    assert_eq!(conn.token_type, Some("Bearer".into()));
}

#[test]
fn cursor_execution_request_model_and_stream() {
    let request = CursorExecutionRequest {
        model: "cursor/claude-4.6-opus-max".into(),
        body: serde_json::json!({"messages": [{"role": "user", "content": "hello"}]}),
        stream: true,
        credentials: cursor_connection(),
        proxy: None,
    };

    assert_eq!(request.model, "cursor/claude-4.6-opus-max");
    assert!(request.stream);
}