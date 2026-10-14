use rusqlite::{params, Connection, OptionalExtension, Row};

use super::models::ServiceConfig;
use super::now;

const DEFAULT_REFRESH_INTERVAL_HOURS: i64 = 24;
const DEFAULT_MERGE_MODE: &str = "merge";

fn from_row(row: &Row) -> rusqlite::Result<ServiceConfig> {
    Ok(ServiceConfig {
        id: row.get("id")?,
        blocklist_refresh_interval_hours: row.get("blocklist_refresh_interval_hours")?,
        registry_policy_merge_mode: row.get("registry_policy_merge_mode")?,
        updated_at: row.get("updated_at")?,
    })
}

/// Returns the singleton config row, creating it with defaults on first
/// access (id is always 1, enforced by the `CHECK (id = 1)` constraint).
pub fn get(conn: &Connection) -> rusqlite::Result<ServiceConfig> {
    let existing = conn
        .query_row("SELECT * FROM service_config WHERE id = 1", [], from_row)
        .optional()?;

    if let Some(config) = existing {
        return Ok(config);
    }

    let ts = now();
    conn.execute(
        "INSERT INTO service_config (id, blocklist_refresh_interval_hours, registry_policy_merge_mode, updated_at)
         VALUES (1, ?1, ?2, ?3)",
        params![DEFAULT_REFRESH_INTERVAL_HOURS, DEFAULT_MERGE_MODE, ts],
    )?;
    conn.query_row("SELECT * FROM service_config WHERE id = 1", [], from_row)
}

pub fn update(
    conn: &Connection,
    refresh_interval_hours: i64,
    merge_mode: &str,
) -> rusqlite::Result<ServiceConfig> {
    // Ensure the row exists first (so this also works on a fresh DB).
    get(conn)?;
    conn.execute(
        "UPDATE service_config SET blocklist_refresh_interval_hours = ?1, registry_policy_merge_mode = ?2, updated_at = ?3 WHERE id = 1",
        params![refresh_interval_hours, merge_mode, now()],
    )?;
    conn.query_row("SELECT * FROM service_config WHERE id = 1", [], from_row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn get_creates_default_row_on_first_access() {
        let conn = open_in_memory().unwrap();
        let config = get(&conn).unwrap();
        assert_eq!(config.id, 1);
        assert_eq!(config.blocklist_refresh_interval_hours, 24);
        assert_eq!(config.registry_policy_merge_mode, "merge");
    }

    #[test]
    fn get_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let first = get(&conn).unwrap();
        let second = get(&conn).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn update_changes_values() {
        let conn = open_in_memory().unwrap();
        get(&conn).unwrap();

        let updated = update(&conn, 12, "overwrite").unwrap();
        assert_eq!(updated.blocklist_refresh_interval_hours, 12);
        assert_eq!(updated.registry_policy_merge_mode, "overwrite");
    }
}
