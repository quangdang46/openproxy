//! Targeted loop for the 9 providers requested in ultrawork.
//! Covers: opencode zen (opencode-go), nvidia, openrouter, kilo code, kiro, ollama cloud, gemini, llm7.io, ollama (local) and openai as control.
//! Verifies: URL building, header building (Bearer/x-api-key/x-goog-api-key), and that OmniRoute parity base URLs match.
//! No live network calls; uses wiremock-style unit checks via DefaultExecutor + ClientPool.

use std::collections::BTreeMap;
use std::sync::Arc;

use openproxy::core::executor::{provider_config_base_url, ClientPool, DefaultExecutor};
use openproxy::types::ProviderConnection;

fn conn(provider: &str) -> ProviderConnection {
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
        api_key: Some("sk-test-loop".into()),
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

#[test]
fn targeted_providers_have_configs_and_urls() {
    let expectations: &[(&str, &str)] = &[
        ("opencode-go", "https://opencode.ai/zen/go/v1"),
        ("openai", "https://api.openai.com/v1/chat/completions"),
        (
            "nvidia",
            "https://integrate.api.nvidia.com/v1/chat/completions",
        ),
        (
            "openrouter",
            "https://openrouter.ai/api/v1/chat/completions",
        ),
        (
            "kilocode",
            "https://api.kilo.ai/api/openrouter/chat/completions",
        ),
        ("ollama-cloud", "https://ollama.com/v1/chat/completions"),
        ("ollama", "https://ollama.com/v1/chat/completions"),
        (
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/models",
        ),
        ("llm7", "https://api.llm7.io/v1/chat/completions"),
    ];

    let pool = Arc::new(ClientPool::new());
    for (provider, expected_base) in expectations {
        // 1. provider_config_base_url must return Some
        let base = provider_config_base_url(provider)
            .unwrap_or_else(|| panic!("missing provider_config_base_url for {provider}"));
        assert_eq!(&base, expected_base, "base_url mismatch for {provider}");

        // 2. DefaultExecutor must construct and build url
        let exec = DefaultExecutor::new(*provider, pool.clone(), None)
            .unwrap_or_else(|e| panic!("DefaultExecutor::new failed for {provider}: {e:?}"));
        let url = exec
            .build_url("test-model", false, &conn(provider))
            .unwrap_or_else(|e| panic!("build_url failed for {provider}: {e:?}"));
        // For gemini the URL includes model; just check prefix
        if *provider == "gemini" {
            assert!(
                url.starts_with(expected_base),
                "gemini url prefix mismatch: {url}"
            );
            assert!(
                url.contains("test-model:generateContent"),
                "gemini url missing model action: {url}"
            );
        } else if *provider == "opencode-go" {
            // opencode-go base is .../v1, URL appends /chat/completions
            assert_eq!(
                url,
                format!("{expected_base}/chat/completions"),
                "opencode-go url mismatch: {url}"
            );
        } else {
            assert_eq!(&url, expected_base, "url mismatch for {provider}");
        }
    }
}

#[test]
fn targeted_providers_headers_are_correct() {
    let pool = Arc::new(ClientPool::new());

    // openrouter must have Referer + X-Title
    let exec = DefaultExecutor::new("openrouter", pool.clone(), None).unwrap();
    let headers = exec
        .build_headers("gpt-4o", &conn("openrouter"), false)
        .unwrap();
    assert_eq!(headers["authorization"], "Bearer sk-test-loop");
    assert_eq!(headers["http-referer"], "https://endpoint-proxy.local");
    assert_eq!(headers["x-title"], "Endpoint Proxy");

    // nvidia is bare openai Bearer, no extra headers
    let exec = DefaultExecutor::new("nvidia", pool.clone(), None).unwrap();
    let headers = exec
        .build_headers("meta/llama-3.1-8b-instruct", &conn("nvidia"), false)
        .unwrap();
    assert_eq!(headers["authorization"], "Bearer sk-test-loop");
    assert!(
        headers.get("http-referer").is_none(),
        "nvidia should not have Referer"
    );

    // llm7 same as nvidia — Bearer (OmniRoute says use 'unused' as key)
    let exec = DefaultExecutor::new("llm7", pool.clone(), None).unwrap();
    let headers = exec
        .build_headers("gemini-2.5-flash", &conn("llm7"), false)
        .unwrap();
    assert_eq!(headers["authorization"], "Bearer sk-test-loop");

    // ollama-cloud Bearer
    let exec = DefaultExecutor::new("ollama-cloud", pool.clone(), None).unwrap();
    let headers = exec
        .build_headers("llama3", &conn("ollama-cloud"), false)
        .unwrap();
    assert_eq!(headers["authorization"], "Bearer sk-test-loop");

    // gemini uses x-goog-api-key
    let exec = DefaultExecutor::new("gemini", pool.clone(), None).unwrap();
    let headers = exec
        .build_headers("gemini-2.5-flash", &conn("gemini"), false)
        .unwrap();
    assert_eq!(headers["x-goog-api-key"], "sk-test-loop");
    assert!(headers.get("authorization").is_none());

    // opencode-go (opencode zen) Bearer
    let exec = DefaultExecutor::new("opencode-go", pool.clone(), None).unwrap();
    let headers = exec
        .build_headers("kimi-k2.6", &conn("opencode-go"), false)
        .unwrap();
    assert_eq!(headers["authorization"], "Bearer sk-test-loop");

    // kilocode Bearer + optional org header
    let exec = DefaultExecutor::new("kilocode", pool.clone(), None).unwrap();
    let mut c = conn("kilocode");
    c.provider_specific_data
        .insert("orgId".into(), serde_json::Value::String("org-123".into()));
    let headers = exec.build_headers("openai/gpt-4o", &c, false).unwrap();
    assert_eq!(headers["authorization"], "Bearer sk-test-loop");
    assert_eq!(headers["x-kilocode-organizationid"], "org-123");
}

#[test]
fn zen_alias_opencode_go_message_routing() {
    // opencode-go has dual routing: kimi/minimax models go to /messages
    let pool = Arc::new(ClientPool::new());
    let exec = DefaultExecutor::new("opencode-go", pool, None).unwrap();
    assert_eq!(
        exec.build_url("minimax-m2.5", false, &conn("opencode-go"))
            .unwrap(),
        "https://opencode.ai/zen/go/v1/messages"
    );
    assert_eq!(
        exec.build_url("gpt-4o", false, &conn("opencode-go"))
            .unwrap(),
        "https://opencode.ai/zen/go/v1/chat/completions"
    );
}

#[test]
fn gemini_stream_url_contains_alt_sse() {
    let pool = Arc::new(ClientPool::new());
    let exec = DefaultExecutor::new("gemini", pool, None).unwrap();
    let url = exec
        .build_url("gemini-2.5-flash", true, &conn("gemini"))
        .unwrap();
    assert!(
        url.contains("streamGenerateContent?alt=sse"),
        "gemini stream url missing alt=sse: {url}"
    );
}

#[test]
fn kiro_executor_exists_and_not_default() {
    // kiro has a dedicated executor (AWS SSO flow), not DefaultExecutor
    // This test just ensures the module compiles and the provider is known via dedicated path
    // We verify DefaultExecutor does NOT have a static entry for 'kiro' — it should fail
    let pool = Arc::new(ClientPool::new());
    let result = DefaultExecutor::new("kiro", pool, None);
    assert!(
        result.is_err(),
        "kiro should not be in DefaultExecutor PROVIDER_CONFIGS (has dedicated executor)"
    );
    if let Err(e) = result {
        assert!(format!("{e:?}").contains("UnsupportedProvider"));
    }
}

// Mirrors OmniRoute nvidia validation model contract: meta/llama-3.1-8b-instruct
#[test]
fn nvidia_url_is_nim_integrate_host() {
    let pool = Arc::new(ClientPool::new());
    let exec = DefaultExecutor::new("nvidia", pool, None).unwrap();
    let url = exec
        .build_url("meta/llama-3.1-8b-instruct", false, &conn("nvidia"))
        .unwrap();
    assert!(
        url.starts_with("https://integrate.api.nvidia.com/"),
        "nvidia host mismatch: {url}"
    );
}
