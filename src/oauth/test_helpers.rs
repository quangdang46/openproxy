//! Shared test utilities for OAuth tests.

use crate::oauth::OAuthProviderConfig;

/// The 9 providers known to `providers::get_config`: PKCE + device-code.
pub const ALL_PROVIDERS: &[&str] = &[
    "claude", "codex", "gitlab", "xai",
    "github", "kiro", "kimi-coding", "kilocode", "codebuddy",
];

/// Provider -> expected scopes (as sorted space-separated string).
pub fn expected_scopes(provider: &str) -> &'static str {
    match provider {
        "claude" => "read connect",
        "codex" => "openid profile email",
        "gitlab" => "api read_user",
        "xai" => "openid profile email openai:write:grok-cli:access",
        "github" => "read:user repo",
        "kiro" => "openid profile",
        "kimi-coding" => "kimi:read",
        "kilocode" => "read",
        "codebuddy" => "read",
        _ => "",
    }
}

/// Provider -> expected authorize URL prefix.
pub fn expected_auth_url_prefix(provider: &str) -> &'static str {
    match provider {
        "claude" => "https://auth.claude.ai/authorize",
        "codex" => "https://codex.ai/oauth/authorize",
        "gitlab" => "https://gitlab.com/oauth/authorize",
        "xai" => "https://auth.x.ai/oauth2/authorize",
        "github" => "https://github.com/login/device/code",
        "kiro" => "https://kiro.ai/oauth/device/code",
        "kimi-coding" => "https://api.moonshot.cn/kimi-device/oauth/device/code",
        "kilocode" => "https://api.kilo.ai/oauth/device/code",
        "codebuddy" => "https://copilot.tencent.com/oauth/device/code",
        _ => "",
    }
}

/// Assert the config's scopes match the expected set (order-independent).
pub fn assert_scopes_match(config: &OAuthProviderConfig, expected: &str) {
    let mut got: Vec<&str> = config.scopes.iter().map(|s| s.as_str()).collect();
    got.sort();
    let mut want: Vec<&str> = expected.split_whitespace().collect();
    want.sort();
    assert_eq!(
        got.join(" "),
        want.join(" "),
        "scope mismatch"
    );
}

/// Assert the auth URL starts with the expected prefix and contains client_id.
pub fn assert_auth_url_shape(url: &str, expected_prefix: &str, client_id: &str) {
    assert!(
        url.starts_with(expected_prefix),
        "URL should start with {expected_prefix}, got: {url}"
    );
    assert!(
        url.contains(&format!("client_id={}", client_id)),
        "URL should contain client_id={client_id}, got: {url}"
    );
}

/// Assert that every expected scope word appears in the URL's scope parameter.
/// The URL joins scopes with `+` (URL-encoded space), so we check the combined
/// scope param value.
pub fn assert_scopes_in_url(url: &str, expected_scopes: &str) {
    // Extract the scope parameter value from the URL
    let url_part = if let Some(pos) = url.find("?") {
        &url[pos..]
    } else {
        url
    };
    // Find the scope= parameter
    let scope_param = url_part
        .split('&')
        .find(|p| p.starts_with("scope="))
        .expect("URL must contain a scope parameter");
    let scope_value = scope_param.strip_prefix("scope=").unwrap_or("");
    let decoded = urlencoding::decode(scope_value).unwrap_or_else(|_| std::borrow::Cow::Borrowed(scope_value));

    for scope in expected_scopes.split_whitespace() {
        assert!(
            decoded.contains(scope) || scope_value.contains(scope),
            "scope param value ({scope_value}) should contain {scope}"
        );
    }
}
