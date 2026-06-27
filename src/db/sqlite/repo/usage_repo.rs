//! Repository for `usageHistory` and `usageDaily` tables.

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::types::{DailySummary, UsageEntry};

pub fn get_history(
    conn: &Connection,
    limit: i64,
    offset: i64,
) -> rusqlite::Result<Vec<UsageEntry>> {
    let mut stmt = conn.prepare(
        "SELECT timestamp, provider, model, connectionId, apiKey, endpoint,
                promptTokens, completionTokens, cost, status, tokens, meta
         FROM usageHistory ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt.query_map(params![limit, offset], row_to_usage)?;
    rows.collect()
}

pub fn insert(conn: &Connection, entry: &UsageEntry) -> rusqlite::Result<()> {
    let tokens_json = entry
        .tokens
        .as_ref()
        .map(|t| serde_json::to_string(t).unwrap_or_default());
    conn.execute(
        "INSERT INTO usageHistory(timestamp, provider, model, connectionId, apiKey, endpoint,
                promptTokens, completionTokens, cost, status, tokens, meta)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            entry.timestamp.as_deref().unwrap_or(""),
            entry.provider.as_deref(),
            entry.model,
            entry.connection_id.as_deref(),
            entry.api_key.as_deref(),
            entry.endpoint.as_deref(),
            entry
                .tokens
                .as_ref()
                .and_then(|t| t.prompt_tokens.or(t.input_tokens))
                .unwrap_or(0) as i64,
            entry
                .tokens
                .as_ref()
                .and_then(|t| t.completion_tokens.or(t.output_tokens))
                .unwrap_or(0) as i64,
            entry.cost,
            entry.status.as_deref(),
            tokens_json,
            None::<String>,
        ],
    )?;
    Ok(())
}

pub fn get_daily(conn: &Connection, date_key: &str) -> rusqlite::Result<Option<DailySummary>> {
    let mut stmt = conn.prepare("SELECT data FROM usageDaily WHERE dateKey = ?1")?;
    let mut rows = stmt.query_map(params![date_key], |row| {
        let s: String = row.get(0)?;
        serde_json::from_str(&s).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })?;
    Ok(rows.next().transpose()?)
}

pub fn upsert_daily(
    conn: &Connection,
    date_key: &str,
    summary: &DailySummary,
) -> rusqlite::Result<()> {
    let json_str = serde_json::to_string(summary).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO usageDaily(dateKey, data) VALUES(?1,?2) ON CONFLICT(dateKey) DO UPDATE SET data = excluded.data",
        params![date_key, json_str],
    )?;
    Ok(())
}

fn row_to_usage(row: &rusqlite::Row<'_>) -> rusqlite::Result<UsageEntry> {
    let timestamp: Option<String> = row.get(0)?;
    let provider: Option<String> = row.get(1)?;
    let model: String = row.get(2)?;
    let _conn_id: Option<String> = row.get(3)?;
    let _api_key: Option<String> = row.get(4)?;
    let _endpoint: Option<String> = row.get(5)?;
    let prompt_tokens: Option<i64> = row.get(6)?;
    let completion_tokens: Option<i64> = row.get(7)?;
    let cost: Option<f64> = row.get(8)?;
    let status: Option<String> = row.get(9)?;
    let tokens_str: Option<String> = row.get(10)?;

    let tokens = tokens_str.and_then(|s| serde_json::from_str(&s).ok());

    Ok(UsageEntry {
        timestamp,
        provider,
        model,
        tokens,
        cost,
        status,
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
        let entry = UsageEntry {
            model: "gpt-4o".into(),
            provider: Some("openai".into()),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        db.with_transaction(|tx| insert(tx, &entry)).unwrap();
        let history = db.with_conn(|c| get_history(c, 10, 0)).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].model, "gpt-4o");
    }
}
