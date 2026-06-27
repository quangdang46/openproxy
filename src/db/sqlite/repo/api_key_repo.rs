//! Repository for `apiKeys` table.

use rusqlite::{params, Connection};

use crate::types::ApiKey;

pub fn get_active(conn: &Connection) -> rusqlite::Result<Vec<ApiKey>> {
    let mut stmt = conn.prepare(
        "SELECT id, key, name, machineId, isActive, createdAt FROM apiKeys WHERE isActive = 1",
    )?;
    let rows = stmt.query_map([], row_to_api_key)?;
    rows.collect()
}

pub fn get_all(conn: &Connection) -> rusqlite::Result<Vec<ApiKey>> {
    let mut stmt = conn.prepare(
        "SELECT id, key, name, machineId, isActive, createdAt FROM apiKeys ORDER BY createdAt DESC",
    )?;
    let rows = stmt.query_map([], row_to_api_key)?;
    rows.collect()
}

pub fn get_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<ApiKey>> {
    let mut stmt = conn.prepare(
        "SELECT id, key, name, machineId, isActive, createdAt FROM apiKeys WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_api_key)?;
    Ok(rows.next().transpose()?)
}

pub fn create(conn: &Connection, k: &ApiKey) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO apiKeys(id, key, name, machineId, isActive, createdAt) VALUES(?1,?2,?3,?4,?5,?6)",
        params![k.id, k.key, k.name, k.machine_id, k.is_active.map(|v| v as i32), k.created_at.as_deref().unwrap_or("")],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, k: &ApiKey) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE apiKeys SET key=?2, name=?3, machineId=?4, isActive=?5 WHERE id=?1",
        params![
            k.id,
            k.key,
            k.name,
            k.machine_id,
            k.is_active.map(|v| v as i32)
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM apiKeys WHERE id = ?1", params![id])?;
    Ok(())
}

fn row_to_api_key(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKey> {
    let id: String = row.get(0)?;
    let key: String = row.get(1)?;
    let name: Option<String> = row.get(2)?;
    let machine_id: Option<String> = row.get(3)?;
    let is_active: Option<i32> = row.get(4)?;
    let created_at: String = row.get(5)?;

    Ok(ApiKey {
        id,
        key,
        name: name.unwrap_or_default(),
        machine_id,
        is_active: is_active.map(|v| v != 0),
        created_at: Some(created_at),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use serde_json::json;

    #[test]
    fn roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        let key = ApiKey {
            id: "k1".into(),
            key: "sk-test-machineid-12345678".into(),
            name: "test".into(),
            machine_id: Some("machine123".into()),
            is_active: Some(true),
            created_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        db.with_transaction(|tx| create(tx, &key)).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "k1")).unwrap().unwrap();
        assert_eq!(read.key, "sk-test-machineid-12345678");
    }

    #[test]
    fn unique_key_constraint() {
        let db = SqliteDb::open_in_memory().unwrap();
        let k1 = ApiKey {
            id: "k1".into(),
            key: "same".into(),
            is_active: Some(true),
            created_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        let mut k2 = k1.clone();
        k2.id = "k2".into();
        db.with_transaction(|tx| create(tx, &k1)).unwrap();
        let result = db.with_transaction(|tx| create(tx, &k2));
        assert!(result.is_err());
    }
}
