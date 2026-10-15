//! Delayed exceptions to the blocklist — the answer to false positives.
//!
//! The downloaded adult-domain list is community-compiled and has mistakes
//! (e.g. a directory of anime sites). Letting the person at the keyboard
//! unblock a site *instantly* would defeat a commitment device, so instead:
//!
//! - **Asking** for an exception only starts a cooldown (`COOLDOWN_HOURS`).
//!   Nothing changes until it has passed, so it can't be used on impulse.
//! - **Removing** an exception (or cancelling a pending one) is instant:
//!   that only ever makes blocking stricter.
//! - Exceptions work during a lock and need no reinstall — the DNS filter
//!   re-reads the in-force set every few seconds (see `dns_filter`).
//!
//! An exception covers the domain and its subdomains, but a *more specific*
//! blocklist entry still wins (see `dns_filter::blocked_entry`), so excepting
//! `tumblr.com` doesn't unblock a listed `bad.tumblr.com`.

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;

use crate::db::allow_exception::{self, AllowException};
use crate::dns_filter::TWO_LEVEL_SUFFIXES;

/// How long an exception request waits before it takes effect.
pub const COOLDOWN_HOURS: i64 = 24;
/// Pending + in-force exceptions allowed at once, so "exceptions" can't be
/// used to mass-unblock a category.
pub const MAX_OPEN: i64 = 25;

/// Cleans up what a person might paste or type (`https://www.Example.com/x?y`)
/// into a bare lowercase registrable-looking domain, or says why it can't be
/// used. Rejects IPs, single labels, and suffixes that are far too broad
/// (`com`, `co.uk`).
pub fn normalize_domain(input: &str) -> Result<String, String> {
    let mut s = input.trim().to_ascii_lowercase();
    if let Some((_, rest)) = s.split_once("://") {
        s = rest.to_string();
    }
    if let Some(i) = s.find(|c| c == '/' || c == '?' || c == '#') {
        s.truncate(i);
    }
    if let Some((_, host)) = s.rsplit_once('@') {
        s = host.to_string();
    }
    if let Some((host, port)) = s.rsplit_once(':') {
        if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            s = host.to_string();
        }
    }
    let s = s.trim_end_matches('.').trim_start_matches("www.").to_string();

    if s.is_empty() {
        return Err("Enter a domain, like example.com.".to_string());
    }
    if s.len() > 253 {
        return Err("That domain name is too long.".to_string());
    }
    let labels: Vec<&str> = s.split('.').collect();
    if labels.len() < 2 {
        return Err("Enter a full domain, like example.com.".to_string());
    }
    for label in &labels {
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !ok {
            return Err(format!("'{s}' isn't a valid domain name."));
        }
    }
    if labels.iter().all(|l| l.chars().all(|c| c.is_ascii_digit())) {
        return Err("Enter a domain name, not an IP address.".to_string());
    }
    if labels.len() == 2 && TWO_LEVEL_SUFFIXES.contains(&s.as_str()) {
        return Err(format!("'{s}' is too broad — enter the specific site, like example.{s}."));
    }
    Ok(s)
}

/// "pending" until `effective_at`, then "active".
pub fn state_of(effective_at: &str, now: DateTime<Utc>) -> &'static str {
    match DateTime::parse_from_rfc3339(effective_at) {
        Ok(t) if t.with_timezone(&Utc) <= now => "active",
        _ => "pending",
    }
}

/// Records a request. Validates the domain, refuses duplicates and caps how
/// many can be open at once. The new row is *pending* for `COOLDOWN_HOURS`.
pub fn request(conn: &Connection, input: &str, now: DateTime<Utc>) -> Result<AllowException, String> {
    let domain = normalize_domain(input)?;
    if allow_exception::get_open_by_domain(conn, &domain).map_err(|e| e.to_string())?.is_some() {
        return Err(format!("There is already an exception for {domain}."));
    }
    if allow_exception::count_open(conn).map_err(|e| e.to_string())? >= MAX_OPEN {
        return Err(format!(
            "You already have {MAX_OPEN} exceptions open. Remove one before adding another."
        ));
    }
    let effective_at = (now + Duration::hours(COOLDOWN_HOURS)).to_rfc3339();
    allow_exception::create(conn, &domain, &effective_at).map_err(|e| e.to_string())
}

/// Domains whose exception is currently in force (cooldown passed, not removed).
pub fn in_force(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<Vec<String>> {
    Ok(allow_exception::list_open(conn)?
        .into_iter()
        .filter(|e| state_of(&e.effective_at, now) == "active")
        .map(|e| e.domain)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn normalizes_what_people_paste() {
        for (input, want) in [
            ("wotaku.wiki", "wotaku.wiki"),
            ("  WOTAKU.wiki  ", "wotaku.wiki"),
            ("https://www.wotaku.wiki/", "wotaku.wiki"),
            ("http://wotaku.wiki:8080/some/path?q=1#frag", "wotaku.wiki"),
            ("user@mail.example.com", "mail.example.com"),
            ("example.com.", "example.com"),
            ("www.example.co.uk", "example.co.uk"),
            ("xn--bcher-kva.example", "xn--bcher-kva.example"),
        ] {
            assert_eq!(normalize_domain(input).as_deref(), Ok(want), "{input}");
        }
    }

    #[test]
    fn rejects_things_that_are_not_a_specific_domain() {
        for bad in ["", "   ", "com", "localhost", "co.uk", "192.168.1.1", "-bad.example.com",
                    "exa mple.com", "exa_mple.com", "a..b.com", "https://", "/path/only"] {
            assert!(normalize_domain(bad).is_err(), "should reject {bad:?}");
        }
        assert!(normalize_domain(&format!("{}.com", "a".repeat(64))).is_err(), "label over 63 chars");
        assert!(normalize_domain(&format!("{}.com", "a.".repeat(130))).is_err(), "name over 253 chars");
    }

    #[test]
    fn a_new_request_is_pending_for_the_full_cooldown() {
        let conn = open_in_memory().unwrap();
        let now = Utc::now();
        let e = request(&conn, "https://www.wotaku.wiki/", now).unwrap();
        assert_eq!(e.domain, "wotaku.wiki");
        assert_eq!(state_of(&e.effective_at, now), "pending");
        assert_eq!(state_of(&e.effective_at, now + Duration::hours(COOLDOWN_HOURS) - Duration::seconds(1)), "pending");
        assert_eq!(state_of(&e.effective_at, now + Duration::hours(COOLDOWN_HOURS)), "active");
        assert!(in_force(&conn, now).unwrap().is_empty(), "nothing is in force until the cooldown ends");
        assert_eq!(
            in_force(&conn, now + Duration::hours(COOLDOWN_HOURS) + Duration::minutes(1)).unwrap(),
            vec!["wotaku.wiki".to_string()]
        );
    }

    #[test]
    fn removing_is_instant_and_stops_it_being_in_force() {
        let conn = open_in_memory().unwrap();
        let now = Utc::now();
        let e = request(&conn, "example.org", now).unwrap();
        let later = now + Duration::hours(COOLDOWN_HOURS + 1);
        assert_eq!(in_force(&conn, later).unwrap().len(), 1);
        assert!(allow_exception::remove(&conn, e.id).unwrap());
        assert!(in_force(&conn, later).unwrap().is_empty());
    }

    #[test]
    fn refuses_duplicates_and_enforces_the_open_cap() {
        let conn = open_in_memory().unwrap();
        let now = Utc::now();
        request(&conn, "example.org", now).unwrap();
        assert!(request(&conn, "https://www.example.org/x", now).unwrap_err().contains("already an exception"));
        for i in 1..MAX_OPEN {
            request(&conn, &format!("site{i}.example.org"), now).unwrap();
        }
        assert_eq!(allow_exception::count_open(&conn).unwrap(), MAX_OPEN);
        assert!(request(&conn, "one-too-many.example.org", now).unwrap_err().contains("exceptions open"));
    }
}
