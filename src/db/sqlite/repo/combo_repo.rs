//! Repository for `combos` table.

use rusqlite::{params, Connection};

use crate::types::Combo;

pub fn get_all(conn: &Connection) -> rusqlite::Result<Vec<Combo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, models, data, createdAt, updatedAt FROM combos ORDER BY name",
    )?;
    let rows = stmt.query_map([], row_to_combo)?;
    rows.collect()
}

pub fn get_by_name(conn: &Connection, name: &str) -> rusqlite::Result<Option<Combo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, models, data, createdAt, updatedAt FROM combos WHERE name = ?1",
    )?;
    let mut rows = stmt.query_map(params![name], row_to_combo)?;
    Ok(rows.next().transpose()?)
}

pub fn create(conn: &Connection, c: &Combo) -> rusqlite::Result<()> {
    let models_json = serde_json::to_string(&c.models).unwrap_or_else(|_| "[]".into());
    let data_json = serde_json::to_string(&c.extra).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO combos(id, name, kind, models, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![c.id, c.name, c.kind, models_json, data_json, c.created_at.as_deref().unwrap_or(""), c.updated_at.as_deref().unwrap_or("")],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, c: &Combo) -> rusqlite::Result<()> {
    let models_json = serde_json::to_string(&c.models).unwrap_or_else(|_| "[]".into());
    let data_json = serde_json::to_string(&c.extra).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "UPDATE combos SET kind=?2, models=?3, data=?4, updatedAt=?5 WHERE id=?1",
        params![
            c.id,
            c.kind,
            models_json,
            data_json,
            c.updated_at.as_deref().unwrap_or("")
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM combos WHERE id = ?1", params![id])?;
    Ok(())
}

fn row_to_combo(row: &rusqlite::Row<'_>) -> rusqlite::Result<Combo> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let kind: Option<String> = row.get(2)?;
    let models_json: String = row.get(3)?;
    let _data: String = row.get(4)?;
    let created_at: String = row.get(5)?;
    let updated_at: String = row.get(6)?;

    let models: Vec<String> = serde_json::from_str(&models_json).unwrap_or_default();

    Ok(Combo {
        id,
        name,
        kind,
        models,
        created_at: Some(created_at),
        updated_at: Some(updated_at),
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
        let combo = Combo {
            id: "c1".into(),
            name: "mycombo".into(),
            kind: Some("fallback".into()),
            models: vec!["openai/gpt-4o".into(), "anthropic/claude-sonnet".into()],
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        db.with_transaction(|tx| create(tx, &combo)).unwrap();
        let read = db
            .with_conn(|c| get_by_name(c, "mycombo"))
            .unwrap()
            .unwrap();
        assert_eq!(read.models.len(), 2);
    }

    #[test]
    fn unique_name_constraint() {
        let db = SqliteDb::open_in_memory().unwrap();
        let c1 = Combo {
            id: "c1".into(),
            name: "same".into(),
            models: vec!["a".into()],
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        let mut c2 = c1.clone();
        c2.id = "c2".into();
        db.with_transaction(|tx| create(tx, &c1)).unwrap();
        let result = db.with_transaction(|tx| create(tx, &c2));
        assert!(result.is_err());
    }
}
