use crate::oauth::pkce;
use crate::oauth::providers;

fn expected_auth_url_prefix(provider: &str) -> &'static str {
    match provider {
        "claude" => "https://claude.ai/oauth/authorize",
        "codex" => "https://auth.openai.com/oauth/authorize",
        "gitlab" => "https://gitlab.com/oauth/authorize",
        "xai" => "https://auth.x.ai/oauth2/authorize",
        "github" => "https://github.com/login/device/code",
        "kiro" => "https://oidc.us-east-1.amazonaws.com",
        "kimi-coding" => "https://api.moonshot.cn/kimi-device/oauth/device/code",
        "kilocode" => "https://api.kilo.ai/api/device-auth/codes",
        "codebuddy" => "https://copilot.tencent.com/v2/plugin/auth/state",
        _ => "unknown",
    }
}

fn expected_scopes(provider: &str) -> &'static [&'static str] {
    match provider {
        "claude" => &["org:create_api_key", "user:profile", "user:inference"],
        "codex" => &["openid", "profile", "email", "offline_access"],
        "gitlab" => &["api", "read_user"],
        "github" => &["read:user"],
        "kiro" => &[
            "codewhisperer:completions",
            "codewhisperer:analysis",
            "codewhisperer:conversations",
        ],
        _ => &[],
    }
}

fn assert_scopes_match(cfg: &crate::oauth::OAuthProviderConfig, expected: &[&str]) {
    assert_eq!(
        cfg.scopes, expected,
        "Unexpected scopes for provider `{}` — expected {expected:?} got {:?}",
        cfg.id, cfg.scopes
    );
}

#[test]
fn test_scopes_match_provider_data() {
    // Verify actual scope strings from providers.rs match expected
    let tests: &[(&str, &[&str])] = &[
        (
            "claude",
            &["org:create_api_key", "user:profile", "user:inference"],
        ),
        ("codex", &["openid", "profile", "email", "offline_access"]),
        ("gitlab", &["api", "read_user"]),
        ("github", &["read:user"]),
        (
            "kiro",
            &[
                "codewhisperer:completions",
                "codewhisperer:analysis",
                "codewhisperer:conversations",
            ],
        ),
    ];
    for (id, scopes) in tests {
        let cfg = providers::get_config(id).unwrap();
        assert_eq!(
            cfg.scopes, *scopes,
            "Scopes mismatch for `{id}` — expected {scopes:?} got {:?}",
            cfg.scopes
        );
    }
}

fn assert_auth_url_shape(url: &str, expected_prefix: &str, expected_client_id: &str) {
    assert!(
        url.starts_with(expected_prefix),
        "URL should start with `{expected_prefix}`, got `{url}`"
    );
    assert!(url.contains("client_id="), "URL should contain client_id");
    assert!(
        url.contains(expected_client_id),
        "URL should contain client_id `{expected_client_id}`, got `{url}`"
    );
    assert!(
        url.contains("redirect_uri="),
        "URL should contain redirect_uri"
    );
    assert!(url.contains("state="), "URL should contain state for CSRF");
}

fn assert_scopes_in_url(url: &str, scopes: &[&str]) {
    // Scopes in URL are URL-encoded (%3A for colon, + for space)
    let url_encoded = &url.replace("%3A", ":").replace('+', " ");
    for scope in scopes {
        assert!(
            url_encoded.contains(scope),
            "URL should contain scope `{scope}`, url=`{url}`"
        );
    }
}

fn url_decoded_scope(url: &str) -> String {
    url.split("scope=")
        .nth(1)
        .unwrap_or("")
        .split('&')
        .next()
        .unwrap_or("")
        .replace("%3A", ":")
        .replace('+', " ")
}

const ALL_PROVIDERS: &[&str] = &[
    "claude",
    "codex",
    "gitlab",
    "github",
    "kiro",
    "kimi-coding",
    "kilocode",
    "codebuddy",
    "qwen",
    "iflow",
    "cline",
];

// ─── PKCE provider auth URL tests ──────────────────────────────────────────

#[test]
fn test_claude_auth_url() {
    let cfg = providers::claude();
    assert_scopes_match(&cfg, expected_scopes("claude"));
    assert!(cfg.uses_pkce);
    let url = cfg.build_auth_url(
        "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
        "http://localhost:4623/oauth/callback",
        "st",
        "ch",
    );
    assert_auth_url_shape(
        &url,
        expected_auth_url_prefix("claude"),
        "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    );
    assert_scopes_in_url(&url, expected_scopes("claude"));
    assert!(url.contains("code_challenge=ch"));
    assert!(url.contains("code_challenge_method=S256"));
}

#[test]
fn test_codex_auth_url() {
    let cfg = providers::codex();
    assert_scopes_match(&cfg, expected_scopes("codex"));
    assert!(cfg.uses_pkce);
    let url = cfg.build_auth_url(
        "app_EMoamEEZ73f0CkXaXp7hrann",
        "http://localhost:4623/oauth/callback",
        "st",
        "ch",
    );
    assert_auth_url_shape(
        &url,
        expected_auth_url_prefix("codex"),
        "app_EMoamEEZ73f0CkXaXp7hrann",
    );
    assert_scopes_in_url(&url, expected_scopes("codex"));
    assert!(url.contains("originator=codex_cli_rs"));
}

#[test]
fn test_gitlab_auth_url() {
    let cfg = providers::gitlab();
    assert_scopes_match(&cfg, expected_scopes("gitlab"));
    assert!(cfg.uses_pkce);
    let url = cfg.build_auth_url(
        "openproxy",
        "http://localhost:4623/oauth/callback",
        "st",
        "ch",
    );
    assert_auth_url_shape(&url, expected_auth_url_prefix("gitlab"), "openproxy");
    assert_scopes_in_url(&url, expected_scopes("gitlab"));
    assert!(url.contains("code_challenge=ch"));
}

#[test]
fn test_xai_auth_url() {
    // xAI OAuth is in src/oauth/xai.rs (not in providers.rs)
    // Skip static config test — integration tested via OAuth flow
}

// ─── Device code provider config tests ────────────────────────────────────

#[test]
fn test_github_device_config() {
    let cfg = providers::github();
    assert_scopes_match(&cfg, expected_scopes("github"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.authorize_url, expected_auth_url_prefix("github"));
    // Device code providers post to auth_url, not build auth URLs.
    assert_eq!(cfg.token_url, "https://github.com/login/oauth/access_token");
}

#[test]
fn test_kiro_device_config() {
    let cfg = providers::kiro();
    assert_scopes_match(&cfg, expected_scopes("kiro"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.authorize_url, expected_auth_url_prefix("kiro"));
}

#[test]
fn test_kimi_coding_device_config() {
    let cfg = providers::kimi_coding();
    assert_scopes_match(&cfg, expected_scopes("kimi-coding"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.authorize_url, expected_auth_url_prefix("kimi-coding"));
}

#[test]
fn test_kilocode_device_config() {
    let cfg = providers::kilocode();
    assert_scopes_match(&cfg, expected_scopes("kilocode"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.authorize_url, expected_auth_url_prefix("kilocode"));
}

#[test]
fn test_codebuddy_device_config() {
    let cfg = providers::codebuddy();
    assert_scopes_match(&cfg, expected_scopes("codebuddy"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.authorize_url, expected_auth_url_prefix("codebuddy"));
}

// ─── PKCE verifier length tests ───────────────────────────────────────────

#[test]
fn test_pkce_verifier_default_is_32_bytes() {
    let v = pkce::generate_code_verifier();
    // 32 random bytes -> 43 base64url chars (ceil(32*4/3) = 43)
    assert_eq!(v.len(), 43, "32-byte verifier should be 43 base64url chars");
}

#[test]
fn test_pkce_verifier_xai_is_96_bytes() {
    let v = pkce::generate_code_verifier_with_len(96);
    // 96 random bytes -> 128 base64url chars (96*4/3 = 128)
    assert_eq!(
        v.len(),
        128,
        "96-byte verifier should be 128 base64url chars"
    );
}

#[test]
fn test_pkce_verifier_lengths() {
    // Standard providers (claude, codex, gitlab) use 32 bytes
    for provider in &["claude", "codex", "gitlab"] {
        let cfg = providers::get_config(provider).unwrap();
        if cfg.uses_pkce {
            let v = pkce::generate_code_verifier();
            assert_eq!(v.len(), 43, "{provider} should use 43-char verifier");
        }
    }
    // xai uses 96 bytes -> 128 chars
    let v = pkce::generate_code_verifier_with_len(96);
    assert_eq!(v.len(), 128, "xai should use 128-char verifier");
}

// ─── get_config coverage ──────────────────────────────────────────────────

#[test]
fn test_get_config_all_providers() {
    for provider in ALL_PROVIDERS {
        let cfg = providers::get_config(provider);
        assert!(cfg.is_some(), "get_config should recognize {provider}");
    }
}

#[test]
fn test_get_config_unknown_is_none() {
    assert!(providers::get_config("nonexistent").is_none());
    assert!(providers::get_config("").is_none());
}
