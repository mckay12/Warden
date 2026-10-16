use rusqlite::{params, Connection, OptionalExtension, Row};

use super::models::Lock;
use super::now;

fn from_row(row: &Row) -> rusqlite::Result<Lock> {
    Ok(Lock {
        id: row.get("id")?,
        status: row.get("status")?,
        is_forever: row.get("is_forever")?,
        initial_duration_seconds: row.get("initial_duration_seconds")?,
        started_at: row.get("started_at")?,
        expires_at: row.get("expires_at")?,
        ended_at: row.get("ended_at")?,
        created_at: row.get("created_at")?,
        cancelled_at: row.get("cancelled_at")?,
    })
}

/// Starts a new lock. Callers are responsible for checking there isn't
/// already an active/forever lock first (per ERD §2, enforced in app logic,
/// not a DB constraint).
pub fn create(
    conn: &Connection,
    is_forever: bool,
    initial_duration_seconds: Option<i64>,
    expires_at: Option<&str>,
) -> rusqlite::Result<Lock> {
    let status = if is_forever { "forever" } else { "active" };
    let ts = now();
    conn.execute(
        "INSERT INTO lock (status, is_forever, initial_duration_seconds, started_at, expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![status, is_forever, initial_duration_seconds, ts, expires_at, ts],
    )?;
    get_by_id(conn, conn.last_insert_rowid())
        .map(|opt| opt.expect("just-inserted lock row must exist"))
}

pub fn get_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<Lock>> {
    conn.query_row("SELECT * FROM lock WHERE id = ?1", params![id], from_row)
        .optional()
}

/// The single lock currently in force, if any (`active` or `forever`).
pub fn get_current(conn: &Connection) -> rusqlite::Result<Option<Lock>> {
    conn.query_row(
        "SELECT * FROM lock WHERE status IN ('active', 'forever') ORDER BY id DESC LIMIT 1",
        [],
        from_row,
    )
    .optional()
}

/// Full lock history, most recent first.
pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Lock>> {
    let mut stmt = conn.prepare("SELECT * FROM lock ORDER BY id DESC")?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

/// Marks a lock as naturally expired.
pub fn end(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE lock SET status = 'expired', ended_at = ?1 WHERE id = ?2",
        params![now(), id],
    )?;
    Ok(())
}

/// Ends a lock early because it was undone inside its undo window. Recorded
/// as an expired lock with `cancelled_at` set, so history shows it. Returns
/// whether a still-running lock was changed.
pub fn cancel(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let ts = now();
    let changed = conn.execute(
        "UPDATE lock SET status = 'expired', ended_at = ?1, cancelled_at = ?1
         WHERE id = ?2 AND status IN ('active', 'forever')",
        params![ts, id],
    )?;
    Ok(changed > 0)
}

/// Recalculates `expires_at` after an extension is applied.
pub fn update_expires_at(conn: &Connection, id: i64, new_expires_at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE lock SET expires_at = ?1 WHERE id = ?2",
        params![new_expires_at, id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn create_and_fetch_timed_lock() {
        let conn = open_in_memory().unwrap();
        let lock = create(&conn, false, Some(3600), Some("2026-08-29T00:00:00+00:00")).unwrap();

        assert_eq!(lock.status, "active");
        assert!(!lock.is_forever);
        assert_eq!(lock.initial_duration_seconds, Some(3600));

        let fetched = get_by_id(&conn, lock.id).unwrap().unwrap();
        assert_eq!(fetched, lock);
    }

    #[test]
    fn create_forever_lock_has_no_duration() {
        let conn = open_in_memory().unwrap();
        let lock = create(&conn, true, None, None).unwrap();

        assert_eq!(lock.status, "forever");
        assert!(lock.is_forever);
        assert_eq!(lock.initial_duration_seconds, None);
        assert_eq!(lock.expires_at, None);
    }

    #[test]
    fn get_current_only_returns_active_or_forever() {
        let conn = open_in_memory().unwrap();
        assert!(get_current(&conn).unwrap().is_none());

        let lock = create(&conn, false, Some(60), Some("x")).unwrap();
        assert_eq!(get_current(&conn).unwrap().unwrap().id, lock.id);

        end(&conn, lock.id).unwrap();
        assert!(get_current(&conn).unwrap().is_none());
    }

    #[test]
    fn extension_updates_expiry() {
        let conn = open_in_memory().unwrap();
        let lock = create(&conn, false, Some(60), Some("2026-08-29T00:00:00+00:00")).unwrap();

        update_expires_at(&conn, lock.id, "2026-08-29T01:00:00+00:00").unwrap();

        let fetched = get_by_id(&conn, lock.id).unwrap().unwrap();
        assert_eq!(fetched.expires_at.as_deref(), Some("2026-08-29T01:00:00+00:00"));
    }

    #[test]
    fn list_returns_most_recent_first() {
        let conn = open_in_memory().unwrap();
        let first = create(&conn, false, Some(60), Some("x")).unwrap();
        end(&conn, first.id).unwrap();
        let second = create(&conn, true, None, None).unwrap();

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, second.id);
        assert_eq!(all[1].id, first.id);
    }
}
