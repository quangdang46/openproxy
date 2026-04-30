use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use openproxy::core::combo::{
    check_fallback_error, execute_combo_strategy, get_combo_models_from_data, get_quota_cooldown,
    get_rotated_models, reset_combo_rotation, rotation_index, ComboAttemptError, ComboStrategy,
};
use openproxy::types::Combo;

fn combo(name: &str, models: &[&str]) -> Combo {
    Combo {
        id: format!("{name}-id"),
        name: name.to_string(),
        models: models.iter().map(|value| value.to_string()).collect(),
        kind: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

#[test]
fn combo_lookup_ignores_provider_prefixed_models() {
    let combos = vec![combo("writer", &["openai/gpt-4.1", "claude/sonnet"])];

    assert_eq!(
        get_combo_models_from_data("writer", &combos),
        Some(vec!["openai/gpt-4.1".into(), "claude/sonnet".into()])
    );
    assert_eq!(get_combo_models_from_data("openai/gpt-4.1", &combos), None);
    assert_eq!(get_combo_models_from_data("missing", &combos), None);
}

#[test]
fn round_robin_rotation_advances_state() {
    reset_combo_rotation(None);
    let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];

    assert_eq!(
        get_rotated_models(&models, Some("writer"), ComboStrategy::RoundRobin),
        vec!["a", "b", "c"]
    );
    assert_eq!(rotation_index("writer"), Some(1));

    assert_eq!(
        get_rotated_models(&models, Some("writer"), ComboStrategy::RoundRobin),
        vec!["b", "c", "a"]
    );
    assert_eq!(rotation_index("writer"), Some(2));

    assert_eq!(
        get_rotated_models(&models, Some("writer"), ComboStrategy::Fallback),
        vec!["a", "b", "c"]
    );
}

#[test]
fn fallback_error_rules_match_js_semantics() {
    let unauthorized = check_fallback_error(401, "bad token", 0);
    assert_eq!(unauthorized.cooldown.as_secs(), 120);

    let quota_1 = check_fallback_error(429, "rate limit exceeded", 0);
    let quota_2 = check_fallback_error(429, "rate limit exceeded", 1);
    assert_eq!(quota_1.cooldown, get_quota_cooldown(1));
    assert_eq!(quota_2.cooldown, get_quota_cooldown(2));
    assert!(quota_2.cooldown > quota_1.cooldown);

    let transient = check_fallback_error(503, "temporary upstream issue", 0);
    assert_eq!(transient.cooldown.as_secs(), 30);
}

#[test]
fn fallback_error_rules_retry_for_transient_gateway_statuses() {
    for status in [502, 503, 504] {
        let decision = check_fallback_error(status, "upstream gateway issue", 0);
        assert!(decision.should_fallback, "{status} should trigger fallback");
        assert_eq!(
            decision.cooldown.as_secs(),
            30,
            "{status} should use transient cooldown"
        );
        assert_eq!(
            decision.new_backoff_level, None,
            "{status} should not bump quota backoff"
        );
    }
}

#[tokio::test]
async fn combo_strategy_stops_after_first_success() {
    reset_combo_rotation(None);
    let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = attempts.clone();

    let result = execute_combo_strategy(
        &models,
        Some("writer"),
        ComboStrategy::Fallback,
        move |model| {
            let seen = seen.clone();
            let model = model.to_string();
            async move {
                seen.lock().push(model.clone());
                match model.as_str() {
                    "a" => Err(ComboAttemptError {
                        status: 503,
                        message: "temporary failure".into(),
                        retry_after: None,
                    }),
                    "b" => Ok("ok"),
                    _ => Err(ComboAttemptError {
                        status: 500,
                        message: "should not reach".into(),
                        retry_after: None,
                    }),
                }
            }
        },
    )
    .await;

    assert_eq!(result, Ok("ok"));
    assert_eq!(
        attempts.lock().clone(),
        vec!["a".to_string(), "b".to_string()]
    );
}

#[tokio::test]
async fn combo_strategy_round_robin_uses_rotated_order() {
    reset_combo_rotation(None);
    let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];

    let first = execute_combo_strategy(
        &models,
        Some("writer"),
        ComboStrategy::RoundRobin,
        |model| {
            let model = model.to_string();
            async move { Ok(model) }
        },
    )
    .await;
    let second = execute_combo_strategy(
        &models,
        Some("writer"),
        ComboStrategy::RoundRobin,
        |model| {
            let model = model.to_string();
            async move { Ok(model) }
        },
    )
    .await;

    assert_eq!(first, Ok("a".to_string()));
    assert_eq!(second, Ok("b".to_string()));
}

#[tokio::test]
async fn combo_strategy_returns_earliest_retry_after_on_exhaustion() {
    reset_combo_rotation(None);
    let models = vec!["a".to_string(), "b".to_string()];
    let early = Utc::now() + Duration::seconds(30);
    let late = Utc::now() + Duration::seconds(90);

    let error = execute_combo_strategy(
        &models,
        Some("writer"),
        ComboStrategy::Fallback,
        move |model| {
            let retry_after = if model == "a" { late } else { early };
            async move {
                Err::<(), _>(ComboAttemptError {
                    status: 401,
                    message: "no credentials".into(),
                    retry_after: Some(retry_after),
                })
            }
        },
    )
    .await
    .expect_err("combo should fail");

    assert_eq!(error.status, 503);
    assert_eq!(error.message, "no credentials");
    assert_eq!(error.earliest_retry_after, Some(early));
}
