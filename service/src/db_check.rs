//! Manual CRUD verification against the real on-disk database. See the
//! `db-check` arm in `main.rs` for why this exists alongside `cargo test`.

use crate::db::{
    blocked_attempt, blocked_domain, blocklist_source, emergency_unlock, keyword_rule, lock,
    lock_extension, service_config,
};

pub fn run() -> Result<(), String> {
    println!("Opening DB at {}", crate::db::db_path().display());
    let mut conn = crate::db::open().map_err(|e| format!("open failed: {e}"))?;
    println!("DB opened (created if missing).");

    // service_config: get-or-create the singleton row.
    let config = service_config::get(&conn).map_err(|e| e.to_string())?;
    println!(
        "service_config: refresh_interval_hours={} merge_mode={}",
        config.blocklist_refresh_interval_hours, config.registry_policy_merge_mode
    );

    // lock + lock_extension: start a timed lock, extend it, end it.
    let started = lock::create(&conn, false, Some(3600), Some("2026-08-29T00:00:00+00:00"))
        .map_err(|e| e.to_string())?;
    lock_extension::create(&conn, started.id, 600).map_err(|e| e.to_string())?;
    lock::update_expires_at(&conn, started.id, "2026-08-29T01:10:00+00:00")
        .map_err(|e| e.to_string())?;
    lock::end(&conn, started.id).map_err(|e| e.to_string())?;
    println!(
        "lock: created id={}, 1 extension recorded, ended.",
        started.id
    );

    // A forever lock + emergency unlock request round trip.
    let forever = lock::create(&conn, true, None, None).map_err(|e| e.to_string())?;
    let request = emergency_unlock::create(&conn, forever.id, "2026-09-05T00:00:00+00:00")
        .map_err(|e| e.to_string())?;
    emergency_unlock::cancel(&conn, request.id).map_err(|e| e.to_string())?;
    println!(
        "lock: forever id={}, emergency_unlock_request id={} created then cancelled.",
        forever.id, request.id
    );

    // blocked_attempt.
    blocked_attempt::create(&conn, "blocklist", "pornhub.com", "pornhub.com")
        .map_err(|e| e.to_string())?;
    blocked_attempt::create(&conn, "keyword", "porn", "example.com").map_err(|e| e.to_string())?;
    let recent = blocked_attempt::list_recent(&conn, 5).map_err(|e| e.to_string())?;
    println!("blocked_attempt: {} row(s) logged.", recent.len());

    // blocklist_source + blocked_domain (wipe-and-reload).
    let source = blocklist_source::create(&conn, "https://example.com/blocklist.txt")
        .map_err(|e| e.to_string())?;
    blocked_domain::replace_for_source(
        &mut conn,
        source.id,
        &["pornhub.com".to_string(), "xvideos.com".to_string()],
    )
    .map_err(|e| e.to_string())?;
    let domains = blocked_domain::list_for_source(&conn, source.id).map_err(|e| e.to_string())?;
    println!(
        "blocklist_source id={}, blocked_domain: {} row(s).",
        source.id,
        domains.len()
    );

    // keyword_rule.
    keyword_rule::create(&conn, "porn", "keyword", false).map_err(|e| e.to_string())?;
    let active_rules = keyword_rule::list_active(&conn).map_err(|e| e.to_string())?;
    println!("keyword_rule: {} active rule(s).", active_rules.len());

    println!("db-check: all CRUD paths exercised successfully.");
    Ok(())
}
