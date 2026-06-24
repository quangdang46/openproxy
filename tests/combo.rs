use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use openproxy::core::combo::{
    check_fallback_error, clear_combo_member_quarantine, clear_combo_quarantine,
    execute_combo_strategy, execute_combo_strategy_with_capacity, get_combo_models_from_data,
    get_quota_cooldown, get_rotated_models, mark_combo_member_quarantined, reset_combo_rotation,
    rotation_index, ComboAttemptError, ComboStrategy, ModelCapacity,
};
use openproxy::types::Combo;

fn combo(name: &str, models: &[&str]) -> Combo {
    Combo {
        id: format!("{name}-id"),
        name: name.to_string(),
        models: models.iter().map(|value| value.to_string()).collect(),
        disabled_models: Vec::new(),
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
    // Use a unique combo name + reset only that name so this test does not
    // race with other tests that mutate the global COMBO_ROTATION_STATE.
    let combo_name = "writer-rotation-advances";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];

    assert_eq!(
        get_rotated_models(&models, Some(combo_name), ComboStrategy::RoundRobin, 0),
        vec!["a", "b", "c"]
    );
    assert_eq!(rotation_index(combo_name), Some(1));

    assert_eq!(
        get_rotated_models(&models, Some(combo_name), ComboStrategy::RoundRobin, 0),
        vec!["b", "c", "a"]
    );
    assert_eq!(rotation_index(combo_name), Some(2));

    assert_eq!(
        get_rotated_models(&models, Some(combo_name), ComboStrategy::Fallback, 0),
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
    let combo_name = "writer-stops-after-success";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = attempts.clone();

    let result = execute_combo_strategy(
        &models,
        Some(combo_name),
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
    let combo_name = "writer-round-robin-rotated";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];

    let first = execute_combo_strategy(
        &models,
        Some(combo_name),
        ComboStrategy::RoundRobin,
        |model| {
            let model = model.to_string();
            async move { Ok(model) }
        },
    )
    .await;
    let second = execute_combo_strategy(
        &models,
        Some(combo_name),
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
    let combo_name = "writer-earliest-retry-after";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string()];
    let early = Utc::now() + Duration::seconds(30);
    let late = Utc::now() + Duration::seconds(90);

    let error = execute_combo_strategy(
        &models,
        Some(combo_name),
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

#[tokio::test]
async fn round_robin_skips_busy_models_when_any_available() {
    // RR rotation starts at "a"; "a" reports Busy, so we expect the handler
    // to be invoked only with "b" (the first Available in rotation order)
    // and never touch "a" or "c" (even though "c" is also Available).
    let combo_name = "writer-skip-busy";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];

    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = attempts.clone();

    let result = execute_combo_strategy_with_capacity(
        &models,
        Some(combo_name),
        ComboStrategy::RoundRobin,
        &[],
        |model| match model {
            "a" => ModelCapacity::Busy,
            _ => ModelCapacity::Available,
        },
        move |model| {
            let seen = seen.clone();
            let model = model.to_string();
            async move {
                seen.lock().push(model.clone());
                Ok(model)
            }
        },
    )
    .await;

    assert_eq!(result, Ok("b".to_string()));
    assert_eq!(attempts.lock().clone(), vec!["b".to_string()]);
}

#[tokio::test]
async fn round_robin_fails_fast_when_all_busy() {
    // Every member reports Busy: the strategy must short-circuit with 503
    // rather than burning latency on per-account fallback inside each
    // saturated provider. This is the multi-repo "stuck agent" guard.
    let combo_name = "writer-all-busy";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string()];

    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(0usize));
    let counter = attempts.clone();

    let error = execute_combo_strategy_with_capacity(
        &models,
        Some(combo_name),
        ComboStrategy::RoundRobin,
        &[],
        |_| ModelCapacity::Busy,
        move |_model| {
            let counter = counter.clone();
            async move {
                *counter.lock() += 1;
                Ok::<_, ComboAttemptError>("should-not-run")
            }
        },
    )
    .await
    .expect_err("expected 503 when every combo member is busy");

    assert_eq!(error.status, 503);
    assert!(error.message.to_lowercase().contains("capacity"));
    assert_eq!(*attempts.lock(), 0);
}

#[tokio::test]
async fn fallback_strategy_ignores_capacity_check() {
    // Fallback semantics must preserve declared priority order: capacity
    // is advisory only. "a" is Busy but it's the configured primary, so
    // we still try it first.
    let combo_name = "writer-fallback-keeps-order";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string()];
    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = attempts.clone();

    let result = execute_combo_strategy_with_capacity(
        &models,
        Some(combo_name),
        ComboStrategy::Fallback,
        &[],
        |_| ModelCapacity::Busy,
        move |model| {
            let seen = seen.clone();
            let model = model.to_string();
            async move {
                seen.lock().push(model.clone());
                Ok(model)
            }
        },
    )
    .await;

    assert_eq!(result, Ok("a".to_string()));
    assert_eq!(attempts.lock().clone(), vec!["a".to_string()]);
}

#[tokio::test]
async fn fallback_skips_explicitly_disabled_members() {
    // The "manual bypass" knob: if the operator put `a` in the disabled
    // list, the dispatcher must skip it entirely even in Fallback mode
    // and dispatch to `b` first. This is what stops the CLI agent from
    // hanging on a known-broken combo member while keeping it visible
    // in the configured member list.
    let combo_name = "writer-disabled-skip-fallback";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string()];
    let disabled = vec!["a".to_string()];
    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = attempts.clone();

    let result = execute_combo_strategy_with_capacity(
        &models,
        Some(combo_name),
        ComboStrategy::Fallback,
        &disabled,
        |_| ModelCapacity::Available,
        move |model| {
            let seen = seen.clone();
            let model = model.to_string();
            async move {
                seen.lock().push(model.clone());
                Ok(model)
            }
        },
    )
    .await;

    assert_eq!(result, Ok("b".to_string()));
    assert_eq!(attempts.lock().clone(), vec!["b".to_string()]);
}

#[tokio::test]
async fn round_robin_skips_explicitly_disabled_members() {
    // Same pre-gate must also apply to round-robin so the rotation
    // index never lands on a muted member.
    let combo_name = "writer-disabled-skip-rr";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let disabled = vec!["b".to_string()];
    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = attempts.clone();

    // Three RR ticks should visit only `a` and `c`, never `b`.
    for _ in 0..3 {
        let seen_inner = seen.clone();
        let _ = execute_combo_strategy_with_capacity(
            &models,
            Some(combo_name),
            ComboStrategy::RoundRobin,
            &disabled,
            |_| ModelCapacity::Available,
            move |model| {
                let seen = seen_inner.clone();
                let model = model.to_string();
                async move {
                    seen.lock().push(model.clone());
                    Ok(model)
                }
            },
        )
        .await;
    }

    let visited = attempts.lock().clone();
    assert!(!visited.contains(&"b".to_string()), "got {visited:?}");
    assert!(visited.contains(&"a".to_string()), "got {visited:?}");
    assert!(visited.contains(&"c".to_string()), "got {visited:?}");
}

#[tokio::test]
async fn all_disabled_returns_clear_error() {
    // Edge case: every member is muted. The dispatcher must surface a
    // clear error instead of falling through with zero attempts.
    let combo_name = "writer-all-disabled";
    reset_combo_rotation(Some(combo_name));
    let models = vec!["a".to_string(), "b".to_string()];
    let disabled = vec!["a".to_string(), "b".to_string()];
    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(0usize));
    let counter = attempts.clone();

    let error = execute_combo_strategy_with_capacity(
        &models,
        Some(combo_name),
        ComboStrategy::Fallback,
        &disabled,
        |_| ModelCapacity::Available,
        move |_model| {
            let counter = counter.clone();
            async move {
                *counter.lock() += 1;
                Ok::<_, ComboAttemptError>("should-not-run")
            }
        },
    )
    .await
    .expect_err("expected error when every combo member is disabled");

    assert_eq!(error.status, 400);
    assert!(error.message.to_lowercase().contains("disabled"));
    assert_eq!(*attempts.lock(), 0);
}

#[tokio::test]
async fn quarantined_members_are_skipped_until_ttl_expires() {
    // The auto pre-gate: once a member is registered in the quarantine
    // map, subsequent combo dispatches must skip it until the TTL
    // elapses. This is what stops "broken model → retry → broken
    // model → retry" from making the CLI agent appear to hang.
    let combo_name = "writer-quarantine-skip";
    reset_combo_rotation(Some(combo_name));
    clear_combo_quarantine(combo_name);
    let models = vec!["a".to_string(), "b".to_string()];

    mark_combo_member_quarantined(combo_name, "a", std::time::Duration::from_secs(30));

    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = attempts.clone();

    let result = execute_combo_strategy_with_capacity(
        &models,
        Some(combo_name),
        ComboStrategy::Fallback,
        &[],
        |_| ModelCapacity::Available,
        move |model| {
            let seen = seen.clone();
            let model = model.to_string();
            async move {
                seen.lock().push(model.clone());
                Ok(model)
            }
        },
    )
    .await;

    assert_eq!(result, Ok("b".to_string()));
    assert_eq!(attempts.lock().clone(), vec!["b".to_string()]);

    // Cleanup so we don't pollute global state for sibling tests.
    clear_combo_member_quarantine(combo_name, "a");
}

#[tokio::test]
async fn quarantine_clear_restores_member_to_rotation() {
    // After `clear_combo_member_quarantine`, the member must be visible
    // to the dispatcher again — confirms the cleanup path used by both
    // the UI "Clear cooldowns" button and the rotation reset on edit.
    let combo_name = "writer-quarantine-clear";
    reset_combo_rotation(Some(combo_name));
    clear_combo_quarantine(combo_name);
    let models = vec!["a".to_string()];

    mark_combo_member_quarantined(combo_name, "a", std::time::Duration::from_secs(30));
    clear_combo_member_quarantine(combo_name, "a");

    let attempts = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = attempts.clone();

    let result = execute_combo_strategy_with_capacity(
        &models,
        Some(combo_name),
        ComboStrategy::Fallback,
        &[],
        |_| ModelCapacity::Available,
        move |model| {
            let seen = seen.clone();
            let model = model.to_string();
            async move {
                seen.lock().push(model.clone());
                Ok(model)
            }
        },
    )
    .await;

    assert_eq!(result, Ok("a".to_string()));
}
