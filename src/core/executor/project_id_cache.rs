//! Lightweight in-memory cache mapping access tokens to their resolved GCP
//! project id. Falls back to `provider_specific_data["projectId"]` and
//! `ProviderConnection.project_id` when the cache is cold.
//!
//! Port of `open-sse/utils/projectIdCache.js` from 9router (P2.2).

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::types::ProviderConnection;

fn cache() -> &'static RwLock<HashMap<String, String>> {
    static CELL: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Look up the project id for `credentials`. Checks, in order:
/// 1. In-memory cache keyed by access_token
/// 2. `provider_specific_data["projectId"]`
/// 3. `provider_specific_data["project"]`
/// 4. `credentials.project_id`
///
/// If found via 2-4 the result is cached under the current access token so
/// subsequent lookups for the same connection hit the fast path.
pub fn lookup_project_id(credentials: &ProviderConnection) -> Option<String> {
    let access_token = credentials.access_token.as_deref().unwrap_or("");

    // 1. Check cache (only when we have a keyable token).
    if !access_token.is_empty() {
        if let Some(cached) = cache().read().get(access_token).cloned() {
            return Some(cached);
        }
    }

    // 2-4. Check fallback sources.
    let found = credentials
        .provider_specific_data
        .get("projectId")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            credentials
                .provider_specific_data
                .get("project")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .or_else(|| credentials.project_id.clone());

    // Cache it so the hot path (1) wins next time.
    if let Some(ref pid) = found {
        if !access_token.is_empty() {
            cache().write().insert(access_token.to_string(), pid.clone());
        }
    }

    found
}

/// Clear the entire cache. Mainly useful for tests.
#[allow(dead_code)]
pub fn clear_cache() {
    cache().write().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use serde_json::Value;

    fn make_creds(
        access_token: Option<&str>,
        project_id: Option<&str>,
        psd: Vec<(&str, &str)>,
    ) -> ProviderConnection {
        let mut data = BTreeMap::new();
        for (k, v) in psd {
            data.insert(k.to_string(), Value::String(v.to_string()));
        }
        ProviderConnection {
            access_token: access_token.map(|s| s.to_string()),
            project_id: project_id.map(|s| s.to_string()),
            provider_specific_data: data,
            ..ProviderConnection::default()
        }
    }

    #[test]
    fn lookup_from_project_id_field() {
        let creds = make_creds(Some("tok1"), Some("proj-main"), vec![]);
        assert_eq!(lookup_project_id(&creds), Some("proj-main".to_string()));
    }

    #[test]
    fn lookup_from_provider_specific_data() {
        let creds = make_creds(Some("tok2"), None, vec![("projectId", "proj-psd")]);
        assert_eq!(lookup_project_id(&creds), Some("proj-psd".to_string()));
    }

    #[test]
    fn lookup_from_provider_specific_project() {
        let creds = make_creds(Some("tok3"), None, vec![("project", "proj-short")]);
        assert_eq!(lookup_project_id(&creds), Some("proj-short".to_string()));
    }

    #[test]
    fn lookup_caches_by_access_token() {
        clear_cache();
        let creds = make_creds(Some("tok-cache"), Some("proj-cached"), vec![]);
        assert_eq!(lookup_project_id(&creds), Some("proj-cached".to_string()));

        // Changing project_id on the creds shouldn't matter — cache hit.
        let creds2 = make_creds(Some("tok-cache"), Some("proj-different"), vec![]);
        assert_eq!(lookup_project_id(&creds2), Some("proj-cached".to_string()));
    }

    #[test]
    fn returns_none_when_no_sources() {
        let creds = make_creds(Some("tok-none"), None, vec![]);
        assert!(lookup_project_id(&creds).is_none());
    }

    #[test]
    fn handles_empty_access_token() {
        let creds = make_creds(None, Some("proj-no-token"), vec![]);
        assert_eq!(lookup_project_id(&creds), Some("proj-no-token".to_string()));
    }

    #[test]
    fn psd_projectid_takes_precedence_over_project_id_field() {
        let creds = make_creds(
            Some("tok-prec"),
            Some("fallback"),
            vec![("projectId", "winner")],
        );
        assert_eq!(lookup_project_id(&creds), Some("winner".to_string()));
    }
}
