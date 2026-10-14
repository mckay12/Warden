use rusqlite::{params, Connection, Row};

use super::models::LockExtension;
use super::now;

fn from_row(row: &Row) -> rusqlite::Result<LockExtension> {
    Ok(LockExtension {
        id: row.get("id")?,
        lock_id: row.get("lock_id")?,
        added_seconds: row.get("added_seconds")?,
        extended_at: row.get("extended_at")?,
    })
}

/// Records an extension. Append-only audit trail (per ERD) — extending is
/// always allowed, so there's no permission check here; the caller is
/// expected to have already updated `lock.expires_at` separately.
pub fn create(conn: &Connection, lock_id: i64, added_seconds: i64) -> rusqlite::Result<LockExtension> {
    let ts = now();
    conn.execute(
        "INSERT INTO lock_extension (lock_id, added_seconds, extended_at) VALUES (?1, ?2, ?3)",
        params![lock_id, added_seconds, ts],
    )?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT * FROM lock_extension WHERE id = ?1",
        params![id],
        from_row,
    )
}

/// All extensions for a lock, oldest first — the transparency trail shown
/// on the dashboard.
pub fn list_for_lock(conn: &Connection, lock_id: i64) -> rusqlite::Result<Vec<LockExtension>> {
    let mut stmt =
        conn.prepare("SELECT * FROM lock_extension WHERE lock_id = ?1 ORDER BY id ASC")?;
    let rows = stmt.query_map(params![lock_id], from_row)?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{lock, open_in_memory};

    #[test]
    fn create_and_list_extensions() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, false, Some(60), Some("x")).unwrap();

        create(&conn, l.id, 300).unwrap();
        create(&conn, l.id, 600).unwrap();

        let extensions = list_for_lock(&conn, l.id).unwrap();
        assert_eq!(extensions.len(), 2);
        assert_eq!(extensions[0].added_seconds, 300);
        assert_eq!(extensions[1].added_seconds, 600);
    }

    #[test]
    fn list_for_unrelated_lock_is_empty() {
        let conn = open_in_memory().unwrap();
        let l1 = lock::create(&conn, false, Some(60), Some("x")).unwrap();
        let l2 = lock::create(&conn, false, Some(60), Some("y")).unwrap();
        create(&conn, l1.id, 100).unwrap();

        assert!(list_for_lock(&conn, l2.id).unwrap().is_empty());
    }
}
