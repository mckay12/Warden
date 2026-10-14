//! Dashboard stats (Phase 12) — streak and week-over-week attempt counts.
//! Both computed at read time from `blocked_attempt`, per the ERD's own
//! call: "derived stats... computed with a query... rather than stored in
//! a separate materialized table."

use chrono::{Duration, Utc};
use rusqlite::Connection;

use crate::db::blocked_attempt;

/// Consecutive clean days counting back from today (inclusive) — PRD §7.5:
/// "consecutive days with zero blocked attempts." Today counts as soon as
/// it has zero attempts so far — that's genuinely true right now, even
/// though it could still change later today; this isn't decoration, it's
/// just what the data says at read time. Day boundaries are UTC (matching
/// how `occurred_at` is stored and how SQLite's `date()` normalizes it),
/// not the viewer's local timezone — a known simplification, not something
/// either the PRD or ERD specified either way.
pub fn streak_days(conn: &Connection) -> rusqlite::Result<i64> {
    let dirty_days = blocked_attempt::distinct_days(conn)?;

    if dirty_days.is_empty() {
        // No attempt has ever been logged. Walking backward with no dirty
        // day to stop at would run away indefinitely (there's no separate
        // "tracking started" timestamp — the ERD's own design note says
        // streaks are purely derived from `blocked_attempt`, no
        // materialized table), so this would otherwise just invent however
        // much history it felt like claiming. Report what's actually known
        // — today, so far, is clean — rather than guess further back.
        return Ok(1);
    }

    let mut streak: i64 = 0;
    let mut day = Utc::now().date_naive();
    loop {
        if dirty_days.contains(&day.format("%Y-%m-%d").to_string()) {
            break;
        }
        streak += 1;
        day = match day.pred_opt() {
            Some(d) => d,
            None => break,
        };
        if streak > 3650 {
            break; // sanity cap — 10 years of history is enough either way
        }
    }
    Ok(streak)
}

/// `(this_week, last_week)` attempt counts — rolling 7-day windows (days
/// 0-6 and 7-13 back from now), not calendar-aligned. Simpler than picking
/// a week-start convention, and matches how the UI/UX spec's mockup already
/// framed it ("This week" / "Last week", not "This calendar week").
pub fn week_counts(conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    let now = Utc::now();
    let since_7d = (now - Duration::days(7)).to_rfc3339();
    let since_14d = (now - Duration::days(14)).to_rfc3339();

    let this_week = blocked_attempt::count_since(conn, &since_7d)?;
    let last_14_days = blocked_attempt::count_since(conn, &since_14d)?;
    Ok((this_week, last_14_days - this_week))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{blocked_attempt, open_in_memory};

    #[test]
    fn streak_is_zero_the_moment_something_is_blocked_today() {
        let conn = open_in_memory().unwrap();
        blocked_attempt::create(&conn, "blocklist", "x.com", "x.com").unwrap();
        assert_eq!(streak_days(&conn).unwrap(), 0);
    }

    #[test]
    fn streak_counts_today_when_nothing_blocked_yet() {
        let conn = open_in_memory().unwrap();
        assert_eq!(streak_days(&conn).unwrap(), 1);
    }

    #[test]
    fn streak_stops_exactly_at_a_dirty_day_several_days_back() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO blocked_attempt (occurred_at, match_type, matched_value, domain) VALUES (?1, 'blocklist', 'x.com', 'x.com')",
            [(Utc::now() - Duration::days(4)).to_rfc3339()],
        )
        .unwrap();

        // Clean today, yesterday, 2 and 3 days ago; dirty 4 days ago —
        // streak should be exactly 4, not run past the dirty day.
        assert_eq!(streak_days(&conn).unwrap(), 4);
    }

    #[test]
    fn week_counts_split_correctly_across_the_7_day_boundary() {
        let conn = open_in_memory().unwrap();
        blocked_attempt::create(&conn, "blocklist", "x.com", "x.com").unwrap();

        // Backdate one attempt into "last week" (9 days ago) directly via
        // SQL — the create() helper always stamps "now".
        conn.execute(
            "INSERT INTO blocked_attempt (occurred_at, match_type, matched_value, domain) VALUES (?1, 'blocklist', 'y.com', 'y.com')",
            [(Utc::now() - Duration::days(9)).to_rfc3339()],
        )
        .unwrap();

        let (this_week, last_week) = week_counts(&conn).unwrap();
        assert_eq!(this_week, 1);
        assert_eq!(last_week, 1);
    }
}
