//! Repository for `proxyPools` table.

use rusqlite::{params, Connection};

use crate::types::ProxyPool;

pub fn get_active(conn: &Connection) -> rusqlite::Result<Vec<ProxyPool>> {
    let mut stmt = conn.prepare(
        "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools WHERE isActive IS NOT 0"
    )?;
    let rows = stmt.query_map([], row_to_pool)?;
    rows.collect()
}

pub fn get_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<ProxyPool>> {
    let mut stmt = conn.prepare(
        "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_pool)?;
    rows.next().transpose()
}

pub fn create(conn: &Connection, p: &ProxyPool) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO proxyPools(id, isActive, testStatus, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6)",
        params![p.id, p.is_active.map(|v| v as i32), p.test_status, pool_to_data(p), p.created_at.as_deref().unwrap_or(""), p.updated_at.as_deref().unwrap_or("")],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, p: &ProxyPool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE proxyPools SET isActive=?2, testStatus=?3, data=?4, updatedAt=?5 WHERE id=?1",
        params![
            p.id,
            p.is_active.map(|v| v as i32),
            p.test_status,
            pool_to_data(p),
            p.updated_at.as_deref().unwrap_or("")
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM proxyPools WHERE id = ?1", params![id])?;
    Ok(())
}

fn row_to_pool(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProxyPool> {
    let id: String = row.get(0)?;
    let is_active: Option<i32> = row.get(1)?;
    let test_status: Option<String> = row.get(2)?;
    let _data_str: String = row.get(3)?;
    let created_at: String = row.get(4)?;
    let updated_at: String = row.get(5)?;

    Ok(ProxyPool {
        id,
        is_active: is_active.map(|v| v != 0),
        test_status,
        created_at: Some(created_at),
        updated_at: Some(updated_at),
        ..Default::default()
    })
}

fn pool_to_data(p: &ProxyPool) -> String {
    let mut fields = serde_json::Map::new();
    for (k, v) in &p.extra {
        fields.insert(k.clone(), v.clone());
    }
    serde_json::to_string(&fields).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use serde_json::json;

    #[test]
    fn roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        let pool = ProxyPool {
            id: "p1".into(),
            is_active: Some(true),
            test_status: Some("active".into()),
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        db.with_transaction(|tx| create(tx, &pool)).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "p1")).unwrap().unwrap();
        assert_eq!(read.test_status.as_deref(), Some("active"));
    }
}
