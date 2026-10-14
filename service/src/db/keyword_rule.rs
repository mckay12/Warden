use rusqlite::{params, Connection, Row};

use super::models::KeywordRule;
use super::now;

fn from_row(row: &Row) -> rusqlite::Result<KeywordRule> {
    Ok(KeywordRule {
        id: row.get("id")?,
        pattern: row.get("pattern")?,
        pattern_type: row.get("pattern_type")?,
        is_custom: row.get("is_custom")?,
        is_active: row.get("is_active")?,
        created_at: row.get("created_at")?,
    })
}

pub fn create(
    conn: &Connection,
    pattern: &str,
    pattern_type: &str,
    is_custom: bool,
) -> rusqlite::Result<KeywordRule> {
    let ts = now();
    conn.execute(
        "INSERT INTO keyword_rule (pattern, pattern_type, is_custom, is_active, created_at)
         VALUES (?1, ?2, ?3, 1, ?4)",
        params![pattern, pattern_type, is_custom, ts],
    )?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT * FROM keyword_rule WHERE id = ?1",
        params![id],
        from_row,
    )
}

/// The set the blocking engine actually applies.
pub fn list_active(conn: &Connection) -> rusqlite::Result<Vec<KeywordRule>> {
    let mut stmt = conn.prepare("SELECT * FROM keyword_rule WHERE is_active = 1 ORDER BY id")?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

pub fn list_all(conn: &Connection) -> rusqlite::Result<Vec<KeywordRule>> {
    let mut stmt = conn.prepare("SELECT * FROM keyword_rule ORDER BY id")?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

pub fn set_active(conn: &Connection, id: i64, is_active: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE keyword_rule SET is_active = ?1 WHERE id = ?2",
        params![is_active, id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn create_defaults_to_active() {
        let conn = open_in_memory().unwrap();
        let rule = create(&conn, "porn", "keyword", false).unwrap();
        assert!(rule.is_active);
        assert!(!rule.is_custom);
    }

    #[test]
    fn set_active_excludes_from_active_list() {
        let conn = open_in_memory().unwrap();
        let rule = create(&conn, "porn", "keyword", false).unwrap();
        assert_eq!(list_active(&conn).unwrap().len(), 1);

        set_active(&conn, rule.id, false).unwrap();
        assert_eq!(list_active(&conn).unwrap().len(), 0);
        // Still visible in the unfiltered list, just not the active one.
        assert_eq!(list_all(&conn).unwrap().len(), 1);
    }
}
