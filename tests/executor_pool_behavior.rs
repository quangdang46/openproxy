use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;

use openproxy::core::executor::{
    ClientPool, DefaultExecutor, ExecutionRequest, ExecutorError, TransportKind,
    CLIENT_POOL_IDLE_TIMEOUT, CLIENT_POOL_MAX_IDLE_PER_HOST, CLIENT_POOL_TCP_KEEPALIVE,
};
use openproxy::core::proxy::{normalize_proxy_url, resolve_proxy_target, ProxyTarget};
use openproxy::types::{AppDb, ProviderConnection, ProviderNode, ProxyPool, Settings};
use serde_json::json;
use tokio::sync::oneshot;
use wiremock::matchers::{body_json, header, method, path};
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
        backoff_level: None,
        consecutive_errors: None,
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
    compatible_connection
        .provider_specific_data
        .insert("apiType".into(), serde_json::Value::String("chat".into()));
    assert_eq!(
        compatible
            .build_url("gpt-4.1", true, &compatible_connection)
            .expect("compatible chat fallback url"),
        "https://fallback.example/v1/chat/completions"
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

    let anthropic =
        DefaultExecutor::new("anthropic", pool.clone(), None).expect("anthropic executor");
    assert_eq!(
        anthropic
            .build_url("claude-sonnet-4.5", false, &connection("anthropic"))
            .expect("anthropic url"),
        "https://api.anthropic.com/v1/messages"
    );

    let gemini = DefaultExecutor::new("gemini", pool.clone(), None).expect("gemini executor");
    assert_eq!(
        gemini
            .build_url("gemini-2.5-flash", false, &connection("gemini"))
            .expect("gemini non-stream url"),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
    );
    assert_eq!(
        gemini
            .build_url("gemini-2.5-flash", true, &connection("gemini"))
            .expect("gemini stream url"),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );

    let cline = DefaultExecutor::new("cline", pool.clone(), None).expect("cline executor");
    assert_eq!(
        cline
            .build_url("gpt-5.4", false, &connection("cline"))
            .expect("cline url"),
        "https://api.cline.bot/api/v1/chat/completions"
    );

    let opencode_go =
        DefaultExecutor::new("opencode-go", pool.clone(), None).expect("opencode-go executor");
    assert_eq!(
        opencode_go
            .build_url("kimi-k2.6", false, &connection("opencode-go"))
            .expect("opencode-go url"),
        "https://opencode.ai/zen/go/v1/chat/completions"
    );
    assert_eq!(
        opencode_go
            .build_url("minimax-m2.5", false, &connection("opencode-go"))
            .expect("opencode-go claude-format url"),
        "https://opencode.ai/zen/go/v1/messages"
    );

    let cloudflare =
        DefaultExecutor::new("cloudflare-ai", pool, None).expect("cloudflare executor");
    let mut cloudflare_connection = connection("cloudflare-ai");
    cloudflare_connection.provider_specific_data.insert(
        "accountId".into(),
        serde_json::Value::String("acct-123".into()),
    );
    assert_eq!(
        cloudflare
            .build_url("@cf/moonshotai/kimi-k2.6", false, &cloudflare_connection,)
            .expect("cloudflare url"),
        "https://api.cloudflare.com/client/v4/accounts/acct-123/ai/v1/chat/completions"
    );
}

#[test]
fn default_executor_supports_current_passthrough_provider_matrix() {
    let pool = Arc::new(ClientPool::new());
    let static_providers = [
        ("openai", "https://api.openai.com/v1/chat/completions"),
        (
            "openrouter",
            "https://openrouter.ai/api/v1/chat/completions",
        ),
        ("deepseek", "https://api.deepseek.com/chat/completions"),
        ("groq", "https://api.groq.com/openai/v1/chat/completions"),
        ("xai", "https://api.x.ai/v1/chat/completions"),
        ("mistral", "https://api.mistral.ai/v1/chat/completions"),
        ("cline", "https://api.cline.bot/api/v1/chat/completions"),
        ("together", "https://api.together.xyz/v1/chat/completions"),
        (
            "fireworks",
            "https://api.fireworks.ai/inference/v1/chat/completions",
        ),
        ("cerebras", "https://api.cerebras.ai/v1/chat/completions"),
        ("cohere", "https://api.cohere.ai/v1/chat/completions"),
        ("nebius", "https://api.studio.nebius.ai/v1/chat/completions"),
        (
            "siliconflow",
            "https://api.siliconflow.cn/v1/chat/completions",
        ),
        (
            "hyperbolic",
            "https://api.hyperbolic.xyz/v1/chat/completions",
        ),
        ("perplexity", "https://api.perplexity.ai/chat/completions"),
        (
            "nanobanana",
            "https://api.nanobananaapi.ai/v1/chat/completions",
        ),
        ("chutes", "https://llm.chutes.ai/v1/chat/completions"),
        ("gitlab", "https://gitlab.com/api/v4/chat/completions"),
        (
            "codebuddy",
            "https://copilot.tencent.com/v1/chat/completions",
        ),
        (
            "kilocode",
            "https://api.kilo.ai/api/openrouter/chat/completions",
        ),
        (
            "opencode-go",
            "https://opencode.ai/zen/go/v1/chat/completions",
        ),
        (
            "glm-cn",
            "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
        ),
        (
            "alicode",
            "https://coding.dashscope.aliyuncs.com/v1/chat/completions",
        ),
        (
            "alicode-intl",
            "https://coding-intl.dashscope.aliyuncs.com/v1/chat/completions",
        ),
        (
            "volcengine-ark",
            "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions",
        ),
        (
            "byteplus",
            "https://ark.ap-southeast.bytepluses.com/api/coding/v3/chat/completions",
        ),
        (
            "nvidia",
            "https://integrate.api.nvidia.com/v1/chat/completions",
        ),
    ];

    for (provider, expected_url) in static_providers {
        let executor = DefaultExecutor::new(provider, pool.clone(), None)
            .unwrap_or_else(|_| panic!("missing passthrough provider config for {provider}"));
        assert_eq!(
            executor
                .build_url("matrix-model", false, &connection(provider))
                .unwrap_or_else(|_| panic!("missing URL builder for {provider}")),
            expected_url,
            "unexpected URL for {provider}"
        );
    }

    for provider in ["glm", "kimi", "minimax", "minimax-cn"] {
        let executor = DefaultExecutor::new(provider, pool.clone(), None)
            .unwrap_or_else(|_| panic!("missing beta provider config for {provider}"));
        let url = executor
            .build_url("matrix-model", false, &connection(provider))
            .unwrap_or_else(|_| panic!("missing URL builder for {provider}"));
        assert!(
            url.ends_with("?beta=true"),
            "expected beta URL for {provider}"
        );
    }
}

#[test]
fn default_executor_builds_expected_headers() {
    let pool = Arc::new(ClientPool::new());
    let openrouter =
        DefaultExecutor::new("openrouter", pool.clone(), None).expect("openrouter executor");
    let headers = openrouter
        .build_headers("gpt-4.1", &connection("openrouter"), true)
        .expect("headers");
    assert_eq!(headers["authorization"], "Bearer sk-test");
    assert_eq!(headers["accept"], "text/event-stream");
    assert_eq!(headers["http-referer"], "https://endpoint-proxy.local");
    assert_eq!(headers["x-title"], "Endpoint Proxy");
    let non_stream_headers = openrouter
        .build_headers("gpt-4.1", &connection("openrouter"), false)
        .expect("non-stream headers");
    assert!(non_stream_headers.get("accept").is_none());

    let mut oauth_connection = connection("openai");
    oauth_connection.api_key = None;
    oauth_connection.access_token = Some("oauth-token".into());
    let oauth_headers = DefaultExecutor::new("openai", pool.clone(), None)
        .expect("openai")
        .build_headers("gpt-4.1", &oauth_connection, false)
        .expect("oauth headers");
    assert_eq!(oauth_headers["authorization"], "Bearer oauth-token");

    let anthropic =
        DefaultExecutor::new("anthropic", pool.clone(), None).expect("anthropic executor");
    let anthropic_headers = anthropic
        .build_headers("claude-sonnet-4.5", &connection("anthropic"), false)
        .expect("anthropic headers");
    assert_eq!(anthropic_headers["x-api-key"], "sk-test");
    assert_eq!(
        anthropic_headers["anthropic-beta"],
        "claude-code-20250219,interleaved-thinking-2025-05-14"
    );
    assert!(anthropic_headers.get("authorization").is_none());

    let gemini = DefaultExecutor::new("gemini", pool.clone(), None).expect("gemini executor");
    let gemini_headers = gemini
        .build_headers("gemini-2.5-flash", &connection("gemini"), true)
        .expect("gemini headers");
    assert_eq!(gemini_headers["x-goog-api-key"], "sk-test");
    assert_eq!(gemini_headers["accept"], "text/event-stream");
    assert!(gemini_headers.get("authorization").is_none());

    let cline = DefaultExecutor::new("cline", pool.clone(), None).expect("cline executor");
    let cline_headers = cline
        .build_headers("gpt-5.4", &connection("cline"), false)
        .expect("cline headers");
    assert_eq!(cline_headers["authorization"], "Bearer sk-test");
    assert_eq!(cline_headers["http-referer"], "https://cline.bot");
    assert_eq!(cline_headers["x-title"], "Cline");

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
        .build_headers("claude-sonnet", &connection("anthropic-node"), false)
        .expect("anthropic headers");
    assert_eq!(headers["x-api-key"], "sk-test");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert!(headers.get("anthropic-beta").is_none());

    let mut oauth_connection = connection("anthropic-node");
    oauth_connection.api_key = None;
    oauth_connection.access_token = Some("oauth-token".into());
    let headers = anthropic
        .build_headers("claude-sonnet", &oauth_connection, false)
        .expect("anthropic oauth headers");
    assert_eq!(headers["authorization"], "Bearer oauth-token");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert!(headers.get("x-api-key").is_none());

    let kilocode =
        DefaultExecutor::new("kilocode", Arc::new(ClientPool::new()), None).expect("kilocode");
    let mut kilocode_connection = connection("kilocode");
    kilocode_connection
        .provider_specific_data
        .insert("orgId".into(), serde_json::Value::String("org-42".into()));
    let headers = kilocode
        .build_headers("openai/gpt-4.1", &kilocode_connection, false)
        .expect("kilocode headers");
    assert_eq!(headers["authorization"], "Bearer sk-test");
    assert_eq!(headers["x-kilocode-organizationid"], "org-42");

    let opencode_go = DefaultExecutor::new("opencode-go", Arc::new(ClientPool::new()), None)
        .expect("opencode-go");
    let headers = opencode_go
        .build_headers("kimi-k2.6", &connection("opencode-go"), false)
        .expect("opencode-go openai headers");
    assert_eq!(headers["authorization"], "Bearer sk-test");
    assert!(headers.get("x-api-key").is_none());

    let claude_headers = opencode_go
        .build_headers("minimax-m2.5", &connection("opencode-go"), false)
        .expect("opencode-go claude headers");
    assert_eq!(claude_headers["x-api-key"], "sk-test");
    assert_eq!(claude_headers["anthropic-version"], "2023-06-01");
    assert!(claude_headers.get("authorization").is_none());
}

#[test]
fn default_executor_builds_beta_provider_urls_and_special_headers() {
    let pool = Arc::new(ClientPool::new());

    let groq = DefaultExecutor::new("groq", pool.clone(), None).expect("groq executor");
    assert_eq!(
        groq.build_url("llama-3.3-70b", false, &connection("groq"))
            .expect("groq url"),
        "https://api.groq.com/openai/v1/chat/completions"
    );

    let glm = DefaultExecutor::new("glm", pool.clone(), None).expect("glm executor");
    assert_eq!(
        glm.build_url("glm-5", false, &connection("glm"))
            .expect("glm url"),
        "https://api.z.ai/api/anthropic/v1/messages?beta=true"
    );
    let headers = glm
        .build_headers("glm-5", &connection("glm"), false)
        .expect("glm headers");
    assert_eq!(headers["x-api-key"], "sk-test");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert_eq!(
        headers["anthropic-beta"],
        "claude-code-20250219,interleaved-thinking-2025-05-14"
    );
    assert!(headers.get("authorization").is_none());

    let minimax = DefaultExecutor::new("minimax", pool.clone(), None).expect("minimax executor");
    assert_eq!(
        minimax
            .build_url("minimax-m2.5", false, &connection("minimax"))
            .expect("minimax url"),
        "https://api.minimax.io/anthropic/v1/messages?beta=true"
    );

    let perplexity =
        DefaultExecutor::new("perplexity", pool.clone(), None).expect("perplexity executor");
    assert_eq!(
        perplexity
            .build_url("sonar", false, &connection("perplexity"))
            .expect("perplexity url"),
        "https://api.perplexity.ai/chat/completions"
    );

    let gitlab = DefaultExecutor::new("gitlab", pool, None).expect("gitlab executor");
    assert_eq!(
        gitlab
            .build_url("duo", false, &connection("gitlab"))
            .expect("gitlab url"),
        "https://gitlab.com/api/v4/chat/completions"
    );
}

#[test]
fn default_executor_builds_bearer_headers_for_openai_passthrough_matrix() {
    let pool = Arc::new(ClientPool::new());
    let providers = [
        "xai",
        "mistral",
        "together",
        "fireworks",
        "cerebras",
        "cohere",
        "nebius",
        "siliconflow",
        "hyperbolic",
        "codebuddy",
        "kilocode",
        "opencode-go",
        "glm-cn",
        "alicode",
        "alicode-intl",
        "volcengine-ark",
        "byteplus",
        "nvidia",
    ];

    for provider in providers {
        let executor =
            DefaultExecutor::new(provider, pool.clone(), None).expect("passthrough executor");
        let headers = executor
            .build_headers("gpt-4.1", &connection(provider), true)
            .expect("passthrough headers");
        assert_eq!(
            headers["authorization"], "Bearer sk-test",
            "{provider} auth"
        );
        assert_eq!(headers["accept"], "text/event-stream", "{provider} stream");
        assert!(headers.get("x-api-key").is_none(), "{provider} auth mode");
    }
}

#[test]
fn default_executor_builds_claude_headers_for_compatible_passthrough_matrix() {
    let pool = Arc::new(ClientPool::new());
    let providers = ["glm", "kimi", "minimax", "minimax-cn"];

    for provider in providers {
        let executor =
            DefaultExecutor::new(provider, pool.clone(), None).expect("claude-compatible executor");
        let headers = executor
            .build_headers("claude-sonnet", &connection(provider), false)
            .expect("claude-compatible headers");
        assert_eq!(
            headers["x-api-key"], "sk-test",
            "{provider} credential header"
        );
        assert_eq!(
            headers["anthropic-version"], "2023-06-01",
            "{provider} version header"
        );
        assert_eq!(
            headers["anthropic-beta"], "claude-code-20250219,interleaved-thinking-2025-05-14",
            "{provider} beta header"
        );
        assert!(
            headers.get("authorization").is_none(),
            "{provider} auth mode"
        );
    }
}

#[test]
fn default_executor_transform_request_is_passthrough() {
    let pool = Arc::new(ClientPool::new());
    let executor = DefaultExecutor::new("openai", pool, None).expect("openai executor");
    let body = json!({
        "model": "gpt-4.1",
        "stream": true,
        "messages": [{"role": "user", "content": "hello"}]
    });

    assert_eq!(executor.transform_request(&body), body);
}

#[tokio::test]
async fn default_executor_execute_posts_expected_request() {
    let upstream = MockServer::start().await;
    let request_body = json!({
        "model": "gpt-4.1",
        "stream": true,
        "messages": [{"role": "user", "content": "hello"}]
    });

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .and(header("accept", "text/event-stream"))
        .and(body_json(request_body.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&upstream)
        .await;

    let provider_node = ProviderNode {
        id: "node-openai".into(),
        r#type: "openai-compatible".into(),
        name: "Node".into(),
        prefix: Some("custom".into()),
        api_type: Some("chat".into()),
        base_url: Some(format!("{}/v1", upstream.uri())),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };

    let executor = DefaultExecutor::new(
        "node-openai",
        Arc::new(ClientPool::new()),
        Some(provider_node),
    )
    .expect("compatible executor");

    let response = executor
        .execute(ExecutionRequest {
            model: "gpt-4.1".into(),
            body: request_body.clone(),
            stream: true,
            credentials: connection("node-openai"),
            proxy: None,
        })
        .await
        .expect("execute request");

    assert_eq!(
        response.url,
        format!("{}/v1/chat/completions", upstream.uri())
    );
    assert_eq!(response.transformed_body, request_body);
    assert_eq!(response.headers["authorization"], "Bearer sk-test");
    assert_eq!(response.response.status(), 200);
    assert_eq!(response.transport, TransportKind::Hyper);
}

#[tokio::test]
async fn default_executor_execute_uses_reqwest_when_proxy_present() {
    let upstream = MockServer::start().await;
    let request_body = json!({
        "model": "gpt-4.1",
        "stream": true,
        "messages": [{"role": "user", "content": "hello"}]
    });

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .and(body_json(request_body.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&upstream)
        .await;

    let provider_node = ProviderNode {
        id: "node-openai".into(),
        r#type: "openai-compatible".into(),
        name: "Node".into(),
        prefix: Some("custom".into()),
        api_type: Some("chat".into()),
        base_url: Some(format!("{}/v1", upstream.uri())),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };

    let executor = DefaultExecutor::new(
        "node-openai",
        Arc::new(ClientPool::new()),
        Some(provider_node),
    )
    .expect("compatible executor");

    let response = executor
        .execute(ExecutionRequest {
            model: "gpt-4.1".into(),
            body: request_body,
            stream: true,
            credentials: connection("node-openai"),
            proxy: Some(ProxyTarget {
                url: "http://127.0.0.1:9".into(),
                no_proxy: "127.0.0.1,localhost".into(),
                strict_proxy: false,
                pool_id: Some("proxy-a".into()),
            }),
        })
        .await
        .expect("execute request");

    assert_eq!(response.response.status(), 200);
    assert_eq!(response.transport, TransportKind::Reqwest);
}

#[tokio::test]
async fn default_executor_execute_uses_reqwest_for_responses_api() {
    let upstream = MockServer::start().await;
    let request_body = json!({
        "model": "gpt-4.1",
        "input": [{"role": "user", "content": "hello"}]
    });

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer sk-test"))
        .and(body_json(request_body.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "resp_1"})))
        .expect(1)
        .mount(&upstream)
        .await;

    let provider_node = ProviderNode {
        id: "node-openai".into(),
        r#type: "openai-compatible".into(),
        name: "Node".into(),
        prefix: Some("custom".into()),
        api_type: Some("responses".into()),
        base_url: Some(format!("{}/v1", upstream.uri())),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };
    let mut credentials = connection("node-openai");
    credentials.provider_specific_data.insert(
        "apiType".into(),
        serde_json::Value::String("responses".into()),
    );

    let executor = DefaultExecutor::new(
        "node-openai",
        Arc::new(ClientPool::new()),
        Some(provider_node),
    )
    .expect("compatible executor");

    let response = executor
        .execute(ExecutionRequest {
            model: "gpt-4.1".into(),
            body: request_body,
            stream: false,
            credentials,
            proxy: None,
        })
        .await
        .expect("execute request");

    assert_eq!(response.response.status(), 200);
    assert_eq!(response.transport, TransportKind::Reqwest);
}

#[test]
fn default_executor_reports_missing_credentials_and_invalid_headers() {
    let pool = Arc::new(ClientPool::new());
    let unknown = match DefaultExecutor::new("unknown-provider", pool.clone(), None) {
        Ok(_) => panic!("unknown provider should fail"),
        Err(error) => error,
    };
    assert!(matches!(
        unknown,
        ExecutorError::UnsupportedProvider(provider) if provider == "unknown-provider"
    ));

    let executor = DefaultExecutor::new("openai", pool, None).expect("openai executor");

    let mut missing = connection("openai");
    missing.api_key = None;
    let error = executor
        .build_headers("gpt-4.1", &missing, false)
        .expect_err("missing credentials should fail");
    assert!(matches!(error, ExecutorError::MissingCredentials(provider) if provider == "openai"));

    let mut invalid = connection("openai");
    invalid.api_key = Some("bad\nkey".into());
    let error = executor
        .build_headers("gpt-4.1", &invalid, false)
        .expect_err("invalid header should fail");
    assert!(matches!(error, ExecutorError::InvalidHeader(_)));

    let anthropic_node = ProviderNode {
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
    let anthropic = DefaultExecutor::new(
        "anthropic-node",
        Arc::new(ClientPool::new()),
        Some(anthropic_node),
    )
    .expect("anthropic executor");

    let mut anthropic_missing = connection("anthropic-node");
    anthropic_missing.api_key = None;
    let error = anthropic
        .build_headers("claude-sonnet", &anthropic_missing, false)
        .expect_err("anthropic missing credentials should fail");
    assert!(matches!(
        error,
        ExecutorError::MissingCredentials(provider) if provider == "anthropic-node"
    ));

    let mut anthropic_invalid = connection("anthropic-node");
    anthropic_invalid.api_key = Some("bad\nkey".into());
    let error = anthropic
        .build_headers("claude-sonnet", &anthropic_invalid, false)
        .expect_err("anthropic invalid api key should fail");
    assert!(matches!(error, ExecutorError::InvalidHeader(_)));

    let cloudflare = DefaultExecutor::new("cloudflare-ai", Arc::new(ClientPool::new()), None)
        .expect("cloudflare");
    let error = cloudflare
        .build_url(
            "@cf/moonshotai/kimi-k2.6",
            false,
            &connection("cloudflare-ai"),
        )
        .expect_err("cloudflare missing accountId should fail");
    assert!(matches!(
        error,
        ExecutorError::MissingProviderSpecificData(provider, field)
            if provider == "cloudflare-ai" && field == "accountId"
    ));
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
fn hyper_client_pool_reuses_same_provider_key() {
    let pool = ClientPool::new();
    let openai = pool
        .get_hyper_direct("openai")
        .expect("openai hyper client");
    let openai_again = pool
        .get_hyper_direct("openai")
        .expect("openai hyper client again");
    let groq = pool.get_hyper_direct("groq").expect("groq hyper client");

    assert!(Arc::ptr_eq(&openai, &openai_again));
    assert!(!Arc::ptr_eq(&openai, &groq));
    assert_eq!(pool.hyper_len(), 2);
}

#[test]
fn client_pool_reuses_single_client_under_concurrent_same_provider_requests() {
    let pool = Arc::new(ClientPool::new());
    let barrier = Arc::new(Barrier::new(16));
    let build_count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    for _ in 0..16 {
        let pool = pool.clone();
        let barrier = barrier.clone();
        let build_count = build_count.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            pool.get_or_insert_with("openai", None, || {
                build_count.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(25));
                reqwest::Client::builder().build().map(Arc::new)
            })
            .expect("shared client")
        }));
    }

    let clients: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread joined"))
        .collect();

    let first = &clients[0];
    assert!(clients.iter().all(|client| Arc::ptr_eq(client, first)));
    assert_eq!(pool.len(), 1);
    assert_eq!(build_count.load(Ordering::SeqCst), 1);

    let cache_hit_build_count = Arc::new(AtomicUsize::new(0));
    let cached = pool
        .get_or_insert_with("openai", None, || {
            cache_hit_build_count.fetch_add(1, Ordering::SeqCst);
            reqwest::Client::builder().build().map(Arc::new)
        })
        .expect("cached client");
    assert!(Arc::ptr_eq(first, &cached));
    assert_eq!(cache_hit_build_count.load(Ordering::SeqCst), 0);
}

#[test]
fn client_pool_exposes_documented_tuning() {
    assert_eq!(CLIENT_POOL_IDLE_TIMEOUT, Duration::from_secs(90));
    assert_eq!(CLIENT_POOL_MAX_IDLE_PER_HOST, 8);
    assert_eq!(CLIENT_POOL_TCP_KEEPALIVE, Duration::from_secs(60));
}

#[test]
fn client_pool_isolates_concurrent_provider_and_proxy_keys() {
    let pool = Arc::new(ClientPool::new());
    let barrier = Arc::new(Barrier::new(24));
    let mut handles = Vec::new();

    for _ in 0..8 {
        let pool = pool.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            pool.get("openai", None).expect("openai client")
        }));
    }

    for _ in 0..8 {
        let pool = pool.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            pool.get("groq", None).expect("groq client")
        }));
    }

    for _ in 0..8 {
        let pool = pool.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            pool.get(
                "openai",
                Some(&ProxyTarget {
                    url: "http://127.0.0.1:8080".into(),
                    no_proxy: "localhost".into(),
                    strict_proxy: false,
                    pool_id: Some("pool-a".into()),
                }),
            )
            .expect("proxied openai client")
        }));
    }

    let clients: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread joined"))
        .collect();

    let openai = &clients[0];
    let groq = &clients[8];
    let proxied_openai = &clients[16];

    assert!(clients[..8]
        .iter()
        .all(|client| Arc::ptr_eq(client, openai)));
    assert!(clients[8..16]
        .iter()
        .all(|client| Arc::ptr_eq(client, groq)));
    assert!(clients[16..]
        .iter()
        .all(|client| Arc::ptr_eq(client, proxied_openai)));

    assert!(!Arc::ptr_eq(openai, groq));
    assert!(!Arc::ptr_eq(openai, proxied_openai));
    assert!(!Arc::ptr_eq(groq, proxied_openai));
    assert_eq!(pool.len(), 3);
}

#[test]
fn client_pool_fingerprint_includes_strict_proxy_and_pool_id() {
    let pool = ClientPool::new();
    let strict_false = pool
        .get(
            "openai",
            Some(&ProxyTarget {
                url: "http://127.0.0.1:8080".into(),
                no_proxy: String::new(),
                strict_proxy: false,
                pool_id: None,
            }),
        )
        .expect("strict false client");
    let strict_true = pool
        .get(
            "openai",
            Some(&ProxyTarget {
                url: "http://127.0.0.1:8080".into(),
                no_proxy: String::new(),
                strict_proxy: true,
                pool_id: None,
            }),
        )
        .expect("strict true client");
    let pool_a = pool
        .get(
            "openai",
            Some(&ProxyTarget {
                url: "http://127.0.0.1:8080".into(),
                no_proxy: String::new(),
                strict_proxy: false,
                pool_id: Some("pool-a".into()),
            }),
        )
        .expect("pool A client");
    let pool_b = pool
        .get(
            "openai",
            Some(&ProxyTarget {
                url: "http://127.0.0.1:8080".into(),
                no_proxy: String::new(),
                strict_proxy: false,
                pool_id: Some("pool-b".into()),
            }),
        )
        .expect("pool B client");

    assert!(!Arc::ptr_eq(&strict_false, &strict_true));
    assert!(!Arc::ptr_eq(&strict_false, &pool_a));
    assert!(!Arc::ptr_eq(&pool_a, &pool_b));
    assert_eq!(pool.len(), 4);
}

#[test]
fn client_pool_keeps_proxied_providers_isolated_even_with_same_proxy() {
    let pool = ClientPool::new();
    let proxy = ProxyTarget {
        url: "http://127.0.0.1:8080".into(),
        no_proxy: "localhost".into(),
        strict_proxy: false,
        pool_id: Some("shared-proxy".into()),
    };

    let openai = pool.get("openai", Some(&proxy)).expect("openai client");
    let groq = pool.get("groq", Some(&proxy)).expect("groq client");

    assert!(!Arc::ptr_eq(&openai, &groq));
    assert_eq!(pool.len(), 2);
}

#[tokio::test]
async fn client_pool_reuses_connection_for_sequential_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind keepalive server");
    let addr = listener.local_addr().expect("keepalive server addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_counter = accepted.clone();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept first connection");
        accepted_counter.fetch_add(1, Ordering::SeqCst);
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        for _ in 0..2 {
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read request line");
            assert!(request_line.starts_with("GET /health HTTP/1.1"));

            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("read header line");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }

            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
                )
                .expect("write response");
            stream.flush().expect("flush response");
        }
    });

    let pool = ClientPool::new();
    let client = pool.get("openai", None).expect("openai client");
    let url = format!("http://{addr}/health");

    let first = client
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .expect("first request")
        .text()
        .await
        .expect("first body");
    let second = client
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .expect("second request")
        .text()
        .await
        .expect("second body");

    assert_eq!(first, "ok");
    assert_eq!(second, "ok");
    server.join().expect("server joined");
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn default_executor_reuses_hyper_connection_for_sequential_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind hyper keepalive server");
    let addr = listener.local_addr().expect("hyper keepalive server addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_counter = accepted.clone();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept first connection");
        accepted_counter.fetch_add(1, Ordering::SeqCst);
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        for _ in 0..2 {
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read request line");
            assert!(request_line.starts_with("POST /v1/chat/completions HTTP/1.1"));

            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("read header line");
                if line == "\r\n" || line.is_empty() {
                    break;
                }

                let lower = line.to_ascii_lowercase();
                if let Some(length) = lower.strip_prefix("content-length:") {
                    content_length = length.trim().parse().expect("parse content-length");
                }
            }

            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).expect("read request body");
            let json: serde_json::Value =
                serde_json::from_slice(&body).expect("decode request body");
            assert_eq!(json["model"], "gpt-4.1");

            let payload = br#"{"ok":true}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                        payload.len()
                    )
                    .as_bytes(),
                )
                .expect("write response headers");
            stream.write_all(payload).expect("write response body");
            stream.flush().expect("flush response");
        }
    });

    let provider_node = ProviderNode {
        id: "node-openai".into(),
        r#type: "openai-compatible".into(),
        name: "Node".into(),
        prefix: Some("custom".into()),
        api_type: Some("chat".into()),
        base_url: Some(format!("http://{addr}/v1")),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    };
    let pool = Arc::new(ClientPool::new());
    let executor = DefaultExecutor::new("node-openai", pool.clone(), Some(provider_node))
        .expect("compatible executor");
    let request_body = json!({
        "model": "gpt-4.1",
        "stream": true,
        "messages": [{"role": "user", "content": "hello"}]
    });

    for _ in 0..2 {
        let response = executor
            .execute(ExecutionRequest {
                model: "gpt-4.1".into(),
                body: request_body.clone(),
                stream: true,
                credentials: connection("node-openai"),
                proxy: None,
            })
            .await
            .expect("execute request");
        assert_eq!(response.transport, TransportKind::Hyper);
        assert_eq!(response.response.status(), 200);
    }

    server.join().expect("server joined");
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    assert_eq!(pool.hyper_len(), 1);
}

#[tokio::test]
async fn client_pool_keeps_provider_traffic_independent_under_concurrent_requests() {
    let openai_listener = TcpListener::bind("127.0.0.1:0").expect("bind openai server");
    let openai_addr = openai_listener.local_addr().expect("openai server addr");
    let groq_listener = TcpListener::bind("127.0.0.1:0").expect("bind groq server");
    let groq_addr = groq_listener.local_addr().expect("groq server addr");
    let (openai_started_tx, openai_started_rx) = oneshot::channel();
    let (release_openai_tx, release_openai_rx) = oneshot::channel();

    let openai_server = thread::spawn(move || {
        let (mut stream, _) = openai_listener.accept().expect("accept openai connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set openai read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("clone openai stream"));
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .expect("read openai request line");
        assert!(request_line.starts_with("GET /slow HTTP/1.1"));

        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .expect("read openai header line");
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }

        openai_started_tx.send(()).expect("signal openai start");
        release_openai_rx
            .blocking_recv()
            .expect("wait for openai release");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nopenai-ok",
            )
            .expect("write openai response");
        stream.flush().expect("flush openai response");
    });

    let groq_server = thread::spawn(move || {
        let (mut stream, _) = groq_listener.accept().expect("accept groq connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set groq read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("clone groq stream"));
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .expect("read groq request line");
        assert!(request_line.starts_with("GET /fast HTTP/1.1"));

        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("read groq header line");
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }

        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\ngroq-ok")
            .expect("write groq response");
        stream.flush().expect("flush groq response");
    });

    let pool = ClientPool::new();
    let openai_client = pool.get("openai", None).expect("openai client");
    let groq_client = pool.get("groq", None).expect("groq client");
    let openai_url = format!("http://{openai_addr}/slow");
    let groq_url = format!("http://{groq_addr}/fast");

    let openai_task = tokio::spawn(async move {
        openai_client
            .get(&openai_url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .expect("openai request")
            .text()
            .await
            .expect("openai body")
    });

    openai_started_rx.await.expect("openai request started");

    let groq_body = groq_client
        .get(&groq_url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .expect("groq request")
        .text()
        .await
        .expect("groq body");

    assert_eq!(groq_body, "groq-ok");
    assert!(!openai_task.is_finished());

    release_openai_tx
        .send(())
        .expect("release delayed openai response");
    assert_eq!(openai_task.await.expect("openai task joined"), "openai-ok");
    openai_server.join().expect("openai server joined");
    groq_server.join().expect("groq server joined");
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
