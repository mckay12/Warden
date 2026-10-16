//! Shared lock/timer logic (Phase 8) — used both by the service's periodic
//! tick (`service.rs`) and defensively at the top of every native-messaging
//! handler that reads or writes lock state, so a freshly-launched host
//! process is never looking at stale data even if the tick hasn't run
//! since expiry (the two run as separate processes; see `db/mod.rs`).

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::db::lock;

/// How long after starting a lock it can still be undone. Short on purpose:
/// it exists to catch a slip (wrong duration, a stray click), not to be a way
/// out. A lock that has been extended can never be undone, and once the window
/// has passed there is no cancel at all.
pub const UNDO_WINDOW_SECONDS: i64 = 300;

/// Seconds of undo window left for a lock started at `started_at`, or 0 if it
/// has passed or the lock has been extended.
pub fn undo_seconds_left(started_at: &str, extended: bool, now: DateTime<Utc>) -> i64 {
    if extended {
        return 0;
    }
    let Ok(started) = DateTime::parse_from_rfc3339(started_at) else {
        return 0;
    };
    let elapsed = (now - started.with_timezone(&Utc)).num_seconds();
    // A start time in the future (clock moved back) gets no extra window.
    if elapsed < 0 {
        return 0;
    }
    (UNDO_WINDOW_SECONDS - elapsed).max(0)
}

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
    fn undo_window_counts_down_and_closes() {
        use chrono::Duration;
        let start = Utc::now();
        let s = start.to_rfc3339();
        assert_eq!(undo_seconds_left(&s, false, start), UNDO_WINDOW_SECONDS);
        assert_eq!(undo_seconds_left(&s, false, start + Duration::seconds(100)), UNDO_WINDOW_SECONDS - 100);
        assert_eq!(undo_seconds_left(&s, false, start + Duration::seconds(UNDO_WINDOW_SECONDS - 1)), 1);
        assert_eq!(undo_seconds_left(&s, false, start + Duration::seconds(UNDO_WINDOW_SECONDS)), 0);
        assert_eq!(undo_seconds_left(&s, false, start + Duration::days(30)), 0);
    }

    #[test]
    fn an_extended_lock_or_bad_data_gets_no_undo() {
        use chrono::Duration;
        let start = Utc::now();
        assert_eq!(undo_seconds_left(&start.to_rfc3339(), true, start), 0, "extended locks can't be undone");
        assert_eq!(undo_seconds_left("not a date", false, start), 0);
        assert_eq!(
            undo_seconds_left(&(start + Duration::hours(1)).to_rfc3339(), false, start),
            0,
            "a start time in the future must not grant a long window"
        );
    }

    #[test]
    fn cancel_ends_only_a_running_lock_and_records_it() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, false, Some(3600), Some("2999-01-01T00:00:00+00:00")).unwrap();
        assert!(lock::cancel(&conn, l.id).unwrap());
        let after = lock::get_by_id(&conn, l.id).unwrap().unwrap();
        assert_eq!(after.status, "expired");
        assert!(after.ended_at.is_some() && after.cancelled_at.is_some());
        assert!(lock::get_current(&conn).unwrap().is_none());
        assert!(!lock::cancel(&conn, l.id).unwrap(), "already ended: nothing to cancel");
    }

    #[test]
    fn cancel_works_on_a_forever_lock_too() {
        let conn = open_in_memory().unwrap();
        let l = lock::create(&conn, true, None, None).unwrap();
        assert!(lock::cancel(&conn, l.id).unwrap());
        assert!(lock::get_current(&conn).unwrap().is_none());
    }

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
