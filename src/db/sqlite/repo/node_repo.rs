//! Repository for `providerNodes` table.

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::types::ProviderNode;

pub fn get_by_type(conn: &Connection, node_type: Option<&str>) -> rusqlite::Result<Vec<ProviderNode>> {
    let rows = if let Some(t) = node_type {
        let mut stmt = conn.prepare(
            "SELECT id, type, name, data, createdAt, updatedAt FROM providerNodes WHERE type = ?1 ORDER BY name"
        )?;
        let rows = stmt.query_map(params![t], row_to_node)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, type, name, data, createdAt, updatedAt FROM providerNodes ORDER BY name"
        )?;
        let rows = stmt.query_map([], row_to_node)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

pub fn get_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<ProviderNode>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, name, data, createdAt, updatedAt FROM providerNodes WHERE id = ?1"
    )?;
    let mut rows = stmt.query_map(params![id], row_to_node)?;
    Ok(rows.next().transpose()?)
}

pub fn create(conn: &Connection, n: &ProviderNode) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO providerNodes(id, type, name, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6)",
        params![n.id, n.r#type, n.name, node_to_data(n), n.created_at.as_deref().unwrap_or(""), n.updated_at.as_deref().unwrap_or("")],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, n: &ProviderNode) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE providerNodes SET type=?2, name=?3, data=?4, updatedAt=?5 WHERE id=?1",
        params![n.id, n.r#type, n.name, node_to_data(n), n.updated_at.as_deref().unwrap_or("")],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM providerNodes WHERE id = ?1", params![id])?;
    Ok(())
}

fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProviderNode> {
    let id: String = row.get(0)?;
    let node_type: Option<String> = row.get(1)?;
    let name: Option<String> = row.get(2)?;
    let data_str: String = row.get(3)?;
    let created_at: String = row.get(4)?;
    let updated_at: String = row.get(5)?;

    let mut node = ProviderNode {
        id,
        r#type: node_type.unwrap_or_default(),
        name,
        created_at: Some(created_at),
        updated_at: Some(updated_at),
        ..Default::default()
    };

    // Merge data blob
    if let Ok(data_val) = serde_json::from_str::<Value>(&data_str) {
        if let Some(obj) = data_val.as_object() {
            let json_str = serde_json::to_string(obj).unwrap_or_default();
            // Merge all fields from data JSON into node (extra, etc.)
            if let Ok(parsed) = serde_json::from_str::<ProviderNode>(&json_str) {
                node.extra = parsed.extra;
            }
        }
    }

    Ok(node)
}

fn node_to_data(n: &ProviderNode) -> String {
    let mut fields = serde_json::Map::new();
    for (k, v) in &n.extra {
        fields.insert(k.clone(), v.clone());
    }
    serde_json::to_string(&fields).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json; use crate::db::sqlite::SqliteDb;

    #[test]
    fn roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        let node = ProviderNode {
            id: "n1".into(),
            r#type: "openai-compatible".into(),
            name: Some("test-node".into()),
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
        };
        db.with_transaction(|tx| create(tx, &node)).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "n1")).unwrap().unwrap();
        assert_eq!(read.name.as_deref(), Some("test-node"));
        let all = db.with_conn(|c| get_by_type(c, Some("openai-compatible"))).unwrap();
        assert_eq!(all.len(), 1);
    }
}
