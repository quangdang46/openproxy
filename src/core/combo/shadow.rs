//! Shadow Routing strategy
//!
//! Sends the request to a primary model and, in the background, sends the same
//! request to one or more shadow models. The primary response is returned to
//! the caller immediately. The shadow response is recorded for offline
//! comparison — latency comparison, quality comparison, or drift detection.
//!
//! The shadow dispatch is fire-and-forget from the caller's perspective: it
//! runs in a spawned tokio task and does not block or influence the primary
//! response path.
//!
//! Graceful degradation:
//! - Primary succeeds → return primary response (log shadow outcomes)
//! - Primary fails  → fall back to shadow result if available; otherwise
//!   surface the primary error

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error returned by [`execute_shadow_strategy`].
#[derive(Debug, Clone)]
pub struct ShadowError {
    /// HTTP status code.
    pub status: u16,
    /// Human-readable description.
    pub message: String,
    /// Whether a shadow fallback was attempted.
    pub shadow_attempted: bool,
    /// Whether the shadow fallback succeeded.
    pub shadow_succeeded: bool,
}

impl fmt::Display for ShadowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} (shadow_attempted={}, shadow_succeeded={})",
            self.status, self.message, self.shadow_attempted, self.shadow_succeeded
        )
    }
}

impl std::error::Error for ShadowError {}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tunable knobs for the shadow routing strategy.
#[derive(Debug, Clone)]
pub struct ShadowConfig {
    /// Maximum time to wait for a shadow response before abandoning it
    /// (the shadow is always backgrounded, but a slow shadow may still
    /// consume resources). Default: 30 seconds.
    pub shadow_timeout_ms: u64,
    /// If true, fall back to a shadow response when the primary fails
    /// (instead of surfacing the primary error). Default: false.
    pub fallback_on_primary_failure: bool,
    /// If true, the shadow request body is a trimmed version that excludes
    /// streaming and tool definitions (similar to fusion's panel body).
    /// Default: true.
    pub trim_shadow_body: bool,
    /// Maximum number of shadow models to dispatch to. 0 = all configured
    /// shadows. Default: 0.
    pub max_shadows: usize,
}

impl Default for ShadowConfig {
    fn default() -> Self {
        Self {
            shadow_timeout_ms: 30_000,
            fallback_on_primary_failure: false,
            trim_shadow_body: true,
            max_shadows: 0,
        }
    }
}

impl ShadowConfig {
    /// Parse a `ShadowConfig` from the `extra.shadowConfig` map.
    pub fn from_extra(extra: &serde_json::Map<String, Value>) -> Self {
        let cfg = extra.get("shadowConfig").and_then(|v| v.as_object());
        let mut s = Self::default();
        if let Some(cfg) = cfg {
            if let Some(v) = cfg.get("shadowTimeoutMs").and_then(Value::as_u64) {
                s.shadow_timeout_ms = v;
            }
            if let Some(v) = cfg.get("fallbackOnPrimaryFailure").and_then(Value::as_bool) {
                s.fallback_on_primary_failure = v;
            }
            if let Some(v) = cfg.get("trimShadowBody").and_then(Value::as_bool) {
                s.trim_shadow_body = v;
            }
            if let Some(v) = cfg.get("maxShadows").and_then(Value::as_u64) {
                s.max_shadows = v as usize;
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Shadow result recording
// ---------------------------------------------------------------------------

/// Outcome of a single shadow model dispatch.
#[derive(Debug, Clone)]
pub struct ShadowOutcome {
    /// The shadow model identifier.
    pub model: String,
    /// Whether the shadow returned a successful response.
    pub success: bool,
    /// Latency in milliseconds (wall-clock time from dispatch to completion
    /// or failure).
    pub latency_ms: u64,
    /// Error message if unsuccessful.
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Body trimming for shadow (muted version of fusion's create_panel_body)
// ---------------------------------------------------------------------------

/// Create a trimmed copy of the request body suitable for shadow dispatch.
/// Strips streaming flags and tool definitions to reduce shadow overhead.
fn create_shadow_body(original_body: &Value) -> Value {
    let mut body = original_body.clone();
    if let Some(obj) = body.as_object_mut() {
        // Shadows never stream.
        obj.insert("stream".into(), json!(false));
        // Strip tool definitions (we don't need the shadow to call tools).
        obj.remove("tools");
        obj.remove("functions");
        obj.remove("tool_choice");
        obj.remove("function_call");
    }
    body
}

/// A fire-and-forget callback type for shadow outcomes.
/// Called after shadow completions so the caller can record metrics.
pub type ShadowOutcomeCallback = Arc<dyn Fn(Vec<ShadowOutcome>) + Send + Sync>;

// ---------------------------------------------------------------------------
// Main orchestrator
// ---------------------------------------------------------------------------

/// Execute the shadow routing strategy.
///
/// # Type parameters
///
/// - `F` / `Fut`: callback that dispatches a single-model request. Receives
///   a model identifier and the request body, returns the provider response
///   `Value` or an `anyhow::Error`.
/// - `G` / `FutG`: callback for shadow dispatch.
///
/// # Arguments
///
/// - `primary_model` — the primary model identifier.
/// - `shadow_models` — shadow model identifiers to dispatch in the background.
/// - `body` — the original request body.
/// - `config` — shadow routing tuning knobs.
/// - `dispatch_primary` — callback to dispatch the primary request.
/// - `dispatch_shadow` — callback to dispatch a shadow request.
/// - `on_shadow_outcome` — optional fire-and-forget callback invoked with
///   shadow outcomes after completion.
pub async fn execute_shadow_strategy<F, Fut, G, HG>(
    primary_model: &str,
    shadow_models: &[String],
    body: &Value,
    config: &ShadowConfig,
    dispatch_primary: F,
    dispatch_shadow: G,
    on_shadow_outcome: Option<ShadowOutcomeCallback>,
) -> Result<Value, ShadowError>
where
    F: FnOnce(String, Value) -> Fut,
    Fut: Future<Output = Result<Value, anyhow::Error>>,
    G: Fn(String, Value) -> HG + Clone + Send + 'static,
    HG: Future<Output = Result<Value, anyhow::Error>> + Send,
{
    if shadow_models.is_empty() {
        // No shadow models configured: behave as passthrough.
        return dispatch_primary(primary_model.to_string(), body.clone())
            .await
            .map_err(|e| ShadowError {
                status: 502,
                message: format!("Primary model failed (no shadow): {e}"),
                shadow_attempted: false,
                shadow_succeeded: false,
            });
    }

    let max_shadows = if config.max_shadows > 0 {
        config.max_shadows.min(shadow_models.len())
    } else {
        shadow_models.len()
    };

    let shadow_body = if config.trim_shadow_body {
        create_shadow_body(body)
    } else {
        body.clone()
    };

    let shadow_timeout = Duration::from_millis(config.shadow_timeout_ms);
    let shadow_slice: Vec<String> = shadow_models[..max_shadows].to_vec();
    let shadow_count = shadow_slice.len();

    // Collect shadow outcomes in an Arc<Mutex<Vec>> so the spawned tasks
    // can write to them and we can optionally read them for fallback.
    let outcomes: Arc<Mutex<Vec<ShadowOutcome>>> = Arc::new(Mutex::new(Vec::new()));
    let fallback_enabled = config.fallback_on_primary_failure;
    let fallback_outcome: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));

    // Spawn shadow dispatches in the background. They run concurrently
    // with the primary.
    for shadow_model in &shadow_slice {
        let model = shadow_model.clone();
        let sb = shadow_body.clone();
        let d = dispatch_shadow.clone();
        let outcomes = outcomes.clone();
        let fallback_outcome = fallback_outcome.clone();
        let sl = shadow_timeout;

        tokio::spawn(async move {
            let start = Instant::now();
            let result = tokio::time::timeout(sl, d(model.clone(), sb)).await;
            let elapsed = start.elapsed().as_millis() as u64;

            match result {
                Ok(Ok(response)) => {
                    let mut out = outcomes.lock();
                    out.push(ShadowOutcome {
                        model: model.clone(),
                        success: true,
                        latency_ms: elapsed,
                        error: None,
                    });
                    if fallback_enabled {
                        let mut fb = fallback_outcome.lock();
                        if fb.is_none() {
                            *fb = Some(response);
                        }
                    }
                }
                Ok(Err(e)) => {
                    let mut out = outcomes.lock();
                    out.push(ShadowOutcome {
                        model,
                        success: false,
                        latency_ms: elapsed,
                        error: Some(format!("{e}")),
                    });
                }
                Err(_) => {
                    let mut out = outcomes.lock();
                    out.push(ShadowOutcome {
                        model,
                        success: false,
                        latency_ms: elapsed,
                        error: Some("timeout".into()),
                    });
                }
            }
        });
    }

    // Dispatch the primary request and await it.
    let primary_result = dispatch_primary(primary_model.to_string(), body.clone()).await;

    // Notify on_shadow_outcome with whatever shadow results have arrived
    // so far. This is fire-and-forget: the caller can record metrics.
    if let Some(ref callback) = on_shadow_outcome {
        let snapshot = outcomes.lock().clone();
        if !snapshot.is_empty() {
            let cb = callback.clone();
            tokio::spawn(async move {
                cb(snapshot);
            });
        }
    }

    match primary_result {
        Ok(response) => Ok(response),
        Err(e) => {
            if fallback_enabled {
                // Brief yield window so spawned shadow tasks can complete
                // before we check the fallback. Use a short poll loop so we
                // don't block the executor if the shadow already finished.
                for _ in 0..5 {
                    {
                        let fb = fallback_outcome.lock();
                        if fb.is_some() {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                let fb = fallback_outcome.lock().take();
                match fb {
                    Some(shadow_response) => Ok(shadow_response),
                    None => Err(ShadowError {
                        status: 502,
                        message: format!("Primary failed and no shadow fallback available: {e}"),
                        shadow_attempted: shadow_count > 0,
                        shadow_succeeded: false,
                    }),
                }
            } else {
                Err(ShadowError {
                    status: 502,
                    message: format!("Primary model failed: {e}"),
                    shadow_attempted: shadow_count > 0,
                    shadow_succeeded: false,
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_shadow_passthrough() {
        let config = ShadowConfig::default();
        let result = execute_shadow_strategy(
            "primary",
            &[],
            &serde_json::json!({"model": "test"}),
            &config,
            |_model: String, _body: Value| async { Ok(serde_json::json!({"ok": true})) },
            |model: String, _body: Value| async move { Ok(serde_json::json!({"from": model})) },
            None,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn primary_error_without_fallback() {
        let config = ShadowConfig::default();
        let result = execute_shadow_strategy(
            "primary",
            &["shadow-1".into()],
            &serde_json::json!({"model": "test"}),
            &config,
            |_model: String, _body: Value| async { Err(anyhow::anyhow!("primary failed")) },
            |model: String, _body: Value| async move { Ok(serde_json::json!({"from": model})) },
            None,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status, 502);
    }

    #[tokio::test]
    async fn shadow_fallback_succeeds() {
        let config = ShadowConfig {
            fallback_on_primary_failure: true,
            shadow_timeout_ms: 5000,
            ..Default::default()
        };
        let result = execute_shadow_strategy(
            "primary",
            &["shadow-1".into()],
            &serde_json::json!({"model": "test"}),
            &config,
            |_model: String, _body: Value| async { Err(anyhow::anyhow!("primary failed")) },
            |model: String, _body: Value| async move { Ok(serde_json::json!({"from": model})) },
            None,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["from"], "shadow-1");
    }

    #[tokio::test]
    async fn shadow_body_is_trimmed() {
        let original = serde_json::json!({
            "model": "test",
            "stream": true,
            "tools": [{"type": "function"}],
            "messages": [{"role": "user", "content": "hi"}]
        });
        let trimmed = create_shadow_body(&original);
        assert_eq!(trimmed["stream"], false);
        assert!(trimmed.get("tools").is_none());
        assert!(trimmed.get("messages").is_some());
    }

    #[test]
    fn shadow_config_parses_extra() {
        let extra = serde_json::from_str::<serde_json::Map<String, Value>>(
            r#"{
                "shadowConfig": {
                    "shadowTimeoutMs": 15000,
                    "fallbackOnPrimaryFailure": true,
                    "trimShadowBody": false,
                    "maxShadows": 2
                }
            }"#,
        )
        .unwrap();

        let config = ShadowConfig::from_extra(&extra);
        assert_eq!(config.shadow_timeout_ms, 15000);
        assert!(config.fallback_on_primary_failure);
        assert!(!config.trim_shadow_body);
        assert_eq!(config.max_shadows, 2);
    }

    #[test]
    fn shadow_error_display() {
        let err = ShadowError {
            status: 502,
            message: "primary failed".into(),
            shadow_attempted: true,
            shadow_succeeded: false,
        };
        let display = format!("{err}");
        assert!(display.contains("502"));
        assert!(display.contains("primary failed"));
    }
}
