use rusqlite::{params, Connection, OptionalExtension, Row};

use super::now;

/// One row of `allow_exception`. `status` is only 'active' or 'removed'; whether
/// an active row is *in force yet* is decided by `effective_at` (see
/// `exceptions::state_of`).
#[derive(Debug, Clone, PartialEq)]
pub struct AllowException {
    pub id: i64,
    pub domain: String,
    pub requested_at: String,
    pub effective_at: String,
    pub status: String,
    pub removed_at: Option<String>,
}

fn from_row(row: &Row) -> rusqlite::Result<AllowException> {
    Ok(AllowException {
        id: row.get("id")?,
        domain: row.get("domain")?,
        requested_at: row.get("requested_at")?,
        effective_at: row.get("effective_at")?,
        status: row.get("status")?,
        removed_at: row.get("removed_at")?,
    })
}

pub fn create(conn: &Connection, domain: &str, effective_at: &str) -> rusqlite::Result<AllowException> {
    conn.execute(
        "INSERT INTO allow_exception (domain, requested_at, effective_at, status)
         VALUES (?1, ?2, ?3, 'active')",
        params![domain, now(), effective_at],
    )?;
    let id = conn.last_insert_rowid();
    conn.query_row("SELECT * FROM allow_exception WHERE id = ?1", params![id], from_row)
}

/// Exceptions that haven't been removed (pending or in force), newest first.
pub fn list_open(conn: &Connection) -> rusqlite::Result<Vec<AllowException>> {
    let mut stmt = conn.prepare("SELECT * FROM allow_exception WHERE status = 'active' ORDER BY id DESC")?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

pub fn get_open_by_domain(conn: &Connection, domain: &str) -> rusqlite::Result<Option<AllowException>> {
    conn.query_row(
        "SELECT * FROM allow_exception WHERE domain = ?1 AND status = 'active'",
        params![domain],
        from_row,
    )
    .optional()
}

pub fn count_open(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM allow_exception WHERE status = 'active'", [], |r| r.get(0))
}

/// Marks an exception removed. Returns whether a still-open row was changed.
pub fn remove(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE allow_exception SET status = 'removed', removed_at = ?2 WHERE id = ?1 AND status = 'active'",
        params![id, now()],
    )?;
    Ok(changed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn create_list_and_remove() {
        let conn = open_in_memory().unwrap();
        let a = create(&conn, "wotaku.wiki", "2030-01-01T00:00:00+00:00").unwrap();
        assert_eq!(a.status, "active");
        assert_eq!(a.removed_at, None);
        assert_eq!(list_open(&conn).unwrap().len(), 1);
        assert_eq!(get_open_by_domain(&conn, "wotaku.wiki").unwrap().unwrap().id, a.id);
        assert_eq!(count_open(&conn).unwrap(), 1);

        assert!(remove(&conn, a.id).unwrap());
        assert!(!remove(&conn, a.id).unwrap(), "removing twice changes nothing");
        assert!(list_open(&conn).unwrap().is_empty());
        assert!(get_open_by_domain(&conn, "wotaku.wiki").unwrap().is_none());
    }

    #[test]
    fn a_removed_domain_can_be_requested_again() {
        let conn = open_in_memory().unwrap();
        let a = create(&conn, "example.org", "2030-01-01T00:00:00+00:00").unwrap();
        remove(&conn, a.id).unwrap();
        let b = create(&conn, "example.org", "2030-01-02T00:00:00+00:00").unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(list_open(&conn).unwrap().len(), 1);
    }
}
