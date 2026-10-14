//! Network-layer secure-DNS lockout. `browser_policy.rs` switches DNS-over-
//! HTTPS off in the browsers that honour machine policy; this covers the ones
//! that don't (Opera, Vivaldi, Tor Browser, ...) by blocking outbound
//! connections to the well-known public secure-DNS servers while a lock is
//! active, using Windows Firewall block rules:
//!
//! - TCP and UDP 443 (DoH, and DoH over QUIC) to a fixed list of public
//!   resolver addresses;
//! - TCP and UDP 853 (DNS-over-TLS / DNS-over-QUIC) to anywhere.
//!
//! Plain DNS (port 53) is deliberately left alone: the DNS filter itself
//! forwards upstream over port 53, and a block rule would override any allow
//! rule for it. Browsers that use plain DNS go through the system resolver,
//! which is the filter.
//!
//! This is a best-effort list, not a guarantee: a resolver not on it, or a
//! DoH server behind an ordinary web hostname, gets through. The DNS filter's
//! own refusal to resolve known DoH hostnames (`dns_filter::DOH_HOSTS`) is the
//! complementary layer for the hostname-based case.
//!
//! Rules are created with `netsh advfirewall` (no COM plumbing). They are
//! re-checked every `CHECK_EVERY_TICKS` ticks while a lock is active — one
//! process spawn per rule per check, so this is deliberately slower than the
//! registry/DNS self-healing — and deleted when the lock ends. If the
//! Windows Firewall service itself is switched off the rules are inert; that
//! is a known limit.

use std::os::windows::process::CommandExt;
use std::process::Command;

use rusqlite::Connection;

use crate::db;
use crate::logging::log_line;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CHECK_EVERY_TICKS: u64 = 10;

/// Public DoH/DoT resolver addresses (IPv4 and IPv6) of the large providers.
const DOH_IPS: &[&str] = &[
    // Cloudflare
    "1.1.1.1", "1.0.0.1", "1.1.1.2", "1.0.0.2", "1.1.1.3", "1.0.0.3",
    "2606:4700:4700::1111", "2606:4700:4700::1001", "2606:4700:4700::1112",
    "2606:4700:4700::1002", "2606:4700:4700::1113", "2606:4700:4700::1003",
    // Google
    "8.8.8.8", "8.8.4.4", "2001:4860:4860::8888", "2001:4860:4860::8844",
    // Quad9
    "9.9.9.9", "149.112.112.112", "9.9.9.10", "149.112.112.10", "9.9.9.11", "149.112.112.11",
    "2620:fe::fe", "2620:fe::9", "2620:fe::10", "2620:fe::fe:10", "2620:fe::11", "2620:fe::fe:11",
    // OpenDNS / Cisco
    "208.67.222.222", "208.67.220.220", "208.67.222.123", "208.67.220.123",
    "2620:119:35::35", "2620:119:53::53",
    // AdGuard
    "94.140.14.14", "94.140.15.15", "94.140.14.140", "94.140.14.141", "94.140.14.15", "94.140.15.16",
    "2a10:50c0::ad1:ff", "2a10:50c0::ad2:ff",
    // CleanBrowsing
    "185.228.168.9", "185.228.169.9", "185.228.168.168", "185.228.169.168", "185.228.168.10", "185.228.169.11",
    "2a0d:2a00:1::2", "2a0d:2a00:2::2",
    // ControlD
    "76.76.2.0", "76.76.10.0", "76.76.2.2", "76.76.10.2", "76.76.2.11", "76.76.10.11",
    "2606:1a40::", "2606:1a40:1::",
    // NextDNS anycast
    "45.90.28.0", "45.90.30.0", "2a07:a8c0::", "2a07:a8c1::",
    // Mullvad
    "194.242.2.2", "194.242.2.3", "194.242.2.4", "194.242.2.5", "194.242.2.6", "194.242.2.9",
    "2a07:e340::2", "2a07:e340::3", "2a07:e340::4",
    // Others commonly offered in browser dropdowns
    "8.26.56.26", "8.20.247.20", // Comodo
    "77.88.8.8", "77.88.8.1",    // Yandex
    "223.5.5.5", "223.6.6.6",    // AliDNS
    "119.29.29.29",              // DNSPod
    "180.76.76.76",              // Baidu
];

struct Rule {
    name: &'static str,
    protocol: &'static str,
    port: &'static str,
    /// `None` = any remote address.
    ips: Option<&'static [&'static str]>,
}

const RULES: &[Rule] = &[
    Rule { name: "Warden DoH block TCP 443", protocol: "TCP", port: "443", ips: Some(DOH_IPS) },
    Rule { name: "Warden DoH block UDP 443", protocol: "UDP", port: "443", ips: Some(DOH_IPS) },
    Rule { name: "Warden DoT block TCP 853", protocol: "TCP", port: "853", ips: None },
    Rule { name: "Warden DoT block UDP 853", protocol: "UDP", port: "853", ips: None },
];

fn netsh(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("netsh.exe")
        .args(["advfirewall", "firewall"])
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
}

fn rule_exists(name: &str) -> bool {
    // Exit code 0 when found, 1 when not — independent of display language.
    netsh(&["show", "rule", &format!("name={name}")])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn add_rule(r: &Rule) -> Result<(), String> {
    let name = format!("name={}", r.name);
    let proto = format!("protocol={}", r.protocol);
    let port = format!("remoteport={}", r.port);
    let ips = r.ips.map(|l| format!("remoteip={}", l.join(",")));
    let mut args: Vec<&str> = vec![
        "add", "rule", &name, "dir=out", "action=block", &proto, &port, "enable=yes", "profile=any",
    ];
    if let Some(ips) = &ips {
        args.push(ips);
    }
    let out = netsh(&args).map_err(|e| format!("could not run netsh: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "netsh add rule '{}' failed: {} {}",
            r.name,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Creates any missing rule; returns how many had to be (re)created.
pub fn apply() -> Result<usize, String> {
    let mut created = 0;
    for r in RULES {
        if !rule_exists(r.name) {
            add_rule(r)?;
            created += 1;
        }
    }
    Ok(created)
}

/// Removes every Warden rule that exists; returns how many were deleted.
pub fn remove() -> Result<usize, String> {
    let mut removed = 0;
    for r in RULES {
        if rule_exists(r.name) {
            let out = netsh(&["delete", "rule", &format!("name={}", r.name)])
                .map_err(|e| format!("could not run netsh: {e}"))?;
            if !out.status.success() {
                return Err(format!("netsh delete rule '{}' failed", r.name));
            }
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn status() -> String {
    let present: Vec<&str> = RULES.iter().filter(|r| rule_exists(r.name)).map(|r| r.name).collect();
    format!("firewall rules present: {}/{}\n", present.len(), RULES.len())
}

/// Call every service tick.
pub struct DohFirewall {
    applied: bool,
}

impl DohFirewall {
    pub fn new() -> Self {
        DohFirewall { applied: false }
    }

    pub fn sync(&mut self, conn: &Connection, tick: u64) {
        let lock_active = db::lock::get_current(conn).map(|l| l.is_some()).unwrap_or(false);
        if lock_active {
            // First tick of a lock (or after a restart mid-lock) acts at once;
            // afterwards re-check on a slower cadence.
            if self.applied && tick % CHECK_EVERY_TICKS != 0 {
                return;
            }
            match apply() {
                Ok(0) => self.applied = true,
                Ok(n) => {
                    log_line(&format!("firewall: (re)created {n} secure-DNS block rule(s)."));
                    self.applied = true;
                }
                Err(e) => log_line(&format!("firewall: apply failed: {e}")),
            }
        } else if self.applied || tick % (CHECK_EVERY_TICKS * 6) == 1 {
            // Also sweeps once a minute while idle, so rules left behind by a
            // crash mid-lock can't outlive the lock indefinitely.
            match remove() {
                Ok(0) => self.applied = false,
                Ok(n) => {
                    log_line(&format!("firewall: removed {n} secure-DNS block rule(s)."));
                    self.applied = false;
                }
                Err(e) => log_line(&format!("firewall: remove failed: {e}")),
            }
        }
    }

    /// Clean service stop with no lock: never leave block rules behind.
    pub fn shutdown(&mut self) {
        if let Err(e) = remove() {
            log_line(&format!("firewall: remove on shutdown failed: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_address_parses() {
        for ip in DOH_IPS {
            assert!(ip.parse::<std::net::IpAddr>().is_ok(), "bad address in DOH_IPS: {ip}");
        }
    }

    #[test]
    fn list_has_no_duplicates_and_no_loopback() {
        let mut seen = std::collections::HashSet::new();
        for ip in DOH_IPS {
            assert!(seen.insert(*ip), "duplicate {ip}");
            assert!(!ip.parse::<std::net::IpAddr>().unwrap().is_loopback());
        }
    }

    #[test]
    fn never_blocks_plain_dns_port() {
        // Blocking 53 would cut off the filter's own upstream forwarding.
        for r in RULES {
            assert_ne!(r.port, "53");
        }
    }

    #[test]
    fn rule_names_are_unique() {
        let names: std::collections::HashSet<_> = RULES.iter().map(|r| r.name).collect();
        assert_eq!(names.len(), RULES.len());
    }
}
