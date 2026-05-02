//! Extended OAuth tests covering PKCE, device code flow, cursor import, and GitLab

use super::*;

mod pkce_extended_tests {
    use crate::oauth::pkce;

    /// code_verifier is 32 random bytes → 43 base64url chars
    #[test]
    fn test_code_verifier_is_32_bytes() {
        let verifier = pkce::generate_code_verifier();
        assert_eq!(verifier.len(), 43);
    }

    /// code_verifier uses only unreserved chars (RFC 7636)
    #[test]
    fn test_code_verifier_chars_are_valid() {
        let verifier = pkce::generate_code_verifier();
        for c in verifier.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '~',
                "Invalid char in verifier: {}",
                c
            );
        }
    }

    /// code_challenge = base64url(SHA256(code_verifier)) — RFC 7636 Appendix B
    #[test]
    fn test_code_challenge_derivation_rfc7636() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = pkce::generate_code_challenge(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    /// code_challenge is deterministic
    #[test]
    fn test_code_challenge_deterministic() {
        let verifier = pkce::generate_code_verifier();
        let c1 = pkce::generate_code_challenge(&verifier);
        let c2 = pkce::generate_code_challenge(&verifier);
        assert_eq!(c1, c2);
    }

    /// verifier-challenge pair roundtrip
    #[test]
    fn test_verifier_challenge_pair() {
        let (verifier, challenge) = pkce::generate_verifier_and_challenge();
        let computed = pkce::generate_code_challenge(&verifier);
        assert_eq!(challenge, computed);
    }

    /// Each call produces unique values
    #[test]
    fn test_uniqueness() {
        let v1 = pkce::generate_code_verifier();
        let v2 = pkce::generate_code_verifier();
        assert_ne!(v1, v2);
    }
}

mod device_code_extended_tests {
    use crate::oauth::{DeviceCodeResponse, OAuthError, TokenResponse};

    #[test]
    fn test_device_code_response_full() {
        let json = r#"{
            "device_code": "dc_123",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://github.com/login/device",
            "verification_uri_complete": "https://github.com/login/device?code=ABCD-EFGH",
            "interval": 5,
            "expires_in": 1800
        }"#;
        let resp: DeviceCodeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.device_code, "dc_123");
        assert_eq!(resp.user_code, "ABCD-EFGH");
        assert_eq!(resp.verification_uri, "https://github.com/login/device");
        assert_eq!(resp.verification_uri_complete, Some("https://github.com/login/device?code=ABCD-EFGH".to_string()));
        assert_eq!(resp.interval, 5);
        assert_eq!(resp.expires_in, Some(1800));
    }

    #[test]
    fn test_device_code_response_minimal() {
        let json = r#"{"device_code":"d","user_code":"U","verification_uri":"https://x","interval":10}"#;
        let resp: DeviceCodeResponse = serde_json::from_str(json).unwrap();
        assert!(resp.verification_uri_complete.is_none());
        assert!(resp.expires_in.is_none());
    }

    #[test]
    fn test_oauth_error_slow_down() {
        let json = r#"{"error":"slow_down","error_description":"Increase interval"}"#;
        let err: OAuthError = serde_json::from_str(json).unwrap();
        assert_eq!(err.error, "slow_down");
    }

    #[test]
    fn test_oauth_error_pending() {
        let json = r#"{"error":"authorization_pending"}"#;
        let err: OAuthError = serde_json::from_str(json).unwrap();
        assert_eq!(err.error, "authorization_pending");
        assert!(err.error_description.is_none());
    }

    #[test]
    fn test_oauth_error_expired() {
        let json = r#"{"error":"expired_token","error_description":"Code expired"}"#;
        let err: OAuthError = serde_json::from_str(json).unwrap();
        assert_eq!(err.error, "expired_token");
    }

    #[test]
    fn test_token_response_full() {
        let json = r#"{"access_token":"gho_xxx","refresh_token":"rgr_xxx","expires_in":3600,"token_type":"Bearer","scope":"read:user repo"}"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "gho_xxx");
        assert_eq!(resp.refresh_token, Some("rgr_xxx".to_string()));
        assert_eq!(resp.expires_in, Some(3600));
    }

    #[test]
    fn test_token_response_with_id_token() {
        let json = r#"{"access_token":"a","id_token":"eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxMjMifQ.sig","expires_in":3600}"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert!(resp.id_token.unwrap().starts_with("eyJ"));
    }

    #[test]
    fn test_token_response_minimal() {
        let json = r#"{"access_token":"t"}"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "t");
        assert!(resp.refresh_token.is_none());
        assert!(resp.expires_in.is_none());
    }
}

mod cursor_import_extended_tests {
    use crate::oauth::cursor_import;

    #[test]
    fn test_cursor_tokens_struct() {
        let tokens = cursor_import::CursorTokens {
            access_token: "test_access".to_string(),
            refresh_token: Some("test_refresh".to_string()),
            expires_at: Some("2025-01-01T00:00:00Z".to_string()),
        };
        assert_eq!(tokens.access_token, "test_access");
        assert!(tokens.refresh_token.is_some());
    }

    #[test]
    fn test_to_token_response() {
        let tokens = cursor_import::CursorTokens {
            access_token: "cursor_token".to_string(),
            refresh_token: Some("cursor_refresh".to_string()),
            expires_at: Some("2025-01-01T00:00:00Z".to_string()),
        };
        let resp = cursor_import::to_token_response(tokens);
        assert_eq!(resp.access_token, "cursor_token");
        assert_eq!(resp.refresh_token, Some("cursor_refresh".to_string()));
        assert_eq!(resp.token_type, Some("Bearer".to_string()));
    }

    #[test]
    fn test_to_token_response_no_refresh() {
        let tokens = cursor_import::CursorTokens {
            access_token: "access_only".to_string(),
            refresh_token: None,
            expires_at: None,
        };
        let resp = cursor_import::to_token_response(tokens);
        assert_eq!(resp.access_token, "access_only");
        assert!(resp.refresh_token.is_none());
    }

    #[test]
    fn test_read_invalid_path() {
        let result = cursor_import::read_cursor_tokens("/nonexistent/path/config.db");
        assert!(result.is_err());
    }
}

mod gitlab_extended_tests {
    use crate::oauth::gitlab_pat;

    #[test]
    fn test_pat_token_response() {
        let pat = "glpat-xxxxxxxxxxxxxxxxxxxx";
        let resp = gitlab_pat::create_token_response(pat);
        assert_eq!(resp.access_token, pat);
        assert_eq!(resp.token_type, Some("Bearer".to_string()));
        assert_eq!(resp.scope, Some("api read_user".to_string()));
        assert!(resp.refresh_token.is_none());
        assert!(resp.expires_in.is_none());
    }

    #[test]
    fn test_is_valid_pat() {
        assert!(gitlab_pat::is_valid_pat("glpat-12345678901234567890"));
        assert!(gitlab_pat::is_valid_pat(&"x".repeat(20)));
        assert!(!gitlab_pat::is_valid_pat(""));
        assert!(!gitlab_pat::is_valid_pat("short"));
    }

    #[test]
    fn test_gitlab_provider_config() {
        let config = crate::oauth::providers::gitlab();
        assert_eq!(config.auth_url, "https://gitlab.com/oauth/authorize");
        assert_eq!(config.token_url, "https://gitlab.com/oauth/token");
        assert!(config.uses_pkce);
    }

    #[test]
    fn test_gitlab_auth_url_pkce() {
        let config = crate::oauth::providers::gitlab();
        let url = config.build_auth_url(
            "openproxy",
            "http://localhost:20128/oauth/callback",
            "state123",
            "challenge456",
        );
        assert!(url.contains("client_id=openproxy"));
        assert!(url.contains("code_challenge=challenge456"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("response_type=code"));
    }

    #[test]
    fn test_gitlab_self_hosted_config() {
        let config = crate::oauth::providers::gitlab_with_baseurl("https://gitlab.example.com");
        assert_eq!(config.auth_url, "https://gitlab.example.com/oauth/authorize");
        assert_eq!(config.token_url, "https://gitlab.example.com/oauth/token");
        assert!(config.uses_pkce);
    }

    #[test]
    fn test_gitlab_self_hosted_auth_url() {
        let config = crate::oauth::providers::gitlab_with_baseurl("https://gitlab.example.com/");
        let url = config.build_auth_url(
            "openproxy",
            "http://localhost:20128/oauth/callback",
            "state",
            "challenge",
        );
        assert!(url.contains("gitlab.example.com"));
        assert!(!url.contains("gitlab.com"));
    }

    #[test]
    fn test_gitlab_self_hosted_trailing_slash() {
        let config = crate::oauth::providers::gitlab_with_baseurl("https://gitlab.example.com/");
        assert_eq!(config.auth_url, "https://gitlab.example.com/oauth/authorize");
    }
}

mod kiro_device_flow_tests {
    use crate::oauth::{DeviceCodeResponse, KiroDeviceFlow};

    #[test]
    fn test_kiro_device_flow_struct() {
        let device = DeviceCodeResponse {
            device_code: "kiro_dc_123".to_string(),
            user_code: "KIRO-USER-123".to_string(),
            verification_uri: "https://kiro.ai/activate".to_string(),
            verification_uri_complete: None,
            interval: 5,
            expires_in: Some(1800),
        };
        let kiro_flow = KiroDeviceFlow {
            device_code: device,
            client_id: "client_abc".to_string(),
            client_secret: "secret_xyz".to_string(),
        };
        assert_eq!(kiro_flow.device_code.device_code, "kiro_dc_123");
        assert_eq!(kiro_flow.client_id, "client_abc");
        assert_eq!(kiro_flow.client_secret, "secret_xyz");
    }

    #[test]
    fn test_kiro_device_flow_with_complete_uri() {
        let device = DeviceCodeResponse {
            device_code: "dc_with_uri".to_string(),
            user_code: "USER123".to_string(),
            verification_uri: "https://kiro.ai/activate".to_string(),
            verification_uri_complete: Some("https://kiro.ai/activate?code=USER123".to_string()),
            interval: 10,
            expires_in: None,
        };
        let kiro_flow = KiroDeviceFlow {
            device_code: device,
            client_id: "id".to_string(),
            client_secret: "".to_string(),
        };
        assert!(kiro_flow.device_code.verification_uri_complete.is_some());
    }
}

mod token_refresh_tests {
    use crate::oauth::token_refresh;
    use chrono::Utc;

    #[test]
    fn test_needs_refresh_none_expires_at() {
        assert!(token_refresh::needs_refresh(&None));
    }

    #[test]
    fn test_needs_refresh_expired() {
        let past = "2020-01-01T00:00:00Z";
        assert!(token_refresh::needs_refresh(&Some(past.to_string())));
    }

    #[test]
    fn test_needs_refresh_not_expired() {
        let future = "2099-12-31T23:59:59Z";
        assert!(!token_refresh::needs_refresh(&Some(future.to_string())));
    }

    #[test]
    fn test_needs_refresh_invalid_format() {
        let invalid = "not-a-date";
        assert!(token_refresh::needs_refresh(&Some(invalid.to_string())));
    }

    #[test]
    fn test_needs_refresh_within_buffer() {
        let now = Utc::now();
        // Token that expired 4 minutes ago (within 5 minute buffer)
        let nearly_expired = (now - chrono::Duration::minutes(4)).to_rfc3339();
        assert!(token_refresh::needs_refresh(&Some(nearly_expired)));
    }

    #[test]
    fn test_needs_refresh_far_future() {
        let far_future = (Utc::now() + chrono::Duration::hours(24)).to_rfc3339();
        assert!(!token_refresh::needs_refresh(&Some(far_future)));
    }
}

mod kiro_credentials_tests {
    use crate::oauth::pending::KiroCredentials;

    #[test]
    fn test_kiro_credentials_create() {
        let creds = KiroCredentials {
            client_id: "test-client-id".to_string(),
            client_secret: "test-client-secret".to_string(),
        };
        assert_eq!(creds.client_id, "test-client-id");
        assert_eq!(creds.client_secret, "test-client-secret");
    }

    #[test]
    fn test_kiro_credentials_empty_secret() {
        let creds = KiroCredentials {
            client_id: "test-id".to_string(),
            client_secret: "".to_string(),
        };
        assert!(creds.client_secret.is_empty());
    }
}

mod pending_oauth_flow_kiro_tests {
    use crate::oauth::pending::{KiroCredentials, PendingOAuthFlow};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn create_kiro_flow(state: &str) -> PendingOAuthFlow {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        PendingOAuthFlow {
            state: state.to_string(),
            code_verifier: "test_verifier".to_string(),
            provider: "kiro".to_string(),
            account_id: "account_123".to_string(),
            redirect_uri: None,
            device_code: Some("kiro_dc_xyz".to_string()),
            user_code: Some("KIRO-123".to_string()),
            created_at: now,
            expires_at: now + 900,
            kiro_credentials: Some(KiroCredentials {
                client_id: "registered_client_id".to_string(),
                client_secret: "registered_client_secret".to_string(),
            }),
        }
    }

    #[test]
    fn test_pending_flow_with_kiro_credentials() {
        let flow = create_kiro_flow("kiro_state");
        assert!(flow.kiro_credentials.is_some());
        let creds = flow.kiro_credentials.unwrap();
        assert_eq!(creds.client_id, "registered_client_id");
        assert_eq!(creds.client_secret, "registered_client_secret");
    }

    #[test]
    fn test_pending_flow_without_kiro_credentials() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let flow = PendingOAuthFlow {
            state: "github_state".to_string(),
            code_verifier: "verifier".to_string(),
            provider: "github".to_string(),
            account_id: "account_456".to_string(),
            redirect_uri: None,
            device_code: Some("gh_dc_123".to_string()),
            user_code: Some("GH-USER".to_string()),
            created_at: now,
            expires_at: now + 900,
            kiro_credentials: None,
        };
        assert!(flow.kiro_credentials.is_none());
        assert_eq!(flow.provider, "github");
    }
}
