//! Hedging routing strategy
//!
//! Sends the same request to multiple provider models simultaneously and uses
//! the first successful response. This reduces tail latency by racing providers
//! against each other — whichever responds first (within the timeout window)
//! wins.
//!
//! Graceful degradation:
//! - 0 successful responses → `HedgingError` with HTTP 503
//! - 1+ successful responses → return the fastest one (cancel remaining in-flight)

use std::fmt;
use std::future::Future;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::time::timeout;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error returned by [`execute_hedging_strategy`] when no model responds in
/// time.
#[derive(Debug, Clone)]
pub struct HedgingError {
    /// HTTP status code the caller should surface (typically 503).
    pub status: u16,
    /// Human-readable description of what went wrong.
    pub message: String,
    /// Number of models that were attempted.
    pub attempted: usize,
    /// Number of models that returned errors (as opposed to timing out).
    pub errored: usize,
}

impl fmt::Display for HedgingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} (attempted={}, errored={})",
            self.status, self.message, self.attempted, self.errored
        )
    }
}

impl std::error::Error for HedgingError {}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tunable knobs for the hedging strategy.
#[derive(Debug, Clone)]
pub struct HedgingConfig {
    /// Maximum time to wait for the first response. Default: 60 seconds.
    pub hedge_timeout_ms: u64,
    /// Whether to cancel the remaining in-flight requests once the first
    /// successful response arrives. Default: true.
    pub cancel_on_first_success: bool,
    /// If true, prefer models that report available capacity when deciding
    /// which models to race (sorted by capacity availability first).
    /// Default: false (race all models).
    pub prioritize_capacity: bool,
}

impl Default for HedgingConfig {
    fn default() -> Self {
        Self {
            hedge_timeout_ms: 60_000,
            cancel_on_first_success: true,
            prioritize_capacity: false,
        }
    }
}

impl HedgingConfig {
    /// Parse a `HedgingConfig` from the `extra.hedgingConfig` map.
    pub fn from_extra(extra: &serde_json::Map<String, Value>) -> Self {
        let cfg = extra.get("hedgingConfig").and_then(|v| v.as_object());
        let mut s = Self::default();
        if let Some(cfg) = cfg {
            if let Some(v) = cfg.get("hedgeTimeoutMs").and_then(Value::as_u64) {
                s.hedge_timeout_ms = v;
            }
            if let Some(v) = cfg.get("cancelOnFirstSuccess").and_then(Value::as_bool) {
                s.cancel_on_first_success = v;
            }
            if let Some(v) = cfg.get("prioritizeCapacity").and_then(Value::as_bool) {
                s.prioritize_capacity = v;
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Main orchestrator
// ---------------------------------------------------------------------------

/// Execute the hedging strategy: race the given models and return the first
/// successful response, or an error if none succeed within the timeout.
///
/// # Type parameters
///
/// - `F` / `Fut`: callback that dispatches a single-model request. Receives
///   a model identifier and returns the provider response `Value` or an
///   `anyhow::Error`.
///
/// # Arguments
///
/// - `models` — model identifiers to race in parallel.
/// - `config` — hedging tuning knobs.
/// - `dispatch` — the per-model dispatch callback.
pub async fn execute_hedging_strategy<F, Fut>(
    models: &[String],
    config: &HedgingConfig,
    dispatch: F,
) -> Result<Value, HedgingError>
where
    F: Fn(String) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Result<Value, anyhow::Error>> + Send,
{
    if models.is_empty() {
        return Err(HedgingError {
            status: 400,
            message: "Hedging requires at least one model".into(),
            attempted: 0,
            errored: 0,
        });
    }

    if models.len() == 1 {
        // Single model shortcut: just dispatch directly.
        return dispatch(models[0].clone()).await.map_err(|e| HedgingError {
            status: 502,
            message: format!("Hedge single model failed: {e}"),
            attempted: 1,
            errored: 1,
        });
    }

    let hedge_timeout = Duration::from_millis(config.hedge_timeout_ms);
    let cancel_on_first = config.cancel_on_first_success;
    let total = models.len();
    let shared_dispatch = dispatch;

    // Spawn all models concurrently and collect via FuturesUnordered
    // for completion-order processing (first response wins).
    let mut futures = futures_util::stream::FuturesUnordered::new();

    for model in models {
        let model = model.clone();
        let d = shared_dispatch.clone();
        futures.push(Box::pin(async move {
            (model.clone(), timeout(hedge_timeout, d(model)).await)
        }));
    }

    // Collect results as they complete (by completion order).
    let mut first_success: Option<Value> = None;
    let mut error_count = 0;

    while let Some((_model, result)) = futures.next().await {
        match result {
            Ok(Ok(response)) => {
                // Successful response.
                if first_success.is_none() {
                    first_success = Some(response);
                }
                // If we have a success and cancellation is enabled, stop
                // waiting — dropping FuturesUnordered cancels remaining tasks.
                if cancel_on_first && first_success.is_some() {
                    break;
                }
            }
            Ok(Err(_)) => {
                error_count += 1;
            }
            Err(_timeout_elapsed) => {
                // Timeout.
                error_count += 1;
            }
        }
    }

    match first_success {
        Some(response) => Ok(response),
        None => Err(HedgingError {
            status: 503,
            message: "All hedge targets failed or timed out".into(),
            attempted: total,
            errored: error_count,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_models_returns_400() {
        let config = HedgingConfig::default();
        let result = execute_hedging_strategy(&[], &config, |_model: String| async {
            Ok(serde_json::json!({"ok": true}))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status, 400);
    }

    #[tokio::test]
    async fn single_model_shortcut() {
        let config = HedgingConfig::default();
        let result =
            execute_hedging_strategy(&["model-a".into()], &config, |_model: String| async {
                Ok(serde_json::json!({"ok": true}))
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn fastest_wins() {
        let config = HedgingConfig::default();
        let call_order = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let co = call_order.clone();

        let result = execute_hedging_strategy(
            &["slow".into(), "fast".into()],
            &config,
            move |model: String| {
                let co = co.clone();
                async move {
                    if model == "slow" {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        co.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(serde_json::json!({"from": "slow"}))
                    } else {
                        // fast responds immediately
                        co.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(serde_json::json!({"from": "fast"}))
                    }
                }
            },
        )
        .await;
        assert!(result.is_ok());
        // The fast response should win.
        assert_eq!(result.unwrap()["from"], "fast");
    }

    #[tokio::test]
    async fn all_fail_returns_503() {
        let config = HedgingConfig::default();
        let result = execute_hedging_strategy(
            &["model-a".into(), "model-b".into()],
            &config,
            |_model: String| async { Err(anyhow::anyhow!("failed")) },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status, 503);
    }

    #[tokio::test]
    async fn hedging_config_parses_extra() {
        let extra = serde_json::from_str::<serde_json::Map<String, Value>>(
            r#"{
                "hedgingConfig": {
                    "hedgeTimeoutMs": 30000,
                    "cancelOnFirstSuccess": false,
                    "prioritizeCapacity": true
                }
            }"#,
        )
        .unwrap();

        let config = HedgingConfig::from_extra(&extra);
        assert_eq!(config.hedge_timeout_ms, 30000);
        assert!(!config.cancel_on_first_success);
        assert!(config.prioritize_capacity);
    }

    #[test]
    fn hedging_error_display() {
        let err = HedgingError {
            status: 503,
            message: "all hedge targets failed".into(),
            attempted: 3,
            errored: 3,
        };
        let display = format!("{err}");
        assert!(display.contains("503"));
        assert!(display.contains("all hedge targets failed"));
    }
}
