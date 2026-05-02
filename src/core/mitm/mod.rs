use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::types::Settings;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
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
            let upstream_url = config_map
                .get("upstreamUrl")
                .cloned()
                .unwrap_or_else(|| {
                    format!("{}/{}", settings.mitm_router_base_url.trim_end_matches('/'), target_name)
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
        target_name.split('-').next().unwrap_or(target_name).to_string()
    }
}

pub struct MitmInterceptor;

impl MitmInterceptor {
    pub fn transform_request(
        target: &str,
        body: &mut Value,
        config: &MitmRouteConfig,
    ) {
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

    pub fn transform_response(
        target: &str,
        body: &mut Value,
        config: &MitmRouteConfig,
    ) {
        if !config.response_transform {
            return;
        }
        let _ = target;
        let _ = body;
    }

    pub fn build_forward_url(_target: &str, config: &MitmRouteConfig, original_path: &str) -> String {
        let base = &config.upstream_url;
        match config.path_prefix {
            Some(ref prefix) => format!("{}{}{}", base.trim_end_matches('/'), prefix, original_path),
            None => format!("{}{}", base.trim_end_matches('/'), original_path),
        }
    }
}

pub const MITM_TARGET_PROVIDERS: &[&str] = &["antigravity", "copilot", "kiro"];

pub fn is_mitm_provider(provider: &str) -> bool {
    MITM_TARGET_PROVIDERS.contains(&provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mitm_target_creation() {
        let target = MitmTarget::new("antigravity-proxy", "antigravity");
        assert_eq!(target.name, "antigravity-proxy");
        assert_eq!(target.provider, "antigravity");
    }

    #[test]
    fn detect_provider_from_target_name() {
        assert_eq!(detect_provider_from_target("antigravity-main"), "antigravity");
        assert_eq!(detect_provider_from_target("copilot-proxy"), "github");
        assert_eq!(detect_provider_from_target("kiro-east"), "kiro");
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
                ("upstreamUrl".to_string(), "https://api.antigravity.ai/v1".to_string()),
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
    }
}
