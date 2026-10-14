//! Shared lock/timer logic (Phase 8) — used both by the service's periodic
//! tick (`service.rs`) and defensively at the top of every native-messaging
//! handler that reads or writes lock state, so a freshly-launched host
//! process is never looking at stale data even if the tick hasn't run
//! since expiry (the two run as separate processes; see `db/mod.rs`).

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::db::lock;

/// Ends the current lock if it's a timed lock past its `expires_at`. A
/// `forever` lock has no expiry and is never touched here — the only way
/// off one is the emergency-unlock flow (Phase 11).
///
/// Returns whether a lock was actually expired, so callers with a logger
/// (the service's tick) can record the moment it happened rather than just
/// silently noticing it later on the next status read.
pub fn maybe_expire(conn: &Connection) -> rusqlite::Result<bool> {
    let Some(current) = lock::get_current(conn)? else {
        return Ok(false);
    };
    if current.is_forever {
        return Ok(false);
    }
    let Some(expires_at) = current.expires_at.as_deref() else {
        return Ok(false);
    };
    let Ok(expires_at) = DateTime::parse_from_rfc3339(expires_at) else {
        return Ok(false);
    };
    if Utc::now() >= expires_at {
        lock::end(conn, current.id)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{lock, open_in_memory};

    #[test]
    fn expires_a_lock_past_its_expiry() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, false, Some(60), Some("2000-01-01T00:00:00+00:00")).unwrap();

        maybe_expire(&conn).unwrap();

        let fetched = lock::get_by_id(&conn, l.id).unwrap().unwrap();
        assert_eq!(fetched.status, "expired");
        assert!(fetched.ended_at.is_some());
    }

    #[test]
    fn leaves_a_not_yet_expired_lock_alone() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, false, Some(3600), Some("2999-01-01T00:00:00+00:00")).unwrap();

        maybe_expire(&conn).unwrap();

        let fetched = lock::get_by_id(&conn, l.id).unwrap().unwrap();
        assert_eq!(fetched.status, "active");
    }

    #[test]
    fn leaves_a_forever_lock_alone_regardless() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, true, None, None).unwrap();

        maybe_expire(&conn).unwrap();

        let fetched = lock::get_by_id(&conn, l.id).unwrap().unwrap();
        assert_eq!(fetched.status, "forever");
    }

    #[test]
    fn no_current_lock_is_a_noop() {
        let conn = open_in_memory().unwrap();
        maybe_expire(&conn).unwrap(); // must not error with nothing to expire
    }
}
