use rusqlite::{params, Connection, OptionalExtension, Row};

use super::models::BlocklistSource;
use super::now;

fn from_row(row: &Row) -> rusqlite::Result<BlocklistSource> {
    Ok(BlocklistSource {
        id: row.get("id")?,
        source_url: row.get("source_url")?,
        last_refreshed_at: row.get("last_refreshed_at")?,
        version_hash: row.get("version_hash")?,
        is_active: row.get("is_active")?,
    })
}

pub fn create(conn: &Connection, source_url: &str) -> rusqlite::Result<BlocklistSource> {
    conn.execute(
        "INSERT INTO blocklist_source (source_url, is_active) VALUES (?1, 1)",
        params![source_url],
    )?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT * FROM blocklist_source WHERE id = ?1",
        params![id],
        from_row,
    )
}

pub fn get_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<BlocklistSource>> {
    conn.query_row(
        "SELECT * FROM blocklist_source WHERE id = ?1",
        params![id],
        from_row,
    )
    .optional()
}

pub fn get_by_url(conn: &Connection, source_url: &str) -> rusqlite::Result<Option<BlocklistSource>> {
    conn.query_row(
        "SELECT * FROM blocklist_source WHERE source_url = ?1",
        params![source_url],
        from_row,
    )
    .optional()
}

/// Finds the source row for a URL, creating it (as active) if this is the
/// first time it's been seen — the refresh job's entry point, so it works
/// on a totally fresh DB without a separate seeding step.
pub fn get_or_create(conn: &Connection, source_url: &str) -> rusqlite::Result<BlocklistSource> {
    if let Some(existing) = get_by_url(conn, source_url)? {
        return Ok(existing);
    }
    create(conn, source_url)
}

pub fn list_active(conn: &Connection) -> rusqlite::Result<Vec<BlocklistSource>> {
    let mut stmt = conn.prepare("SELECT * FROM blocklist_source WHERE is_active = 1 ORDER BY id")?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

/// Records that a refresh happened, and what it fetched — the version hash
/// lets a later refresh skip reloading `blocked_domain` rules when nothing
/// changed (ERD §2).
pub fn record_refresh(conn: &Connection, id: i64, version_hash: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE blocklist_source SET last_refreshed_at = ?1, version_hash = ?2 WHERE id = ?3",
        params![now(), version_hash, id],
    )?;
    Ok(())
}

pub fn set_active(conn: &Connection, id: i64, is_active: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE blocklist_source SET is_active = ?1 WHERE id = ?2",
        params![is_active, id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn create_and_fetch() {
        let conn = open_in_memory().unwrap();
        let source = create(&conn, "https://example.com/blocklist.txt").unwrap();
        assert!(source.is_active);
        assert!(source.last_refreshed_at.is_none());

        let fetched = get_by_id(&conn, source.id).unwrap().unwrap();
        assert_eq!(fetched, source);
    }

    #[test]
    fn record_refresh_sets_hash_and_timestamp() {
        let conn = open_in_memory().unwrap();
        let source = create(&conn, "https://example.com/blocklist.txt").unwrap();

        record_refresh(&conn, source.id, "abc123").unwrap();

        let fetched = get_by_id(&conn, source.id).unwrap().unwrap();
        assert_eq!(fetched.version_hash.as_deref(), Some("abc123"));
        assert!(fetched.last_refreshed_at.is_some());
    }

    #[test]
    fn get_or_create_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let first = get_or_create(&conn, "https://example.com/blocklist.txt").unwrap();
        let second = get_or_create(&conn, "https://example.com/blocklist.txt").unwrap();
        assert_eq!(first.id, second.id);
    }

    #[test]
    fn set_active_excludes_from_list() {
        let conn = open_in_memory().unwrap();
        let source = create(&conn, "https://example.com/blocklist.txt").unwrap();
        assert_eq!(list_active(&conn).unwrap().len(), 1);

        set_active(&conn, source.id, false).unwrap();
        assert_eq!(list_active(&conn).unwrap().len(), 0);
    }
}
