//! Export the entire SQLite DB to the canonical JSON format used by
//! legacy `db.json` / `usage.json`. The output shape matches 9router's
//! `exportDb()` so that backups are interchangeable.

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::types::{AppDb, UsageDb};

use super::SqliteDb;

/// Export ALL scopes to the canonical JSON format. Returns pretty-printed
/// bytes plus a filename hint.
pub fn export_db(db: &SqliteDb) -> (Vec<u8>, String) {
    let json_val = db
        .with_conn(|conn| -> rusqlite::Result<Value> { export_all(conn) })
        .unwrap_or(Value::Null);
    let bytes = serde_json::to_vec_pretty(&json_val).unwrap_or_default();
    let stamp = chrono_like_stamp();
    (bytes, format!("openproxy-db-{stamp}.json"))
}

/// Export usage history to canonical format.
pub fn export_usage(db: &SqliteDb) -> (Vec<u8>, String) {
    let json_val = db
        .with_conn(|conn| -> rusqlite::Result<Value> { export_usage_impl(conn) })
        .unwrap_or(Value::Null);
    let bytes = serde_json::to_vec_pretty(&json_val).unwrap_or_default();
    let stamp = chrono_like_stamp();
    (bytes, format!("openproxy-usage-{stamp}.json"))
}

pub(crate) fn export_all(conn: &Connection) -> rusqlite::Result<Value> {
    // Settings (single row)
    let settings: Value = conn
        .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
            let s: String = row.get(0)?;
            Ok(serde_json::from_str(&s).unwrap_or(Value::Null))
        })
        .unwrap_or(Value::Null);

    // Connections
    let provider_connections: Vec<Value> = {
        let mut stmt = conn.prepare(
            "SELECT id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt FROM providerConnections ORDER BY provider, priority"
        )?;
        let rows = stmt.query_map([], |row| -> rusqlite::Result<Value> {
            let data_str: String = row.get(7)?;
            let mut data: Value =
                serde_json::from_str(&data_str).unwrap_or(Value::Object(Default::default()));
            if let Some(obj) = data.as_object_mut() {
                obj.insert("id".into(), json!(row.get::<_, String>(0)?));
                obj.insert("provider".into(), json!(row.get::<_, String>(1)?));
                obj.insert("authType".into(), json!(row.get::<_, String>(2)?));
                if let Ok(Some(v)) = row.get::<_, Option<String>>(3) {
                    obj.insert("name".into(), json!(v));
                }
                if let Ok(Some(v)) = row.get::<_, Option<String>>(4) {
                    obj.insert("email".into(), json!(v));
                }
                if let Ok(v) = row.get::<_, Option<i64>>(5) {
                    obj.insert("priority".into(), json!(v));
                }
                if let Ok(v) = row.get::<_, Option<bool>>(6) {
                    obj.insert("isActive".into(), json!(v));
                }
                obj.insert("createdAt".into(), json!(row.get::<_, String>(8)?));
                obj.insert("updatedAt".into(), json!(row.get::<_, String>(9)?));
            }
            Ok(data)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // Nodes
    let provider_nodes: Vec<Value> = {
        let mut stmt =
            conn.prepare("SELECT id, type, name, data, createdAt, updatedAt FROM providerNodes")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let node_type: Option<String> = row.get(1)?;
            let name: Option<String> = row.get(2)?;
            let data_str: String = row.get(3)?;
            let created_at: String = row.get(4)?;
            let updated_at: String = row.get(5)?;
            let data: Value =
                serde_json::from_str(&data_str).unwrap_or(Value::Object(Default::default()));
            Ok(json!({
                "id": id, "type": node_type, "name": name,
                "createdAt": created_at, "updatedAt": updated_at,
                "data": data,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // Proxy pools
    let proxy_pools: Vec<Value> = {
        let mut stmt = conn.prepare(
            "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let is_active: Option<bool> = row
                .get::<_, Option<i32>>(1)
                .map(|v| v.map(|x| x != 0))
                .unwrap_or(None);
            let test_status: Option<String> = row.get(2)?;
            let created_at: String = row.get(4)?;
            let updated_at: String = row.get(5)?;
            Ok(json!({
                "id": id, "isActive": is_active, "testStatus": test_status,
                "createdAt": created_at, "updatedAt": updated_at,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // API keys
    let api_keys: Vec<Value> = {
        let mut stmt =
            conn.prepare("SELECT id, key, name, machineId, isActive, createdAt FROM apiKeys")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let key: String = row.get(1)?;
            let name: Option<String> = row.get(2)?;
            let machine_id: Option<String> = row.get(3)?;
            let is_active: Option<bool> = row
                .get::<_, Option<i32>>(4)
                .map(|v| v.map(|x| x != 0))
                .unwrap_or(None);
            let created_at: String = row.get(5)?;
            Ok(json!({
                "id": id, "key": key, "name": name, "machineId": machine_id,
                "isActive": is_active, "createdAt": created_at,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // Combos
    let combos: Vec<Value> = {
        let mut stmt =
            conn.prepare("SELECT id, name, kind, models, data, createdAt, updatedAt FROM combos")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let name: String = row.get(1)?;
            let kind: Option<String> = row.get(2)?;
            let models_str: String = row.get(3)?;
            let models: Vec<String> = serde_json::from_str(&models_str).unwrap_or_default();
            let created_at: String = row.get(5)?;
            let updated_at: String = row.get(6)?;
            Ok(json!({
                "id": id, "name": name, "kind": kind, "models": models,
                "createdAt": created_at, "updatedAt": updated_at,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // KV scopes
    let model_aliases: Value = kv_scope_to_map(conn, "modelAliases");
    let custom_models: Vec<Value> = kv_scope_to_array(conn, "customModels");
    let mitm_alias: Value = kv_scope_to_map(conn, "mitmAlias");
    let pricing: Value = kv_scope_to_map(conn, "pricing");

    // Disabled models
    let disabled_models: Vec<Value> = {
        let mut stmt = conn.prepare("SELECT provider, model FROM disabledModels")?;
        let rows = stmt.query_map([], |row| {
            Ok(json!({ "provider": row.get::<_, String>(0)?, "model": row.get::<_, String>(1)? }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    Ok(json!({
        "schemaVersion": 2,
        "settings": settings,
        "providerConnections": provider_connections,
        "providerNodes": provider_nodes,
        "proxyPools": proxy_pools,
        "apiKeys": api_keys,
        "combos": combos,
        "modelAliases": model_aliases,
        "customModels": custom_models,
        "mitmAlias": mitm_alias,
        "pricing": pricing,
        "disabledModels": disabled_models,
    }))
}

pub(crate) fn export_usage_impl(conn: &Connection) -> rusqlite::Result<Value> {
    let history: Vec<Value> = {
        let mut stmt = conn.prepare(
            "SELECT timestamp, provider, model, connectionId, apiKey, endpoint,
                    promptTokens, completionTokens, cost, status, tokens, meta
             FROM usageHistory ORDER BY timestamp DESC LIMIT 10000",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(json!({
                "timestamp": row.get::<_, String>(0)?,
                "provider": row.get::<_, Option<String>>(1)?,
                "model": row.get::<_, String>(2)?,
                "tokens": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                "cost": row.get::<_, Option<f64>>(8)?,
                "status": row.get::<_, Option<String>>(9)?,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    Ok(json!({
        "history": history,
        "totalRequestsLifetime": history.len(),
    }))
}

fn kv_scope_to_map(conn: &Connection, scope: &str) -> Value {
    let mut stmt = conn
        .prepare("SELECT key, value FROM kv WHERE scope = ?1")
        .unwrap();
    let rows: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![scope], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    let map: HashMap<String, Value> = rows
        .into_iter()
        .filter_map(|(k, v)| serde_json::from_str(&v).ok().map(|val: Value| (k, val)))
        .collect();
    json!(map)
}

fn kv_scope_to_array(conn: &Connection, scope: &str) -> Vec<Value> {
    let mut stmt = conn
        .prepare("SELECT value FROM kv WHERE scope = ?1")
        .unwrap();
    stmt.query_map(rusqlite::params![scope], |row| {
        let s: String = row.get(0)?;
        Ok(serde_json::from_str(&s).unwrap_or(Value::Null))
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

fn chrono_like_stamp() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs_of_day = n % 86_400;
    let h = secs_of_day / 3600;
    let m = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{}{:02}{:02}{:02}", n / 86_400, h, m, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_returns_json_with_all_keys() {
        let db = SqliteDb::open_in_memory().unwrap();
        let (bytes, _) = export_db(&db);
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        for key in &[
            "schemaVersion",
            "settings",
            "providerConnections",
            "providerNodes",
            "proxyPools",
            "apiKeys",
            "combos",
            "modelAliases",
            "customModels",
            "mitmAlias",
            "pricing",
            "disabledModels",
        ] {
            assert!(val.get(*key).is_some(), "missing key {key}");
        }
    }

    #[test]
    fn export_usage_returns_history() {
        let db = SqliteDb::open_in_memory().unwrap();
        let (bytes, _) = export_usage(&db);
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(val.get("history").is_some());
    }
}
