use rusqlite::{params, Connection, OptionalExtension, Row};

use super::models::EmergencyUnlockRequest;
use super::now;

fn from_row(row: &Row) -> rusqlite::Result<EmergencyUnlockRequest> {
    Ok(EmergencyUnlockRequest {
        id: row.get("id")?,
        lock_id: row.get("lock_id")?,
        requested_at: row.get("requested_at")?,
        cooldown_ends_at: row.get("cooldown_ends_at")?,
        status: row.get("status")?,
        cancelled_at: row.get("cancelled_at")?,
        applied_at: row.get("applied_at")?,
    })
}

/// Creates a pending request. The caller is responsible for the "only one
/// pending request at a time" rule (ERD open question, resolved as
/// application-logic per its own recommendation) — this function just
/// records what it's told.
pub fn create(
    conn: &Connection,
    lock_id: i64,
    cooldown_ends_at: &str,
) -> rusqlite::Result<EmergencyUnlockRequest> {
    let ts = now();
    conn.execute(
        "INSERT INTO emergency_unlock_request (lock_id, requested_at, cooldown_ends_at, status)
         VALUES (?1, ?2, ?3, 'pending')",
        params![lock_id, ts, cooldown_ends_at],
    )?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT * FROM emergency_unlock_request WHERE id = ?1",
        params![id],
        from_row,
    )
}

pub fn get_pending(conn: &Connection) -> rusqlite::Result<Option<EmergencyUnlockRequest>> {
    conn.query_row(
        "SELECT * FROM emergency_unlock_request WHERE status = 'pending' ORDER BY id DESC LIMIT 1",
        [],
        from_row,
    )
    .optional()
}

pub fn cancel(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE emergency_unlock_request SET status = 'cancelled', cancelled_at = ?1 WHERE id = ?2",
        params![now(), id],
    )?;
    Ok(())
}

pub fn apply(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE emergency_unlock_request SET status = 'applied', applied_at = ?1 WHERE id = ?2",
        params![now(), id],
    )?;
    Ok(())
}

/// Full request history for a lock, most recent first — the dashboard's
/// "emergency unlock history" section (per PRD §7.4: every request and
/// outcome stays visible permanently).
pub fn list_for_lock(conn: &Connection, lock_id: i64) -> rusqlite::Result<Vec<EmergencyUnlockRequest>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM emergency_unlock_request WHERE lock_id = ?1 ORDER BY id DESC",
    )?;
    let rows = stmt.query_map(params![lock_id], from_row)?;
    rows.collect()
}

/// Full history across every lock, most recent first — same idea as
/// `list_for_lock` but not scoped to one lock, since the extension side
/// (Phase 11 popup, Phase 12 dashboard) just wants "everything that's ever
/// happened" rather than needing to know which lock to ask about.
pub fn list_all(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<EmergencyUnlockRequest>> {
    let mut stmt =
        conn.prepare("SELECT * FROM emergency_unlock_request ORDER BY id DESC LIMIT ?1")?;
    let rows = stmt.query_map(params![limit], from_row)?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{lock, open_in_memory};

    #[test]
    fn create_pending_and_fetch() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, true, None, None).unwrap();

        let req = create(&conn, l.id, "2026-09-05T00:00:00+00:00").unwrap();
        assert_eq!(req.status, "pending");

        let pending = get_pending(&conn).unwrap().unwrap();
        assert_eq!(pending.id, req.id);
    }

    #[test]
    fn cancel_clears_pending() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, true, None, None).unwrap();
        let req = create(&conn, l.id, "2026-09-05T00:00:00+00:00").unwrap();

        cancel(&conn, req.id).unwrap();

        assert!(get_pending(&conn).unwrap().is_none());
        let history = list_for_lock(&conn, l.id).unwrap();
        assert_eq!(history[0].status, "cancelled");
        assert!(history[0].cancelled_at.is_some());
    }

    #[test]
    fn apply_clears_pending() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, true, None, None).unwrap();
        let req = create(&conn, l.id, "2026-09-05T00:00:00+00:00").unwrap();

        apply(&conn, req.id).unwrap();

        assert!(get_pending(&conn).unwrap().is_none());
        let history = list_for_lock(&conn, l.id).unwrap();
        assert_eq!(history[0].status, "applied");
        assert!(history[0].applied_at.is_some());
    }

    #[test]
    fn list_all_spans_every_lock_newest_first() {
        let conn = open_in_memory().unwrap();
        let l1 = lock::create(&conn, true, None, None).unwrap();
        lock::end(&conn, l1.id).unwrap();
        let l2 = lock::create(&conn, true, None, None).unwrap();

        let r1 = create(&conn, l1.id, "2026-09-01T00:00:00+00:00").unwrap();
        let r2 = create(&conn, l2.id, "2026-09-05T00:00:00+00:00").unwrap();

        let all = list_all(&conn, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, r2.id);
        assert_eq!(all[1].id, r1.id);
    }
}
