use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::types::Settings;

pub mod capture;
pub mod cert;
pub mod server;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MitmTarget {
    pub name: String,
    pub provider: String,
}

impl MitmTarget {
    pub fn new(name: &str, provider: &str) -> Self {
        Self {
            name: name.to_string(),
            provider: provider.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MitmRouteConfig {
    pub upstream_url: String,
    pub path_prefix: Option<String>,
    pub request_transform: bool,
    pub response_transform: bool,
}

#[derive(Debug, Clone, Default)]
pub struct MitmState {
    pub routes: BTreeMap<String, MitmRouteConfig>,
    pub active_targets: BTreeMap<String, Vec<String>>,
}

impl MitmState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_db(
        mitm_alias: &BTreeMap<String, BTreeMap<String, String>>,
        settings: &Settings,
    ) -> Self {
        let mut routes = BTreeMap::new();
        let mut active_targets: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for (target_name, config_map) in mitm_alias {
            let upstream_url = config_map.get("upstreamUrl").cloned().unwrap_or_else(|| {
                format!(
                    "{}/{}",
                    settings.mitm_router_base_url.trim_end_matches('/'),
                    target_name
                )
            });

            let path_prefix = config_map.get("pathPrefix").cloned();
            let request_transform = config_map
                .get("requestTransform")
                .map(|v| v == "true")
                .unwrap_or(false);
            let response_transform = config_map
                .get("responseTransform")
                .map(|v| v == "true")
                .unwrap_or(false);

            let config = MitmRouteConfig {
                upstream_url,
                path_prefix,
                request_transform,
                response_transform,
            };

            let provider = detect_provider_from_target(target_name);
            active_targets
                .entry(provider.clone())
                .or_default()
                .push(target_name.clone());

            routes.insert(target_name.clone(), config);
        }

        Self {
            routes,
            active_targets,
        }
    }

    pub fn has_target_for_provider(&self, provider: &str) -> bool {
        self.active_targets
            .get(provider)
            .map(|targets| !targets.is_empty())
            .unwrap_or(false)
    }

    pub fn get_targets_for_provider(&self, provider: &str) -> Vec<&String> {
        self.active_targets
            .get(provider)
            .map(|targets| targets.iter().collect())
            .unwrap_or_default()
    }

    pub fn get_route(&self, target_name: &str) -> Option<&MitmRouteConfig> {
        self.routes.get(target_name)
    }

    pub fn should_transform_request(&self, target_name: &str) -> bool {
        self.routes
            .get(target_name)
            .map(|r| r.request_transform)
            .unwrap_or(false)
    }

    pub fn should_transform_response(&self, target_name: &str) -> bool {
        self.routes
            .get(target_name)
            .map(|r| r.response_transform)
            .unwrap_or(false)
    }
}

fn detect_provider_from_target(target_name: &str) -> String {
    let lower = target_name.to_lowercase();
    if lower.contains("antigravity") || lower.contains("ag") {
        "antigravity".to_string()
    } else if lower.contains("copilot") || lower.contains("github") || lower.contains("gh") {
        "github".to_string()
    } else if lower.contains("kiro") || lower.contains("kr") {
        "kiro".to_string()
    } else {
        target_name
            .split('-')
            .next()
            .unwrap_or(target_name)
            .to_string()
    }
}

pub struct MitmInterceptor;

impl MitmInterceptor {
    pub fn transform_request(target: &str, body: &mut Value, config: &MitmRouteConfig) {
        if !config.request_transform {
            return;
        }

        if let Some(obj) = body.as_object_mut() {
            if let Some(model) = obj.get("model").and_then(|v| v.as_str()) {
                obj.insert("original_model".into(), Value::String(model.to_string()));
            }
        }
        let _ = target;
    }

    pub fn transform_response(target: &str, body: &mut Value, config: &MitmRouteConfig) {
        if !config.response_transform {
            return;
        }
        let _ = target;
        let _ = body;
    }

    pub fn build_forward_url(
        _target: &str,
        config: &MitmRouteConfig,
        original_path: &str,
    ) -> String {
        let base = &config.upstream_url;
        match config.path_prefix {
            Some(ref prefix) => {
                format!("{}{}{}", base.trim_end_matches('/'), prefix, original_path)
            }
            None => format!("{}{}", base.trim_end_matches('/'), original_path),
        }
    }
}

pub const MITM_TARGET_PROVIDERS: &[&str] = &["antigravity", "copilot", "kiro"];

/// Model IDs that MUST NOT be re-routed through the MITM proxy. These are
/// IDE-internal models that the CLI uses for completion/indexing (e.g. `tab_*`)
/// and sending them through the MITM tunnel would break the editor experience.
pub const MODEL_NO_MAP: &[&str] = &["tab_"];

pub fn is_mitm_provider(provider: &str) -> bool {
    MITM_TARGET_PROVIDERS.contains(&provider)
}

/// Returns true iff `model` should NOT be re-routed through the MITM proxy.
/// These are IDE-internal models (e.g. `tab_*` for completion/indexing) that
/// must always bypass the MITM tunnel.
pub fn is_model_no_map(model: &str) -> bool {
    MODEL_NO_MAP.iter().any(|prefix| model.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mitm_target_creation() {
        let target = MitmTarget::new("antigravity-proxy", "antigravity");
        assert_eq!(target.name, "antigravity-proxy");
        assert_eq!(target.provider, "antigravity");
    }

    #[test]
    fn detect_provider_from_target_name() {
        assert_eq!(
            detect_provider_from_target("antigravity-main"),
            "antigravity"
        );
        assert_eq!(detect_provider_from_target("copilot-proxy"), "github");
        assert_eq!(detect_provider_from_target("kiro-east"), "kiro");
        // AG abbreviation
        assert_eq!(detect_provider_from_target("ag-proxy"), "antigravity");
        // GH abbreviation
        assert_eq!(detect_provider_from_target("gh-proxy"), "github");
        // KR abbreviation
        assert_eq!(detect_provider_from_target("kr-proxy"), "kiro");
    }

    #[test]
    fn mitm_route_config_default() {
        let config = MitmRouteConfig::default();
        assert!(config.upstream_url.is_empty());
        assert!(!config.request_transform);
        assert!(!config.response_transform);
    }

    #[test]
    fn mitm_state_from_db() {
        let mut alias: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        alias.insert(
            "antigravity-main".to_string(),
            BTreeMap::from([
                (
                    "upstreamUrl".to_string(),
                    "https://api.antigravity.ai/v1".to_string(),
                ),
                ("requestTransform".to_string(), "true".to_string()),
            ]),
        );

        let settings = Settings::default();
        let state = MitmState::from_db(&alias, &settings);

        assert!(state.has_target_for_provider("antigravity"));
        let targets = state.get_targets_for_provider("antigravity");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0], "antigravity-main");
    }

    #[test]
    fn is_mitm_provider_check() {
        assert!(is_mitm_provider("antigravity"));
        assert!(is_mitm_provider("copilot"));
        assert!(is_mitm_provider("kiro"));
        assert!(!is_mitm_provider("openai"));
        assert!(!is_mitm_provider(""));
    }

    #[test]
    fn model_no_map_matches_tab_models() {
        // `tab_*` models must be excluded from MITM re-routing
        assert!(is_model_no_map("tab_close"));
        assert!(is_model_no_map("tab_complete"));
        assert!(is_model_no_map("tab_index"));
        assert!(is_model_no_map("tab_suggestion"));
        // Non-tab models must NOT be excluded
        assert!(!is_model_no_map("gemini-3.5-flash-low"));
        assert!(!is_model_no_map("claude-sonnet-4-6"));
        assert!(!is_model_no_map("gpt-4o"));
        assert!(!is_model_no_map(""));
    }

    #[test]
    fn mitm_target_providers_exhaustive() {
        let expected: std::collections::HashSet<&str> =
            ["antigravity", "copilot", "kiro"].into();
        let actual: std::collections::HashSet<&str> =
            MITM_TARGET_PROVIDERS.iter().copied().collect();
        assert_eq!(expected, actual);
    }

    #[test]
    fn model_no_map_prefixes_only() {
        // MODEL_NO_MAP should be prefix matches, not substring/suffix
        for prefix in MODEL_NO_MAP {
            assert!(is_model_no_map(prefix));
            assert!(is_model_no_map(&format!("{}something", prefix)));
            assert!(!is_model_no_map(&format!("something{}", prefix)));
        }
    }

    #[test]
    fn mitm_interceptor_transform_request_saves_original_model() {
        let config = MitmRouteConfig {
            request_transform: true,
            ..Default::default()
        };
        let mut body = json!({"model": "gemini-3.5-flash-low"});
        MitmInterceptor::transform_request("antigravity", &mut body, &config);
        assert_eq!(body["original_model"], "gemini-3.5-flash-low");
        assert_eq!(body["model"], "gemini-3.5-flash-low");
    }

    #[test]
    fn mitm_interceptor_transform_request_skipped_when_disabled() {
        let config = MitmRouteConfig {
            request_transform: false,
            ..Default::default()
        };
        let mut body = json!({"model": "gemini-3.5-flash-low"});
        MitmInterceptor::transform_request("antigravity", &mut body, &config);
        // original_model must NOT be inserted when transforms are disabled
        assert!(body.get("original_model").is_none());
    }

    #[test]
    fn mitm_interceptor_build_forward_url_with_path_prefix() {
        let config = MitmRouteConfig {
            upstream_url: "https://api.antigravity.ai".to_string(),
            path_prefix: Some("/v1internal".to_string()),
            request_transform: false,
            response_transform: false,
        };
        let url = MitmInterceptor::build_forward_url("antigravity", &config, "/streamGenerateContent");
        assert_eq!(
            url,
            "https://api.antigravity.ai/v1internal/streamGenerateContent"
        );
    }

    #[test]
    fn mitm_interceptor_build_forward_url_without_path_prefix() {
        let config = MitmRouteConfig {
            upstream_url: "https://api.githubcopilot.com".to_string(),
            path_prefix: None,
            request_transform: false,
            response_transform: false,
        };
        let url = MitmInterceptor::build_forward_url("copilot", &config, "/chat/completions");
        assert_eq!(
            url,
            "https://api.githubcopilot.com/chat/completions"
        );
    }

    #[test]
    fn mitm_interceptor_build_forward_url_trailing_slash_base() {
        let config = MitmRouteConfig {
            upstream_url: "https://q.us-east-1.amazonaws.com/".to_string(),
            path_prefix: None,
            request_transform: false,
            response_transform: false,
        };
        let url = MitmInterceptor::build_forward_url("kiro", &config, "/q/generateAssistantResponse");
        assert_eq!(
            url,
            "https://q.us-east-1.amazonaws.com/q/generateAssistantResponse"
        );
    }

    #[test]
    fn mitm_build_forward_url_both_with_trailing_slash() {
        let config = MitmRouteConfig {
            upstream_url: "https://api.antigravity.ai/".to_string(),
            path_prefix: Some("/v1internal/".to_string()),
            request_transform: false,
            response_transform: false,
        };
        let url = MitmInterceptor::build_forward_url("antigravity", &config, "/streamGenerateContent");
        // Both trailing slashes trimmed: base + prefix = one /
        assert_eq!(
            url,
            "https://api.antigravity.ai/v1internal//streamGenerateContent"
        );
    }

    #[test]
    fn mitm_state_has_no_target_for_unknown_provider() {
        let state = MitmState::new();
        assert!(!state.has_target_for_provider("unknown"));
        assert!(state.get_targets_for_provider("unknown").is_empty());
    }

    #[test]
    fn mitm_state_multiple_targets_per_provider() {
        let mut alias: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        alias.insert("antigravity-main".to_string(), BTreeMap::new());
        alias.insert("antigravity-backup".to_string(), BTreeMap::new());

        let settings = Settings::default();
        let state = MitmState::from_db(&alias, &settings);

        assert!(state.has_target_for_provider("antigravity"));
        let targets = state.get_targets_for_provider("antigravity");
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn mitm_state_route_by_name() {
        let mut alias: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        alias.insert(
            "copilot-proxy".to_string(),
            BTreeMap::from([
                (
                    "upstreamUrl".to_string(),
                    "https://api.individual.githubcopilot.com".to_string(),
                ),
                ("pathPrefix".to_string(), "/v1".to_string()),
            ]),
        );

        let settings = Settings::default();
        let state = MitmState::from_db(&alias, &settings);

        let route = state.get_route("copilot-proxy").expect("route exists");
        assert_eq!(
            route.upstream_url,
            "https://api.individual.githubcopilot.com"
        );
        assert_eq!(route.path_prefix.as_deref(), Some("/v1"));
    }
}
