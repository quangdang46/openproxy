use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use openproxy::core::executor::{ClientPool, DefaultExecutor};
use openproxy::core::proxy::{normalize_proxy_url, resolve_proxy_target, ProxyTarget};
use openproxy::types::{AppDb, ProviderConnection, ProviderNode, ProxyPool, Settings};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

#[test]
fn default_executor_builds_static_and_compatible_urls() {
    let pool = Arc::new(ClientPool::new());
    let openai = DefaultExecutor::new("openai", pool.clone(), None).expect("openai executor");
    assert_eq!(
        openai
            .build_url("gpt-4.1", true, &connection("openai"))
            .expect("openai url"),
        "https://api.openai.com/v1/chat/completions"
    );

    let deepseek = DefaultExecutor::new("deepseek", pool.clone(), None).expect("deepseek executor");
    assert_eq!(
        deepseek
            .build_url("deepseek-chat", false, &connection("deepseek"))
            .expect("deepseek url"),
        "https://api.deepseek.com/chat/completions"
    );

    let mut compatible_connection = connection("node-openai");
    compatible_connection.provider_specific_data.insert(
        "baseUrl".into(),
        serde_json::Value::String("https://example.com/v1/".into()),
    );
    let compatible_node = ProviderNode {
        id: "node-openai".into(),
        r#type: "openai-compatible".into(),
        name: "Node".into(),
        prefix: Some("custom".into()),
        api_type: Some("chat".into()),
        base_url: Some("https://fallback.example/v1".into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };
    let compatible = DefaultExecutor::new("node-openai", pool.clone(), Some(compatible_node))
        .expect("compatible");
    assert_eq!(
        compatible
            .build_url("gpt-4.1", true, &compatible_connection)
            .expect("compatible url"),
        "https://example.com/v1/chat/completions"
    );

    compatible_connection
        .provider_specific_data
        .insert("baseUrl".into(), serde_json::Value::String("   ".into()));
    assert_eq!(
        compatible
            .build_url("gpt-4.1", true, &compatible_connection)
            .expect("compatible blank baseUrl fallback"),
        "https://fallback.example/v1/chat/completions"
    );

    compatible_connection.provider_specific_data.insert(
        "apiType".into(),
        serde_json::Value::String("responses".into()),
    );
    assert_eq!(
        compatible
            .build_url("gpt-4.1", true, &compatible_connection)
            .expect("compatible responses url"),
        "https://fallback.example/v1/responses"
    );

    let anthropic_node = ProviderNode {
        id: "node-anthropic".into(),
        r#type: "anthropic-compatible".into(),
        name: "Anthropic Node".into(),
        prefix: Some("anthropic".into()),
        api_type: None,
        base_url: Some("https://anthropic.example/v1".into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };
    let anthropic = DefaultExecutor::new("node-anthropic", pool.clone(), Some(anthropic_node))
        .expect("anthropic");
    let mut anthropic_connection = connection("node-anthropic");
    anthropic_connection
        .provider_specific_data
        .insert("baseUrl".into(), serde_json::Value::String("".into()));
    assert_eq!(
        anthropic
            .build_url("claude-sonnet", false, &anthropic_connection)
            .expect("anthropic fallback url"),
        "https://anthropic.example/v1/messages"
    );
}

#[test]
fn default_executor_builds_expected_headers() {
    let pool = Arc::new(ClientPool::new());
    let openrouter =
        DefaultExecutor::new("openrouter", pool.clone(), None).expect("openrouter executor");
    let headers = openrouter
        .build_headers(&connection("openrouter"), true)
        .expect("headers");
    assert_eq!(headers["authorization"], "Bearer sk-test");
    assert_eq!(headers["accept"], "text/event-stream");
    assert_eq!(headers["http-referer"], "https://endpoint-proxy.local");

    let compatible_node = ProviderNode {
        id: "anthropic-node".into(),
        r#type: "anthropic-compatible".into(),
        name: "Anthropic".into(),
        prefix: Some("custom".into()),
        api_type: None,
        base_url: Some("https://example.com".into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };
    let anthropic =
        DefaultExecutor::new("anthropic-node", pool, Some(compatible_node)).expect("anthropic");
    let headers = anthropic
        .build_headers(&connection("anthropic-node"), false)
        .expect("anthropic headers");
    assert_eq!(headers["x-api-key"], "sk-test");
    assert_eq!(headers["anthropic-version"], "2023-06-01");

    let mut oauth_connection = connection("anthropic-node");
    oauth_connection.api_key = None;
    oauth_connection.access_token = Some("oauth-token".into());
    let headers = anthropic
        .build_headers(&oauth_connection, false)
        .expect("anthropic oauth headers");
    assert_eq!(headers["authorization"], "Bearer oauth-token");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert!(headers.get("x-api-key").is_none());
}

#[test]
fn client_pool_reuses_same_provider_key_and_splits_by_proxy_fingerprint() {
    let pool = ClientPool::new();
    let direct = pool.get("openai", None).expect("direct client");
    let direct_again = pool.get("openai", None).expect("direct again");
    assert!(Arc::ptr_eq(&direct, &direct_again));

    let proxied = pool
        .get(
            "openai",
            Some(&ProxyTarget {
                url: "http://127.0.0.1:8080".into(),
                no_proxy: String::new(),
                strict_proxy: false,
                pool_id: None,
            }),
        )
        .expect("proxied client");
    assert!(!Arc::ptr_eq(&direct, &proxied));

    let proxied_again = pool
        .get(
            "openai",
            Some(&ProxyTarget {
                url: "http://127.0.0.1:8080".into(),
                no_proxy: String::new(),
                strict_proxy: false,
                pool_id: None,
            }),
        )
        .expect("proxied client again");
    assert!(Arc::ptr_eq(&proxied, &proxied_again));

    let proxied_with_no_proxy = pool
        .get(
            "openai",
            Some(&ProxyTarget {
                url: "http://127.0.0.1:8080".into(),
                no_proxy: "localhost".into(),
                strict_proxy: false,
                pool_id: None,
            }),
        )
        .expect("proxied client with no_proxy");
    assert!(!Arc::ptr_eq(&proxied, &proxied_with_no_proxy));
    assert_eq!(pool.len(), 3);
}

#[test]
fn proxy_resolution_prefers_connection_override_then_pool_then_settings() {
    let mut db = AppDb::default();
    db.proxy_pools.push(ProxyPool {
        id: "pool-1".into(),
        name: "Primary".into(),
        proxy_url: "proxy.internal:8080".into(),
        no_proxy: "localhost".into(),
        r#type: "http".into(),
        is_active: Some(true),
        strict_proxy: Some(true),
        test_status: None,
        last_tested_at: None,
        last_error: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    });
    let settings = Settings {
        outbound_proxy_enabled: true,
        outbound_proxy_url: "corp.proxy:9000".into(),
        outbound_no_proxy: "127.0.0.1".into(),
        ..Settings::default()
    };

    let mut conn = connection("openai");
    conn.provider_specific_data.insert(
        "connectionProxyEnabled".into(),
        serde_json::Value::Bool(true),
    );
    conn.provider_specific_data.insert(
        "connectionProxyUrl".into(),
        serde_json::Value::String("direct.proxy:7000".into()),
    );
    assert_eq!(
        resolve_proxy_target(&db, &conn, &settings)
            .expect("direct proxy")
            .url,
        "http://direct.proxy:7000"
    );

    conn.provider_specific_data.insert(
        "connectionProxyPoolId".into(),
        serde_json::Value::String("pool-1".into()),
    );
    let resolved = resolve_proxy_target(&db, &conn, &settings).expect("pool proxy");
    assert_eq!(resolved.url, "http://proxy.internal:8080");
    assert_eq!(resolved.pool_id.as_deref(), Some("pool-1"));

    let mut legacy_conn = connection("openai");
    legacy_conn.provider_specific_data.insert(
        "connectionProxyEnabled".into(),
        serde_json::Value::Bool(true),
    );
    legacy_conn.provider_specific_data.insert(
        "proxyPoolId".into(),
        serde_json::Value::String("pool-1".into()),
    );
    let legacy_resolved =
        resolve_proxy_target(&db, &legacy_conn, &settings).expect("legacy pool proxy");
    assert_eq!(legacy_resolved.url, "http://proxy.internal:8080");
    assert_eq!(legacy_resolved.pool_id.as_deref(), Some("pool-1"));

    let conn = connection("openai");
    let resolved = resolve_proxy_target(&db, &conn, &settings).expect("settings proxy");
    assert_eq!(resolved.url, "http://corp.proxy:9000");
    assert_eq!(resolved.no_proxy, "127.0.0.1");
}

#[test]
fn proxy_url_normalization_adds_scheme_when_missing() {
    assert_eq!(normalize_proxy_url("host:8080"), "http://host:8080");
    assert_eq!(
        normalize_proxy_url("https://host:8080"),
        "https://host:8080"
    );
}

#[test]
fn proxy_pool_type_drives_scheme_for_schemeless_urls() {
    let mut db = AppDb::default();
    db.proxy_pools.push(ProxyPool {
        id: "pool-socks".into(),
        name: "SOCKS".into(),
        proxy_url: "127.0.0.1:1080".into(),
        no_proxy: String::new(),
        r#type: "socks5".into(),
        is_active: Some(true),
        strict_proxy: Some(false),
        test_status: None,
        last_tested_at: None,
        last_error: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    });

    let mut conn = connection("openai");
    conn.provider_specific_data.insert(
        "connectionProxyEnabled".into(),
        serde_json::Value::Bool(true),
    );
    conn.provider_specific_data.insert(
        "connectionProxyPoolId".into(),
        serde_json::Value::String("pool-socks".into()),
    );

    let resolved =
        resolve_proxy_target(&db, &conn, &Settings::default()).expect("socks pool proxy");
    assert_eq!(resolved.url, "socks5://127.0.0.1:1080");
}

#[test]
fn client_pool_accepts_socks_proxy_urls() {
    let pool = ClientPool::new();
    let client = pool.get(
        "openai",
        Some(&ProxyTarget {
            url: "socks5://127.0.0.1:1080".into(),
            no_proxy: String::new(),
            strict_proxy: false,
            pool_id: None,
        }),
    );

    assert!(client.is_ok());
}

#[tokio::test]
async fn no_proxy_bypasses_unreachable_proxy() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind unreachable proxy port");
    let proxy_addr = listener.local_addr().expect("proxy addr");
    drop(listener);

    let pool = ClientPool::new();
    let client = pool
        .get(
            "openai",
            Some(&ProxyTarget {
                url: format!("http://{proxy_addr}"),
                no_proxy: "127.0.0.1,localhost".into(),
                strict_proxy: false,
                pool_id: None,
            }),
        )
        .expect("client with no_proxy");

    let response = client
        .get(format!("{}/health", server.uri()))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .expect("request should bypass proxy");

    assert_eq!(response.status(), 200);
}
