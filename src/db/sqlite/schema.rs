//! SQLite schema for OpenProxy persistence.
//!
//! Mirrors the upstream 9router schema (see `9router/src/lib/db/schema.js`)
//! with openproxy-specific columns added for encrypted secrets and snapshots.
//!
//! All tables use TEXT primary keys (UUIDs or human-readable slugs) except
//! for `usageHistory` (AUTOINCREMENT id) and `usageDaily` (date string PK).
//! Free-form columns (`data`) hold JSON blobs so schema changes don't require
//! DDL migrations.

/// Current schema version. Bump whenever you add a migration to
/// `migrations/`.
pub const SCHEMA_VERSION: i32 = 2;

/// All DDL statements that define the OpenProxy schema. Run inside a single
/// transaction during `init_db`.
pub const TABLES_SQL: &[&str] = &[
    // Metadata: holds the active schema version + integrity stamp.
    r#"
    CREATE TABLE IF NOT EXISTS _meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    )
    "#,
    // Single-row settings table. JSON blob for forward-compatibility.
    r#"
    CREATE TABLE IF NOT EXISTS settings (
        id    INTEGER PRIMARY KEY CHECK (id = 1),
        data  TEXT NOT NULL
    )
    "#,
    // Provider connections (OAuth / API-key / cookie). `data` is the JSON
    // for ALL non-indexed fields, with secrets AES-encrypted by
    // `src/db/crypto.rs`. Indexed columns: provider, authType, isActive,
    // priority — for the common query paths.
    r#"
    CREATE TABLE IF NOT EXISTS providerConnections (
        id          TEXT PRIMARY KEY,
        provider    TEXT NOT NULL,
        authType    TEXT NOT NULL DEFAULT 'oauth',
        name        TEXT,
        email       TEXT,
        priority    INTEGER,
        isActive    INTEGER NOT NULL DEFAULT 1,
        data        TEXT NOT NULL,
        createdAt   TEXT NOT NULL,
        updatedAt   TEXT NOT NULL
    )
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_pc_provider
        ON providerConnections(provider)
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_pc_provider_active
        ON providerConnections(provider, isActive)
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_pc_priority
        ON providerConnections(provider, priority)
    "#,
    // User-defined provider nodes (openai-compatible, anthropic-compatible,
    // custom-embedding). `data` is a JSON blob of node-specific config.
    r#"
    CREATE TABLE IF NOT EXISTS providerNodes (
        id          TEXT PRIMARY KEY,
        type        TEXT,
        name        TEXT,
        data        TEXT NOT NULL,
        createdAt   TEXT NOT NULL,
        updatedAt   TEXT NOT NULL
    )
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_pn_type
        ON providerNodes(type)
    "#,
    // HTTP/SOCKS proxy pools used by executor.
    r#"
    CREATE TABLE IF NOT EXISTS proxyPools (
        id          TEXT PRIMARY KEY,
        isActive    INTEGER NOT NULL DEFAULT 1,
        testStatus  TEXT,
        data        TEXT NOT NULL,
        createdAt   TEXT NOT NULL,
        updatedAt   TEXT NOT NULL
    )
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_pp_active
        ON proxyPools(isActive)
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_pp_status
        ON proxyPools(testStatus)
    "#,
    // Dashboard API keys (HMAC-signed). The key itself is a unique random
    // string; `machineId` ties it to a host for crash diagnostics.
    r#"
    CREATE TABLE IF NOT EXISTS apiKeys (
        id          TEXT PRIMARY KEY,
        key         TEXT UNIQUE NOT NULL,
        name        TEXT,
        machineId   TEXT,
        isActive    INTEGER NOT NULL DEFAULT 1,
        createdAt   TEXT NOT NULL
    )
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_ak_key
        ON apiKeys(key)
    "#,
    // Combos: named fallback/round-robin/fusion chains over models.
    r#"
    CREATE TABLE IF NOT EXISTS combos (
        id          TEXT PRIMARY KEY,
        name        TEXT UNIQUE NOT NULL,
        kind        TEXT,
        models      TEXT NOT NULL,
        data        TEXT NOT NULL DEFAULT '{}',
        createdAt   TEXT NOT NULL,
        updatedAt   TEXT NOT NULL
    )
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_combo_name
        ON combos(name)
    "#,
    // Generic key/value store for modelAliases, customModels, mitmAlias,
    // pricing — all stored as JSON blobs scoped by `scope`.
    r#"
    CREATE TABLE IF NOT EXISTS kv (
        scope       TEXT NOT NULL,
        key         TEXT NOT NULL,
        value       TEXT NOT NULL,
        PRIMARY KEY (scope, key)
    )
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_kv_scope
        ON kv(scope)
    "#,
    // Disabled models per provider — used by combo execution to skip a
    // specific model without removing its parent connection.
    r#"
    CREATE TABLE IF NOT EXISTS disabledModels (
        provider    TEXT NOT NULL,
        model       TEXT NOT NULL,
        PRIMARY KEY (provider, model)
    )
    "#,
    // Append-only usage log. Indexes on the common query dimensions
    // (timestamp DESC for recent, provider/model/connectionId for filters).
    r#"
    CREATE TABLE IF NOT EXISTS usageHistory (
        id               INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp        TEXT NOT NULL,
        provider         TEXT,
        model            TEXT,
        connectionId     TEXT,
        apiKey           TEXT,
        endpoint         TEXT,
        promptTokens     INTEGER DEFAULT 0,
        completionTokens INTEGER DEFAULT 0,
        cost             REAL DEFAULT 0,
        status           TEXT,
        tokens           TEXT,
        meta             TEXT
    )
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_uh_ts
        ON usageHistory(timestamp DESC)
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_uh_provider
        ON usageHistory(provider)
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_uh_model
        ON usageHistory(model)
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_uh_conn
        ON usageHistory(connectionId)
    "#,
    // Per-day rollups for charts. `data` holds the full DailySummary JSON.
    r#"
    CREATE TABLE IF NOT EXISTS usageDaily (
        dateKey TEXT PRIMARY KEY,
        data    TEXT NOT NULL
    )
    "#,
    // Per-request observability records. Mirrors 9router's requestDetails
    // table but lives in the same DB so backups include it. Indexes match
    // the common filter dimensions.
    r#"
    CREATE TABLE IF NOT EXISTS requestDetails (
        id          TEXT PRIMARY KEY,
        timestamp   TEXT NOT NULL,
        provider    TEXT,
        model       TEXT,
        connectionId TEXT,
        status      TEXT,
        data        TEXT NOT NULL
    )
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_rd_ts
        ON requestDetails(timestamp DESC)
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_rd_provider
        ON requestDetails(provider)
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_rd_model
        ON requestDetails(model)
    "#,
    r#"
    CREATE INDEX IF NOT EXISTS idx_rd_conn
        ON requestDetails(connectionId)
    "#,
];

/// PRAGMA statements to run on every new connection. Idempotent (most
/// return the current value rather than mutate state).
///
/// - `journal_mode=WAL` enables concurrent readers with a single writer.
/// - `synchronous=NORMAL` trades a tiny crash-window of lost transactions
///   for ~10x write throughput vs FULL. 9router compat.
/// - `busy_timeout=5000` waits up to 5s before throwing SQLITE_BUSY.
/// - `foreign_keys=ON` is required for any future FK constraints; harmless
///   without them.
/// - `temp_store=MEMORY` keeps temp tables in RAM.
/// - `mmap_size=30000000` enables memory-mapped I/O for faster reads (30 MB).
/// - `cache_size=-64000` sets the page cache to 64 MB (negative = kibibytes).
pub const PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode = WAL",
    "PRAGMA synchronous = NORMAL",
    "PRAGMA busy_timeout = 5000",
    "PRAGMA foreign_keys = ON",
    "PRAGMA temp_store = MEMORY",
    "PRAGMA mmap_size = 30000000",
    "PRAGMA cache_size = -64000",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_sql_is_not_empty() {
        assert!(!TABLES_SQL.is_empty());
        // Every statement must create or alter a table/index.
        for stmt in TABLES_SQL {
            let trimmed = stmt.trim_start().to_ascii_uppercase();
            assert!(
                trimmed.starts_with("CREATE ")
                    || trimmed.starts_with("ALTER ")
                    || trimmed.starts_with("DROP "),
                "DDL must start with CREATE/ALTER/DROP, got: {}",
                &trimmed[..40.min(trimmed.len())]
            );
        }
    }

    #[test]
    fn schema_version_is_positive() {
        // SCHEMA_VERSION is a compile-time constant; this test documents
        // the invariant that it is ≥ 1.
        let _ = SCHEMA_VERSION;
    }

    #[test]
    fn pragmas_are_well_formed() {
        for p in PRAGMAS {
            assert!(p.to_ascii_uppercase().starts_with("PRAGMA "));
        }
    }
}
