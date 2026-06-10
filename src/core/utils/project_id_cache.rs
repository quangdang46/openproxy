//! Thread-safe cache for Antigravity project IDs.
//!
//! Maps a provider connection's stable database id to a project ID
//! (obtained from the `loadCodeAssist` endpoint) with a configurable TTL.
//! Used by the Antigravity executor to avoid re-fetching the project ID
//! on every request, and invalidated from the token refresh path so a
//! refreshed token triggers a fresh lookup.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use serde_json::Value;

// ── Internal cache types ────────────────────────────────────────────────

struct CachedProject {
    project_id: String,
    fetched_at: Instant,
}

/// A TTL-bearing cache for project IDs keyed by connection id.
///
/// - `get` returns `None` when the entry is missing or stale.
/// - `set` inserts or overwrites with the current time.
/// - `invalidate` removes the entry unconditionally.
pub struct ProjectIdCache {
    inner: Mutex<HashMap<String, CachedProject>>,
    ttl: Duration,
}

impl ProjectIdCache {
    /// Create a new cache with the given per-entry TTL.
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Look up a cached project ID. Returns `None` if missing or expired.
    pub fn get(&self, id: &str) -> Option<String> {
        let map = self.inner.lock().ok()?;
        if let Some(cached) = map.get(id) {
            if cached.fetched_at.elapsed() < self.ttl {
                return Some(cached.project_id.clone());
            }
        }
        None
    }

    /// Insert (or update) a cached project ID with the current timestamp.
    pub fn set(&self, id: &str, project_id: String) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(
                id.to_string(),
                CachedProject {
                    project_id,
                    fetched_at: Instant::now(),
                },
            );
        }
    }

    /// Remove a cached entry. Safe to call even if the key does not exist.
    pub fn invalidate(&self, id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(id);
        }
    }
}

// ── Global singleton ────────────────────────────────────────────────────

fn cache() -> &'static ProjectIdCache {
    static CACHE: OnceLock<ProjectIdCache> = OnceLock::new();
    CACHE.get_or_init(|| ProjectIdCache::new(Duration::from_secs(300))) // 5-minute TTL
}

/// Get a cached project ID for the given connection id.
pub fn get_cached_project_id(connection_id: &str) -> Option<String> {
    cache().get(connection_id)
}

/// Store a project ID in the cache for the given connection id.
pub fn set_cached_project_id(connection_id: &str, project_id: String) {
    cache().set(connection_id, project_id);
}

/// Invalidate (remove) the cached project ID for the given connection id.
pub fn invalidate_cached_project_id(connection_id: &str) {
    cache().invalidate(connection_id);
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Extract the Google Cloud project id from a `loadCodeAssist` response
/// payload.  Returns `None` if the expected key (`cloudaicompanionProject`)
/// is absent or empty.
///
/// Mirrors the identically-named helper in `src/server/api/oauth.rs`.
pub fn extract_google_project_id(payload: &Value) -> Option<String> {
    let project = payload.get("cloudaicompanionProject")?;
    project
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| project.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cache_get_miss_returns_none() {
        let cache = ProjectIdCache::new(Duration::from_secs(300));
        assert!(cache.get("nonexistent").is_none());
    }

    #[test]
    fn cache_set_then_get_works() {
        let cache = ProjectIdCache::new(Duration::from_secs(300));
        cache.set("conn-1", "my-project-123".to_string());
        assert_eq!(cache.get("conn-1").as_deref(), Some("my-project-123"));
    }

    #[test]
    fn cache_invalidate_removes_entry() {
        let cache = ProjectIdCache::new(Duration::from_secs(300));
        cache.set("conn-1", "my-project-123".to_string());
        cache.invalidate("conn-1");
        assert!(cache.get("conn-1").is_none());
    }

    #[test]
    fn cache_invalidate_missing_key_is_noop() {
        let cache = ProjectIdCache::new(Duration::from_secs(300));
        cache.invalidate("never-set"); // must not panic
    }

    #[test]
    fn extract_google_project_id_finds_nested_id() {
        let payload = json!({
            "cloudaicompanionProject": {
                "id": "projects/foo-123"
            }
        });
        assert_eq!(
            extract_google_project_id(&payload).as_deref(),
            Some("projects/foo-123")
        );
    }

    #[test]
    fn extract_google_project_id_falls_back_to_flat_string() {
        let payload = json!({
            "cloudaicompanionProject": "projects/bar-456"
        });
        assert_eq!(
            extract_google_project_id(&payload).as_deref(),
            Some("projects/bar-456")
        );
    }

    #[test]
    fn extract_google_project_id_missing_returns_none() {
        let payload = json!({});
        assert!(extract_google_project_id(&payload).is_none());
    }

    #[test]
    fn extract_google_project_id_empty_string_returns_none() {
        let payload = json!({
            "cloudaicompanionProject": {"id": ""}
        });
        assert!(extract_google_project_id(&payload).is_none());
    }

    #[test]
    fn global_cache_works() {
        // Clear any previous state by using a unique key.
        let key = "global-test-conn";
        invalidate_cached_project_id(key);
        assert!(get_cached_project_id(key).is_none());
        set_cached_project_id(key, "global-proj".to_string());
        assert_eq!(get_cached_project_id(key).as_deref(), Some("global-proj"));
        invalidate_cached_project_id(key);
        assert!(get_cached_project_id(key).is_none());
    }
}
