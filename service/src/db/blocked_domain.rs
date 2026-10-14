use rusqlite::{params, Connection, Row};

use super::models::BlockedDomain;
use super::now;

fn from_row(row: &Row) -> rusqlite::Result<BlockedDomain> {
    Ok(BlockedDomain {
        id: row.get("id")?,
        blocklist_source_id: row.get("blocklist_source_id")?,
        domain: row.get("domain")?,
        added_at: row.get("added_at")?,
    })
}

pub fn list_for_source(conn: &Connection, source_id: i64) -> rusqlite::Result<Vec<BlockedDomain>> {
    let mut stmt =
        conn.prepare("SELECT * FROM blocked_domain WHERE blocklist_source_id = ?1 ORDER BY domain")?;
    let rows = stmt.query_map(params![source_id], from_row)?;
    rows.collect()
}

pub fn exists(conn: &Connection, domain: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM blocked_domain WHERE domain = ?1)",
        params![domain],
        |row| row.get(0),
    )
}

/// Wipe-and-reload for one source (per ERD §4 open question, resolved in
/// favor of bulk replace over row-by-row diffing — simpler and sufficient
/// at this data volume). Runs inside a transaction so a refresh can't leave
/// the table half-updated if it fails partway through.
pub fn replace_for_source(
    conn: &mut Connection,
    source_id: i64,
    domains: &[String],
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM blocked_domain WHERE blocklist_source_id = ?1",
        params![source_id],
    )?;
    let ts = now();
    {
        let mut stmt = tx.prepare(
            "INSERT INTO blocked_domain (blocklist_source_id, domain, added_at) VALUES (?1, ?2, ?3)",
        )?;
        for domain in domains {
            stmt.execute(params![source_id, domain, ts])?;
        }
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{blocklist_source, open_in_memory};

    #[test]
    fn replace_wipes_and_reloads() {
        let mut conn = open_in_memory().unwrap();
        let source = blocklist_source::create(&conn, "https://example.com/list.txt").unwrap();

        replace_for_source(
            &mut conn,
            source.id,
            &["a.com".to_string(), "b.com".to_string()],
        )
        .unwrap();
        assert_eq!(list_for_source(&conn, source.id).unwrap().len(), 2);

        // A second refresh with a smaller list should fully replace, not merge.
        replace_for_source(&mut conn, source.id, &["c.com".to_string()]).unwrap();
        let domains = list_for_source(&conn, source.id).unwrap();
        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].domain, "c.com");
    }

    #[test]
    fn exists_checks_across_sources() {
        let mut conn = open_in_memory().unwrap();
        let source = blocklist_source::create(&conn, "https://example.com/list.txt").unwrap();
        replace_for_source(&mut conn, source.id, &["known.com".to_string()]).unwrap();

        assert!(exists(&conn, "known.com").unwrap());
        assert!(!exists(&conn, "unknown.com").unwrap());
    }
}
