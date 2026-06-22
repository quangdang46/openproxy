//! Repository for `requestDetails` table (observability).

use rusqlite::{params, Connection};
use serde_json::Value;

pub fn save(conn: &Connection, id: &str, timestamp: &str, provider: Option<&str>,
            model: Option<&str>, connection_id: Option<&str>, status: Option<&str>,
            data: &Value) -> rusqlite::Result<()> {
    let data_str = serde_json::to_string(data).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO requestDetails(id, timestamp, provider, model, connectionId, status, data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![id, timestamp, provider, model, connection_id, status, data_str],
    )?;
    Ok(())
}

pub fn get_recent(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<(String, String, Option<String>, Option<String>, Option<String>, Option<String>, Value)>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, provider, model, connectionId, status, data FROM requestDetails ORDER BY timestamp DESC LIMIT ?1"
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        let id: String = row.get(0)?;
        let ts: String = row.get(1)?;
        let provider: Option<String> = row.get(2)?;
        let model: Option<String> = row.get(3)?;
        let conn_id: Option<String> = row.get(4)?;
        let status: Option<String> = row.get(5)?;
        let data_str: String = row.get(6)?;
        let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
        Ok((id, ts, provider, model, conn_id, status, data))
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json; use crate::db::sqlite::SqliteDb;

    #[test]
    fn roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| {
            save(tx, "r1", "2026-01-01T00:00:00Z", Some("openai"), Some("gpt-4o"), Some("c1"), Some("success"), &serde_json::json!({"prompt": "hello"}))
        }).unwrap();
        let recent = db.with_conn(|c| get_recent(c, 10)).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].0, "r1");
    }
}
