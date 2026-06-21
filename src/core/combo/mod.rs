use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::core::account_fallback::{BACKOFF_BASE_MS, BACKOFF_MAX_MS, MAX_BACKOFF_LEVEL};
use crate::types::Combo;

const LONG_COOLDOWN: Duration = Duration::from_secs(120);
const SHORT_COOLDOWN: Duration = Duration::from_secs(5);
const TRANSIENT_COOLDOWN: Duration = Duration::from_secs(30);

static COMBO_ROTATION_STATE: Lazy<Mutex<HashMap<String, usize>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// In-memory quarantine map keyed by `(combo_name, model)`. Members get
/// added when [`mark_combo_member_quarantined`] is called and removed
/// either when the TTL expires or via [`clear_combo_member_quarantine`] /
/// [`clear_combo_quarantine`]. Lives alongside `COMBO_ROTATION_STATE` so
/// the dispatcher can consult it without per-request DB I/O.
static COMBO_MEMBER_QUARANTINE: Lazy<Mutex<HashMap<(String, String), Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComboPlan {
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ComboStrategy {
    #[default]
    Fallback,
    RoundRobin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboAttemptError {
    pub status: u16,
    pub message: String,
    pub retry_after: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboExecutionError {
    pub status: u16,
    pub message: String,
    pub earliest_retry_after: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackDecision {
    pub should_fallback: bool,
    pub cooldown: Duration,
    pub new_backoff_level: Option<u32>,
}

/// Whether a combo member model currently has capacity to serve a new request.
///
/// `Available` means at least one underlying provider account has a free
/// in-flight slot and is not rate-limited / locked. `Busy` means every
/// matching account is currently saturated, so picking this model would
/// either fail fast (when all members are Busy) or just burn time on the
/// inner per-account fallback before bouncing to the next combo member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCapacity {
    Available,
    Busy,
}

pub fn get_quota_cooldown(backoff_level: u32) -> Duration {
    // 9router bug fixed: was `saturating_sub(1)` which made levels 0 and 1
    // both produce BASE*2^0 = same delay. Correct formula: BASE * 2^level
    // so level 0 = BASE (2s), level 1 = 2*BASE (4s), level 2 = 4*BASE (8s), etc.
    let cooldown_ms = BACKOFF_BASE_MS.saturating_mul(2u64.saturating_pow(backoff_level));
    Duration::from_millis(cooldown_ms.min(BACKOFF_MAX_MS))
}

pub fn check_fallback_error(status: u16, error_text: &str, backoff_level: u32) -> FallbackDecision {
    let lower = error_text.to_lowercase();

    for text_rule in [
        ("no credentials", Some(LONG_COOLDOWN), false),
        ("request not allowed", Some(SHORT_COOLDOWN), false),
        ("improperly formed request", Some(LONG_COOLDOWN), false),
        ("rate limit", None, true),
        ("too many requests", None, true),
        ("quota exceeded", None, true),
        ("capacity", None, true),
        ("overloaded", None, true),
    ] {
        if lower.contains(text_rule.0) {
            return if text_rule.2 {
                let new_level = (backoff_level + 1).min(MAX_BACKOFF_LEVEL);
                FallbackDecision {
                    should_fallback: true,
                    cooldown: get_quota_cooldown(new_level),
                    new_backoff_level: Some(new_level),
                }
            } else {
                FallbackDecision {
                    should_fallback: true,
                    cooldown: text_rule.1.unwrap_or(TRANSIENT_COOLDOWN),
                    new_backoff_level: None,
                }
            };
        }
    }

    match status {
        // 9router bug fixed: 4xx client errors should NOT trigger fallback.
        // The original code returned should_fallback: true for all of 401-404,
        // causing account rotation on bad requests that will fail on any account.
        400 | 404 => FallbackDecision {
            should_fallback: false,
            cooldown: Duration::ZERO,
            new_backoff_level: None,
        },
        401 | 403 => FallbackDecision {
            should_fallback: true,
            cooldown: LONG_COOLDOWN,
            new_backoff_level: None,
        },
        429 => {
            let new_level = (backoff_level + 1).min(MAX_BACKOFF_LEVEL);
            FallbackDecision {
                should_fallback: true,
                cooldown: get_quota_cooldown(new_level),
                new_backoff_level: Some(new_level),
            }
        }
        _ => FallbackDecision {
            should_fallback: true,
            cooldown: TRANSIENT_COOLDOWN,
            new_backoff_level: None,
        },
    }
}

pub fn get_rotated_models(
    models: &[String],
    combo_name: Option<&str>,
    strategy: ComboStrategy,
) -> Vec<String> {
    if models.len() <= 1 || strategy != ComboStrategy::RoundRobin {
        return models.to_vec();
    }

    let Some(combo_name) = combo_name else {
        return models.to_vec();
    };

    let mut state = COMBO_ROTATION_STATE.lock();
    let current_index = *state.get(combo_name).unwrap_or(&0);
    let mut rotated = models.to_vec();

    for _ in 0..current_index {
        if let Some(first) = rotated.first().cloned() {
            rotated.remove(0);
            rotated.push(first);
        }
    }

    state.insert(combo_name.to_string(), (current_index + 1) % models.len());
    rotated
}

pub fn reset_combo_rotation(combo_name: Option<&str>) {
    let mut state = COMBO_ROTATION_STATE.lock();
    if let Some(combo_name) = combo_name {
        state.remove(combo_name);
    } else {
        state.clear();
    }
}

pub fn rotation_index(combo_name: &str) -> Option<usize> {
    COMBO_ROTATION_STATE.lock().get(combo_name).copied()
}

/// Mark a single `(combo_name, model)` pair as quarantined for `ttl`.
///
/// This is used by the chat dispatcher when *every* underlying account
/// for a combo member has just failed: rather than letting the next
/// request hit the same broken model immediately, we record it here so
/// [`execute_combo_strategy_with_capacity`] can skip it on subsequent
/// calls until the TTL elapses. This is in-memory only (matches the
/// existing `COMBO_ROTATION_STATE` semantics) and resets on restart.
pub fn mark_combo_member_quarantined(combo_name: &str, model: &str, ttl: Duration) {
    let until = Instant::now() + ttl;
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.insert((combo_name.to_string(), model.to_string()), until);
}

/// Clear quarantine for a specific `(combo_name, model)` pair.
pub fn clear_combo_member_quarantine(combo_name: &str, model: &str) {
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.remove(&(combo_name.to_string(), model.to_string()));
}

/// Clear all quarantined members for a combo (e.g. after the operator
/// edited the member list).
pub fn clear_combo_quarantine(combo_name: &str) {
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.retain(|(name, _), _| name != combo_name);
}

/// Returns members currently quarantined for `combo_name` together with
/// the absolute `Instant` their cooldown expires. Stale entries are
/// pruned as a side effect.
pub fn combo_quarantine_for(combo_name: &str) -> Vec<(String, Instant)> {
    let now = Instant::now();
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.retain(|_, until| *until > now);
    guard
        .iter()
        .filter_map(|((name, model), until)| {
            if name == combo_name {
                Some((model.clone(), *until))
            } else {
                None
            }
        })
        .collect()
}

fn quarantined_members(combo_name: &str) -> HashSet<String> {
    let now = Instant::now();
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.retain(|_, until| *until > now);
    guard
        .iter()
        .filter_map(|((name, model), _)| {
            if name == combo_name {
                Some(model.clone())
            } else {
                None
            }
        })
        .collect()
}

pub fn get_combo_models_from_data(model_str: &str, combos: &[Combo]) -> Option<Vec<String>> {
    if model_str.contains('/') {
        return None;
    }

    combos
        .iter()
        .find(|combo| combo.name == model_str && !combo.models.is_empty())
        .map(|combo| combo.models.clone())
}

/// Returns the set of disabled members for a combo by name, or empty if
/// the combo doesn't exist.
pub fn get_disabled_members_for_combo(combo_name: &str, combos: &[Combo]) -> Vec<String> {
    combos
        .iter()
        .find(|combo| combo.name == combo_name)
        .map(|combo| combo.disabled_models.clone())
        .unwrap_or_default()
}

pub async fn execute_combo_strategy<T, F, Fut>(
    models: &[String],
    combo_name: Option<&str>,
    strategy: ComboStrategy,
    handle_single_model: F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, ComboAttemptError>>,
{
    execute_combo_strategy_with_capacity(
        models,
        combo_name,
        strategy,
        &[],
        |_| ModelCapacity::Available,
        handle_single_model,
    )
    .await
}

/// Same as [`execute_combo_strategy`], but consults a capacity callback to
/// short-circuit on saturated providers in `RoundRobin` mode and applies
/// two additional pre-gates that skip combo members *before* dispatch:
///
/// 1. **`disabled_members`** — explicit operator-supplied list of combo
///    members to never dispatch to. Filtered out in both `Fallback` and
///    `RoundRobin`. This is the "manual bypass" knob exposed via the UI
///    when a member is known to be broken but the operator wants to keep
///    it in the configured list (for visibility / quick re-enable)
///    instead of removing it.
/// 2. **Auto-quarantine** — `(combo_name, model)` pairs registered via
///    [`mark_combo_member_quarantined`]. Used by the chat dispatcher to
///    park a member for the same cooldown duration `check_fallback_error`
///    already returns when every underlying account has just failed, so
///    the next request doesn't immediately retry a known-broken model
///    and make the CLI agent appear to hang.
///
/// When at least one rotated member reports `ModelCapacity::Available`, only
/// those members are tried (in rotation order). Busy members are skipped
/// entirely — otherwise a slow request against a saturated provider would
/// pin the caller while it spins through the per-account inner fallback,
/// which is the failure mode that makes multi-repo coding agents appear to
/// hang. If every member is `Busy`, we fail fast with a 503 and surface
/// the earliest known retry-after so the caller can back off instead of
/// piling more load onto already-saturated providers.
///
/// `Fallback` strategy keeps its declared priority order for capacity —
/// capacity is advisory only and we still attempt every non-disabled,
/// non-quarantined member sequentially so the configured primary/
/// secondary semantics are preserved. Disabled/quarantined members are
/// *always* skipped regardless of strategy.
pub async fn execute_combo_strategy_with_capacity<T, F, Fut, C>(
    models: &[String],
    combo_name: Option<&str>,
    strategy: ComboStrategy,
    disabled_members: &[String],
    capacity_check: C,
    mut handle_single_model: F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, ComboAttemptError>>,
    C: Fn(&str) -> ModelCapacity,
{
    // Manual disable + auto-quarantine pre-gate. Applied to the raw
    // member list *before* rotation so the round-robin index doesn't
    // burn turns on members that will never be dispatched to.
    let mut skip: HashSet<String> = disabled_members.iter().cloned().collect();
    if let Some(name) = combo_name {
        skip.extend(quarantined_members(name));
    }

    let active: Vec<String> = models
        .iter()
        .filter(|model| !skip.contains(model.as_str()))
        .cloned()
        .collect();

    if active.is_empty() {
        // Distinguish "operator muted everything" from "transient
        // quarantine" so the caller can decide whether to surface a
        // 4xx vs 503.
        let only_quarantine = !models.is_empty()
            && disabled_members.is_empty()
            && combo_name.is_some()
            && models.iter().all(|m| skip.contains(m));
        return Err(ComboExecutionError {
            status: if only_quarantine { 503 } else { 400 },
            message: if only_quarantine {
                "All combo members are currently quarantined after recent failures".into()
            } else {
                "All combo members are disabled".into()
            },
            earliest_retry_after: None,
        });
    }

    let rotated = get_rotated_models(&active, combo_name, strategy);

    if strategy == ComboStrategy::RoundRobin && rotated.len() > 1 {
        let available: Vec<String> = rotated
            .iter()
            .filter(|model| capacity_check(model.as_str()) == ModelCapacity::Available)
            .cloned()
            .collect();

        if available.is_empty() {
            return Err(ComboExecutionError {
                status: 503,
                message: "All combo providers are at max in-flight capacity".into(),
                earliest_retry_after: None,
            });
        }

        return iterate_combo_models(&available, &mut handle_single_model).await;
    }

    iterate_combo_models(&rotated, &mut handle_single_model).await
}

async fn iterate_combo_models<T, F, Fut>(
    order: &[String],
    handle_single_model: &mut F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, ComboAttemptError>>,
{
    let mut last_error = None;
    let mut earliest_retry_after = None;

    for model in order {
        match handle_single_model(model).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                if let Some(retry_after) = error.retry_after {
                    earliest_retry_after = match earliest_retry_after {
                        Some(current) if current <= retry_after => Some(current),
                        _ => Some(retry_after),
                    };
                }

                let decision = check_fallback_error(error.status, &error.message, 0);
                if !decision.should_fallback {
                    return Err(ComboExecutionError {
                        status: error.status,
                        message: error.message,
                        earliest_retry_after,
                    });
                }

                last_error = Some(error);
            }
        }
    }

    let fallback_error = last_error.unwrap_or(ComboAttemptError {
        status: 503,
        message: "All combo models unavailable".into(),
        retry_after: earliest_retry_after,
    });

    let status = if fallback_error
        .message
        .to_lowercase()
        .contains("no credentials")
    {
        503
    } else {
        fallback_error.status.max(500)
    };

    Err(ComboExecutionError {
        status,
        message: fallback_error.message,
        earliest_retry_after,
    })
}
