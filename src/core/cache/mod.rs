//! Response cache for OpenProxy.
//!
//! Provides an exact-match response cache keyed by SHA-256 of the serialized
//! request body. Non-streaming chat completions are cached with a configurable
//! TTL (default 60 seconds). Upstream `Cache-Control` headers are respected.
//!
//! This is the equivalent of OmniRoute's response cache, ported to OpenProxy.

use std::collections::BTreeMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Default TTL for cached responses when no per-provider override is set.
///
/// Free-tier operators run on $0 providers with hard daily quotas, so a long
/// TTL maximizes cache reuse (a repeated prompt never leaves the box). 24h is
/// the default; upstream `Cache-Control` or a per-provider override can shorten
/// it. The previous 60s default was tuned for paid per-token billing where
/// freshness matters more than quota savings.
const DEFAULT_CACHE_TTL_SECS: u64 = 86_400; // 24 hours

/// Caps the upstream `Cache-Control` `max-age`/`s-maxage` only. The default
/// and per-provider TTLs may be larger (e.g. the 24h free-tier default); this
/// limit just prevents an upstream from forcing an unbounded hold.
const MAX_UPSTREAM_CACHE_TTL_SECS: u64 = 3600; // 1 hour

/// A single cached entry.
#[derive(Clone)]
struct CacheEntry {
    /// The serialized response JSON body as bytes.
    body: Vec<u8>,
    /// When this entry was inserted.
    created_at: Instant,
    /// TTL for this specific entry (may come from upstream Cache-Control).
    ttl: Duration,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= self.ttl
    }
}

/// SHA-256 based response cache using DashMap.
///
/// Thread-safe, lock-free reads (DashMap shards internally). Entries are
/// lazily evicted on access — expired entries are skipped and removed.
#[derive(Clone)]
pub struct ResponseCache {
    inner: Arc<DashMap<[u8; 32], CacheEntry>>,
    /// Per-provider TTL overrides. Keyed by provider name (e.g. "openai").
    /// Falls back to [`DEFAULT_CACHE_TTL_SECS`].
    provider_ttls: Arc<DashMap<String, Duration>>,
    /// Global default TTL.
    default_ttl: Duration,
    /// Maximum number of entries before proactive eviction.
    max_entries: usize,
    /// Monotonic hit counter (cache served a stored response).
    hits: Arc<AtomicU64>,
    /// Monotonic miss counter (lookup found nothing / expired / no-cache).
    misses: Arc<AtomicU64>,
    /// Monotonic store counter (a response was inserted).
    sets: Arc<AtomicU64>,
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            provider_ttls: Arc::new(DashMap::new()),
            default_ttl: Duration::from_secs(DEFAULT_CACHE_TTL_SECS),
            max_entries: 10_000,
            hits: Arc::new(AtomicU64::new(0)),
            misses: Arc::new(AtomicU64::new(0)),
            sets: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl ResponseCache {
    /// Create a new response cache with the given default TTL.
    pub fn new(default_ttl_secs: u64) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            provider_ttls: Arc::new(DashMap::new()),
            default_ttl: Duration::from_secs(default_ttl_secs.max(1)),
            max_entries: 10_000,
            hits: Arc::new(AtomicU64::new(0)),
            misses: Arc::new(AtomicU64::new(0)),
            sets: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Set a per-provider TTL override.
    pub fn set_provider_ttl(&self, provider: &str, ttl_secs: u64) {
        self.provider_ttls
            .insert(provider.to_string(), Duration::from_secs(ttl_secs.max(1)));
    }

    /// Get the effective TTL for a provider.
    fn ttl_for_provider(&self, provider: &str) -> Duration {
        self.provider_ttls
            .get(provider)
            .map(|d| *d)
            .unwrap_or(self.default_ttl)
    }

    /// Build a cache key from the canonical serialization of a request body.
    ///
    /// The key is SHA-256 of the canonical JSON, which includes only the
    /// semantically relevant fields: `model`, `messages`, `tools`, `tool_choice`,
    /// `temperature`, `top_p`, `max_tokens`, `stop`, `presence_penalty`,
    /// `frequency_penalty`, `logit_bias`, `user`, `response_format`, `seed`,
    /// `reasoning_effort`, `thinking`, `metadata`, `n`, `modalities`, `audio`.
    ///
    /// Fields like `stream` are excluded because they control transport only,
    /// and we only cache non-streaming responses anyway.
    fn build_cache_key(body: &Value) -> [u8; 32] {
        let canonical = Self::canonicalize_body(body);
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        let result = hasher.finalize();
        let mut key = [0u8; 32];
        key.copy_from_slice(&result);
        key
    }

    /// Extract the semantically relevant fields from a request body and
    /// serialize to a canonical JSON string (sorted keys, no whitespace).
    fn canonicalize_body(body: &Value) -> String {
        // If we have an object, extract only the relevant fields in sorted order
        let relevant_keys = [
            "model",
            "messages",
            "tools",
            "tool_choice",
            "temperature",
            "top_p",
            "max_tokens",
            "stop",
            "presence_penalty",
            "frequency_penalty",
            "logit_bias",
            "user",
            "response_format",
            "seed",
            "reasoning_effort",
            "thinking",
            "metadata",
            "n",
            "modalities",
            "audio",
            "input",
            "instructions",
            "include",
        ];

        let canonical = match body {
            Value::Object(map) => {
                let mut filtered = BTreeMap::new();
                for key in &relevant_keys {
                    if let Some(val) = map.get(*key) {
                        filtered.insert(*key, val.clone());
                    }
                }
                // Also include any keys from the extra/flattened namespace that
                // are not in the relevant_keys but are present in the body.
                // This ensures provider-specific parameters are included.
                for (key, val) in map.iter() {
                    if !relevant_keys.contains(&key.as_str()) {
                        // Skip transport-only/transient fields
                        if !key.starts_with('_') && *key != "stream" && *key != "stream_options" {
                            filtered.insert(key.as_str(), val.clone());
                        }
                    }
                }
                serde_json::to_string(&filtered).unwrap_or_default()
            }
            _ => serde_json::to_string(body).unwrap_or_default(),
        };

        canonical
    }

    /// Attempt to retrieve a cached response for the given request body.
    ///
    /// Returns `None` if no cache entry exists, if it has expired, or if the
    /// request carries `Cache-Control: no-cache`. Increments the hit/miss
    /// counters used for the dashboard hit-rate display.
    pub fn get(&self, body: &Value) -> Option<Vec<u8>> {
        self.get_with_ttl(body).map(|(body, _ttl)| body)
    }

    /// Like [`ResponseCache::get`] but also returns the remaining TTL (seconds)
    /// of the served entry. Used by the HIT path to report `ttl_remaining` in
    /// the `openproxy.v1.cache.hit` robot envelope.
    pub fn get_with_ttl(&self, body: &Value) -> Option<(Vec<u8>, u64)> {
        // Honor explicit Cache-Control: no-cache from the client request
        if has_no_cache_directive(body) {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        let key = Self::build_cache_key(body);

        // Fast path: check existence. A missing entry is a miss.
        let entry = match self.inner.get(&key) {
            Some(entry) => entry,
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };

        if entry.is_expired() {
            // Drop the guard before removing to avoid deadlock on the same shard
            drop(entry);
            self.inner.remove(&key);
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        let remaining = entry
            .ttl
            .saturating_sub(entry.created_at.elapsed())
            .as_secs();
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some((entry.body.clone(), remaining))
    }

    /// Store a response in the cache.
    ///
    /// `upstream_cache_control` is an optional `Cache-Control` header value from
    /// the upstream response. If present, its `max-age` or `s-maxage` directive
    /// is used as the TTL (capped at [`MAX_UPSTREAM_CACHE_TTL_SECS`]).
    ///
    /// Provider-scoped TTL is used when the upstream response does not specify
    /// a `Cache-Control` header.
    ///
    /// # Eviction strategy
    ///
    /// Every call to `set` triggers proactive eviction when the entry count
    /// exceeds `max_entries`. The strategy is deliberately cheap to keep the
    /// hot insert path fast:
    ///
    /// 1. **Remove expired entries** — a linear `O(n)` scan via `retain`,
    ///    freeing space without allocation.
    /// 2. **Arbitrary eviction** — if still over the limit, `take(excess)`
    ///    entries from the DashMap iterator and remove them. This is `O(1)`
    ///    in the evicted count but **arbitrary**: it does not prefer the
    ///    oldest or soonest-to-expire entry.
    ///
    /// ## Why not sort by age?
    ///
    /// Finding the oldest entries requires materialising a list of all live
    /// entries, sorting by `created_at`, and removing the oldest. That is
    /// `O(n log n)` on every `set` past the threshold — unacceptable for a
    /// high-throughput cache on the hot path.
    ///
    /// ## Tradeoff
    ///
    /// Under sustained over-capacity pressure, arbitrary eviction may drop
    /// long-TTL entries that still have useful life, degrading the hit rate.
    /// Cache correctness (freshness) is never affected — expired entries are
    /// never returned — but the hit rate can degrade slightly until load
    /// subsides. The periodic `retain` on every over-capacity `set` removes
    /// expired entries opportunistically, which mitigates pressure in the
    /// common case where the bulk of entries are short-lived.
    ///
    /// ## Recommendation for a future improvement
    ///
    /// To eliminate the tradeoff without paying `O(n log n)` on the hot path,
    /// move eviction off the critical path:
    ///
    /// - **Background eviction task** — spawn a periodic task (every
    ///   `N` seconds or when `len > max_entries`) that runs in a separate
    ///   thread/task. This task can afford an `O(n)` scan that collects
    ///   `(created_at, key)` pairs, sorts by age once (`O(n log n)` for
    ///   the batch), and evicts the oldest entries. The hot `set` path
    ///   never sorts.
    /// - **Approximate LRU via shard-local ordering** — DashMap already
    ///   shards its internal `HashMap`s. Maintain a per-shard
    ///   `BTreeMap<Instant, Vec<[u8; 32]>>` (or a `BinaryHeap` keyed on
    ///   `created_at`). On eviction, pop the oldest entry from the
    ///   shard with the most entries. Insertion/update is `O(log m)` per
    ///   shard (where `m` is entries in that shard) rather than
    ///   `O(n log n)` for the whole map.
    /// - **Admission gate with ring buffer** — instead of evicting on
    ///   `set`, maintain an upper bound admission gate. When at capacity,
    ///   evict one entry from an approximation ring buffer (e.g. CLOCK
    ///   algorithm or second-chance). The dashmap-dedup crate or a
    ///   separate `Cache` wrapper can provide `O(1)` eviction with
    ///   age/residency heuristics.
    ///
    /// Of these, the **background eviction task** is the simplest to
    /// implement correctly and defers all sorting cost to a non-blocking
    /// context.
    pub fn set(
        &self,
        body: &Value,
        response_body: Vec<u8>,
        provider: &str,
        upstream_cache_control: Option<&str>,
    ) {
        // Don't cache error responses or empty bodies
        if response_body.is_empty() {
            return;
        }

        let key = Self::build_cache_key(body);

        // Determine TTL: upstream Cache-Control > provider TTL > default
        let ttl = upstream_cache_control
            .and_then(parse_max_age_from_cache_control)
            .map(|secs| Duration::from_secs(secs.min(MAX_UPSTREAM_CACHE_TTL_SECS)))
            .unwrap_or_else(|| self.ttl_for_provider(provider));

        let entry = CacheEntry {
            body: response_body,
            created_at: Instant::now(),
            ttl,
        };

        self.inner.insert(key, entry);
        self.sets.fetch_add(1, Ordering::Relaxed);

        // Proactive eviction when cache grows too large
        if self.inner.len() > self.max_entries {
            // First remove all expired entries (linear scan, no allocation).
            self.inner.retain(|_key, entry| !entry.is_expired());

            // If still over the limit, evict arbitrary entries to get back
            // under it. This avoids an O(n log n) sort of all entries that
            // would be needed to guarantee oldest-first eviction.
            //
            // LIMITATION: Under sustained over-capacity pressure, entries
            // with long TTLs may be evicted before older entries that have
            // already expired (those are cleaned lazily on subsequent get/set
            // calls). Cache correctness (freshness) is unaffected — no
            // expired entry is ever returned — only the hit rate may degrade
            // slightly under sustained overflow, because a soon-to-expire
            // entry could be dropped before it would have served a request.
            // In practice this is acceptable: expired entries are removed
            // opportunistically on every insert past the limit, and the
            // cheaper strategy saves an O(n log n) sort per insertion.
            // See the struct-level note on `ResponseCache` for a recommended
            // improvement path.
            if self.inner.len() > self.max_entries {
                let excess = self.inner.len() - self.max_entries;
                let to_remove: Vec<[u8; 32]> =
                    self.inner.iter().take(excess).map(|e| *e.key()).collect();
                for key in &to_remove {
                    self.inner.remove(key);
                }
            }
        }
    }

    /// Remove a cached entry by request body. Useful when a subsequent request
    /// indicates the cached response is stale.
    pub fn invalidate(&self, body: &Value) {
        let key = Self::build_cache_key(body);
        self.inner.remove(&key);
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        self.inner.clear();
    }

    /// Return the number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns true if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Snapshot of cache counters for the dashboard hit-rate display.
    ///
    /// `hit_rate` is `hits / (hits + misses)` in the range `[0.0, 1.0]`. Lookups
    /// that are skipped because the client sent `Cache-Control: no-cache` count
    /// as misses (they never consult storage). Stores are not part of the
    /// hit-rate denominator — a stored response is only a "hit" when it is
    /// later served.
    pub fn stats(&self) -> CacheStats {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        let hit_rate = if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        };
        CacheStats {
            hits,
            misses,
            sets: self.sets.load(Ordering::Relaxed),
            entries: self.inner.len(),
            hit_rate,
        }
    }
}

/// Cache counters surfaced to the dashboard and `/api/cache/stats`.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct CacheStats {
    /// Number of responses served from cache.
    pub hits: u64,
    /// Number of lookups that found nothing (missing / expired / no-cache).
    pub misses: u64,
    /// Number of responses stored.
    pub sets: u64,
    /// Live entry count.
    pub entries: usize,
    /// `hits / (hits + misses)`, `0.0` when no lookups have occurred.
    pub hit_rate: f64,
}

/// Check if the request body carries a `Cache-Control: no-cache` directive
/// (in a provider-agnostic way via the `extra` map or `metadata` field).
fn has_no_cache_directive(body: &Value) -> bool {
    // Check metadata.cache_control.no_cache
    if let Some(metadata) = body.get("metadata").and_then(|m| m.as_object()) {
        if let Some(cc) = metadata.get("cache_control") {
            // Direct boolean: cache_control: true (same as "no-cache")
            if cc.as_bool() == Some(true) {
                return true;
            }
            // String: cache_control: "no-cache"
            if cc.as_str() == Some("no-cache") {
                return true;
            }
            // Object: cache_control: { no_cache: true }
            if let Some(obj) = cc.as_object() {
                if obj.get("no_cache").and_then(|v| v.as_bool()) == Some(true) {
                    return true;
                }
            }
        }
    }

    false
}

/// Parse `max-age` (or `s-maxage`) from a `Cache-Control` header value.
///
/// Supports the formats:
/// - `Cache-Control: max-age=3600`
/// - `Cache-Control: s-maxage=3600`
/// - `Cache-Control: public, max-age=60`
/// - `Cache-Control: no-cache` (returns `Some(0)`)
fn parse_max_age_from_cache_control(header_value: &str) -> Option<u64> {
    let lower = header_value.to_lowercase();

    // Check for no-cache/no-store first
    if lower.contains("no-cache") || lower.contains("no-store") {
        return Some(0);
    }

    // s-maxage takes precedence over max-age (per HTTP spec for shared caches)
    for directive in lower.split(',').map(|d| d.trim()) {
        if let Some(value) = directive
            .strip_prefix("s-maxage=")
            .or_else(|| directive.strip_prefix("max-age="))
        {
            if let Ok(secs) = value.trim().parse::<u64>() {
                return Some(secs);
            }
        }
    }

    None
}

impl std::fmt::Debug for ResponseCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseCache")
            .field("entries", &self.inner.len())
            .field("default_ttl", &self.default_ttl)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_cache_key_stable() {
        let body1 = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "temperature": 0.7,
            "stream": true
        });
        let body2 = json!({
            "stream": true,
            "messages": [{"role": "user", "content": "Hello"}],
            "model": "gpt-4o",
            "temperature": 0.7,
        });

        let key1 = ResponseCache::build_cache_key(&body1);
        let key2 = ResponseCache::build_cache_key(&body2);

        // Should produce same key regardless of field order
        assert_eq!(key1, key2, "keys should be order-independent");
    }

    #[test]
    fn test_build_cache_key_different_models() {
        let body1 = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        let body2 = json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "Hello"}],
        });

        let key1 = ResponseCache::build_cache_key(&body1);
        let key2 = ResponseCache::build_cache_key(&body2);

        assert_ne!(key1, key2, "different models should produce different keys");
    }

    #[test]
    fn test_build_cache_key_different_messages() {
        let body1 = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        let body2 = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Goodbye"}],
        });

        let key1 = ResponseCache::build_cache_key(&body1);
        let key2 = ResponseCache::build_cache_key(&body2);

        assert_ne!(
            key1, key2,
            "different content should produce different keys"
        );
    }

    #[test]
    fn test_strip_stream_from_cache_key() {
        let body_with_stream = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        });
        let body_without_stream = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false,
        });

        let key1 = ResponseCache::build_cache_key(&body_with_stream);
        let key2 = ResponseCache::build_cache_key(&body_without_stream);

        assert_eq!(key1, key2, "stream field should not affect cache key");
    }

    #[test]
    fn test_get_set_roundtrip() {
        let cache = ResponseCache::new(60);

        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        let response = br#"{"id":"chatcmpl-123","choices":[{"message":{"role":"assistant","content":"Hi!"}}]}"#.to_vec();

        assert!(cache.get(&body).is_none(), "cache should start empty");

        cache.set(&body, response.clone(), "openai", None);

        let cached = cache.get(&body);
        assert!(cached.is_some(), "should find cached entry");
        assert_eq!(cached.unwrap(), response);
    }

    #[test]
    fn test_respect_upstream_max_age() {
        let cache = ResponseCache::new(300);

        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        let response = b"response".to_vec();

        // Upstream says cache for 10 seconds
        cache.set(
            &body,
            response.clone(),
            "openai",
            Some("public, max-age=10"),
        );

        let cached = cache.get(&body);
        assert!(cached.is_some(), "should return cached entry within TTL");
    }

    #[test]
    fn test_respect_upstream_no_cache() {
        let cache = ResponseCache::new(60);

        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        let response = b"response".to_vec();

        // Upstream says no caching
        cache.set(&body, response.clone(), "openai", Some("no-cache"));

        // Entry should still be stored (the TTL is 0), but get should skip it
        // Actually with TTL=0 it expires immediately
        std::thread::sleep(Duration::from_millis(10));
        let cached = cache.get(&body);
        assert!(cached.is_none(), "no-cache entry should expire immediately");
    }

    #[test]
    fn test_no_cache_directive_from_request() {
        let cache = ResponseCache::new(60);

        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        let response = b"response".to_vec();

        // First set a cached entry
        cache.set(&body, response.clone(), "openai", None);

        // Now request with no-cache directive
        let body_with_no_cache = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "metadata": {
                "cache_control": "no-cache"
            }
        });

        let cached = cache.get(&body_with_no_cache);
        assert!(cached.is_none(), "no-cache directive should bypass cache");
    }

    #[test]
    fn test_invalidate() {
        let cache = ResponseCache::new(60);

        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        let response = b"response".to_vec();

        cache.set(&body, response, "openai", None);
        assert!(cache.get(&body).is_some(), "should be cached");

        cache.invalidate(&body);
        assert!(cache.get(&body).is_none(), "should be invalidated");
    }

    #[test]
    fn test_clear() {
        let cache = ResponseCache::new(60);

        let body1 = json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "A"}]});
        let body2 = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "B"}]});

        cache.set(&body1, b"resp1".to_vec(), "openai", None);
        cache.set(&body2, b"resp2".to_vec(), "openai", None);

        assert_eq!(cache.len(), 2);

        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_provider_ttl_override() {
        let cache = ResponseCache::new(60);
        cache.set_provider_ttl("openai", 10);

        // Verify TTL for provider (internal detail, but let's confirm
        // that the returned TTL is the overridden one by setting a
        // provider-specific entry and checking basic behavior)
        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        let response = b"response".to_vec();

        cache.set(&body, response, "openai", None);
        assert!(cache.get(&body).is_some());
    }

    #[test]
    fn test_parse_cache_control_max_age() {
        assert_eq!(parse_max_age_from_cache_control("max-age=3600"), Some(3600));
        assert_eq!(
            parse_max_age_from_cache_control("s-maxage=7200"),
            Some(7200)
        );
        assert_eq!(
            parse_max_age_from_cache_control("public, max-age=60"),
            Some(60)
        );
        assert_eq!(
            parse_max_age_from_cache_control("private, s-maxage=300, max-age=60"),
            Some(300)
        ); // s-maxage wins
        assert_eq!(parse_max_age_from_cache_control("no-cache"), Some(0));
        assert_eq!(parse_max_age_from_cache_control("no-store"), Some(0));
        assert_eq!(parse_max_age_from_cache_control("public"), None);
        assert_eq!(parse_max_age_from_cache_control(""), None);
    }

    #[test]
    fn test_cache_key_includes_extra_params() {
        // Body with provider-specific params should differ from bare body
        let bare = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        let with_seed = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "seed": 42,
        });

        let key_bare = ResponseCache::build_cache_key(&bare);
        let key_seed = ResponseCache::build_cache_key(&with_seed);

        assert_ne!(key_bare, key_seed, "seed should differentiate cache keys");
    }

    #[test]
    fn test_dont_cache_empty_body() {
        let cache = ResponseCache::new(60);

        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });

        // Setting empty body should not insert
        cache.set(&body, Vec::new(), "openai", None);
        assert!(cache.is_empty(), "empty body should not be cached");
    }

    #[test]
    fn test_has_no_cache_directive_object() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "metadata": {
                "cache_control": {
                    "no_cache": true
                }
            }
        });
        assert!(has_no_cache_directive(&body));
    }

    #[test]
    fn test_default_ttl_is_24h() {
        // Free-tier default: a repeated prompt should stay cached for a day.
        let cache = ResponseCache::default();
        assert_eq!(
            cache.default_ttl,
            Duration::from_secs(86_400),
            "default TTL must be 24h for free-tier quota savings"
        );

        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        cache.set(&body, b"response".to_vec(), "openai", None);

        // Immediately after set, the entry is live and served.
        assert!(cache.get(&body).is_some());
    }

    #[test]
    fn test_stats_counters_track_hits_misses_sets() {
        let cache = ResponseCache::new(86_400);

        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });

        // Miss before anything is stored.
        assert!(cache.get(&body).is_none());
        let s0 = cache.stats();
        assert_eq!(s0.hits, 0);
        assert_eq!(s0.misses, 1);
        assert_eq!(s0.sets, 0);
        assert_eq!(s0.hit_rate, 0.0);

        // Store a response.
        cache.set(&body, b"response".to_vec(), "openai", None);
        let s1 = cache.stats();
        assert_eq!(s1.sets, 1);

        // Subsequent identical lookup is a hit.
        assert!(cache.get(&body).is_some());
        let s2 = cache.stats();
        assert_eq!(s2.hits, 1);
        assert_eq!(s2.misses, 1);
        assert!((s2.hit_rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_stats_no_cache_directive_counts_as_miss() {
        let cache = ResponseCache::new(86_400);
        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "metadata": { "cache_control": "no-cache" }
        });
        assert!(cache.get(&body).is_none());
        let s = cache.stats();
        assert_eq!(s.misses, 1);
        assert_eq!(s.hits, 0);
    }

    #[test]
    fn test_get_with_ttl_reports_remaining() {
        let cache = ResponseCache::new(86_400);
        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
        });
        cache.set(&body, b"response".to_vec(), "openai", None);

        let (served, remaining) = cache.get_with_ttl(&body).expect("should hit");
        assert_eq!(served, b"response");
        // 24h TTL, just inserted → remaining is essentially the full day.
        assert!(remaining > 86_000, "remaining TTL should be ~24h");
    }
}
