//! Generic key-value store repository for modelAliases, customModels,
//! mitmAlias, and pricing — everything stored in the `kv` table.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use serde_json::Value;

pub fn get(conn: &Connection, scope: &str, key: &str) -> rusqlite::Result<Option<Value>> {
    let mut stmt = conn.prepare("SELECT value FROM kv WHERE scope = ?1 AND key = ?2")?;
    let mut rows = stmt.query_map(params![scope, key], |row| row.get::<_, String>(0))?;
    match rows.next() {
        Some(Ok(s)) => Ok(serde_json::from_str(&s).ok()),
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

pub fn set(conn: &Connection, scope: &str, key: &str, value: &Value) -> rusqlite::Result<()> {
    let json_str = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
    conn.execute(
        "INSERT INTO kv(scope, key, value) VALUES(?1,?2,?3) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
        params![scope, key, json_str],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, scope: &str, key: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM kv WHERE scope = ?1 AND key = ?2",
        params![scope, key],
    )?;
    Ok(())
}

pub fn get_all(conn: &Connection, scope: &str) -> rusqlite::Result<HashMap<String, Value>> {
    let mut stmt = conn.prepare("SELECT key, value FROM kv WHERE scope = ?1")?;
    let rows = stmt.query_map(params![scope], |row| {
        let key: String = row.get(0)?;
        let val_str: String = row.get(1)?;
        let val: Value = serde_json::from_str(&val_str).unwrap_or(Value::Null);
        Ok((key, val))
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use serde_json::json;

    #[test]
    fn roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| {
            set(
                tx,
                "modelAliases",
                "gpt",
                &Value::String("openai/gpt-4o".into()),
            )
        })
        .unwrap();
        let v = db.with_conn(|c| get(c, "modelAliases", "gpt")).unwrap();
        assert_eq!(v, Some(Value::String("openai/gpt-4o".into())));
    }

    #[test]
    fn get_all_scope() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| {
            set(
                tx,
                "pricing",
                "openai",
                &serde_json::json!({"gpt-4o": 0.01}),
            )?;
            set(
                tx,
                "pricing",
                "anthropic",
                &serde_json::json!({"claude": 0.015}),
            )?;
            Ok(())
        })
        .unwrap();
        let all = db.with_conn(|c| get_all(c, "pricing")).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn upsert_replaces() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| set(tx, "kv", "k1", &Value::String("old".into())))
            .unwrap();
        db.with_transaction(|tx| set(tx, "kv", "k1", &Value::String("new".into())))
            .unwrap();
        let v = db.with_conn(|c| get(c, "kv", "k1")).unwrap();
        assert_eq!(v, Some(Value::String("new".into())));
    }
}
