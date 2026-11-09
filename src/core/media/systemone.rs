//! System One (Jev) decision endpoint — port of 9router
//! `open-sse/handlers/systemoneCore.js` + the registry `systemoneConfig`
//! blocks (`providers/registry/{v1m,openrouter,opencode,opencode-zen,
//! ollama-local,cloudflare-ai}.js`).
//!
//! Decision models have no chat translation layer: the request body
//! (`state` + typed `questions`) and the JSON reply are forwarded untouched.
//! URL/headers come from the per-provider config below; the only rewrite is
//! `body.model` → the catalog `upstreamModelId` when one is declared
//! (Cloudflare's `clef-flash` selector).

use serde_json::Value;

/// Per-provider System One transport, mirroring upstream `systemoneConfig`.
#[derive(Debug, Clone)]
pub struct SystemoneConfig {
    /// Full upstream URL. May contain `{accountId}` and `{model}`
    /// placeholders (Cloudflare Workers AI `ai/run/{model}`).
    pub url: &'static str,
    /// Optional path joined onto a saved `providerSpecificData.baseUrl`
    /// host override (upstream `systemoneConfig.path`, e.g. ollama-local's
    /// `/v1/systemone`).
    pub path: Option<&'static str>,
    /// Extra static headers from the registry `systemoneConfig.headers`.
    pub headers: &'static [(&'static str, &'static str)],
    /// Whether a credential is required. The no-auth lanes (opencode free,
    /// ollama-local) still send `Authorization` when a token exists.
    pub auth_required: bool,
    /// Zen lanes expect the official client session header
    /// (`x-opencode-session`), generated per request upstream
    /// (`OPENCODE_PROVIDERS` in systemoneCore.js).
    pub opencode_session: bool,
}

const OPENROUTER_HEADERS: &'static [(&'static str, &'static str)] = &[
    ("HTTP-Referer", "https://endpoint-proxy.local"),
    ("X-Title", "Endpoint Proxy"),
];

const OPENCODE_ZEN_HEADERS: &'static [(&'static str, &'static str)] = &[
    ("x-opencode-client", "desktop"),
    ("User-Agent", "opencode/1.18.31"),
];

/// Registry lookup. `None` means the provider has no System One lane —
/// upstream answers `Provider '<id>' does not support System One.`
pub fn systemone_config(provider: &str) -> Option<SystemoneConfig> {
    let cfg = match provider {
        // registry/v1m.js — v1m.ir calibrated decision engine.
        "v1m" => SystemoneConfig {
            url: "https://v1m.ir/v1/systemone",
            path: None,
            headers: &[],
            auth_required: true,
            opencode_session: false,
        },
        // registry/openrouter.js — TypeSafe-compatible decision API.
        "openrouter" => SystemoneConfig {
            url: "https://openrouter.ai/api/v1/systemone",
            path: None,
            headers: OPENROUTER_HEADERS,
            auth_required: true,
            opencode_session: false,
        },
        // registry/opencode.js + opencode-zen.js — same /zen/v1/systemone
        // lane; the free `opencode` provider needs no credential.
        "opencode" | "opencode-zen" => SystemoneConfig {
            url: "https://opencode.ai/zen/v1/systemone",
            path: None,
            headers: OPENCODE_ZEN_HEADERS,
            auth_required: false,
            opencode_session: true,
        },
        // registry/ollama-local.js — local daemon, auth optional; a saved
        // host override joins `path` without doubling /v1.
        "ollama-local" => SystemoneConfig {
            url: "http://localhost:11434/v1/systemone",
            path: Some("/v1/systemone"),
            headers: &[],
            auth_required: false,
            opencode_session: false,
        },
        // registry/cloudflare-ai.js — Workers AI `ai/run/{model}` embeds the
        // account and model in the path.
        "cloudflare-ai" => SystemoneConfig {
            url: "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/run/{model}",
            path: None,
            headers: &[],
            auth_required: true,
            opencode_session: false,
        },
        // registry/nace.js — Nace Drex decision models on console.nace.ai;
        // a saved host override joins `path` ("/v1/systemone").
        "nace" => SystemoneConfig {
            url: "https://console.nace.ai/v1/systemone",
            path: Some("/v1/systemone"),
            headers: &[],
            auth_required: true,
            opencode_session: false,
        },
        // registry/ollama.js — Ollama Cloud decision lane; apikey bearer,
        // no path override (upstream systemoneConfig has no `path`).
        "ollama" => SystemoneConfig {
            url: "https://ollama.com/v1/systemone",
            path: None,
            headers: &[],
            auth_required: true,
            opencode_session: false,
        },
        _ => return None,
    };
    Some(cfg)
}

/// Upstream `buildSystemoneUrl`: a saved host override is joined with the
/// registry `path` (e.g. "/v1/systemone") without doubling a "/v1" or
/// "/v1/systemone" the user already included. Without `path`, the override
/// is used as a full URL.
pub fn build_systemone_url(override_base: Option<&str>, cfg: &SystemoneConfig) -> String {
    let base = override_base.map(str::trim).filter(|s| !s.is_empty());
    let Some(base) = base else {
        return cfg.url.to_string();
    };
    let Some(path) = cfg.path else {
        return base.to_string();
    };
    let host = base.trim_end_matches('/');
    let path = path.trim_end_matches('/');
    if host.ends_with(path) {
        return host.to_string();
    }
    let prefix = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    if !prefix.is_empty() && host.ends_with(prefix) {
        return format!("{}{}", host, &path[prefix.len()..]);
    }
    format!("{}{}", host, path)
}

/// Substitute the `{accountId}` / `{model}` placeholders upstream expands in
/// `handleSystemoneCore`. `{accountId}` comes from
/// `providerSpecificData.accountId` (Cloudflare); `{model}` is the resolved
/// model id.
pub fn expand_systemone_url(
    url: &str,
    account_id: Option<&str>,
    model: &str,
) -> Result<String, String> {
    let mut out = url.to_string();
    if out.contains("{accountId}") {
        let account_id = account_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "requires accountId in providerSpecificData".to_string())?;
        out = out.replace("{accountId}", account_id);
    }
    if out.contains("{model}") {
        out = out.replace("{model}", model);
    }
    Ok(out)
}

/// Validate the trust-boundary fields upstream checks in both the Next
/// handler and `handleSystemoneCore`: `state` present, `questions` a
/// non-array object. Question-level shape is upstream's job.
pub fn validate_systemone_body(body: &Value) -> Result<(), &'static str> {
    match body.get("state") {
        Some(v) if !v.is_null() => {}
        _ => return Err("Missing required field: state"),
    }
    match body.get("questions") {
        Some(v) if v.is_object() => {}
        _ => return Err("Missing required field: questions"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_configs_match_upstream_urls() {
        assert_eq!(
            systemone_config("v1m").unwrap().url,
            "https://v1m.ir/v1/systemone"
        );
        assert_eq!(
            systemone_config("openrouter").unwrap().url,
            "https://openrouter.ai/api/v1/systemone"
        );
        for p in ["opencode", "opencode-zen"] {
            let cfg = systemone_config(p).unwrap();
            assert_eq!(cfg.url, "https://opencode.ai/zen/v1/systemone");
            assert!(cfg.opencode_session);
            assert!(!cfg.auth_required);
        }
        let ollama = systemone_config("ollama-local").unwrap();
        assert_eq!(ollama.url, "http://localhost:11434/v1/systemone");
        assert_eq!(ollama.path, Some("/v1/systemone"));
        let cf = systemone_config("cloudflare-ai").unwrap();
        assert!(cf.url.contains("{accountId}"));
        assert!(cf.url.contains("{model}"));
        assert!(systemone_config("deepseek").is_none());
    }

    #[test]
    fn saved_host_override_joins_path_without_doubling() {
        let cfg = systemone_config("ollama-local").unwrap();
        // No override → registry URL.
        assert_eq!(build_systemone_url(None, &cfg), cfg.url);
        // Bare host → host + path.
        assert_eq!(
            build_systemone_url(Some("http://localhost:11434"), &cfg),
            "http://localhost:11434/v1/systemone"
        );
        // Override already carrying the path → used verbatim.
        assert_eq!(
            build_systemone_url(Some("http://localhost:11434/v1/systemone"), &cfg),
            "http://localhost:11434/v1/systemone"
        );
        // Override carrying only the /v1 prefix → path suffix appended.
        assert_eq!(
            build_systemone_url(Some("http://localhost:11434/v1"), &cfg),
            "http://localhost:11434/v1/systemone"
        );
    }

    #[test]
    fn placeholders_expand_and_account_id_is_required() {
        let url = "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/run/{model}";
        assert_eq!(
            expand_systemone_url(url, Some("acct-1"), "clef-flash").unwrap(),
            "https://api.cloudflare.com/client/v4/accounts/acct-1/ai/run/clef-flash"
        );
        assert!(expand_systemone_url(url, None, "clef-flash").is_err());
        // {model} alone never requires accountId.
        assert_eq!(
            expand_systemone_url("https://x/{model}", None, "m").unwrap(),
            "https://x/m"
        );
    }

    #[test]
    fn body_validation_matches_upstream() {
        assert!(validate_systemone_body(&json!({"state": {}, "questions": {"q": {}}})).is_ok());
        assert_eq!(
            validate_systemone_body(&json!({"questions": {}})).unwrap_err(),
            "Missing required field: state"
        );
        assert_eq!(
            validate_systemone_body(&json!({"state": null, "questions": {}})).unwrap_err(),
            "Missing required field: state"
        );
        assert_eq!(
            validate_systemone_body(&json!({"state": {}, "questions": []})).unwrap_err(),
            "Missing required field: questions"
        );
        assert_eq!(
            validate_systemone_body(&json!({"state": {}, "questions": "x"})).unwrap_err(),
            "Missing required field: questions"
        );
    }
}
