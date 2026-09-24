//! openproxy-63hh — a combo's dispatch strategy must live in
//! `Combo.extra["strategy"]`, never in `Combo.kind`.
//!
//! `kind` is the media modality (`llm` / `tts` / `image`). `combo create/
//! edit/apply --strategy X` wrote X into `kind`, which (a) hid the combo from
//! the Combos page (`!c.kind || c.kind === "llm"`) and from `GET /v1/models`,
//! and (b) no-op'd the strategy, because `strategy_for_combo` reads
//! `settings.combo_strategies` → `extra["strategy"]` → the global default and
//! never looks at `kind`.
//!
//! Also covers the two halves of the same contract defect: the advertised
//! combo schema (`openproxy schema show combo`) must match what the write
//! path honours, and rows already corrupted on disk must be repaired.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use openproxy::cli::combo::{run as run_combo, ComboCmd};
use openproxy::cli::output::OutputCtx;
use openproxy::cli::schema::{example_for, schema_for};
use openproxy::core::combo::{parse_combo_strategy, strategy_for_combo, ComboStrategy};
use openproxy::db::sqlite::patch;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, Combo};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "combo-strategy-kind-test-key";

/// Every strategy `parse_combo_strategy` arms. `sticky-round-robin` is absent
/// on purpose: it is advertised by the frozen schema enum but still has no
/// dispatch arm (tracked separately — see the notes on this bead).
const ARMED_STRATEGIES: &[(&str, ComboStrategy)] = &[
    ("fallback", ComboStrategy::Fallback),
    ("round-robin", ComboStrategy::RoundRobin),
    ("fusion", ComboStrategy::Fusion),
    ("auto-combo", ComboStrategy::AutoCombo),
    ("hedging", ComboStrategy::Hedging),
    ("shadow", ComboStrategy::Shadow),
    ("cheapest", ComboStrategy::Cheapest),
    ("fastest", ComboStrategy::Fastest),
    ("quality", ComboStrategy::Quality),
];

fn models() -> Vec<String> {
    vec![
        "openai/gpt-4o".to_string(),
        "anthropic/claude-sonnet-4-5".to_string(),
    ]
}

fn combo_named(name: &str, kind: Option<&str>) -> Combo {
    Combo {
        id: format!("{name}-id"),
        name: name.to_string(),
        models: models(),
        disabled_models: Vec::new(),
        kind: kind.map(str::to_string),
        created_at: Some("2026-01-01T00:00:00Z".into()),
        updated_at: Some("2026-01-01T00:00:00Z".into()),
        extra: BTreeMap::new(),
    }
}

fn strategy_of(combo: &Combo) -> Option<&str> {
    combo.extra.get("strategy").and_then(Value::as_str)
}

#[tokio::test]
async fn cli_create_stores_strategy_in_extra_and_leaves_kind_unset() {
    for (idx, (strategy, expected)) in ARMED_STRATEGIES.iter().enumerate() {
        let dir = tempdir().expect("tempdir");
        let db = Db::load_from(dir.path()).await.expect("load db");
        let name = format!("create-{idx}");

        run_combo(
            ComboCmd::Create {
                name: name.clone(),
                models: models(),
                strategy: (*strategy).to_string(),
            },
            &db,
            OutputCtx::robot(),
        )
        .await
        .expect("combo create");

        let combo = db.combo_by_name(&name).expect("combo exists after create");
        assert_eq!(
            combo.kind, None,
            "--strategy {strategy} must not be written into kind (kind is the media modality)"
        );
        assert_eq!(
            strategy_of(&combo),
            Some(*strategy),
            "strategy must be stored in extra"
        );
        assert_eq!(
            strategy_for_combo(&db.snapshot(), &name),
            *expected,
            "requested strategy {strategy} must be honoured at dispatch"
        );
    }
}

#[tokio::test]
async fn cli_edit_moves_strategy_into_extra_and_preserves_modality_kind() {
    let dir = tempdir().expect("tempdir");
    let db = Db::load_from(dir.path()).await.expect("load db");

    // A combo created through the API carries a real modality in `kind`.
    db.update(|app| app.combos = vec![combo_named("written", Some("llm"))])
        .await
        .expect("seed combo");

    run_combo(
        ComboCmd::Edit {
            name: "written".to_string(),
            models: None,
            strategy: Some("round-robin".to_string()),
        },
        &db,
        OutputCtx::robot(),
    )
    .await
    .expect("combo edit");

    let combo = db
        .combo_by_name("written")
        .expect("combo exists after edit");
    assert_eq!(
        combo.kind.as_deref(),
        Some("llm"),
        "editing the strategy must leave the modality kind untouched"
    );
    assert_eq!(strategy_of(&combo), Some("round-robin"));
    assert_eq!(
        strategy_for_combo(&db.snapshot(), "written"),
        ComboStrategy::RoundRobin
    );
}

#[tokio::test]
async fn cli_apply_writes_strategy_to_extra_and_repairs_legacy_kind() {
    let dir = tempdir().expect("tempdir");
    let db = Db::load_from(dir.path()).await.expect("load db");

    // `legacy` is a row already corrupted by the old `--strategy` behaviour.
    db.update(|app| app.combos = vec![combo_named("legacy", Some("round-robin"))])
        .await
        .expect("seed combo");

    let doc = json!([
        { "name": "legacy", "models": ["openai/gpt-4o"], "strategy": "fusion" },
        { "name": "modality", "models": ["openai/gpt-4o"], "strategy": "cheapest", "kind": "llm" },
        { "name": "no-kind", "models": ["openai/gpt-4o"] }
    ]);
    let doc_path = dir.path().join("combos.json");
    std::fs::write(&doc_path, doc.to_string()).expect("write apply doc");

    run_combo(
        ComboCmd::Apply {
            from_file: doc_path.to_string_lossy().into_owned(),
            prune: false,
        },
        &db,
        OutputCtx::robot(),
    )
    .await
    .expect("combo apply");

    let legacy = db.combo_by_name("legacy").expect("legacy combo");
    assert_eq!(
        legacy.kind, None,
        "apply must clear a strategy that a previous release stored in kind"
    );
    assert_eq!(strategy_of(&legacy), Some("fusion"));
    assert_eq!(
        strategy_for_combo(&db.snapshot(), "legacy"),
        ComboStrategy::Fusion
    );

    let modality = db.combo_by_name("modality").expect("modality combo");
    assert_eq!(
        modality.kind.as_deref(),
        Some("llm"),
        "an explicit modality kind must survive apply"
    );
    assert_eq!(strategy_of(&modality), Some("cheapest"));
    assert_eq!(
        strategy_for_combo(&db.snapshot(), "modality"),
        ComboStrategy::Cheapest
    );

    let plain = db.combo_by_name("no-kind").expect("no-kind combo");
    assert_eq!(plain.kind, None);
    assert_eq!(strategy_of(&plain), None);
    assert_eq!(
        strategy_for_combo(&db.snapshot(), "no-kind"),
        ComboStrategy::Fallback,
        "a combo with no strategy follows the global default"
    );
}

#[tokio::test]
async fn sqlite_repair_clears_strategy_values_from_combo_kind() {
    let dir = tempdir().expect("tempdir");
    let db = Db::load_from(dir.path()).await.expect("load db");

    db.update(|app| {
        app.combos = vec![
            combo_named("legacy-rr", Some("round-robin")),
            combo_named("legacy-default", Some("fallback")),
            combo_named("legacy-mixed-case", Some("Sticky-Round-Robin")),
            combo_named("modality", Some("llm")),
            combo_named("no-kind", None),
        ];
    })
    .await
    .expect("seed combos");

    let changed = db
        .sqlite_handle()
        .with_conn(|conn| patch::clear_combo_kind_strategy_leak(conn))
        .expect("run repair");
    assert_eq!(
        changed, 3,
        "only the three strategy-valued rows are rewritten"
    );

    let kinds = |name: &str| -> Option<String> {
        db.sqlite_handle()
            .with_conn(|conn| {
                conn.query_row("SELECT kind FROM combos WHERE name = ?1", [name], |row| {
                    row.get::<_, Option<String>>(0)
                })
            })
            .unwrap_or(None)
    };

    assert_eq!(kinds("legacy-rr"), None, "round-robin must be cleared");
    assert_eq!(kinds("legacy-default"), None, "fallback must be cleared");
    assert_eq!(
        kinds("legacy-mixed-case"),
        None,
        "the repair must be case-insensitive"
    );
    assert_eq!(
        kinds("modality").as_deref(),
        Some("llm"),
        "a real modality is not a strategy and must be preserved"
    );
    assert_eq!(kinds("no-kind"), None);

    let again = db
        .sqlite_handle()
        .with_conn(|conn| patch::clear_combo_kind_strategy_leak(conn))
        .expect("re-run repair");
    assert_eq!(again, 0, "the repair is idempotent");
}

#[test]
fn sqlite_repair_is_a_noop_when_the_combos_table_is_absent() {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    let changed = patch::clear_combo_kind_strategy_leak(&conn).expect("repair on empty db");
    assert_eq!(changed, 0);
}

async fn seeded_app() -> (Router, Arc<Db>) {
    let dir = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(dir.path()).await.expect("db"));
    db.update(|app| {
        app.api_keys = vec![ApiKey {
            id: "key-1".into(),
            name: "Local".into(),
            key: TEST_KEY.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            monthly_budget_usd: None,
            extra: BTreeMap::new(),
        }];
    })
    .await
    .expect("seed api key");
    (openproxy::build_app(AppState::new(db.clone())), db)
}

#[tokio::test]
async fn schema_example_combo_round_trips_strategy_through_post_api() {
    let example = example_for("combo").expect("combo example");
    let strategy = example["strategy"]
        .as_str()
        .expect("the combo example advertises a strategy")
        .to_string();
    assert_ne!(
        strategy, "fallback",
        "the example must exercise a non-default strategy so a dropped field is visible"
    );

    let (app, db) = seeded_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/combos")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(example.to_string()))
                .unwrap(),
        )
        .await
        .expect("post combo");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .expect("read body");
    let created: Value = serde_json::from_slice(&body).expect("json body");

    assert_eq!(
        created["strategy"],
        json!(strategy),
        "POST /api/combos must not silently drop the advertised strategy"
    );
    assert_eq!(created["kind"], Value::Null, "modality stays unset");

    let name = created["name"].as_str().expect("name").to_string();
    assert_eq!(
        strategy_for_combo(&db.snapshot(), &name),
        parse_combo_strategy(&strategy),
        "the round-tripped strategy is the one honoured at dispatch"
    );
}

#[tokio::test]
async fn update_combo_api_round_trips_strategy_and_is_active() {
    let (app, db) = seeded_app().await;

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/combos")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "editable", "models": ["openai/gpt-4o"] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("post combo");
    assert_eq!(create.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(create.into_body(), 8192)
        .await
        .expect("read body");
    let created: Value = serde_json::from_slice(&body).expect("json body");
    let id = created["id"].as_str().expect("id").to_string();

    let update = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/combos/{id}"))
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "strategy": "round-robin", "isActive": false }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("put combo");
    assert_eq!(update.status(), StatusCode::OK);
    let body = axum::body::to_bytes(update.into_body(), 8192)
        .await
        .expect("read body");
    let updated: Value = serde_json::from_slice(&body).expect("json body");

    assert_eq!(updated["strategy"], json!("round-robin"));
    assert_eq!(updated["isActive"], json!(false));
    assert_eq!(updated["kind"], Value::Null);
    assert_eq!(
        strategy_for_combo(&db.snapshot(), "editable"),
        ComboStrategy::RoundRobin
    );
}

#[test]
fn combo_schema_advertises_what_the_write_path_honours() {
    let schema = schema_for("combo").expect("combo schema");
    let properties = &schema["properties"];

    for required in ["name", "models", "strategy", "isActive", "kind"] {
        assert!(
            properties.get(required).is_some(),
            "the combo schema must advertise `{required}`"
        );
    }
    assert_eq!(
        properties["kind"]["type"],
        json!(["string", "null"]),
        "kind is the media modality (llm / tts / image), not a strategy"
    );

    let strategies: Vec<&str> = properties["strategy"]["enum"]
        .as_array()
        .expect("strategy enum")
        .iter()
        .map(|v| v.as_str().expect("enum member is a string"))
        .collect();

    for (name, _) in ARMED_STRATEGIES {
        assert!(
            strategies.contains(name),
            "the schema omits `{name}`, which parse_combo_strategy arms and the Combos page offers"
        );
    }
    // Frozen namespace: removing an advertised member is breaking, so
    // `sticky-round-robin` stays advertised until its dispatch arm exists.
    assert!(
        strategies.contains(&"sticky-round-robin"),
        "removing a member from the frozen v1 enum is a breaking change"
    );

    for name in &strategies {
        // `fallback` is the documented default (Fallback is its arm), and
        // `sticky-round-robin` is a known gap with no dispatch arm yet.
        if *name == "fallback" || *name == "sticky-round-robin" {
            continue;
        }
        assert_ne!(
            parse_combo_strategy(name),
            ComboStrategy::Fallback,
            "the schema advertises `{name}` but the dispatcher has no arm for it"
        );
    }
}
