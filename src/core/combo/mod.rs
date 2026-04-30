use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::types::Combo;

const LONG_COOLDOWN: Duration = Duration::from_secs(120);
const SHORT_COOLDOWN: Duration = Duration::from_secs(5);
const TRANSIENT_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_BACKOFF_LEVEL: u32 = 15;
const BACKOFF_BASE_MS: u64 = 2_000;
const BACKOFF_MAX_MS: u64 = 5 * 60 * 1_000;

static COMBO_ROTATION_STATE: Lazy<Mutex<HashMap<String, usize>>> =
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

pub fn get_quota_cooldown(backoff_level: u32) -> Duration {
    let level = backoff_level.saturating_sub(1) as u32;
    let cooldown_ms = BACKOFF_BASE_MS.saturating_mul(2u64.saturating_pow(level));
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
        401..=404 => FallbackDecision {
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

pub fn get_combo_models_from_data(model_str: &str, combos: &[Combo]) -> Option<Vec<String>> {
    if model_str.contains('/') {
        return None;
    }

    combos
        .iter()
        .find(|combo| combo.name == model_str && !combo.models.is_empty())
        .map(|combo| combo.models.clone())
}

pub async fn execute_combo_strategy<T, F, Fut>(
    models: &[String],
    combo_name: Option<&str>,
    strategy: ComboStrategy,
    mut handle_single_model: F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, ComboAttemptError>>,
{
    let rotated = get_rotated_models(models, combo_name, strategy);
    let mut last_error = None;
    let mut earliest_retry_after = None;

    for model in &rotated {
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
