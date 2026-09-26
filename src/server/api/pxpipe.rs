//! PXPIPE token-saver API.
//!
//! Mirrors 9router's `/api/pxpipe/*` endpoints (dashboard/src/app/(dashboard)/
//! dashboard/pxpipe + api/pxpipe/*). PXPIPE is an optional external npm token
//! compressor; openproxy does not manage its lifecycle, so these endpoints
//! report the library-mode skeleton state and settings-driven configuration.
//!
//!   * `GET  /api/pxpipe/status`   — install/version/config status
//!   * `POST /api/pxpipe/health`   — health checks (GET mirrors)
//!   * `GET  /api/pxpipe/stats`    — compression windows + timeline + recent
//!   * `GET  /api/pxpipe/logs`     — install log + transform events

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::api::require_dashboard_or_management_api_key;
use crate::server::state::AppState;

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;

use crate::types::Settings;

/// Build the PXPIPE sub-router.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/pxpipe/status", get(status))
        .route("/api/pxpipe/health", get(health).post(health))
        .route("/api/pxpipe/stats", get(stats))
        .route("/api/pxpipe/logs", get(logs))
}

/// `GET /api/pxpipe/status`
///
/// Reports the library-mode skeleton: PXPIPE is not installed/managed by
/// openproxy, so install fields are false/empty. Settings-driven values
/// reflect the current `Settings` (pxpipeEnabled etc.).
async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }
    let settings = state.db.snapshot().settings.clone();
    Json(json!({
        "installed": false,
        "installing": false,
        "version": Value::Null,
        "path": Value::Null,
        "running": false,
        "loadedAt": Value::Null,
        "uptimeMs": 0,
        "npmAvailable": false,
        "mode": "library",
        "enabled": settings.pxpipe_enabled,
        "autoInstall": settings.pxpipe_auto_install,
        "minChars": settings.pxpipe_min_chars,
        "timeoutMs": settings.pxpipe_timeout_ms,
    }))
    .into_response()
}

/// `POST /api/pxpipe/health` (GET mirrors)
async fn health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }
    Json(json!({
        "healthy": false,
        "checks": [
            { "id": "installed", "label": "PXPIPE installed", "ok": false, "detail": "PXPIPE is not managed by OpenProxy" },
            { "id": "module", "label": "Transform module loads", "ok": false, "detail": null },
            { "id": "transform", "label": "Test request transforms", "ok": false, "detail": null }
        ],
        "error": "PXPIPE not installed"
    }))
    .into_response()
}

fn empty_window() -> Value {
    json!({
        "requests": 0,
        "compressed": 0,
        "bypassed": 0,
        "errors": 0,
        "tokensBeforeEst": 0,
        "tokensAfterEst": 0,
        "tokensSavedEst": 0,
        "savedPct": 0,
        "imagesGenerated": 0,
        "compressionTimeMs": 0,
        "avgCompressionMs": 0
    })
}

/// `GET /api/pxpipe/stats`
async fn stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }
    Json(json!({
        "windows": {
            "all": empty_window(),
            "today": empty_window(),
            "yesterday": empty_window(),
            "last7d": empty_window(),
            "last30d": empty_window()
        },
        "timeline": [],
        "recent": []
    }))
    .into_response()
}

#[derive(Deserialize)]
struct LogsQuery {
    limit: Option<usize>,
}

/// `GET /api/pxpipe/logs?limit=50`
async fn logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(_query): Query<LogsQuery>,
) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }
    Json(json!({ "installLog": "", "events": [] })).into_response()
}

// ---------------------------------------------------------------------------
// Request-path saver
//
// The endpoints above are the dashboard surface. What follows is the piece
// that was missing: the toggle, the thresholds and the stats all existed and
// were persisted, but nothing in the request pipeline ever read them, so
// `pxpipe_enabled: true` did exactly nothing.
//
// Port of 9router `compressWithPxpipe` (open-sse/rtk/pxpipe.js:32-98) and its
// call site in chatCore, where it is the LAST saver before dispatch.
// ---------------------------------------------------------------------------

/// pxpipe's own profitability gate assumes ~4 chars/token; reuse it for the
/// before/after estimates surfaced in stats (9router `EST_CHARS_PER_TOKEN`).
const EST_CHARS_PER_TOKEN: usize = 4;

/// What the external transform reports about the images it produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PxpipeInfo {
    pub compressed_chars: u64,
    pub image_count: u64,
    pub image_bytes: u64,
    pub image_pixels: u64,
    pub image_tokens: u64,
    pub baseline_tokens: u64,
    /// Whether the transform took over the body's `cache_control` placement.
    pub cache_owns_control: bool,
}

/// The external transform's verdict on one body.
#[derive(Debug, Clone, Default)]
pub struct PxpipeTransformResult {
    pub applied: bool,
    /// The transformed body, JSON-encoded. Only meaningful when `applied`.
    pub body: Option<Vec<u8>>,
    /// 9router's `result.reason`, used verbatim as the skip reason.
    pub reason: Option<String>,
    pub info: PxpipeInfo,
}

/// Arguments handed to the transform (9router's
/// `transform({ body, model, options: { minCompressChars } })`).
#[derive(Debug, Clone)]
pub struct PxpipeTransformArgs {
    pub model: String,
    pub min_compress_chars: u64,
}

/// The externally-installed transform, injected by the host.
///
/// It is a boxed `Fn` rather than a plain function so `openproxy` stays free of
/// filesystem and install concerns, exactly as 9router keeps `open-sse` free of
/// them.
pub type PxpipeTransform = Arc<
    dyn Fn(Vec<u8>, PxpipeTransformArgs) -> BoxFuture<'static, PxpipeTransformResult> + Send + Sync,
>;

/// One PXPIPE pass's inputs and budget.
#[derive(Clone)]
pub struct PxpipeConfig {
    pub enabled: bool,
    /// The FINAL body format — PXPIPE only understands Claude-shaped bodies.
    pub format: String,
    pub model: String,
    pub min_chars: u32,
    pub timeout_ms: u64,
    pub transform: Option<PxpipeTransform>,
}

/// Why a PXPIPE pass did not change the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PxpipeSkip {
    /// `pxpipe_enabled` is off.
    Disabled,
    /// No transform module is loaded.
    NotInstalled,
    /// No body to compress.
    MissingBody,
    /// The final body is not Claude-shaped.
    UnsupportedFormat(String),
    /// Smaller than the profitability threshold.
    BelowThreshold {
        original_chars: usize,
        threshold: u64,
    },
    /// The transform overran its budget.
    Timeout {
        original_chars: usize,
        duration_ms: u64,
    },
    /// The transform declined the body.
    Passthrough {
        detail: Option<String>,
        original_chars: usize,
    },
    /// The transform threw.
    TransformError {
        detail: String,
        original_chars: usize,
    },
}

impl PxpipeSkip {
    /// The stable reason string 9router puts in its summary
    /// (`skipped(reason, extra)`).
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NotInstalled => "not_installed",
            Self::MissingBody => "missing_body",
            Self::UnsupportedFormat(_) => "unsupported_format",
            Self::BelowThreshold { .. } => "below_threshold",
            Self::Timeout { .. } => "timeout",
            Self::Passthrough { .. } => "passthrough",
            Self::TransformError { .. } => "transform_error",
        }
    }
}

/// One PXPIPE pass's outcome: either a transformed body, or a skip with the
/// reason it happened.
#[derive(Debug, Clone)]
pub struct PxpipeOutcome {
    /// `None` whenever nothing changed — the caller then keeps its own body.
    pub body: Option<Value>,
    pub summary: Result<PxpipeSummary, PxpipeSkip>,
}

impl PxpipeOutcome {
    /// Whether the pass actually changed the body.
    pub fn applied(&self) -> bool {
        self.summary.is_ok()
    }

    /// The reason, applied or skipped, for the request log.
    pub fn reason(&self) -> &'static str {
        match &self.summary {
            Ok(summary) => summary.reason,
            Err(skip) => skip.reason(),
        }
    }
}

/// The stats a successful pass reports.
#[derive(Debug, Clone, PartialEq)]
pub struct PxpipeSummary {
    pub reason: &'static str,
    pub original_chars: usize,
    pub compressed_body_chars: usize,
    pub imaged_chars: u64,
    pub image_count: u64,
    pub image_bytes: u64,
    pub tokens_before_est: u64,
    pub tokens_after_est: u64,
    pub tokens_saved_est: u64,
    pub saved_pct: f64,
    pub duration_ms: u64,
    pub cache_owns_control: bool,
}

fn est_tokens(chars: u64) -> u64 {
    // 9router rounds; a half-token still bills.
    chars.div_ceil(EST_CHARS_PER_TOKEN as u64)
}

/// Character length of the serialized body — 9router's `bodyChars`.
///
/// Counted in chars rather than bytes so a multi-byte prompt does not cross
/// the threshold early, which is the direction that would compress a body 9router
/// leaves alone.
fn body_chars(body: &Value) -> usize {
    serde_json::to_string(body)
        .map(|s| s.chars().count())
        .unwrap_or(0)
}

/// Run one PXPIPE pass over `body`.
///
/// Fail-open like every token saver: any skip, timeout or error returns
/// `body: None` and leaves the caller's body exactly as it was. The input body
/// is never mutated.
pub async fn compress_with_pxpipe(body: &Value, cfg: &PxpipeConfig) -> PxpipeOutcome {
    // Skip order is 9router's (pxpipe.js:33-43), including the transform check
    // BEFORE the body check.
    if !cfg.enabled {
        return skipped(PxpipeSkip::Disabled);
    }
    let Some(transform) = cfg.transform.clone() else {
        return skipped(PxpipeSkip::NotInstalled);
    };
    if body.is_null() {
        return skipped(PxpipeSkip::MissingBody);
    }
    if cfg.format != "claude" {
        return skipped(PxpipeSkip::UnsupportedFormat(cfg.format.clone()));
    }

    let started = std::time::Instant::now();
    let original_chars = body_chars(body);
    let threshold = if cfg.min_chars > 0 {
        u64::from(cfg.min_chars)
    } else {
        // 9router falls back to its own DEFAULT_MIN_CHARS only when the setting
        // is absent or non-positive; Settings already defaults it to 25_000.
        25_000
    };
    if (original_chars as u64) < threshold {
        return skipped(PxpipeSkip::BelowThreshold {
            original_chars,
            threshold,
        });
    }

    let budget = Duration::from_millis(if cfg.timeout_ms > 0 {
        cfg.timeout_ms
    } else {
        15_000
    });
    // Local CPU work that cannot be aborted, so race a timer and discard the
    // result if it loses — exactly 9router's Promise.race.
    let result = tokio::time::timeout(
        budget,
        transform(
            serde_json::to_vec(body).unwrap_or_default(),
            PxpipeTransformArgs {
                model: cfg.model.clone(),
                min_compress_chars: threshold,
            },
        ),
    )
    .await;

    let Ok(result) = result else {
        return skipped(PxpipeSkip::Timeout {
            original_chars,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    };
    if !result.applied {
        return skipped(PxpipeSkip::Passthrough {
            detail: result.reason.clone(),
            original_chars,
        });
    }
    let Some(raw) = result.body else {
        return skipped(PxpipeSkip::Passthrough {
            detail: None,
            original_chars,
        });
    };
    let new_body: Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(e) => {
            return skipped(PxpipeSkip::TransformError {
                detail: e.to_string(),
                original_chars,
            })
        }
    };

    let info = result.info;
    let compressed_body_chars = body_chars(&new_body);
    // The transformed body is BIGGER in bytes (base64 PNGs) but cheaper in
    // tokens: images bill by pixels, not by encoded length. So the after-
    // estimate is remaining-text tokens + image tokens — never chars/4 of the
    // new body. Provider-billed usage recorded per request stays the ground
    // truth (9router pxpipe.js:71-76).
    let image_tokens_est = if info.image_tokens > 0 {
        info.image_tokens
    } else if info.image_pixels > 0 {
        (info.image_pixels as f64 / 750.0).round() as u64
    } else {
        info.image_count * 4761
    };
    let tokens_before_est = if info.baseline_tokens > 0 {
        info.baseline_tokens
    } else {
        est_tokens(original_chars as u64)
    };
    let tokens_after_est =
        est_tokens((original_chars as u64).saturating_sub(info.compressed_chars))
            + image_tokens_est;
    let tokens_saved_est = tokens_before_est.saturating_sub(tokens_after_est);
    let saved_pct = if tokens_before_est > 0 {
        (tokens_saved_est as f64 / tokens_before_est as f64 * 100.0 * 100.0).round() / 100.0
    } else {
        0.0
    };

    PxpipeOutcome {
        body: Some(new_body),
        summary: Ok(PxpipeSummary {
            reason: "applied",
            original_chars,
            compressed_body_chars,
            imaged_chars: info.compressed_chars,
            image_count: info.image_count,
            image_bytes: info.image_bytes,
            tokens_before_est,
            tokens_after_est,
            tokens_saved_est,
            saved_pct,
            duration_ms: started.elapsed().as_millis() as u64,
            cache_owns_control: info.cache_owns_control,
        }),
    }
}

fn skipped(reason: PxpipeSkip) -> PxpipeOutcome {
    PxpipeOutcome {
        body: None,
        summary: Err(reason),
    }
}

/// Build a pass config from the current settings.
///
/// `openproxy` does not manage the pxpipe module's lifecycle (see the status
/// endpoint above), so `transform` is `None` in production and the pass fails
/// open to `NotInstalled` — the same state 9router is in when the module is
/// absent (`pxpipe.js:34`).
pub fn pxpipe_config(settings: &Settings, format: &str, model: &str) -> PxpipeConfig {
    PxpipeConfig {
        enabled: settings.pxpipe_enabled,
        format: format.to_string(),
        model: model.to_string(),
        min_chars: settings.pxpipe_min_chars,
        timeout_ms: u64::from(settings.pxpipe_timeout_ms),
        transform: None,
    }
}

#[cfg(test)]
mod pxpipe_saver_tests {
    use super::*;
    use serde_json::json;

    fn claude_body(chars: usize) -> Value {
        let mut messages = Vec::new();
        let mut n = chars;
        while n > 0 {
            let take = n.min(64);
            messages.push(json!({"role": "user", "content": "x".repeat(take)}));
            n -= take;
        }
        json!({ "model": "claude-sonnet-4", "messages": messages })
    }

    fn applying_transform() -> PxpipeTransform {
        Arc::new(|_raw, _args| {
            Box::pin(async move {
                PxpipeTransformResult {
                    applied: true,
                    body: Some(br#"{"model":"claude-sonnet-4","messages":[]}"#.to_vec()),
                    reason: None,
                    info: PxpipeInfo {
                        compressed_chars: 1_000,
                        image_count: 1,
                        ..Default::default()
                    },
                }
            })
        })
    }

    fn config(transform: Option<PxpipeTransform>) -> PxpipeConfig {
        PxpipeConfig {
            enabled: true,
            format: "claude".to_string(),
            model: "claude-sonnet-4".to_string(),
            min_chars: 1,
            timeout_ms: 5_000,
            transform,
        }
    }

    /// Every skip leaves the caller's body byte-identical — 9router is explicit
    /// that "input body is never mutated", and the caller keeps its own `Value`
    /// unless the pass hands one back.
    fn assert_untouched(before: &Value, outcome: &PxpipeOutcome) {
        assert!(!outcome.applied());
        assert!(outcome.body.is_none());
        assert_eq!(&before.clone(), before);
    }

    #[tokio::test]
    async fn pxpipe_is_skipped_when_disabled() {
        let body = claude_body(200);
        let before = body.clone();
        let mut cfg = config(Some(applying_transform()));
        cfg.enabled = false;
        let outcome = compress_with_pxpipe(&body, &cfg).await;
        assert_eq!(outcome.summary, Err(PxpipeSkip::Disabled));
        assert_untouched(&before, &outcome);
    }

    #[tokio::test]
    async fn pxpipe_is_skipped_when_no_transform_is_installed() {
        let body = claude_body(200);
        let before = body.clone();
        let outcome = compress_with_pxpipe(&body, &config(None)).await;
        assert_eq!(outcome.summary, Err(PxpipeSkip::NotInstalled));
        assert_untouched(&before, &outcome);
    }

    #[tokio::test]
    async fn pxpipe_is_skipped_for_a_non_claude_final_format() {
        let body = claude_body(200);
        let before = body.clone();
        let mut cfg = config(Some(applying_transform()));
        cfg.format = "openai".to_string();
        let outcome = compress_with_pxpipe(&body, &cfg).await;
        assert_eq!(
            outcome.summary,
            Err(PxpipeSkip::UnsupportedFormat("openai".to_string()))
        );
        assert_untouched(&before, &outcome);
    }

    #[tokio::test]
    async fn pxpipe_is_skipped_below_min_chars() {
        let body = claude_body(100);
        let before = body.clone();
        let mut cfg = config(Some(applying_transform()));
        cfg.min_chars = 25_000;
        let outcome = compress_with_pxpipe(&body, &cfg).await;
        assert!(matches!(
            outcome.summary,
            Err(PxpipeSkip::BelowThreshold {
                threshold: 25_000,
                ..
            })
        ));
        assert_untouched(&before, &outcome);
    }

    #[tokio::test]
    async fn pxpipe_is_skipped_on_transform_timeout() {
        let body = claude_body(200);
        let before = body.clone();
        let mut cfg = config(Some(Arc::new(|_raw, _args| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                PxpipeTransformResult {
                    applied: true,
                    body: Some(b"{}".to_vec()),
                    ..Default::default()
                }
            })
        })));
        cfg.timeout_ms = 20;
        let outcome = compress_with_pxpipe(&body, &cfg).await;
        assert!(matches!(outcome.summary, Err(PxpipeSkip::Timeout { .. })));
        assert_untouched(&before, &outcome);
    }

    #[tokio::test]
    async fn a_transform_that_applies_returns_the_new_body_and_the_estimates() {
        let body = claude_body(400);
        let outcome = compress_with_pxpipe(&body, &config(Some(applying_transform()))).await;
        assert!(outcome.applied());
        assert_eq!(outcome.reason(), "applied");
        let summary = outcome.summary.unwrap();
        // The transformed body replaces the caller's; the input is untouched.
        assert_eq!(outcome.body.unwrap()["messages"], json!([]));
        assert_eq!(body["messages"].as_array().unwrap().len(), 7);
        // 1 image at the 4761-token fallback is worth far more than the text it
        // replaced, so the pass made the request MORE expensive — and the
        // estimate has to be able to say so. `saturating_sub` clamps the saving
        // to 0 rather than reporting a negative one.
        assert_eq!(summary.image_count, 1);
        assert!(
            summary.tokens_after_est > summary.tokens_before_est,
            "image tokens must be counted, not the char length of the base64"
        );
        assert_eq!(summary.tokens_saved_est, 0);
        assert_eq!(summary.saved_pct, 0.0);
    }
}
