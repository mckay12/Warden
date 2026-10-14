use std::collections::HashSet;

use rusqlite::{params, Connection, Row};

use super::models::BlockedAttempt;
use super::now;

fn from_row(row: &Row) -> rusqlite::Result<BlockedAttempt> {
    Ok(BlockedAttempt {
        id: row.get("id")?,
        occurred_at: row.get("occurred_at")?,
        match_type: row.get("match_type")?,
        matched_value: row.get("matched_value")?,
        domain: row.get("domain")?,
    })
}

/// Logs one blocked navigation. No FK to `blocked_domain`/`keyword_rule` by
/// design (ERD §2) — blocklists change over time, so the match is recorded
/// as plain text to keep historical entries accurate regardless.
pub fn create(
    conn: &Connection,
    match_type: &str,
    matched_value: &str,
    domain: &str,
) -> rusqlite::Result<BlockedAttempt> {
    let ts = now();
    conn.execute(
        "INSERT INTO blocked_attempt (occurred_at, match_type, matched_value, domain)
         VALUES (?1, ?2, ?3, ?4)",
        params![ts, match_type, matched_value, domain],
    )?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT * FROM blocked_attempt WHERE id = ?1",
        params![id],
        from_row,
    )
}

/// Most recent attempts, newest first, optionally capped — the dashboard's
/// default "last 30 days" view can filter by `occurred_at` at the call site
/// once real duration math is wired up; this just orders and limits.
pub fn list_recent(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<BlockedAttempt>> {
    let mut stmt =
        conn.prepare("SELECT * FROM blocked_attempt ORDER BY id DESC LIMIT ?1")?;
    let rows = stmt.query_map(params![limit], from_row)?;
    rows.collect()
}

/// Count of attempts at or after a given RFC3339 timestamp — the building
/// block for daily/weekly counts and the streak counter (ERD §1: computed
/// at read time, not stored).
pub fn count_since(conn: &Connection, since: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM blocked_attempt WHERE occurred_at >= ?1",
        params![since],
        |row| row.get(0),
    )
}

/// Attempts at or after a given RFC3339 timestamp, newest first — the
/// dashboard's date-windowed view (default: last 30 days), as opposed to
/// `list_recent`'s plain "last N regardless of when" the popup uses.
pub fn list_since(conn: &Connection, since: &str, limit: i64) -> rusqlite::Result<Vec<BlockedAttempt>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM blocked_attempt WHERE occurred_at >= ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since, limit], from_row)?;
    rows.collect()
}

/// Every distinct UTC calendar date (`YYYY-MM-DD`) that has at least one
/// attempt — the building block for the streak counter. SQLite's `date()`
/// parses the RFC3339 timestamps we store (including their `+00:00` suffix)
/// and normalizes to UTC, matching the UTC-based day boundary
/// `stats::streak_days` uses on the Rust side.
pub fn distinct_days(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT date(occurred_at) FROM blocked_attempt")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn create_and_list() {
        let conn = open_in_memory().unwrap();
        create(&conn, "blocklist", "pornhub.com", "pornhub.com").unwrap();
        create(&conn, "keyword", "porn", "example.com").unwrap();

        let recent = list_recent(&conn, 10).unwrap();
        assert_eq!(recent.len(), 2);
        // Newest first.
        assert_eq!(recent[0].match_type, "keyword");
        assert_eq!(recent[1].match_type, "blocklist");
    }

    #[test]
    fn list_recent_respects_limit() {
        let conn = open_in_memory().unwrap();
        for i in 0..5 {
            create(&conn, "blocklist", "x.com", &format!("x{i}.com")).unwrap();
        }
        assert_eq!(list_recent(&conn, 3).unwrap().len(), 3);
    }

    #[test]
    fn count_since_filters_by_timestamp() {
        let conn = open_in_memory().unwrap();
        create(&conn, "blocklist", "x.com", "x.com").unwrap();
        create(&conn, "blocklist", "y.com", "y.com").unwrap();

        // Every row was just inserted "now", so a far-future cutoff excludes all.
        assert_eq!(count_since(&conn, "9999-01-01T00:00:00+00:00").unwrap(), 0);
        // And a far-past cutoff includes all.
        assert_eq!(count_since(&conn, "2000-01-01T00:00:00+00:00").unwrap(), 2);
    }

    #[test]
    fn list_since_filters_and_orders_newest_first() {
        let conn = open_in_memory().unwrap();
        create(&conn, "blocklist", "x.com", "x.com").unwrap();
        create(&conn, "keyword", "porn", "y.com").unwrap();

        let all = list_since(&conn, "2000-01-01T00:00:00+00:00", 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].domain, "y.com");

        assert!(list_since(&conn, "9999-01-01T00:00:00+00:00", 10).unwrap().is_empty());
    }

    #[test]
    fn distinct_days_dedupes_same_day_entries() {
        let conn = open_in_memory().unwrap();
        create(&conn, "blocklist", "x.com", "x.com").unwrap();
        create(&conn, "blocklist", "y.com", "y.com").unwrap();

        // Both rows were inserted "now", so they land on the same UTC date.
        let days = distinct_days(&conn).unwrap();
        assert_eq!(days.len(), 1);

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        assert!(days.contains(&today));
    }
}
