//! A model the operator switched off must stay switched off after a restart.
//!
//! `AGENTS.md` makes this a core requirement for the providers page: "user
//! controls Available Models (disable/enable/custom) … persisted in SQLite —
//! must survive binary rebuilds/updates."
//!
//! The rows were reaching SQLite. `AppDb.extra["disabledModels"]` holds
//! `{"provider": ["model", …]}`, `diff_disabled_models` flattens that map into
//! the `(provider, model)` pairs table, and on the way back `export_all`
//! reassembles the table into an array of `{provider, model}` rows. The load
//! path handed that array straight to `extra`, and `disabled_models_from_db`
//! deserialized it as a map — failed, and fell back to an empty set through
//! `unwrap_or_default()`.
//!
//! The net effect was silent and asymmetric: the disable took effect for the
//! life of the process, the rows sat in SQLite looking perfectly healthy, and
//! every restart brought the models back with no error anywhere. Only a
//! round-trip through a real reload catches it — asserting on the in-memory
//! map alone would pass even while the reload path was broken.
use openproxy::db::Db;
use serde_json::json;
use tempfile::tempdir;

const DISABLED: &str = "kilocode";
const MODELS: [&str; 2] = ["kc/openai/gpt-4.1", "kc/google/gemini-2.5-flash"];

fn expected() -> serde_json::Value {
    json!({ DISABLED: MODELS })
}

/// The disabled list is a set — `is_model_disabled` asks whether an id is in
/// it, and nothing downstream depends on order. Storage is a flat
/// `(provider, model)` table, so a reload hands the ids back in primary-key
/// order. Compare membership, not position.
fn assert_same_disabled_set(actual: Option<&serde_json::Value>, what: &str) {
    let Some(value) = actual else {
        panic!("{what}: disabledModels is missing entirely");
    };
    let map: std::collections::BTreeMap<String, Vec<String>> =
        serde_json::from_value(value.clone()).unwrap_or_else(|e| {
            panic!("{what}: disabledModels is not a provider->ids map ({e}): {value}")
        });

    let mut got = map.get(DISABLED).cloned().unwrap_or_default();
    let mut want = MODELS.to_vec();
    got.sort();
    want.sort();
    assert_eq!(
        got, want,
        "{what}: the wrong models are disabled for {DISABLED}"
    );
}

/// Disable two models, drop the handle the way a process exit does, then
/// reload from the same data dir — exactly what the next boot does. The
/// disabled map must come back identical.
#[tokio::test]
async fn disabled_models_survive_a_reload() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    {
        let db = Db::load_from(dir).await.expect("db");
        db.update(|state| {
            state.extra.insert("disabledModels".to_string(), expected());
        })
        .await
        .expect("seed");
    } // handle dropped — the process "exits" here

    let db = Db::load_from(dir).await.expect("reload");
    let snapshot = db.snapshot();
    let stored = snapshot.extra.get("disabledModels");

    assert_same_disabled_set(
        stored,
        "after a reload the export shape (a flat array of {provider, model} \
         rows) must be folded back into the map the readers deserialize, or \
         the operator's disabled models silently come back enabled",
    );
}

/// The round-trip has to be stable, or every restart would drift the shape.
/// A fresh install must also not carry a `disabledModels` key at all, so an
/// export/import of an untouched install stays byte-identical.
#[tokio::test]
async fn disabled_models_round_trip_is_stable() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    let db = Db::load_from(dir).await.expect("db");
    db.update(|state| {
        state.extra.insert("disabledModels".to_string(), expected());
    })
    .await
    .expect("seed");

    assert_same_disabled_set(
        db.snapshot().extra.get("disabledModels"),
        "writing a disabled map must not change its shape before it hits disk",
    );

    drop(db);
    let reloaded = Db::load_from(dir).await.expect("reload");
    assert_same_disabled_set(
        reloaded.snapshot().extra.get("disabledModels"),
        "a second load produced a different shape than the first",
    );

    let empty = tempdir().expect("tempdir2");
    let fresh = Db::load_from(empty.path()).await.expect("fresh");
    let snapshot = fresh.snapshot();
    assert!(
        !snapshot.extra.contains_key("disabledModels"),
        "a fresh install must not carry a disabledModels key"
    );
}
