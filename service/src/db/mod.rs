// Phase 5 delivers the full CRUD surface per the ERD; most of it isn't
// wired into service logic until later phases (locks in Phase 8, blocklist
// refresh in Phase 13, etc.), so `cargo build` sees a lot of not-yet-called
// functions. That's expected here — allow it rather than trim the surface
// down to only what's used today.
#![allow(dead_code)]

mod schema;

pub mod allow_exception;
pub mod blocked_attempt;
pub mod blocked_domain;
pub mod blocklist_source;
pub mod emergency_unlock;
pub mod keyword_rule;
pub mod lock;
pub mod lock_extension;
pub mod models;
pub mod service_config;

use std::path::PathBuf;

use rusqlite::Connection;

pub fn db_dir() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".to_string());
    PathBuf::from(base).join("Warden")
}

pub fn db_path() -> PathBuf {
    db_dir().join("warden.db")
}

/// Opens the on-disk database, creating the directory and file (and the
/// full schema, via `IF NOT EXISTS`) if this is the first run.
pub fn open() -> rusqlite::Result<Connection> {
    std::fs::create_dir_all(db_dir()).map_err(|e| {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some(format!("could not create {}: {e}", db_dir().display())),
        )
    })?;
    let conn = Connection::open(db_path())?;
    init(&conn)?;
    Ok(conn)
}

/// In-memory database with the schema applied — used by tests so the CRUD
/// layer is exercised without touching disk or depending on the service
/// process at all.
#[cfg(test)]
pub fn open_in_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    init(&conn)?;
    Ok(conn)
}

fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.execute_batch(schema::SCHEMA)?;
    // `CREATE TABLE IF NOT EXISTS` never alters a table that already exists, so
    // columns added after a database was first created are added here.
    add_column_if_missing(conn, "lock", "cancelled_at", "TEXT")?;
    Ok(())
}

fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|name| name == column);
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
    }
    Ok(())
}

/// Current UTC time as RFC3339 — the format every DATETIME column in the
/// ERD is stored as (SQLite has no native datetime type).
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn an_older_database_without_cancelled_at_is_upgraded_in_place() {
        let conn = Connection::open_in_memory().unwrap();
        // The `lock` table exactly as it was before undo existed.
        conn.execute_batch(
            "CREATE TABLE lock (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                status TEXT NOT NULL CHECK (status IN ('active', 'expired', 'forever')),
                is_forever INTEGER NOT NULL CHECK (is_forever IN (0, 1)),
                initial_duration_seconds INTEGER,
                started_at TEXT NOT NULL,
                expires_at TEXT,
                ended_at TEXT,
                created_at TEXT NOT NULL
            );
            INSERT INTO lock (status, is_forever, initial_duration_seconds, started_at, expires_at, created_at)
            VALUES ('active', 0, 3600, '2026-01-01T00:00:00+00:00', '2999-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00');",
        )
        .unwrap();

        init(&conn).unwrap();
        init(&conn).unwrap(); // running it again must be harmless

        // existing data survives, and the new column is there and empty
        let cancelled: Option<String> = conn
            .query_row("SELECT cancelled_at FROM lock WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cancelled, None);
        let current = lock::get_current(&conn).unwrap().expect("the old lock must still be readable");
        assert_eq!(current.initial_duration_seconds, Some(3600));
    }
}
