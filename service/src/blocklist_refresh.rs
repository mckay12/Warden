//! Remote domain-blocklist refresh (Phase 13). Runs on its own background
//! thread — network I/O must never delay the tick loop's lock-expiry /
//! registry-self-healing checks, so this polls independently rather than
//! living in `service.rs`'s 1-second loop.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::thread;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;

use crate::db::{self, blocked_domain, blocklist_source, service_config};
use crate::logging::log_line;

/// The Block List Project's maintained adult-content domain list — public,
/// actively maintained, MIT-licensed: exactly the kind of "known
/// adult/hentai domains, refreshed from a maintained remote list" source
/// the PRD (§7.1) calls for.
pub const SOURCE_URL: &str =
    "https://raw.githubusercontent.com/blocklistproject/Lists/master/porn.txt";

/// The real list has 900K+ entries as of this writing — several orders of
/// magnitude past what Chrome's `declarativeNetRequest` can hold as actual
/// browser rules (the dynamic rule ceiling is in the low thousands). This
/// cap is a real platform constraint, confirmed against the live source
/// before writing this, not an arbitrary round number — see
/// `service/README.md`'s Phase 13 findings.
pub const MAX_DOMAINS: usize = 2000;

const BUNDLED: &str = include_str!("bundled_blocklist.txt");

pub fn spawn() {
    thread::spawn(|| loop {
        match db::open() {
            Ok(mut conn) => {
                if let Err(e) = maybe_refresh(&mut conn) {
                    log_line(&format!("blocklist_refresh: {e}"));
                }
            }
            Err(e) => log_line(&format!("blocklist_refresh: db open failed: {e}")),
        }
        // Interval math only needs hour-level precision (default 24h) —
        // checking once a minute is more than enough headroom, and cheap.
        thread::sleep(StdDuration::from_secs(60));
    });
}

/// Checks whether a refresh is due (per `service_config`'s configured
/// interval) and, if so, performs one. Exposed separately from `spawn()` so
/// it can be driven directly — e.g. from a manual verification harness —
/// without waiting on the poll loop.
pub fn maybe_refresh(conn: &mut Connection) -> Result<(), String> {
    let source = blocklist_source::get_or_create(conn, SOURCE_URL).map_err(|e| e.to_string())?;
    let config = service_config::get(conn).map_err(|e| e.to_string())?;

    let due = match &source.last_refreshed_at {
        None => true,
        Some(last) => match DateTime::parse_from_rfc3339(last) {
            Ok(last) => {
                Utc::now() >= last + Duration::hours(config.blocklist_refresh_interval_hours)
            }
            Err(_) => true,
        },
    };
    // An install upgraded from before the DNS filter has a fresh DB refresh
    // timestamp but no full-list file yet; don't make it wait out the interval.
    let dns_list_missing = !crate::dns_filter::list_path().exists();
    if !due && !dns_list_missing {
        return Ok(());
    }

    match fetch(SOURCE_URL) {
        Ok(body) => {
            let hash = content_hash(&body);
            // The DNS filter has no rule-count ceiling, so it gets the full
            // list from a file rather than the 2000-row DB table. Rewritten
            // only when missing or the source changed.
            if source.version_hash.as_deref() != Some(hash.as_str())
                || !crate::dns_filter::list_path().exists()
            {
                write_dns_list(&body);
            }
            if source.version_hash.as_deref() == Some(hash.as_str()) {
                // Unchanged since last time — record the check, skip the
                // reload (ERD §2: the whole point of the version hash).
                blocklist_source::record_refresh(conn, source.id, &hash)
                    .map_err(|e| e.to_string())?;
                log_line("blocklist_refresh: fetched, unchanged, skipped reload.");
                return Ok(());
            }

            let domains = parse_hosts_format(&body, MAX_DOMAINS);
            let domain_count = domains.len();
            blocked_domain::replace_for_source(conn, source.id, &domains)
                .map_err(|e| e.to_string())?;
            blocklist_source::record_refresh(conn, source.id, &hash)
                .map_err(|e| e.to_string())?;
            log_line(&format!(
                "blocklist_refresh: reloaded {domain_count} domains from {SOURCE_URL}."
            ));
        }
        Err(e) => {
            log_line(&format!("blocklist_refresh: fetch failed ({e})."));
            // Offline resilience (PRD §7.1): if this source has never
            // successfully produced any domains, seed the bundled fallback
            // so blocking still works before the network is ever reached.
            // If we already have data from an earlier successful fetch,
            // leave it alone — last known good beats nothing.
            let existing =
                blocked_domain::list_for_source(conn, source.id).map_err(|e| e.to_string())?;
            if existing.is_empty() {
                let bundled = parse_plain_list(BUNDLED);
                let bundled_count = bundled.len();
                blocked_domain::replace_for_source(conn, source.id, &bundled)
                    .map_err(|e| e.to_string())?;
                log_line(&format!(
                    "blocklist_refresh: seeded {bundled_count} bundled fallback domains."
                ));
            } else {
                log_line("blocklist_refresh: keeping existing domains (last known good).");
            }
        }
    }
    Ok(())
}

fn fetch(url: &str) -> Result<String, String> {
    let mut response = ureq::get(url).call().map_err(|e| e.to_string())?;
    response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
}

/// Writes every domain in the source (no cap) for the DNS filter. Written to
/// a temp file and renamed so the filter's reload never sees a half-written
/// list. Best-effort: a failure here must not fail the refresh.
fn write_dns_list(body: &str) {
    let domains = parse_hosts_format(body, usize::MAX);
    let path = crate::dns_filter::list_path();
    let tmp = path.with_extension("txt.tmp");
    let result = std::fs::write(&tmp, domains.join("\n")).and_then(|_| std::fs::rename(&tmp, &path));
    match result {
        Ok(()) => log_line(&format!(
            "blocklist_refresh: wrote {} domains for the DNS filter.",
            domains.len()
        )),
        Err(e) => log_line(&format!("blocklist_refresh: DNS list write failed ({e}).")),
    }
}

/// Non-cryptographic — this is purely a "did the content change since last
/// time" check (ERD §2), not a security boundary, so `DefaultHasher`
/// avoids pulling in a dedicated hashing crate for no real benefit.
fn content_hash(s: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Parses `hosts`-format lines (`0.0.0.0 example.com`), skipping comments
/// (`#`) and blanks, deduping, capped at `max`.
fn parse_hosts_format(body: &str, max: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let _address = parts.next();
        let Some(domain) = parts.next() else {
            continue;
        };
        let domain = domain.to_lowercase();
        if seen.insert(domain.clone()) {
            out.push(domain);
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Parses the bundled fallback file: one plain domain per line.
fn parse_plain_list(body: &str) -> Vec<String> {
    body.lines()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hosts_format_skips_comments_and_dedupes() {
        let body = "# header comment\n\n0.0.0.0 a.com\n0.0.0.0 b.com\n0.0.0.0 a.com\n";
        let domains = parse_hosts_format(body, 100);
        assert_eq!(domains, vec!["a.com".to_string(), "b.com".to_string()]);
    }

    #[test]
    fn parse_hosts_format_respects_cap() {
        let body = "0.0.0.0 a.com\n0.0.0.0 b.com\n0.0.0.0 c.com\n";
        let domains = parse_hosts_format(body, 2);
        assert_eq!(domains.len(), 2);
    }

    #[test]
    fn parse_plain_list_reads_bundled_fallback() {
        let domains = parse_plain_list(BUNDLED);
        assert!(!domains.is_empty());
        assert!(domains.contains(&"pornhub.com".to_string()));
    }

    #[test]
    fn content_hash_is_stable_and_change_sensitive() {
        assert_eq!(content_hash("same"), content_hash("same"));
        assert_ne!(content_hash("a"), content_hash("b"));
    }
}
