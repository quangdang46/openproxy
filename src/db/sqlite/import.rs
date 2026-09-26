//! Import JSON payload (matching the canonical export shape from `db.json`
//! or from [`super::export::export_db`]) into the SQLite database.
//!
//! The entire import is wrapped in a single transaction — on any error all
//! changes are rolled back, so the DB is never left in a partially-imported
//! state (fixing 9router bug: orphaned rows on partial import).

use rusqlite::Connection;
use serde_json::{json, Value};

use super::{patch::custom_model_key, SqliteDb};

/// Import an `AppDb`-shaped JSON payload into the SQLite database.
/// Wipes existing data and reinserts in an atomic transaction.
/// Returns the number of provider connections imported.
pub fn import_db(db: &SqliteDb, payload: &Value) -> anyhow::Result<usize> {
    db.with_transaction(|conn| -> rusqlite::Result<usize> { import_all(conn, payload) })
        .map_err(|e| anyhow::anyhow!("SQLite import: {e}"))
}

/// Import usage JSON payload.
pub fn import_usage(db: &SqliteDb, payload: &Value) -> anyhow::Result<usize> {
    db.with_transaction(|conn| -> rusqlite::Result<usize> { import_usage_impl(conn, payload) })
        .map_err(|e| anyhow::anyhow!("SQLite usage import: {e}"))
}

/// Import the legacy `disabledModels.json` (`{"disabled": {provider: [model]}}`).
/// 9router stores this map in a `kv` scope; OpenProxy owns a dedicated
/// `disabledModels` table holding the same rows.
pub fn import_legacy_disabled(db: &SqliteDb, payload: &Value) -> anyhow::Result<usize> {
    db.with_transaction(|conn| -> rusqlite::Result<usize> {
        let Some(disabled) = payload.get("disabled").and_then(Value::as_object) else {
            return Ok(0);
        };
        let mut count = 0;
        for (provider, models) in disabled {
            for model in models
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                conn.execute(
                    "INSERT OR IGNORE INTO disabledModels(provider, model) VALUES(?1,?2)",
                    rusqlite::params![provider, model],
                )?;
                count += 1;
            }
        }
        Ok(count)
    })
    .map_err(|e| anyhow::anyhow!("SQLite disabledModels import: {e}"))
}

/// Import the legacy `request-details.json` (`{"records": [...]}`) into
/// `requestDetails`. A record with no timestamp is stamped now, as 9router does.
pub fn import_legacy_details(db: &SqliteDb, payload: &Value) -> anyhow::Result<usize> {
    db.with_transaction(|conn| -> rusqlite::Result<usize> {
        let Some(records) = payload.get("records").and_then(Value::as_array) else {
            return Ok(0);
        };
        let now = chrono::Utc::now().to_rfc3339();
        let mut count = 0;
        for record in records {
            let Some(id) = record.get("id").and_then(Value::as_str) else {
                continue;
            };
            super::repo::request_repo::save(
                conn,
                id,
                record
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .unwrap_or(&now),
                record.get("provider").and_then(Value::as_str),
                record.get("model").and_then(Value::as_str),
                record.get("connectionId").and_then(Value::as_str),
                record.get("status").and_then(Value::as_str),
                record,
            )?;
            count += 1;
        }
        Ok(count)
    })
    .map_err(|e| anyhow::anyhow!("SQLite requestDetails import: {e}"))
}

fn import_all(conn: &Connection, payload: &Value) -> rusqlite::Result<usize> {
    // Wipe the configuration tables (keep `_meta`) — the same set 9router
    // clears. `usageHistory` / `usageDaily` / `requestDetails` are appended
    // incrementally and never appear in an `AppDb` payload, so wiping them
    // would destroy usage data that nothing here can write back.
    let tables = [
        "settings",
        "providerConnections",
        "providerNodes",
        "proxyPools",
        "apiKeys",
        "combos",
        "kv",
        "disabledModels",
    ];
    for table in &tables {
        conn.execute(&format!("DELETE FROM {table}"), [])?;
    }

    // Settings
    if let Some(s) = payload.get("settings") {
        let data_str = serde_json::to_string(s).unwrap_or_else(|_| "{}".into());
        conn.execute(
            "INSERT INTO settings(id, data) VALUES(1, ?1) ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            rusqlite::params![data_str],
        )?;
    }

    // Connections
    if let Some(arr) = payload.get("providerConnections").and_then(Value::as_array) {
        for item in arr {
            // Encrypt secrets if encryption key is set
            let item_json = serde_json::to_string(item).unwrap_or_default();
            let mut parsed: crate::types::ProviderConnection =
                serde_json::from_str(&item_json).unwrap_or_default();
            let enc_key = crate::db::crypto::encryption_key().unwrap_or_default();
            crate::db::crypto::encrypt_connection(&mut parsed, &enc_key);

            let data_json = serde_json::to_string(&parsed).unwrap_or_default();
            conn.execute(
                "INSERT INTO providerConnections(id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                rusqlite::params![
                    item.get("id").and_then(Value::as_str).unwrap_or(""),
                    item.get("provider").and_then(Value::as_str).unwrap_or(""),
                    item.get("authType").and_then(Value::as_str).unwrap_or("oauth"),
                    item.get("name").and_then(Value::as_str),
                    item.get("email").and_then(Value::as_str),
                    item.get("priority").and_then(Value::as_i64),
                    item.get("isActive").and_then(Value::as_bool).map(|v| v as i32).unwrap_or(1),
                    data_json,
                    item.get("createdAt").and_then(Value::as_str).unwrap_or(""),
                    item.get("updatedAt").and_then(Value::as_str).unwrap_or(""),
                ],
            )?;
        }
    }

    // Nodes
    if let Some(arr) = payload.get("providerNodes").and_then(Value::as_array) {
        for item in arr {
            // Extract known ProviderNode fields into the data JSON column
            let mut data_map = serde_json::Map::new();
            if let Some(v) = item.get("baseUrl").and_then(Value::as_str) {
                data_map.insert("baseUrl".into(), json!(v));
            }
            if let Some(v) = item.get("prefix").and_then(Value::as_str) {
                data_map.insert("prefix".into(), json!(v));
            }
            if let Some(v) = item.get("apiType").and_then(Value::as_str) {
                data_map.insert("apiType".into(), json!(v));
            }
            // Merge extra fields
            if let Some(extra) = item.get("extra").and_then(Value::as_object) {
                for (k, v) in extra {
                    data_map.insert(k.clone(), v.clone());
                }
            }
            let data_str = serde_json::to_string(&data_map).unwrap_or_default();
            conn.execute(
                "INSERT INTO providerNodes(id, type, name, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    item.get("id").and_then(Value::as_str).unwrap_or(""),
                    item.get("type").and_then(Value::as_str),
                    item.get("name").and_then(Value::as_str),
                    data_str,
                    item.get("createdAt").and_then(Value::as_str).unwrap_or(""),
                    item.get("updatedAt").and_then(Value::as_str).unwrap_or(""),
                ],
            )?;
        }
    }

    // Proxy pools
    if let Some(arr) = payload.get("proxyPools").and_then(Value::as_array) {
        for item in arr {
            conn.execute(
                "INSERT INTO proxyPools(id, isActive, testStatus, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    item.get("id").and_then(Value::as_str).unwrap_or(""),
                    item.get("isActive").and_then(Value::as_bool).map(|v| v as i32).unwrap_or(1),
                    item.get("testStatus").and_then(Value::as_str),
                    // Preserve the full payload — pool fields (name, proxyUrl,
                    // strictProxy, …) live in the JSON `data` blob.
                    &serde_json::to_string(item).unwrap_or_else(|_| "{}".into()),
                    item.get("createdAt").and_then(Value::as_str).unwrap_or(""),
                    item.get("updatedAt").and_then(Value::as_str).unwrap_or(""),
                ],
            )?;
        }
    }

    // API keys
    if let Some(arr) = payload.get("apiKeys").and_then(Value::as_array) {
        for item in arr {
            conn.execute(
                "INSERT INTO apiKeys(id, key, name, machineId, isActive, createdAt, monthly_budget_usd) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![
                    item.get("id").and_then(Value::as_str).unwrap_or(""),
                    item.get("key").and_then(Value::as_str).unwrap_or(""),
                    item.get("name").and_then(Value::as_str),
                    item.get("machineId").and_then(Value::as_str),
                    item.get("isActive").and_then(Value::as_bool).map(|v| v as i32).unwrap_or(1),
                    item.get("createdAt").and_then(Value::as_str).unwrap_or(""),
                    item.get("monthlyBudgetUsd").and_then(Value::as_f64),
                ],
            )?;
        }
    }

    // Combos
    if let Some(arr) = payload.get("combos").and_then(Value::as_array) {
        for item in arr {
            let models_vec = Value::Array(vec![]);
            let models_val = item.get("models").unwrap_or(&models_vec);
            let models_str = serde_json::to_string(models_val).unwrap_or_else(|_| "[]".into());
            conn.execute(
                "INSERT INTO combos(id, name, kind, models, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![
                    item.get("id").and_then(Value::as_str).unwrap_or(""),
                    item.get("name").and_then(Value::as_str).unwrap_or(""),
                    item.get("kind").and_then(Value::as_str),
                    models_str,
                    combo_data_json(item),
                    item.get("createdAt").and_then(Value::as_str).unwrap_or(""),
                    item.get("updatedAt").and_then(Value::as_str).unwrap_or(""),
                ],
            )?;
        }
    }

    // KV scopes
    import_kv_scope(conn, "modelAliases", payload.get("modelAliases"))?;
    if let Some(arr) = payload.get("customModels").and_then(Value::as_array) {
        for (idx, item) in arr.iter().enumerate() {
            let field = |name: &str| item.get(name).and_then(Value::as_str).unwrap_or("");
            let model_type = match field("type") {
                "" => "llm",
                model_type => model_type,
            };
            // A well-formed row is addressed the way 9router's `customKey()`
            // does, so two providers may each customize the same model id —
            // keyed on the id alone the second INSERT would abort the whole
            // import on the `(scope, key)` primary key.
            let key = match field("id") {
                "" => format!("idx{idx}"),
                id => custom_model_key(field("providerAlias"), id, model_type),
            };
            let val_str = serde_json::to_string(item).unwrap_or_else(|_| "null".into());
            conn.execute(
                "INSERT INTO kv(scope, key, value) VALUES('customModels', ?1, ?2)",
                rusqlite::params![key, val_str],
            )?;
        }
    }
    import_kv_scope(conn, "mitmAlias", payload.get("mitmAlias"))?;
    import_kv_scope(conn, "pricing", payload.get("pricing"))?;
    import_kv_scope(conn, "providerFilters", payload.get("providerFilters"))?;
    import_kv_scope(conn, "favoriteModels", payload.get("favoriteModels"))?;

    // Disabled models
    if let Some(arr) = payload.get("disabledModels").and_then(Value::as_array) {
        for item in arr {
            if let (Some(provider), Some(model)) = (
                item.get("provider").and_then(Value::as_str),
                item.get("model").and_then(Value::as_str),
            ) {
                conn.execute(
                    "INSERT INTO disabledModels(provider, model) VALUES(?1,?2)",
                    rusqlite::params![provider, model],
                )?;
            }
        }
    }

    let count = conn
        .query_row("SELECT COUNT(*) FROM providerConnections", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0);

    Ok(count as usize)
}

/// Rebuild the `combos.data` blob — i.e. `Combo.extra` — from an exported
/// combo object. `export_all` flattens the blob back to the top level, so the
/// canonical columns are stripped here. A nested `extra` object, as written by
/// hand-authored backups, is folded in as the base.
fn combo_data_json(item: &Value) -> String {
    let mut obj = match item.get("extra") {
        Some(Value::Object(fields)) => fields.clone(),
        _ => serde_json::Map::new(),
    };
    if let Some(fields) = item.as_object() {
        for (key, value) in fields {
            if matches!(
                key.as_str(),
                "id" | "name" | "kind" | "models" | "createdAt" | "updatedAt" | "extra"
            ) {
                continue;
            }
            obj.insert(key.clone(), value.clone());
        }
    }
    serde_json::to_string(&Value::Object(obj)).unwrap_or_else(|_| "{}".into())
}

fn import_usage_impl(conn: &Connection, payload: &Value) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM usageHistory", [])?;
    conn.execute("DELETE FROM usageDaily", [])?;

    if let Some(arr) = payload.get("history").and_then(Value::as_array) {
        for item in arr {
            let tokens_str = item
                .get("tokens")
                .map(|t| serde_json::to_string(t).unwrap_or_default());
            conn.execute(
                "INSERT INTO usageHistory(timestamp, provider, model, cost, status, tokens)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    item.get("timestamp").and_then(Value::as_str).unwrap_or(""),
                    item.get("provider").and_then(Value::as_str),
                    item.get("model").and_then(Value::as_str).unwrap_or(""),
                    item.get("cost").and_then(Value::as_f64),
                    item.get("status").and_then(Value::as_str),
                    tokens_str,
                ],
            )?;
        }
    }

    let count = payload
        .get("history")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);

    // Day rollups outlive `usageHistory` in the 90-day window, so a payload
    // that carries them has to restore them rather than let the loader
    // re-derive from a history list that no longer covers those days.
    if let Some(days) = payload.get("dailySummary").and_then(Value::as_object) {
        for (date_key, day) in days {
            let Ok(summary) = serde_json::from_value::<crate::types::DailySummary>(day.clone())
            else {
                continue;
            };
            super::repo::usage_repo::upsert_daily(conn, date_key, &summary)?;
        }
    }
    if let Some(total) = payload.get("totalRequestsLifetime").and_then(Value::as_u64) {
        super::repo::meta_repo::set(conn, super::repo::meta_repo::TOTAL_REQUESTS_LIFETIME, total)?;
    }

    Ok(count)
}

fn import_kv_scope(conn: &Connection, scope: &str, val: Option<&Value>) -> rusqlite::Result<()> {
    let Some(Value::Object(obj)) = val else {
        return Ok(());
    };
    for (key, value) in obj {
        let val_str = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
        conn.execute(
            "INSERT INTO kv(scope, key, value) VALUES(?1,?2,?3) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
            rusqlite::params![scope, key, val_str],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_export_import() {
        let db = SqliteDb::open_in_memory().unwrap();

        // Insert some data
        db.with_transaction(|conn| {
            conn.execute(
                "INSERT INTO providerConnections(id, provider, authType, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params!["c1", "openai", "apikey", "{}", "2026-01-01", "2026-01-01"],
            )?;
            Ok::<_, rusqlite::Error>(())
        }).unwrap();

        // Export
        let (bytes, _) = crate::db::sqlite::export::export_db(&db);
        let exported: Value = serde_json::from_slice(&bytes).unwrap();

        // Wipe and re-import
        let db2 = SqliteDb::open_in_memory().unwrap();
        let count = import_db(&db2, &exported).unwrap();
        assert_eq!(count, 1);

        // Verify data persisted
        let verified: i64 = db2
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM providerConnections", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .unwrap();
        assert_eq!(verified, 1);
    }

    #[test]
    fn import_rolls_back_on_error() {
        let db = SqliteDb::open_in_memory().unwrap();
        let invalid = json!({"providerConnections": "not_an_array"});
        let result = import_db(&db, &invalid).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn import_usage_restores_daily_rollup_and_lifetime_counter() {
        let db = SqliteDb::open_in_memory().unwrap();
        import_usage(
            &db,
            &json!({
                "history": [{ "timestamp": "2026-01-01T00:00:00Z", "model": "gpt-4o", "cost": 0.5 }],
                "dailySummary": { "2026-01-01": { "requests": 9, "cost": 4.5 } },
                "totalRequestsLifetime": 4242,
            }),
        )
        .unwrap();

        // Both outlive the history list in the payload — the rollup covers 90
        // days against history's 30, and the count covers everything ever seen.
        let days = db
            .with_conn(|conn| crate::db::sqlite::repo::usage_repo::all_daily(conn))
            .unwrap();
        assert_eq!(days["2026-01-01"].requests, 9);
        assert_eq!(days["2026-01-01"].cost, 4.5);

        let lifetime = db
            .with_conn(|conn| {
                crate::db::sqlite::repo::meta_repo::get(
                    conn,
                    crate::db::sqlite::repo::meta_repo::TOTAL_REQUESTS_LIFETIME,
                )
            })
            .unwrap();
        assert_eq!(lifetime, Some(4242));
    }

    #[test]
    fn import_keeps_two_providers_customizing_the_same_model_id() {
        let db = SqliteDb::open_in_memory().unwrap();
        import_db(
            &db,
            &json!({
                "customModels": [
                    { "providerAlias": "openai", "id": "gpt-4o", "type": "llm" },
                    { "providerAlias": "anthropic", "id": "gpt-4o", "type": "llm" },
                ],
            }),
        )
        .unwrap();

        let keys: Vec<String> = db
            .with_conn(|conn| {
                let mut stmt =
                    conn.prepare("SELECT key FROM kv WHERE scope = 'customModels' ORDER BY key")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap();
        assert_eq!(keys, vec!["anthropic|gpt-4o|llm", "openai|gpt-4o|llm"]);
    }

    #[test]
    fn import_db_preserves_usage_and_request_detail_tables() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|conn| {
            conn.execute(
                "INSERT INTO usageHistory(timestamp, model) VALUES('2026-01-01T00:00:00Z','gpt-4o')",
                [],
            )?;
            conn.execute(
                "INSERT INTO usageDaily(dateKey, data) VALUES('2026-01-01','{\"requests\":1}')",
                [],
            )?;
            crate::db::sqlite::repo::request_repo::save(
                conn,
                "r1",
                "2026-01-01T00:00:00Z",
                Some("openai"),
                Some("gpt-4o"),
                None,
                Some("ok"),
                &json!({"prompt": "hi"}),
            )
        })
        .unwrap();

        // An AppDb payload carries no usage data, so an import that wiped
        // these tables would destroy rows nothing puts back.
        import_db(&db, &json!({"providerConnections": []})).unwrap();

        for table in ["usageHistory", "usageDaily", "requestDetails"] {
            let count: i64 = db
                .with_conn(|conn| {
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                })
                .unwrap();
            assert_eq!(count, 1, "{table} must survive an app-db import");
        }
    }
}
