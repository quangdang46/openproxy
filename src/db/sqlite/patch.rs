//! Incremental SQLite patch application (H19).
//!
//! The previous write path (`Db::update` → `import_db`) deleted and re-inserted
//! every row of 11 tables on *every* config change — even a single settings
//! toggle. That churned the WAL, rewrote unchanged rows, and wiped the
//! append-only `usageHistory`/`usageDaily`/`requestDetails` tables on every
//! config change even though the in-memory `AppDb` payload never contains
//! usage data.
//!
//! This module replaces that with a **diff**: given the old and new in-memory
//! `AppDb` snapshots, it computes the rows that were added / removed / changed
//! and writes only those, inside a single transaction (atomic — rollback on
//! error, same guarantee as `import_db`). Unchanged rows are never written.
//!
//! `usageHistory` / `usageDaily` / `requestDetails` are never touched here —
//! they are append-only observability tables handled by the repo layer.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::types::{AppDb, ModelAliasTarget, PricingTable, Settings};

use super::repo::{
    api_key_repo, combo_repo, connection_repo, kv_repo, node_repo, pool_repo,
};

/// The in-memory representation of disabled models: `provider -> [model ids]`.
type DisabledMap = BTreeMap<String, Vec<String>>;

/// Apply the difference between two `AppDb` snapshots to SQLite in one
/// transaction. Never touches `usageHistory` / `usageDaily` / `requestDetails`.
pub fn apply_app_db_diff(conn: &Connection, old: &AppDb, new: &AppDb) -> rusqlite::Result<()> {
    if old == new {
        return Ok(());
    }

    if old.settings != new.settings {
        settings_upsert(conn, &new.settings)?;
    }

    diff_by_id(
        conn,
        &old.provider_connections,
        &new.provider_connections,
        |c| &c.id,
        |c, x| connection_repo::create(c, x),
        |c, x| connection_repo::update(c, x),
        |c, id| connection_repo::delete(c, id),
    )?;
    diff_by_id(
        conn,
        &old.provider_nodes,
        &new.provider_nodes,
        |n| &n.id,
        |c, x| node_repo::create(c, x),
        |c, x| node_repo::update(c, x),
        |c, id| node_repo::delete(c, id),
    )?;
    diff_by_id(
        conn,
        &old.proxy_pools,
        &new.proxy_pools,
        |p| &p.id,
        |c, x| pool_repo::create(c, x),
        |c, x| pool_repo::update(c, x),
        |c, id| pool_repo::delete(c, id),
    )?;
    diff_by_id(
        conn,
        &old.api_keys,
        &new.api_keys,
        |k| &k.id,
        |c, x| api_key_repo::create(c, x),
        |c, x| api_key_repo::update(c, x),
        |c, id| api_key_repo::delete(c, id),
    )?;
    diff_by_id(
        conn,
        &old.combos,
        &new.combos,
        |c| &c.id,
        |c, x| combo_repo::create(c, x),
        |c, x| combo_repo::update(c, x),
        |c, id| combo_repo::delete(c, id),
    )?;

    diff_kv_scope(conn, "modelAliases", kv_map(&old.model_aliases), kv_map(&new.model_aliases))?;
    diff_kv_scope(
        conn,
        "mitmAlias",
        kv_map_nested(&old.mitm_alias),
        kv_map_nested(&new.mitm_alias),
    )?;
    diff_kv_scope(conn, "pricing", kv_map_pricing(&old.pricing), kv_map_pricing(&new.pricing))?;
    diff_kv_scope(
        conn,
        "customModels",
        custom_models_map(&old.custom_models),
        custom_models_map(&new.custom_models),
    )?;

    diff_disabled_models(
        conn,
        &disabled_from_extra(&old.extra),
        &disabled_from_extra(&new.extra),
    )?;

    Ok(())
}

/// Write a single-row `settings` upsert. Same SQL as `Db::update_settings`.
fn settings_upsert(conn: &Connection, settings: &Settings) -> rusqlite::Result<()> {
    let settings_str = serde_json::to_string(settings).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO settings(id, data) VALUES(1, ?1) ON CONFLICT(id) DO UPDATE SET data = excluded.data",
        params![settings_str],
    )?;
    Ok(())
}

/// Generic per-id diff: index old/new into hashmaps keyed by `id`, then
/// `create` rows only in `new`, `delete` rows only in `old`, and `update` rows
/// present in both but changed. Unchanged rows are never written.
fn diff_by_id<C, K, Create, Update, Delete>(
    conn: &Connection,
    old: &[C],
    new: &[C],
    key: impl Fn(&C) -> &K,
    create: Create,
    update: Update,
    delete: Delete,
) -> rusqlite::Result<()>
where
    K: Eq + std::hash::Hash + Clone,
    C: PartialEq + Clone,
    Create: Fn(&Connection, &C) -> rusqlite::Result<()>,
    Update: Fn(&Connection, &C) -> rusqlite::Result<()>,
    Delete: Fn(&Connection, &K) -> rusqlite::Result<()>,
{
    let old_by_key: HashMap<&K, &C> = old.iter().map(|c| (key(c), c)).collect();
    let new_by_key: HashMap<&K, &C> = new.iter().map(|c| (key(c), c)).collect();

    // Rows added.
    for (k, c) in &new_by_key {
        if !old_by_key.contains_key(k) {
            create(conn, c)?;
        }
    }
    // Rows removed.
    for (k, _) in &old_by_key {
        if !new_by_key.contains_key(k) {
            delete(conn, k)?;
        }
    }
    // Rows present in both but changed.
    for (k, new_c) in &new_by_key {
        if let Some(old_c) = old_by_key.get(k) {
            if **old_c != **new_c {
                update(conn, new_c)?;
            }
        }
    }
    Ok(())
}

/// Diff two KV scope maps (key → value) and apply add/remove/change via the
/// existing `kv_repo` UPSERT.
fn diff_kv_scope(
    conn: &Connection,
    scope: &str,
    old: HashMap<String, Value>,
    new: HashMap<String, Value>,
) -> rusqlite::Result<()> {
    for (k, v) in &new {
        match old.get(k) {
            Some(old_v) if old_v == v => {}
            _ => kv_repo::set(conn, scope, k, v)?,
        }
    }
    for k in old.keys() {
        if !new.contains_key(k) {
            kv_repo::delete(conn, scope, k)?;
        }
    }
    Ok(())
}

/// Serialize a `BTreeMap<String, ModelAliasTarget>` into a `String → Value` map.
fn kv_map(map: &BTreeMap<String, ModelAliasTarget>) -> HashMap<String, Value> {
    map.iter()
        .map(|(k, v)| {
            (
                k.clone(),
                serde_json::to_value(v).unwrap_or(Value::Null),
            )
        })
        .collect()
}

/// Serialize `mitm_alias` (`BTreeMap<String, BTreeMap<String, String>>`) into a
/// `String → Value` map.
fn kv_map_nested(map: &BTreeMap<String, BTreeMap<String, String>>) -> HashMap<String, Value> {
    map.iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
        .collect()
}

/// Serialize `PricingTable` into a `String → Value` map.
fn kv_map_pricing(map: &PricingTable) -> HashMap<String, Value> {
    map.iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
        .collect()
}

/// Serialize `custom_models` (`Vec<CustomModel>`) into `id → Value`, matching
/// the keying the `import_all` path uses (kv key = model id).
fn custom_models_map(models: &[crate::types::CustomModel]) -> HashMap<String, Value> {
    models
        .iter()
        .map(|m| (m.id.clone(), serde_json::to_value(m).unwrap_or(Value::Null)))
        .collect()
}

/// Diff the `disabledModels` in-memory map against the SQLite pairs table.
///
/// The runtime toggles (`models_disabled.rs`) store disabled models in
/// `AppDb.extra["disabledModels"]` as `provider -> [model ids]`, while the
/// SQLite `disabledModels` table stores flattened `(provider, model)` rows.
/// Compute the set of pairs from each side and insert/delete only the delta.
fn diff_disabled_models(
    conn: &Connection,
    old: &DisabledMap,
    new: &DisabledMap,
) -> rusqlite::Result<()> {
    let old_pairs: BTreeSet<(String, String)> = pairs_from_map(old);
    let new_pairs: BTreeSet<(String, String)> = pairs_from_map(new);

    for (provider, model) in old_pairs.difference(&new_pairs) {
        conn.execute(
            "DELETE FROM disabledModels WHERE provider = ?1 AND model = ?2",
            params![provider, model],
        )?;
    }
    for (provider, model) in new_pairs.difference(&old_pairs) {
        conn.execute(
            "INSERT OR IGNORE INTO disabledModels(provider, model) VALUES(?1,?2)",
            params![provider, model],
        )?;
    }
    Ok(())
}

/// Flatten a `provider -> [model ids]` map into a set of `(provider, model)`
/// pairs.
fn pairs_from_map(map: &DisabledMap) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for (provider, models) in map {
        for m in models {
            out.insert((provider.clone(), m.clone()));
        }
    }
    out
}

/// Read the disabled-model map out of `AppDb.extra["disabledModels"]`.
///
/// The runtime writes it as a `BTreeMap<String, Vec<String>>`. On a fresh
/// reload from a full export it may appear as the flattened array of
/// `{provider, model}` objects (the `import_all`/`export_all` shape) — accept
/// both so the diff is robust to either representation.
fn disabled_from_extra(extra: &BTreeMap<String, Value>) -> DisabledMap {
    let Some(value) = extra.get("disabledModels") else {
        return BTreeMap::new();
    };
    match value {
        Value::Object(_) => {
            serde_json::from_value::<DisabledMap>(value.clone()).unwrap_or_default()
        }
        Value::Array(items) => {
            let mut out: DisabledMap = BTreeMap::new();
            for item in items {
                let provider = item.get("provider").and_then(Value::as_str).unwrap_or("");
                let model = item.get("model").and_then(Value::as_str).unwrap_or("");
                if !provider.is_empty() && !model.is_empty() {
                    out.entry(provider.to_string()).or_default().push(model.to_string());
                }
            }
            out
        }
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use crate::types::{CustomModel, ModelAliasTarget, ProviderConnection, ProviderModelRef};
    use serde_json::json;
    use tempfile::TempDir;

    fn open() -> SqliteDb {
        SqliteDb::open_in_memory().unwrap()
    }

    fn count_rows(db: &SqliteDb, table: &str) -> i64 {
        db.with_conn(|c| {
            c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        })
        .unwrap_or(0)
    }

    fn seed_connection(id: &str, provider: &str, api_key: &str) -> ProviderConnection {
        ProviderConnection {
            id: id.into(),
            provider: provider.into(),
            api_key: Some(api_key.into()),
            is_active: Some(true),
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
            ..Default::default()
        }
    }

    #[test]
    fn noop_when_equal() {
        let db = open();
        let old = AppDb::default();
        let new = AppDb::default();
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        // Nothing written; settings row absent (created only on first write).
        assert_eq!(count_rows(&db, "settings"), 0);
        assert_eq!(count_rows(&db, "providerConnections"), 0);
    }

    #[test]
    fn create_update_delete_connection() {
        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();

        // Create.
        new.provider_connections.push(seed_connection("c1", "openai", "sk-1"));
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        assert_eq!(count_rows(&db, "providerConnections"), 1);

        // Update (change provider + api key).
        old = new.clone();
        new.provider_connections[0].provider = "anthropic".into();
        new.provider_connections[0].api_key = Some("sk-2".into());
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        assert_eq!(count_rows(&db, "providerConnections"), 1);
        let stored = db
            .with_conn(|c| crate::db::sqlite::repo::connection_repo::get_by_id(c, "c1"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.provider, "anthropic");
        assert_eq!(stored.api_key.as_deref(), Some("sk-2"));

        // Delete.
        old = new.clone();
        new.provider_connections.clear();
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        assert_eq!(count_rows(&db, "providerConnections"), 0);
    }

    #[test]
    fn unchanged_rows_not_written() {
        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();
        new.provider_connections.push(seed_connection("c1", "openai", "sk-1"));
        new.provider_connections.push(seed_connection("c2", "anthropic", "sk-2"));
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();

        // Change only c2; c1 must be untouched.
        old = new.clone();
        new.provider_connections[1].api_key = Some("sk-2b".into());
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        let c1 = db
            .with_conn(|c| crate::db::sqlite::repo::connection_repo::get_by_id(c, "c1"))
            .unwrap()
            .unwrap();
        assert_eq!(c1.api_key.as_deref(), Some("sk-1"));
    }

    #[test]
    fn settings_upsert() {
        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();
        new.settings.tunnel_enabled = true;
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        assert_eq!(count_rows(&db, "settings"), 1);

        old = new.clone();
        new.settings.tunnel_enabled = false;
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        assert_eq!(count_rows(&db, "settings"), 1);
    }

    #[test]
    fn kv_scope_add_remove_change() {
        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();
        new.model_aliases.insert(
            "gpt".into(),
            ModelAliasTarget::Mapping(ProviderModelRef {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                extra: BTreeMap::new(),
            }),
        );
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        let all = db.with_conn(|c| kv_repo::get_all(c, "modelAliases")).unwrap();
        assert_eq!(all.len(), 1);

        // Change value.
        old = new.clone();
        new.model_aliases.insert(
            "gpt".into(),
            ModelAliasTarget::Mapping(ProviderModelRef {
                provider: "anthropic".into(),
                model: "claude".into(),
                extra: BTreeMap::new(),
            }),
        );
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        let v = db
            .with_conn(|c| kv_repo::get(c, "modelAliases", "gpt"))
            .unwrap()
            .unwrap();
        assert_eq!(v["provider"], "anthropic");

        // Remove.
        old = new.clone();
        new.model_aliases.clear();
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        let all = db.with_conn(|c| kv_repo::get_all(c, "modelAliases")).unwrap();
        assert_eq!(all.len(), 0);
    }

    #[test]
    fn custom_models_add_remove() {
        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();
        new.custom_models.push(CustomModel {
            provider_alias: "openai".into(),
            id: "cm-1".into(),
            r#type: "llm".into(),
            name: Some("custom-1".into()),
            extra: BTreeMap::new(),
        });
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        let all = db.with_conn(|c| kv_repo::get_all(c, "customModels")).unwrap();
        assert_eq!(all.len(), 1);

        old = new.clone();
        new.custom_models.clear();
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        let all = db.with_conn(|c| kv_repo::get_all(c, "customModels")).unwrap();
        assert_eq!(all.len(), 0);
    }

    #[test]
    fn disabled_models_add_remove() {
        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();
        new.extra.insert(
            "disabledModels".into(),
            json!({ "openai": ["gpt-4o", "gpt-4"] }),
        );
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        let rows: i64 = db
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM disabledModels", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(rows, 2);

        // Remove one model.
        old = new.clone();
        new.extra.insert("disabledModels".into(), json!({ "openai": ["gpt-4o"] }));
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        let rows: i64 = db
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM disabledModels", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(rows, 1);

        // Remove all.
        old = new.clone();
        new.extra.remove("disabledModels");
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        let rows: i64 = db
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM disabledModels", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn usage_tables_untouched() {
        let db = open();
        // Seed a usage history row directly.
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO usageHistory(timestamp, provider, model) VALUES('2026-01-01','openai','gpt-4o')",
                [],
            )
        })
        .unwrap();

        // A settings-only update must NOT wipe usageHistory.
        let mut old = AppDb::default();
        let mut new = AppDb::default();
        new.settings.tunnel_enabled = true;
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        assert_eq!(count_rows(&db, "usageHistory"), 1);
    }

    #[test]
    fn multi_entity_closure() {
        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();
        // Simulate a closure touching settings + mitm_alias together.
        new.settings.tunnel_enabled = true;
        new.mitm_alias.insert("router".into(), BTreeMap::from([("a".into(), "b".into())]));
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();
        assert_eq!(count_rows(&db, "settings"), 1);
        let all = db.with_conn(|c| kv_repo::get_all(c, "mitmAlias")).unwrap();
        assert_eq!(all.len(), 1);
    }

    /// Property test: seed an AppDb, apply a sequence of deterministic
    /// mutations through `apply_app_db_diff`, then export-all + round-trip
    /// through `AppDb::from_json_value` and assert it equals the mutated
    /// in-memory AppDb. This catches any diffing bug comprehensively.
    #[test]
    fn property_roundtrip_matches_mutated_snapshot() {
        use crate::types::CustomModel;
        let db = open();

        let mut state = AppDb::default();
        // Deterministic PRNG (xorshift) so the test is reproducible.
        let mut seed: u64 = 0x9e3779b97f4a7c15;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        for step in 0..200 {
            let mut next = state.clone();
            match rng() % 5 {
                0 => {
                    // Add or update a connection.
                    let id = format!("c{}", rng() % 20);
                    let existing = next.provider_connections.iter_mut().find(|c| c.id == id);
                    match existing {
                        Some(c) => c.api_key = Some(format!("sk-{}", rng() % 1000)),
                        None => next.provider_connections.push(seed_connection(
                            &id,
                            &format!("prov{}", rng() % 4),
                            &format!("sk-{}", rng() % 1000),
                        )),
                    }
                }
                1 => {
                    let id = format!("c{}", rng() % 20);
                    next.provider_connections.retain(|c| c.id != id);
                }
                2 => {
                    // Toggle settings.
                    next.settings.tunnel_enabled = rng() % 2 == 0;
                    next.settings.combo_strategy = format!("strat{}", rng() % 3);
                }
                3 => {
                    // Mutate model aliases.
                    let key = format!("alias{}", rng() % 5);
                    if rng() % 2 == 0 {
                        next.model_aliases.insert(
                            key,
                            ModelAliasTarget::Mapping(ProviderModelRef {
                                provider: format!("prov{}", rng() % 4),
                                model: format!("m{}", rng() % 10),
                                extra: BTreeMap::new(),
                            }),
                        );
                    } else {
                        next.model_aliases.remove(&key);
                    }
                }
                _ => {
                    // Mutate custom models.
                    let id = format!("cm{}", rng() % 5);
                    if rng() % 2 == 0 {
                        if !next.custom_models.iter().any(|m| m.id == id) {
                            next.custom_models.push(CustomModel {
                                provider_alias: format!("prov{}", rng() % 4),
                                id,
                                r#type: "llm".into(),
                                name: Some(format!("n{}", rng() % 100)),
                                extra: BTreeMap::new(),
                            });
                        }
                    } else {
                        next.custom_models.retain(|m| m.id != id);
                    }
                }
            }
            // Disabled models occasionally.
            if rng() % 10 == 0 {
                next.extra.insert(
                    "disabledModels".into(),
                    json!({ "openai": ["gpt-4o", "gpt-4"] }),
                );
            } else {
                next.extra.remove("disabledModels");
            }
            next.normalize();

            db.with_transaction(|tx| apply_app_db_diff(tx, &state, &next))
                .unwrap_or_else(|e| panic!("step {step}: diff failed: {e}"));
            state = next;
        }

        // Round-trip the final SQLite state back to an AppDb and compare.
        let exported: Value = db
            .with_conn(|c| crate::db::sqlite::export::export_all(c))
            .unwrap();
        let roundtripped = AppDb::from_json_value(exported);
        // normalize both so api_key_map (rebuilt) and defaults don't differ.
        let mut expected = state.clone();
        expected.normalize();
        let mut actual = roundtripped;
        actual.normalize();
        // Benign export artifacts: `export_all` hardcodes schemaVersion=2,
        // always emits a (possibly empty) disabledModels array, and sorts
        // connections by (provider, priority) rather than insertion order.
        // Normalize these away before diffing.
        actual.schema_version = expected.schema_version;
        for side in [&mut actual, &mut expected] {
            if matches!(side.extra.get("disabledModels"), Some(Value::Array(a)) if a.is_empty()) {
                side.extra.remove("disabledModels");
            }
            side
                .provider_connections
                .sort_by(|a, b| (&a.provider, &a.id).cmp(&(&b.provider, &b.id)));
        }
        assert_eq!(
            actual, expected,
            "round-trip through SQLite must equal the mutated snapshot"
        );
    }

    #[test]
    fn encryption_boundary_data_column_holds_ciphertext() {
        // With OPENPROXY_ENCRYPTION_KEY set, the `data` column must hold the
        // encrypted (prefixed) form, and get_by_id must decrypt back.
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        std::env::set_var("OPENPROXY_ENCRYPTION_KEY", "test-encryption-key-123");
        std::env::set_var("DATA_DIR", temp.path());

        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();
        new.provider_connections.push(seed_connection("c1", "openai", "sk-secret"));
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new)).unwrap();

        // Raw data column holds ciphertext.
        let raw: String = db
            .with_conn(|c| c.query_row("SELECT data FROM providerConnections WHERE id='c1'", [], |r| r.get(0)))
            .unwrap();
        assert!(raw.contains("opxenc1:") || raw.contains("opxenc2:"), "expected ciphertext in data column, got {raw:?}");

        // get_by_id decrypts back.
        let c = db
            .with_conn(|c| crate::db::sqlite::repo::connection_repo::get_by_id(c, "c1"))
            .unwrap()
            .unwrap();
        assert_eq!(c.api_key.as_deref(), Some("sk-secret"));

        std::env::remove_var("OPENPROXY_ENCRYPTION_KEY");
        std::env::remove_var("DATA_DIR");
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
