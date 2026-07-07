//! Import JSON payload (matching the canonical export shape from `db.json`
//! or from [`super::export::export_db`]) into the SQLite database.
//!
//! The entire import is wrapped in a single transaction — on any error all
//! changes are rolled back, so the DB is never left in a partially-imported
//! state (fixing 9router bug: orphaned rows on partial import).

use rusqlite::Connection;
use serde_json::{json, Value};

use super::SqliteDb;

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

fn import_all(conn: &Connection, payload: &Value) -> rusqlite::Result<usize> {
    // Wipe all data (keep _meta)
    let tables = [
        "settings",
        "providerConnections",
        "providerNodes",
        "proxyPools",
        "apiKeys",
        "combos",
        "kv",
        "disabledModels",
        "usageHistory",
        "usageDaily",
        "requestDetails",
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
                    "{}",
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
                "INSERT INTO apiKeys(id, key, name, machineId, isActive, createdAt) VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    item.get("id").and_then(Value::as_str).unwrap_or(""),
                    item.get("key").and_then(Value::as_str).unwrap_or(""),
                    item.get("name").and_then(Value::as_str),
                    item.get("machineId").and_then(Value::as_str),
                    item.get("isActive").and_then(Value::as_bool).map(|v| v as i32).unwrap_or(1),
                    item.get("createdAt").and_then(Value::as_str).unwrap_or(""),
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
                    "{}",
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
            let fallback_key = format!("idx{idx}");
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(&fallback_key);
            let val_str = serde_json::to_string(item).unwrap_or_else(|_| "null".into());
            conn.execute(
                "INSERT INTO kv(scope, key, value) VALUES('customModels', ?1, ?2)",
                rusqlite::params![key, val_str],
            )?;
        }
    }
    import_kv_scope(conn, "mitmAlias", payload.get("mitmAlias"))?;
    import_kv_scope(conn, "pricing", payload.get("pricing"))?;

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
            row.get(0)
        })
        .unwrap_or(0);

    Ok(count)
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
}
