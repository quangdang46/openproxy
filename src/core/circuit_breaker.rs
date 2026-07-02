/// Three-layer circuit breaker for provider requests.
///
/// Each provider+endpoint pair has its own circuit breaker state machine:
///
/// ```text
///         ┌──────────┐
///    ┌───>│  Closed  │◄──── success ────┐
///    │    └────┬─────┘                  │
///    │         │ failure threshold hit  │
///    │         v                        │
///    │    ┌──────────┐                  │
///    │    │   Open   │                  │
///    │    └────┬─────┘                  │
///    │         │ timeout elapses        │
///    │         v                        │
///    │    ┌──────────┐                  │
///    │    │ Half-Open│─── success ──────┘
///    │    └────┬─────┘
///    │         │ failure (re-opens)
///    └─────────┘
/// ```
///
/// The breaker is thread-safe and uses `DashMap` for O(1) lookups per
/// provider+endpoint key, with `Mutex`-guarded per-entry state to allow
/// atomic transitions.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Observable state of a single circuit breaker entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation – requests pass through.
    Closed,
    /// Failures are being fast-rejected.
    Open,
    /// A single probe request is allowed to test recovery.
    HalfOpen,
}

/// Configuration for a circuit breaker instance.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before the circuit opens.
    pub failure_threshold: usize,
    /// Duration the circuit stays open before transitioning to half-open.
    pub open_timeout: Duration,
    /// Number of probe requests allowed in the half-open state.
    pub half_open_max_probes: usize,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            open_timeout: Duration::from_secs(30),
            half_open_max_probes: 1,
        }
    }
}

/// Outcome of a request, used to feed the circuit breaker.
#[derive(Debug, Clone, Copy)]
pub enum RequestOutcome {
    /// The request succeeded (2xx).
    Success,
    /// The request failed with a transient error (5xx, timeout, connection refused).
    Failure,
    /// The request failed with a client error (4xx) – does not count toward
    /// circuit opening, but also doesn't reset the failure count.
    ClientError,
}

// ---------------------------------------------------------------------------
// Internal per-entry state
// ---------------------------------------------------------------------------

struct Entry {
    state: CircuitState,
    failure_count: usize,
    /// When the circuit transitioned to Open.
    opened_at: Option<Instant>,
    /// How many probes have been issued in the current half-open window.
    probes_used: usize,
    /// Config for this entry (falls back to global defaults).
    config: CircuitBreakerConfig,
}

impl Entry {
    fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: CircuitState::Closed,
            failure_count: 0,
            opened_at: None,
            probes_used: 0,
            config,
        }
    }

    fn transition_to_open(&mut self, now: Instant) {
        self.state = CircuitState::Open;
        self.opened_at = Some(now);
        self.probes_used = 0;
    }

    fn transition_to_half_open(&mut self) {
        self.state = CircuitState::HalfOpen;
        self.probes_used = 0;
    }

    fn transition_to_closed(&mut self) {
        self.state = CircuitState::Closed;
        self.failure_count = 0;
        self.opened_at = None;
        self.probes_used = 0;
    }
}

// ---------------------------------------------------------------------------
// CircuitBreakerRegistry
// ---------------------------------------------------------------------------

/// Thread-safe registry of circuit breakers, keyed by `provider:endpoint`.
///
/// # Key format
///
/// Keys are of the form `"{provider_name}:{endpoint}"`, e.g. `"openai:/chat/completions"`
/// or `"claude:/v1/messages"`. This gives per-provider-per-endpoint granularity.
pub struct CircuitBreakerRegistry {
    entries: DashMap<String, Mutex<Entry>>,
    default_config: CircuitBreakerConfig,
    /// Global unique request counter used for telemetry / ordering.
    request_counter: AtomicUsize,
}

impl CircuitBreakerRegistry {
    /// Create a new registry with the given default configuration.
    pub fn new(default_config: CircuitBreakerConfig) -> Self {
        Self {
            entries: DashMap::new(),
            default_config,
            request_counter: AtomicUsize::new(0),
        }
    }

    // ---- helpers to build the key ----

    /// Build a circuit-breaker key from a provider name and endpoint path.
    pub fn key(provider: &str, endpoint: &str) -> String {
        format!("{}:{}", provider, endpoint)
    }

    // ---- querying ----

    /// Return the current state for a given key.
    ///
    /// Before returning, the method checks whether an Open circuit has timed out
    /// and automatically transitions it to HalfOpen. This is the only place the
    /// time-dependent Open→HalfOpen transition happens.
    pub fn state(&self, key: &str) -> CircuitState {
        let mut entry = match self.entries.get(key) {
            Some(e) => e,
            None => return CircuitState::Closed, // unknown = closed
        };

        let now = Instant::now();
        let mut entry = entry.lock();
        self.maybe_transition_to_half_open(&mut entry, now);
        entry.state
    }

    /// Check whether a request should be allowed through for the given key.
    ///
    /// Returns `true` if the request is allowed, `false` if the circuit is open
    /// and the request should be fast-rejected.
    pub fn allow_request(&self, key: &str) -> bool {
        let mut entry = match self.entries.get(key) {
            Some(e) => e,
            None => return true, // unknown key = allow
        };

        let now = Instant::now();
        let mut entry = entry.lock();

        // Check for timed-out Open → HalfOpen transition.
        self.maybe_transition_to_half_open(&mut entry, now);

        match entry.state {
            CircuitState::Closed => true,
            CircuitState::Open => false,
            CircuitState::HalfOpen => {
                if entry.probes_used < entry.config.half_open_max_probes {
                    entry.probes_used += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record the outcome of a request for the given key.
    pub fn record(&self, key: &str, outcome: RequestOutcome) {
        let mut entry = match self.entries.get(key) {
            Some(e) => e,
            None => return, // not tracked – ignore
        };

        let now = Instant::now();
        let mut entry = entry.lock();

        match outcome {
            RequestOutcome::Success => match entry.state {
                CircuitState::HalfOpen => {
                    // Probe succeeded → back to Closed.
                    entry.transition_to_closed();
                }
                CircuitState::Closed => {
                    // Reset failure count on success.
                    entry.failure_count = 0;
                }
                CircuitState::Open => {
                    // Open + success??? Shouldn't happen normally (we reject before sending),
                    // but if someone bypasses the guard, treat it as a recovery signal.
                    entry.transition_to_closed();
                }
            },

            RequestOutcome::Failure => {
                entry.failure_count += 1;

                match entry.state {
                    CircuitState::Closed => {
                        if entry.failure_count >= entry.config.failure_threshold {
                            entry.transition_to_open(now);
                        }
                    }
                    CircuitState::HalfOpen => {
                        // Probe failed → back to Open.
                        entry.transition_to_open(now);
                    }
                    CircuitState::Open => {
                        // Already open; refresh the timeout so the circuit stays open longer.
                        entry.opened_at = Some(now);
                    }
                }
            }

            RequestOutcome::ClientError => {
                // Client errors do NOT affect the failure count (they're the caller's fault).
                // But they also don't reset the count – they're neutral.
            }
        }
    }

    /// Ensure a key exists in the registry with the given (or default) config.
    pub fn register(&self, key: &str, config: Option<CircuitBreakerConfig>) {
        self.entries.entry(key.to_string()).or_insert_with(|| {
            Mutex::new(Entry::new(
                config.unwrap_or_else(|| self.default_config.clone()),
            ))
        });
    }

    /// Reset a specific entry back to Closed (useful for manual intervention or tests).
    pub fn reset(&self, key: &str) {
        if let Some(entry) = self.entries.get(key) {
            entry.lock().transition_to_closed();
        }
    }

    /// Number of registered circuit breaker entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Next global request ID.
    pub fn next_request_id(&self) -> usize {
        self.request_counter.fetch_add(1, Ordering::Relaxed)
    }

    // ---- internal ----

    fn maybe_transition_to_half_open(&self, entry: &mut Entry, now: Instant) {
        if entry.state == CircuitState::Open {
            if let Some(opened_at) = entry.opened_at {
                if now.duration_since(opened_at) >= entry.config.open_timeout {
                    entry.transition_to_half_open();
                }
            }
        }
    }
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn new_registry() -> CircuitBreakerRegistry {
        CircuitBreakerRegistry::new(CircuitBreakerConfig {
            failure_threshold: 3,
            open_timeout: Duration::from_secs(60),
            half_open_max_probes: 1,
        })
    }

    #[test]
    fn test_initial_state_is_closed() {
        let registry = new_registry();
        let key = "test:endpoint";
        registry.register(key, None);
        assert_eq!(registry.state(key), CircuitState::Closed);
        assert!(registry.allow_request(key));
    }

    #[test]
    fn test_opens_after_threshold_failures() {
        let registry = new_registry();
        let key = "test:endpoint";
        registry.register(key, None);

        // 3 failures → threshold hit
        registry.record(key, RequestOutcome::Failure);
        assert_eq!(registry.state(key), CircuitState::Closed);
        registry.record(key, RequestOutcome::Failure);
        assert_eq!(registry.state(key), CircuitState::Closed);
        registry.record(key, RequestOutcome::Failure);
        assert_eq!(registry.state(key), CircuitState::Open);

        // Requests should be rejected
        assert!(!registry.allow_request(key));
    }

    #[test]
    fn test_success_resets_failure_count() {
        let registry = new_registry();
        let key = "test:endpoint";
        registry.register(key, None);

        registry.record(key, RequestOutcome::Failure);
        registry.record(key, RequestOutcome::Success);
        // One more failure should not trigger open (count was reset)
        registry.record(key, RequestOutcome::Failure);
        assert_eq!(registry.state(key), CircuitState::Closed);
    }

    #[test]
    fn test_client_error_does_not_affect_count() {
        let registry = new_registry();
        let key = "test:endpoint";
        registry.register(key, None);

        // 3 client errors → should NOT open circuit
        registry.record(key, RequestOutcome::ClientError);
        registry.record(key, RequestOutcome::ClientError);
        registry.record(key, RequestOutcome::ClientError);
        assert_eq!(registry.state(key), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_to_closed_on_probe_success() {
        let registry = new_registry();
        let key = "test:endpoint";
        registry.register(key, None);

        // Trigger open
        registry.record(key, RequestOutcome::Failure);
        registry.record(key, RequestOutcome::Failure);
        registry.record(key, RequestOutcome::Failure);
        assert_eq!(registry.state(key), CircuitState::Open);
        assert!(!registry.allow_request(key));

        // Fast-forward timeout by manipulating opened_at
        // We'll just use a very short timeout config
        let key2 = "test:fast";
        let cfg = CircuitBreakerConfig {
            failure_threshold: 1,
            open_timeout: Duration::from_millis(10),
            half_open_max_probes: 1,
        };
        let fast_registry = CircuitBreakerRegistry::new(cfg);
        fast_registry.register(key2, None);
        fast_registry.record(key2, RequestOutcome::Failure);
        assert_eq!(fast_registry.state(key2), CircuitState::Open);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(20));

        // allow_request should detect the timeout and transition to half-open
        assert!(fast_registry.allow_request(key2));
        // After allow, state should be HalfOpen while probe is in-flight
        // (allow_request consumed a probe slot)
        // Now record success on the probe
        fast_registry.record(key2, RequestOutcome::Success);
        assert_eq!(fast_registry.state(key2), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_to_open_on_probe_failure() {
        let registry = new_registry();
        let key = "test:endpoint";
        registry.register(key, None);

        // Trigger open with short timeout
        // Register with a short timeout by directly manipulating the entry
        let cfg = CircuitBreakerConfig {
            failure_threshold: 1,
            open_timeout: Duration::from_millis(10),
            half_open_max_probes: 1,
        };
        let fast_registry = CircuitBreakerRegistry::new(cfg);
        fast_registry.register(key, None);
        fast_registry.record(key, RequestOutcome::Failure);
        assert_eq!(fast_registry.state(key), CircuitState::Open);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(20));

        // Half-open probe
        assert!(fast_registry.allow_request(key));
        // Probe fails
        fast_registry.record(key, RequestOutcome::Failure);
        assert_eq!(fast_registry.state(key), CircuitState::Open);
        // Still rejecting
        assert!(!fast_registry.allow_request(key));
    }

    #[test]
    fn test_reset() {
        let registry = new_registry();
        let key = "test:endpoint";
        registry.register(key, None);

        registry.record(key, RequestOutcome::Failure);
        registry.record(key, RequestOutcome::Failure);
        registry.record(key, RequestOutcome::Failure);
        assert_eq!(registry.state(key), CircuitState::Open);

        registry.reset(key);
        assert_eq!(registry.state(key), CircuitState::Closed);
        assert!(registry.allow_request(key));
    }

    #[test]
    fn test_unknown_key_allows() {
        let registry = new_registry();
        assert!(registry.allow_request("unknown:key"));
    }

    #[test]
    fn test_key_format() {
        let key = CircuitBreakerRegistry::key("openai", "/chat/completions");
        assert_eq!(key, "openai:/chat/completions");
    }
}
