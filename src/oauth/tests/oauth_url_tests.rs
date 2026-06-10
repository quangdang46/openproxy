use crate::oauth::pkce;
use crate::oauth::providers;
use crate::oauth::test_helpers::*;

// ─── PKCE provider auth URL tests ──────────────────────────────────────────

#[test]
fn test_claude_auth_url() {
    let cfg = providers::claude();
    assert_scopes_match(&cfg, expected_scopes("claude"));
    assert!(cfg.uses_pkce);
    let url = cfg.build_auth_url("9d1c250a-e61b-44d9-88ed-5944d1962f5e",
        "http://localhost:4623/oauth/callback", "st", "ch");
    assert_auth_url_shape(&url, expected_auth_url_prefix("claude"),
        "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    assert_scopes_in_url(&url, expected_scopes("claude"));
    assert!(url.contains("code_challenge=ch"));
    assert!(url.contains("code_challenge_method=S256"));
}

#[test]
fn test_codex_auth_url() {
    let cfg = providers::codex();
    assert_scopes_match(&cfg, expected_scopes("codex"));
    assert!(cfg.uses_pkce);
    let url = cfg.build_auth_url("app_EMoamEEZ73f0CkXaXp7hrann",
        "http://localhost:4623/oauth/callback", "st", "ch");
    assert_auth_url_shape(&url, expected_auth_url_prefix("codex"),
        "app_EMoamEEZ73f0CkXaXp7hrann");
    assert_scopes_in_url(&url, expected_scopes("codex"));
    assert!(url.contains("prompt=select_account"));
}

#[test]
fn test_gitlab_auth_url() {
    let cfg = providers::gitlab();
    assert_scopes_match(&cfg, expected_scopes("gitlab"));
    assert!(cfg.uses_pkce);
    let url = cfg.build_auth_url("openproxy",
        "http://localhost:4623/oauth/callback", "st", "ch");
    assert_auth_url_shape(&url, expected_auth_url_prefix("gitlab"), "openproxy");
    assert_scopes_in_url(&url, expected_scopes("gitlab"));
    assert!(url.contains("code_challenge=ch"));
}

#[test]
fn test_xai_auth_url() {
    let cfg = providers::xai();
    assert_scopes_match(&cfg, expected_scopes("xai"));
    assert!(cfg.uses_pkce);
    let url = cfg.build_auth_url("b1a00492-073a-073a-47ea-816f-4c329264a828",
        "http://localhost:4623/oauth/callback", "st", "ch");
    assert_auth_url_shape(&url, expected_auth_url_prefix("xai"),
        "b1a00492-073a-073a-47ea-816f-4c329264a828");
    assert_scopes_in_url(&url, expected_scopes("xai"));
}

// ─── Device code provider config tests ────────────────────────────────────

#[test]
fn test_github_device_config() {
    let cfg = providers::github();
    assert_scopes_match(&cfg, expected_scopes("github"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.auth_url, expected_auth_url_prefix("github"));
    // Device code providers post to auth_url, not build auth URLs.
    assert_eq!(cfg.token_url, "https://github.com/login/oauth/access_token");
}

#[test]
fn test_kiro_device_config() {
    let cfg = providers::kiro();
    assert_scopes_match(&cfg, expected_scopes("kiro"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.auth_url, expected_auth_url_prefix("kiro"));
}

#[test]
fn test_kimi_coding_device_config() {
    let cfg = providers::kimi_coding();
    assert_scopes_match(&cfg, expected_scopes("kimi-coding"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.auth_url, expected_auth_url_prefix("kimi-coding"));
}

#[test]
fn test_kilocode_device_config() {
    let cfg = providers::kilocode();
    assert_scopes_match(&cfg, expected_scopes("kilocode"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.auth_url, expected_auth_url_prefix("kilocode"));
}

#[test]
fn test_codebuddy_device_config() {
    let cfg = providers::codebuddy();
    assert_scopes_match(&cfg, expected_scopes("codebuddy"));
    assert!(!cfg.uses_pkce);
    assert_eq!(cfg.auth_url, expected_auth_url_prefix("codebuddy"));
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
    assert_eq!(v.len(), 128, "96-byte verifier should be 128 base64url chars");
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
