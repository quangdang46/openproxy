//! openproxy-82ki — `Combo.extra` (strategy, isActive, fusionConfig,
//! judgeModel) must survive a process restart, and a disabled combo must stop
//! being dispatched.
//!
//! Before the fix `row_to_combo` bound the `data` column to `let _data` and
//! rebuilt the struct with `..Default::default()`, and `export_all` never read
//! column 4, so every restart silently dropped the whole extra blob.

use std::collections::BTreeMap;

use openproxy::core::combo::get_combo_models_from_data;
use openproxy::db::Db;
use openproxy::types::Combo;
use serde_json::{json, Value};
use tempfile::tempdir;
use tempfile::TempDir;

fn combo_with_extra(name: &str, extra: BTreeMap<String, Value>) -> Combo {
    Combo {
        id: format!("{name}-id"),
        name: name.to_string(),
        models: vec!["openai/gpt-4o".into(), "anthropic/claude-sonnet-4-5".into()],
        disabled_models: Vec::new(),
        kind: Some("fallback".into()),
        created_at: Some("2026-01-01T00:00:00Z".into()),
        updated_at: Some("2026-01-01T00:00:00Z".into()),
        extra,
    }
}

fn rich_extra() -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("strategy".into(), json!("round-robin")),
        ("judgeModel".into(), json!("openai/gpt-4o")),
        (
            "fusionConfig".into(),
            json!({"mode": "weighted", "weights": [0.7, 0.3]}),
        ),
    ])
}

/// Re-open the same data dir — the same work `Db::load_from` does on boot,
/// i.e. rebuild the in-memory snapshot purely from the SQLite snapshot.
async fn restart(data_dir: &TempDir) -> Db {
    Db::load_from(data_dir.path()).await.expect("reload db")
}

#[tokio::test]
async fn combo_extra_survives_restart() {
    let data_dir = tempdir().expect("tempdir");
    let extra = rich_extra();

    {
        let db = Db::load_from(data_dir.path()).await.expect("load db");
        db.update(|app| {
            app.combos = vec![combo_with_extra("mymix", extra.clone())];
        })
        .await
        .expect("persist combo");
    }

    let reopened = restart(&data_dir).await;
    let snapshot = reopened.snapshot();
    let stored = snapshot
        .combos
        .iter()
        .find(|c| c.name == "mymix")
        .expect("combo survived restart");

    assert_eq!(
        stored.extra, extra,
        "Combo.extra must survive a restart byte-identical"
    );
    assert_eq!(stored.models.len(), 2);
}

#[tokio::test]
async fn combo_disable_stops_dispatch_and_survives_restart() {
    let data_dir = tempdir().expect("tempdir");
    // This is exactly what `openproxy combo disable <name>` writes.
    let mut extra = rich_extra();
    extra.insert("isActive".into(), Value::Bool(false));

    {
        let db = Db::load_from(data_dir.path()).await.expect("load db");
        db.update(|app| {
            app.combos = vec![combo_with_extra("mymix", extra.clone())];
        })
        .await
        .expect("persist combo");
    }

    let reopened = restart(&data_dir).await;
    let combos = &reopened.snapshot().combos;

    assert_eq!(
        combos
            .iter()
            .find(|c| c.name == "mymix")
            .and_then(|c| c.extra.get("isActive"))
            .cloned(),
        Some(Value::Bool(false)),
        "isActive:false must survive a restart"
    );
    assert_eq!(
        get_combo_models_from_data("mymix", combos),
        None,
        "a disabled combo must not be dispatched"
    );
}

#[tokio::test]
async fn combo_with_empty_extra_still_loads() {
    let data_dir = tempdir().expect("tempdir");

    {
        let db = Db::load_from(data_dir.path()).await.expect("load db");
        db.update(|app| {
            app.combos = vec![combo_with_extra("plain", BTreeMap::new())];
        })
        .await
        .expect("persist combo");
    }

    let reopened = restart(&data_dir).await;
    let combos = &reopened.snapshot().combos;
    let stored = combos
        .iter()
        .find(|c| c.name == "plain")
        .expect("combo loaded");

    assert!(stored.extra.is_empty());
    // An unmarked combo stays dispatchable.
    assert_eq!(
        get_combo_models_from_data("plain", combos),
        Some(vec![
            "openai/gpt-4o".to_string(),
            "anthropic/claude-sonnet-4-5".to_string()
        ])
    );
}
