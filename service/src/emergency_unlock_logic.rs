//! Applies a pending emergency unlock once its cooldown has passed
//! (Phase 11) — the escape hatch for a "forever" lock (PRD §7.4). Same
//! shape as `lock_logic::maybe_expire`: polled from `service.rs`'s tick,
//! not something that fires exactly on the second, but within ~1s of it.

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::db::{emergency_unlock, lock};

/// Ends the forever lock and marks the request applied, once
/// `cooldown_ends_at` has passed. Returns whether it actually applied
/// something this call, so the caller can log the moment it happened.
pub fn maybe_apply(conn: &Connection) -> rusqlite::Result<bool> {
    let Some(request) = emergency_unlock::get_pending(conn)? else {
        return Ok(false);
    };
    let Ok(cooldown_ends_at) = DateTime::parse_from_rfc3339(&request.cooldown_ends_at) else {
        return Ok(false);
    };
    if Utc::now() < cooldown_ends_at {
        return Ok(false);
    }

    emergency_unlock::apply(conn, request.id)?;
    // Reuses the same terminal state a timed lock reaches on expiry — the
    // ERD's `lock.status` enum only has `active` | `expired` | `forever`,
    // and "an emergency unlock ended this lock" is exactly what `expired`
    // already means: the lock is no longer in force.
    lock::end(conn, request.lock_id)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{emergency_unlock, lock, open_in_memory};

    #[test]
    fn applies_once_cooldown_has_passed() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, true, None, None).unwrap();
        let req = emergency_unlock::create(&conn, l.id, "2000-01-01T00:00:00+00:00").unwrap();

        let applied = maybe_apply(&conn).unwrap();
        assert!(applied);

        let fetched_lock = lock::get_by_id(&conn, l.id).unwrap().unwrap();
        assert_eq!(fetched_lock.status, "expired");
        assert!(fetched_lock.ended_at.is_some());

        let history = emergency_unlock::list_all(&conn, 10).unwrap();
        assert_eq!(history[0].id, req.id);
        assert_eq!(history[0].status, "applied");
        assert!(history[0].applied_at.is_some());
    }

    #[test]
    fn leaves_a_not_yet_due_request_alone() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, true, None, None).unwrap();
        emergency_unlock::create(&conn, l.id, "2999-01-01T00:00:00+00:00").unwrap();

        let applied = maybe_apply(&conn).unwrap();
        assert!(!applied);

        let fetched_lock = lock::get_by_id(&conn, l.id).unwrap().unwrap();
        assert_eq!(fetched_lock.status, "forever");
    }

    #[test]
    fn no_pending_request_is_a_noop() {
        let conn = open_in_memory().unwrap();
        let applied = maybe_apply(&conn).unwrap();
        assert!(!applied);
    }

    #[test]
    fn cancelled_request_is_not_applied() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, true, None, None).unwrap();
        let req = emergency_unlock::create(&conn, l.id, "2000-01-01T00:00:00+00:00").unwrap();
        emergency_unlock::cancel(&conn, req.id).unwrap();

        let applied = maybe_apply(&conn).unwrap();
        assert!(!applied);

        let fetched_lock = lock::get_by_id(&conn, l.id).unwrap().unwrap();
        assert_eq!(fetched_lock.status, "forever");
    }
}
