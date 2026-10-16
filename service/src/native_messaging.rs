//! Chrome Native Messaging host. Chrome launches this exe fresh for each
//! connection (per `chrome.runtime.sendNativeMessage` / `connectNative`
//! call) with the extension's origin as argv[1] — see `main.rs`'s dispatch.
//! Each message on stdin is length-prefixed (4-byte little-endian u32) JSON;
//! each response on stdout is written the same way. The loop just runs
//! until Chrome closes the pipe (EOF on stdin).

use std::io::{self, Read, Write};

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use crate::db;
use crate::db::models::{EmergencyUnlockRequest, Lock};
use crate::emergency_unlock_logic;
use crate::lock_logic::maybe_expire;

/// Longest lock, or single extension, the service will accept: 100 years of
/// 365 days. Anything larger is rejected up front so date arithmetic can't
/// overflow. Keep in step with MAX_SECONDS in `extension/duration-picker.js`.
const MAX_LOCK_SECONDS: i64 = 100 * 365 * 24 * 60 * 60;

fn check_lock_seconds(seconds: i64) -> Result<i64, String> {
    if seconds > MAX_LOCK_SECONDS {
        Err(format!("a lock can be at most 100 years ({MAX_LOCK_SECONDS} seconds)"))
    } else {
        Ok(seconds)
    }
}

pub fn run() {
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        let mut len_buf = [0u8; 4];
        if stdin.read_exact(&mut len_buf).is_err() {
            break; // EOF: the extension side closed the port.
        }
        let len = u32::from_le_bytes(len_buf) as usize;

        let mut msg_buf = vec![0u8; len];
        if stdin.read_exact(&mut msg_buf).is_err() {
            break;
        }

        let response = match serde_json::from_slice::<Value>(&msg_buf) {
            Ok(request) => handle(&request),
            Err(e) => json!({ "ok": false, "error": format!("invalid JSON: {e}") }),
        };

        let response_bytes = serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec());
        let response_len = (response_bytes.len() as u32).to_le_bytes();

        if stdout.write_all(&response_len).is_err() {
            break;
        }
        if stdout.write_all(&response_bytes).is_err() {
            break;
        }
        if stdout.flush().is_err() {
            break;
        }
    }
}

/// Message protocol. Everything is wired up for real as of Phase 11.
///
/// Note what's deliberately absent: there is no `shorten_lock` message type
/// at all — a lock can only ever be extended. The one exception to "can't
/// be cancelled" is `cancel_lock`, a slip-catcher: it only works for
/// `lock_logic::UNDO_WINDOW_SECONDS` after a lock starts, never on a lock that
/// has been extended, and the service checks that itself (see
/// `handle_cancel_lock`). Sending a `shorten_lock` hits the `unknown message
/// type` branch below like any other made-up type would.
fn handle(request: &Value) -> Value {
    let msg_type = request.get("type").and_then(Value::as_str).unwrap_or("");

    match msg_type {
        "get_status" => handle_get_status(),
        "log_attempt" => handle_log_attempt(request),
        "get_attempts" => handle_get_attempts(request),
        "start_lock" => handle_start_lock(request),
        "extend_lock" => handle_extend_lock(request),
        "request_emergency_unlock" => handle_request_emergency_unlock(),
        "cancel_emergency_unlock" => handle_cancel_emergency_unlock(),
        "get_emergency_unlock_history" => handle_get_emergency_unlock_history(request),
        "get_dashboard_stats" => handle_get_dashboard_stats(),
        "get_blocklist_domains" => handle_get_blocklist_domains(),
        "cancel_lock" => handle_cancel_lock(),
        "request_exception" => handle_request_exception(request),
        "list_exceptions" => handle_list_exceptions(),
        "remove_exception" => handle_remove_exception(request),
        "" => json!({ "ok": false, "error": "missing 'type' field" }),
        other => json!({ "ok": false, "error": format!("unknown message type '{other}'") }),
    }
}

fn err(msg: impl Into<String>) -> Value {
    json!({ "ok": false, "error": msg.into() })
}

/// `undo_seconds_left` is worked out here (server clock) so the UI never has
/// to trust or compare clocks: > 0 means "Undo" is still offered.
fn lock_json(conn: &rusqlite::Connection, lock: &Lock) -> Value {
    let extended = db::lock_extension::list_for_lock(conn, lock.id)
        .map(|e| !e.is_empty())
        .unwrap_or(true); // if we can't tell, don't offer an undo
    json!({
        "status": lock.status,
        "is_forever": lock.is_forever,
        "expires_at": lock.expires_at,
        "undo_seconds_left": crate::lock_logic::undo_seconds_left(&lock.started_at, extended, Utc::now()),
    })
}

fn emergency_unlock_json(r: &EmergencyUnlockRequest) -> Value {
    json!({
        "id": r.id,
        "lock_id": r.lock_id,
        "requested_at": r.requested_at,
        "cooldown_ends_at": r.cooldown_ends_at,
        "status": r.status,
        "cancelled_at": r.cancelled_at,
        "applied_at": r.applied_at,
    })
}

fn handle_get_status() -> Value {
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };
    // Defensive: a fresh CLI-launched host process shouldn't report a
    // stale "active" lock just because the service's tick hasn't run
    // since it actually expired (or since a due emergency unlock hasn't
    // been applied yet).
    if let Err(e) = maybe_expire(&conn) {
        return err(format!("expiry check failed: {e}"));
    }
    if let Err(e) = emergency_unlock_logic::maybe_apply(&conn) {
        return err(format!("emergency unlock check failed: {e}"));
    }

    match db::lock::get_current(&conn) {
        Ok(Some(lock)) => {
            // Only a forever lock can have a meaningful pending request —
            // saves the extension a second round trip to render the
            // popup's forever view in one shot (spec §3.8).
            let pending = if lock.is_forever {
                db::emergency_unlock::get_pending(&conn)
                    .ok()
                    .flatten()
                    .map(|r| emergency_unlock_json(&r))
            } else {
                None
            };
            json!({ "ok": true, "lock": lock_json(&conn, &lock), "emergency_unlock": pending })
        }
        Ok(None) => json!({ "ok": true, "lock": null, "emergency_unlock": null }),
        Err(e) => err(format!("db query failed: {e}")),
    }
}

/// Requests an emergency unlock for the current lock — only valid while
/// it's `forever` (PRD §7.4: the escape hatch for that state specifically;
/// a timed lock already ends on its own). Fixed 7-day cooldown, not
/// user-configurable, per the PRD. Rejects a second request while one is
/// already pending (ERD §4 open question, resolved as one-at-a-time).
fn handle_request_emergency_unlock() -> Value {
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };
    if let Err(e) = maybe_expire(&conn) {
        return err(format!("expiry check failed: {e}"));
    }

    let current = match db::lock::get_current(&conn) {
        Ok(Some(l)) => l,
        Ok(None) => return err("no active lock"),
        Err(e) => return err(format!("db query failed: {e}")),
    };
    if !current.is_forever {
        return err("emergency unlock only applies to a forever lock");
    }

    match db::emergency_unlock::get_pending(&conn) {
        Ok(Some(_)) => return err("an emergency unlock request is already pending"),
        Ok(None) => {}
        Err(e) => return err(format!("db query failed: {e}")),
    }

    let cooldown_ends_at = (Utc::now() + Duration::days(7)).to_rfc3339();
    match db::emergency_unlock::create(&conn, current.id, &cooldown_ends_at) {
        Ok(request) => json!({ "ok": true, "request": emergency_unlock_json(&request) }),
        Err(e) => err(format!("db insert failed: {e}")),
    }
}

/// Cancels the currently pending request, if any — PRD §7.4: "can be
/// cancelled during the cooldown if the user changes their mind." Once
/// applied, there's nothing left to cancel; once cancelled, there's no
/// un-cancelling — a fresh `request_emergency_unlock` starts a new one.
fn handle_cancel_emergency_unlock() -> Value {
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };

    let pending = match db::emergency_unlock::get_pending(&conn) {
        Ok(Some(r)) => r,
        Ok(None) => return err("no pending emergency unlock request"),
        Err(e) => return err(format!("db query failed: {e}")),
    };

    match db::emergency_unlock::cancel(&conn, pending.id) {
        Ok(()) => json!({ "ok": true }),
        Err(e) => err(format!("db update failed: {e}")),
    }
}

/// Every emergency unlock request ever made, newest first, regardless of
/// outcome — PRD §7.4: "every emergency unlock request and its outcome is
/// recorded permanently... visible, not something you can quietly do and
/// forget." Phase 11 exposes the data; Phase 12 builds the dashboard
/// section that actually displays it (per the build plan's own note that
/// this can stub the format now and finish the view later).
fn handle_get_emergency_unlock_history(request: &Value) -> Value {
    let limit = request.get("limit").and_then(Value::as_i64).unwrap_or(30);

    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };

    match db::emergency_unlock::list_all(&conn, limit) {
        Ok(requests) => {
            let requests: Vec<Value> = requests.iter().map(emergency_unlock_json).collect();
            json!({ "ok": true, "requests": requests })
        }
        Err(e) => err(format!("db query failed: {e}")),
    }
}

/// Starts a new lock — timed (`duration_seconds`) or `forever: true`.
/// Rejects the request outright if a lock is already active/forever
/// (ERD §2: only one at a time, enforced here in application logic).
/// Undoes a lock that was *just* started (slip protection). Allowed only while
/// the undo window is open and the lock has never been extended; after that
/// there is no way to cancel or shorten a lock. Checked here, on the service's
/// own clock and data - the UI only decides whether to show the button.
fn handle_cancel_lock() -> Value {
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };
    if let Err(e) = maybe_expire(&conn) {
        return err(format!("expiry check failed: {e}"));
    }
    let lock = match db::lock::get_current(&conn) {
        Ok(Some(l)) => l,
        Ok(None) => return err("there is no active lock to undo"),
        Err(e) => return err(format!("db query failed: {e}")),
    };
    match db::lock_extension::list_for_lock(&conn, lock.id) {
        Ok(ext) if ext.is_empty() => {}
        Ok(_) => return err("a lock that has been extended can't be undone"),
        Err(e) => return err(format!("db query failed: {e}")),
    }
    if crate::lock_logic::undo_seconds_left(&lock.started_at, false, Utc::now()) <= 0 {
        return err(format!(
            "the {}-minute undo window has passed; a lock can't be cancelled now",
            crate::lock_logic::UNDO_WINDOW_SECONDS / 60
        ));
    }
    match db::lock::cancel(&conn, lock.id) {
        Ok(true) => json!({ "ok": true }),
        Ok(false) => err("the lock had already ended"),
        Err(e) => err(format!("db update failed: {e}")),
    }
}

/// JSON for one exception, with its state worked out against the clock:
/// "pending" (cooldown still running) or "active" (in force).
fn exception_json(e: &db::allow_exception::AllowException, now: DateTime<Utc>) -> Value {
    let effective = DateTime::parse_from_rfc3339(&e.effective_at).ok().map(|t| t.with_timezone(&Utc));
    let seconds_left = effective.map(|t| (t - now).num_seconds().max(0)).unwrap_or(0);
    json!({
        "id": e.id,
        "domain": e.domain,
        "requested_at": e.requested_at,
        "effective_at": e.effective_at,
        "state": crate::exceptions::state_of(&e.effective_at, now),
        "seconds_left": seconds_left,
    })
}

/// Asks for an exception to the blocklist for one domain (a false positive).
/// It only *starts a cooldown* - the domain stays blocked until it ends - so
/// this can't be used to unblock something on impulse. See `exceptions`.
fn handle_request_exception(request: &Value) -> Value {
    let Some(domain) = request.get("domain").and_then(Value::as_str) else {
        return err("request_exception requires a 'domain' string");
    };
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };
    let now = Utc::now();
    match crate::exceptions::request(&conn, domain, now) {
        Ok(e) => json!({
            "ok": true,
            "exception": exception_json(&e, now),
            "cooldown_hours": crate::exceptions::COOLDOWN_HOURS,
        }),
        Err(msg) => err(msg),
    }
}

fn handle_list_exceptions() -> Value {
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };
    match db::allow_exception::list_open(&conn) {
        Ok(rows) => {
            let now = Utc::now();
            json!({
                "ok": true,
                "exceptions": rows.iter().map(|e| exception_json(e, now)).collect::<Vec<_>>(),
                "cooldown_hours": crate::exceptions::COOLDOWN_HOURS,
            })
        }
        Err(e) => err(format!("db query failed: {e}")),
    }
}

/// Cancels a pending exception or removes one that is in force. Instant: it
/// can only make blocking stricter, so it needs no cooldown.
fn handle_remove_exception(request: &Value) -> Value {
    let Some(id) = request.get("id").and_then(Value::as_i64) else {
        return err("remove_exception requires an integer 'id'");
    };
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };
    match db::allow_exception::remove(&conn, id) {
        Ok(true) => json!({ "ok": true }),
        Ok(false) => err("no such open exception"),
        Err(e) => err(format!("db update failed: {e}")),
    }
}

fn handle_start_lock(request: &Value) -> Value {
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };
    if let Err(e) = maybe_expire(&conn) {
        return err(format!("expiry check failed: {e}"));
    }

    match db::lock::get_current(&conn) {
        Ok(Some(_)) => {
            return err("a lock is already active; only one lock can be active at a time")
        }
        Ok(None) => {}
        Err(e) => return err(format!("db query failed: {e}")),
    }

    let forever = request.get("forever").and_then(Value::as_bool).unwrap_or(false);

    let result = if forever {
        db::lock::create(&conn, true, None, None)
    } else {
        let duration = match request.get("duration_seconds").and_then(Value::as_i64) {
            Some(d) if d > 0 => d,
            _ => {
                return err(
                    "start_lock requires a positive integer 'duration_seconds', or 'forever': true",
                )
            }
        };
        if let Err(e) = check_lock_seconds(duration) {
            return err(e);
        }
        let expires_at = (Utc::now() + Duration::seconds(duration)).to_rfc3339();
        db::lock::create(&conn, false, Some(duration), Some(&expires_at))
    };

    match result {
        Ok(lock) => json!({ "ok": true, "lock": lock_json(&conn, &lock) }),
        Err(e) => err(format!("db insert failed: {e}")),
    }
}

/// Adds time to the current timed lock. Always allowed for a timed lock
/// (PRD §7.3) — but `added_seconds` must be positive, so this can't be used
/// to shorten one; that's the one "exposed message" a shortening attempt
/// could realistically hide behind, so it's checked explicitly rather than
/// just relying on there being no dedicated shorten message.
fn handle_extend_lock(request: &Value) -> Value {
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };
    if let Err(e) = maybe_expire(&conn) {
        return err(format!("expiry check failed: {e}"));
    }

    let current = match db::lock::get_current(&conn) {
        Ok(Some(l)) => l,
        Ok(None) => return err("no active lock to extend"),
        Err(e) => return err(format!("db query failed: {e}")),
    };
    if current.is_forever {
        return err("cannot extend a forever lock (it has no expiry)");
    }

    let added = match request.get("added_seconds").and_then(Value::as_i64) {
        Some(a) if a > 0 => a,
        _ => {
            return err(
                "extend_lock requires a positive integer 'added_seconds' — shortening is not permitted",
            )
        }
    };

    let Some(expires_at) = current.expires_at.as_deref() else {
        return err("lock has no expiry to extend (unexpected state)");
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(expires_at) else {
        return err("could not parse current expiry");
    };
    if let Err(e) = check_lock_seconds(added) {
        return err(e);
    }
    let new_expiry = parsed + Duration::seconds(added);
    // The lock as a whole is capped too, not just each extension.
    if (new_expiry.with_timezone(&Utc) - Utc::now()).num_seconds() > MAX_LOCK_SECONDS {
        return err("extending would make the lock longer than 100 years");
    }
    let new_expires_at = new_expiry.to_rfc3339();

    if let Err(e) = db::lock::update_expires_at(&conn, current.id, &new_expires_at) {
        return err(format!("db update failed: {e}"));
    }
    if let Err(e) = db::lock_extension::create(&conn, current.id, added) {
        return err(format!("db insert failed: {e}"));
    }

    match db::lock::get_by_id(&conn, current.id) {
        Ok(Some(lock)) => json!({ "ok": true, "lock": lock_json(&conn, &lock) }),
        Ok(None) => err("lock disappeared after update (unexpected)"),
        Err(e) => err(format!("db query failed: {e}")),
    }
}

/// Logs one blocked navigation — the extension's replacement for the Phase 3
/// `chrome.storage.local` log (Phase 7 cutover). Expects `match_type`
/// (`"blocklist"` | `"keyword"`), `matched_value`, and `domain`, all strings.
fn handle_log_attempt(request: &Value) -> Value {
    let match_type = request.get("match_type").and_then(Value::as_str);
    let matched_value = request.get("matched_value").and_then(Value::as_str);
    let domain = request.get("domain").and_then(Value::as_str);

    let (match_type, matched_value, domain) = match (match_type, matched_value, domain) {
        (Some(mt), Some(mv), Some(d)) => (mt, mv, d),
        _ => {
            return json!({
                "ok": false,
                "error": "log_attempt requires string fields: match_type, matched_value, domain",
            })
        }
    };

    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": format!("db open failed: {e}") }),
    };

    match db::blocked_attempt::create(&conn, match_type, matched_value, domain) {
        Ok(attempt) => json!({ "ok": true, "id": attempt.id }),
        Err(e) => json!({ "ok": false, "error": format!("db insert failed: {e}") }),
    }
}

/// Recent attempts, newest first — backs both the popup's short list (Phase
/// 7) and the dashboard's date-windowed view (Phase 12). Optional `limit`
/// field, default 30, used as-is for the plain "last N" case; if a `days`
/// field is also present, switches to a date-windowed query instead (the
/// dashboard's "last 30 days" default per the ERD's open question) with
/// `limit` acting as a cap rather than the primary constraint.
fn handle_get_attempts(request: &Value) -> Value {
    let limit = request.get("limit").and_then(Value::as_i64).unwrap_or(30);
    let days = request.get("days").and_then(Value::as_i64);

    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": format!("db open failed: {e}") }),
    };

    let result = match days {
        Some(days) if days > 0 => {
            let since = (Utc::now() - Duration::days(days)).to_rfc3339();
            db::blocked_attempt::list_since(&conn, &since, limit.max(1000))
        }
        _ => db::blocked_attempt::list_recent(&conn, limit),
    };

    match result {
        Ok(attempts) => {
            let attempts: Vec<Value> = attempts
                .into_iter()
                .map(|a| {
                    json!({
                        "occurred_at": a.occurred_at,
                        "match_type": a.match_type,
                        "matched_value": a.matched_value,
                        "domain": a.domain,
                    })
                })
                .collect();
            json!({ "ok": true, "attempts": attempts })
        }
        Err(e) => json!({ "ok": false, "error": format!("db query failed: {e}") }),
    }
}

/// Streak + this-week/last-week attempt counts for the dashboard sidebar
/// (Phase 12) — see `stats.rs` for how each is computed.
fn handle_get_dashboard_stats() -> Value {
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };

    let streak = match crate::stats::streak_days(&conn) {
        Ok(s) => s,
        Err(e) => return err(format!("db query failed: {e}")),
    };
    let (this_week, last_week) = match crate::stats::week_counts(&conn) {
        Ok(w) => w,
        Err(e) => return err(format!("db query failed: {e}")),
    };

    json!({ "ok": true, "streak_days": streak, "this_week": this_week, "last_week": last_week })
}

/// The full current domain blocklist, across every active source — the
/// extension's replacement for the Phase 1 static `rules/blocklist.json`
/// (Phase 13). Deduped and sorted for a stable, diffable response.
fn handle_get_blocklist_domains() -> Value {
    let conn = match db::open() {
        Ok(c) => c,
        Err(e) => return err(format!("db open failed: {e}")),
    };

    let sources = match db::blocklist_source::list_active(&conn) {
        Ok(s) => s,
        Err(e) => return err(format!("db query failed: {e}")),
    };

    let mut domains: Vec<String> = Vec::new();
    for source in sources {
        match db::blocked_domain::list_for_source(&conn, source.id) {
            Ok(rows) => domains.extend(rows.into_iter().map(|d| d.domain)),
            Err(e) => return err(format!("db query failed: {e}")),
        }
    }
    domains.sort();
    domains.dedup();

    json!({ "ok": true, "domains": domains })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_length_cap_is_one_hundred_years() {
        assert_eq!(MAX_LOCK_SECONDS, 3_153_600_000);
        assert_eq!(check_lock_seconds(1), Ok(1));
        assert_eq!(check_lock_seconds(MAX_LOCK_SECONDS), Ok(MAX_LOCK_SECONDS));
        assert!(check_lock_seconds(MAX_LOCK_SECONDS + 1).is_err());
        assert!(check_lock_seconds(i64::MAX).is_err(), "huge values must not reach date math");
    }
}
