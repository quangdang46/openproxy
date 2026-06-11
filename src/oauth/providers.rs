//! OAuth provider constants — field-tested values matching 9router.
//!
//! Each provider function returns a static config that encodes the
//! authorize / token URLs, scopes, PKCE usage, extra query parameters,
//! and a refresh lead time.

use url::form_urlencoded;

/// Static OAuth provider configuration.
#[derive(Debug, Clone, Copy)]
pub struct OAuthProviderConfig {
    pub id: &'static str,
    pub client_id: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static [&'static str],
    pub uses_pkce: bool,
    pub extra_params: &'static [(&'static str, &'static str)],
    pub refresh_lead_ms: u64,
}

impl OAuthProviderConfig {
    /// Build a full authorization URL (PKCE auth-code flow).
    pub fn build_auth_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        code_challenge: &str,
    ) -> String {
        let mut pairs: Vec<(String, String)> = vec![
            ("client_id".to_string(), client_id.to_string()),
            ("redirect_uri".to_string(), redirect_uri.to_string()),
            ("response_type".to_string(), "code".to_string()),
            ("state".to_string(), state.to_string()),
        ];

        if self.uses_pkce {
            pairs.push(("code_challenge".to_string(), code_challenge.to_string()));
            pairs.push(("code_challenge_method".to_string(), "S256".to_string()));
        }

        if !self.scopes.is_empty() {
            pairs.push(("scope".to_string(), self.scopes.join(" ")));
        }

        for (key, value) in self.extra_params.iter() {
            pairs.push((key.to_string(), value.to_string()));
        }

        let query_string = pairs
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    k,
                    form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("&");

        format!("{}?{}", self.authorize_url, query_string)
    }

    /// Look up a custom extra parameter by key.
    pub fn get_param(&self, key: &str) -> Option<&'static str> {
        self.extra_params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
    }
}

// ---------------------------------------------------------------------------
// Provider definitions
// ---------------------------------------------------------------------------

pub fn claude() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "claude",
        client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
        authorize_url: "https://claude.ai/oauth/authorize",
        token_url: "https://api.anthropic.com/v1/oauth/token",
        scopes: &["org:create_api_key", "user:profile", "user:inference"],
        uses_pkce: true,
        extra_params: &[("code", "true")],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

pub fn codex() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "codex",
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
        authorize_url: "https://auth.openai.com/oauth/authorize",
        token_url: "https://auth.openai.com/oauth/token",
        scopes: &["openid", "profile", "email", "offline_access"],
        uses_pkce: true,
        extra_params: &[
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "codex_cli_rs"),
        ],
        refresh_lead_ms: 5 * 24 * 60 * 60 * 1000,
    }
}

/// GitHub — device-code flow (not PKCE auth code).
/// The `authorize_url` is the device-code endpoint.
pub fn github() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "github",
        client_id: "Iv1.b507a08c87ecfe98",
        authorize_url: "https://github.com/login/device/code",
        token_url: "https://github.com/login/oauth/access_token",
        scopes: &["read:user"],
        uses_pkce: false,
        extra_params: &[],
        refresh_lead_ms: 0,
    }
}

/// Kiro — basic AWS SSO OIDC config only (5 auth methods go in P1.3).
pub fn kiro() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "kiro",
        client_id: "",
        authorize_url: "https://oidc.us-east-1.amazonaws.com",
        token_url: "",
        scopes: &[
            "codewhisperer:completions",
            "codewhisperer:analysis",
            "codewhisperer:conversations",
        ],
        uses_pkce: false,
        extra_params: &[("client_name", "kiro-oauth-client")],
        refresh_lead_ms: 0,
    }
}

/// Qwen — device-code flow (authorize_url is the device-code endpoint).
pub fn qwen() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "qwen",
        client_id: "f0304373b74a44d2b584a3fb70ca9e56",
        authorize_url: "https://chat.qwen.ai/api/v1/oauth2/device/code",
        token_url: "https://chat.qwen.ai/api/v1/oauth2/token",
        scopes: &["openid", "profile", "email", "model.completion"],
        uses_pkce: false,
        extra_params: &[],
        refresh_lead_ms: 0,
    }
}

/// iflow — standard OAuth with client_secret (no PKCE).
pub fn iflow() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "iflow",
        client_id: "10009311001",
        authorize_url: "https://iflow.cn/oauth",
        token_url: "https://iflow.cn/oauth/token",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[
            ("client_secret", "4Z3YjXycVsQvyGF1etiNlIBB4RsqSDtW"),
            ("userinfo_url", "https://iflow.cn/api/oauth/getUserInfo"),
        ],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

/// Kimi Coding — device-code flow.
pub fn kimi_coding() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "kimi-coding",
        client_id: "17e5f671-d194-4dfb-9706-5516cb48c098",
        authorize_url: "https://api.moonshot.cn/kimi-device/oauth/device/code",
        token_url: "https://api.moonshot.cn/kimi-device/oauth/token",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[],
        refresh_lead_ms: 0,
    }
}

/// KiloCode — device-code flow (same URL for both endpoints).
pub fn kilocode() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "kilocode",
        client_id: "openproxy",
        authorize_url: "https://api.kilo.ai/api/device-auth/codes",
        token_url: "https://api.kilo.ai/api/device-auth/codes",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[],
        refresh_lead_ms: 0,
    }
}

/// Cline — PKCE auth-code flow with a dedicated refresh endpoint.
pub fn cline() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "cline",
        client_id: "openproxy",
        authorize_url: "https://api.cline.bot/api/v1/auth/authorize",
        token_url: "https://api.cline.bot/api/v1/auth/token",
        scopes: &[],
        uses_pkce: true,
        extra_params: &[("refresh_url", "https://api.cline.bot/api/v1/auth/refresh")],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

/// GitLab (cloud) — PKCE auth-code flow.
pub fn gitlab() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "gitlab",
        client_id: "openproxy",
        authorize_url: "https://gitlab.com/oauth/authorize",
        token_url: "https://gitlab.com/oauth/token",
        scopes: &["api", "read_user"],
        uses_pkce: true,
        extra_params: &[],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

/// CodeBuddy — device-code flow.
pub fn codebuddy() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "codebuddy",
        client_id: "openproxy",
        authorize_url: "https://copilot.tencent.com/v2/plugin/auth/state",
        token_url: "https://copilot.tencent.com/v2/plugin/auth/token",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[],
        refresh_lead_ms: 0,
    }
}

/// OpenAI Native — PKCE auth-code flow (not codex).
pub fn openai_native() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "openai-native",
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
        authorize_url: "https://auth.openai.com/oauth/authorize",
        token_url: "https://auth.openai.com/oauth/token",
        scopes: &["openid", "profile", "email", "offline_access"],
        uses_pkce: true,
        extra_params: &[("originator", "openai_native")],
        refresh_lead_ms: 5 * 24 * 60 * 60 * 1000,
    }
}

/// Self-hosted GitLab — dynamic base URL constructor.
/// Uses `Box::leak` internally so the returned config lives for the
/// program's lifetime (acceptable since this is called once at setup).
pub fn gitlab_with_baseurl(base_url: &str) -> OAuthProviderConfig {
    let base = base_url.trim_end_matches('/');
    let authorize_url = alloc_string(&format!("{}/oauth/authorize", base));
    let token_url = alloc_string(&format!("{}/oauth/token", base));
    OAuthProviderConfig {
        id: "gitlab-selfhost",
        client_id: "openproxy",
        authorize_url,
        token_url,
        scopes: &["api", "read_user"],
        uses_pkce: true,
        extra_params: &[],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

fn alloc_string(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

pub fn get_config(provider: &str) -> Option<OAuthProviderConfig> {
    match provider {
        "claude" => Some(claude()),
        "codex" => Some(codex()),
        "github" => Some(github()),
        "kiro" => Some(kiro()),
        "qwen" => Some(qwen()),
        "iflow" => Some(iflow()),
        "kimi-coding" => Some(kimi_coding()),
        "kilocode" => Some(kilocode()),
        "cline" => Some(cline()),
        "gitlab" => Some(gitlab()),
        "codebuddy" => Some(codebuddy()),
        "openai-native" => Some(openai_native()),
        "xai" | "gemini-cli" | "antigravity" | "openai" => None,
        _ => None,
    }
}
