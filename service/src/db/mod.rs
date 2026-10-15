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
    Ok(())
}

/// Current UTC time as RFC3339 — the format every DATETIME column in the
/// ERD is stored as (SQLite has no native datetime type).
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
