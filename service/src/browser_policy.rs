//! Browser secure-DNS lockout. A browser with DNS-over-HTTPS turned on never
//! asks the system resolver, so it would walk straight past the DNS filter
//! (`dns_filter.rs` / `dns_enforce.rs`). While a lock is active this forces
//! DoH off in every browser that honours machine policy:
//!
//! - Chrome, Edge, Brave: `DnsOverHttpsMode = "off"` (also greys out the
//!   "Use secure DNS" setting, so it can't be flipped back by hand).
//! - Brave: `TorDisabled = 1` and `BraveVPNDisabled = 1` (Tor windows and the
//!   built-in VPN both carry DNS past the system resolver).
//! - Firefox: `DNSOverHTTPS\Enabled = 0` with `Locked = 1`.
//!
//! Same discipline as `registry_policy.rs` and `dns_enforce.rs`: record each
//! value's previous state before the first write, re-assert every tick for
//! as long as the lock lasts, restore from the record (not by "undoing").
//!
//! Not covered: browsers that don't read these policy keys (Opera, Vivaldi,
//! Tor Browser, LibreWolf, ...). The filter also answers Firefox's DoH
//! "canary" domain so Firefox-family browsers default DoH off on their own,
//! but that's a hint, not enforcement.

use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::{json, Value};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_LOCAL_MACHINE, REG_DWORD,
    REG_SZ, RRF_RT_ANY,
};

use crate::db;
use crate::logging::log_line;

#[derive(Clone, Debug, PartialEq)]
enum Data {
    Sz(String),
    Dword(u32),
}

/// The value we want, in a form usable in a `const` table.
#[derive(Clone, Copy)]
enum Want {
    Sz(&'static str),
    Dword(u32),
}

impl Want {
    fn data(self) -> Data {
        match self {
            Want::Sz(s) => Data::Sz(s.to_string()),
            Want::Dword(v) => Data::Dword(v),
        }
    }
}

struct Policy {
    key: &'static str,
    name: &'static str,
    value: Want,
}

const POLICIES: &[Policy] = &[
    Policy { key: r"SOFTWARE\Policies\Google\Chrome", name: "DnsOverHttpsMode", value: Want::Sz("off") },
    Policy { key: r"SOFTWARE\Policies\Microsoft\Edge", name: "DnsOverHttpsMode", value: Want::Sz("off") },
    Policy { key: r"SOFTWARE\Policies\BraveSoftware\Brave", name: "DnsOverHttpsMode", value: Want::Sz("off") },
    // Brave's "Private window with Tor" resolves names inside the Tor network,
    // so those lookups never reach the system resolver. Off while locked.
    Policy { key: r"SOFTWARE\Policies\BraveSoftware\Brave", name: "TorDisabled", value: Want::Dword(1) },
    // Brave's built-in VPN tunnels all traffic, DNS included, past the filter.
    Policy { key: r"SOFTWARE\Policies\BraveSoftware\Brave", name: "BraveVPNDisabled", value: Want::Dword(1) },
    Policy { key: r"SOFTWARE\Policies\Mozilla\Firefox\DNSOverHTTPS", name: "Enabled", value: Want::Dword(0) },
    Policy { key: r"SOFTWARE\Policies\Mozilla\Firefox\DNSOverHTTPS", name: "Locked", value: Want::Dword(1) },
];

pub fn backup_path() -> PathBuf {
    db::db_dir().join("browser_policy_backup.json")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// What's currently in the registry for one value. `Err` = present but of a
/// type we don't manage (we then leave it alone rather than clobber it).
fn read(key: &str, name: &str) -> Result<Option<Data>, ()> {
    let (k, n) = (wide(key), wide(name));
    let mut buf = [0u8; 1024];
    let mut size = buf.len() as u32;
    let mut ty = 0u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(k.as_ptr()),
            PCWSTR(n.as_ptr()),
            RRF_RT_ANY,
            Some(&mut ty as *mut u32 as *mut _),
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if rc != ERROR_SUCCESS {
        return Err(());
    }
    match ty {
        t if t == REG_DWORD.0 && size >= 4 => {
            Ok(Some(Data::Dword(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))))
        }
        t if t == REG_SZ.0 => {
            let units: Vec<u16> = buf[..size as usize]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            Ok(Some(Data::Sz(String::from_utf16_lossy(&units))))
        }
        _ => Err(()),
    }
}

fn write(key: &str, name: &str, data: Data) -> Result<(), String> {
    let (k, n) = (wide(key), wide(name));
    let (ty, bytes): (_, Vec<u8>) = match data {
        Data::Dword(v) => (REG_DWORD, v.to_le_bytes().to_vec()),
        Data::Sz(s) => (REG_SZ, wide(&s).iter().flat_map(|u| u.to_le_bytes()).collect()),
    };
    let rc = unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(k.as_ptr()),
            PCWSTR(n.as_ptr()),
            ty.0,
            Some(bytes.as_ptr() as *const _),
            bytes.len() as u32,
        )
    };
    if rc == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("write {key}\\{name} failed: {}", rc.0))
    }
}

fn delete(key: &str, name: &str) -> Result<(), String> {
    let (k, n) = (wide(key), wide(name));
    let rc = unsafe { RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, PCWSTR(k.as_ptr()), PCWSTR(n.as_ptr())) };
    if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("delete {key}\\{name} failed: {}", rc.0))
    }
}

// --- backup: [{key, name, prev: null | {"sz": ".."} | {"dword": n}}] -----

fn encode(d: Option<Data>) -> Value {
    match d {
        None => Value::Null,
        Some(Data::Sz(s)) => json!({ "sz": s }),
        Some(Data::Dword(v)) => json!({ "dword": v }),
    }
}

fn decode(v: &Value) -> Option<Data> {
    if let Some(s) = v.get("sz").and_then(Value::as_str) {
        return Some(Data::Sz(s.to_string()));
    }
    v.get("dword").and_then(Value::as_u64).map(|n| Data::Dword(n as u32))
}

fn read_backup() -> Option<Vec<Value>> {
    let text = std::fs::read_to_string(backup_path()).ok()?;
    serde_json::from_str::<Value>(&text).ok()?.get("values")?.as_array().cloned()
}

fn write_backup(entries: &[Value]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&json!({ "values": entries })).unwrap_or_default();
    let tmp = backup_path().with_extension("json.tmp");
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, backup_path()))
        .map_err(|e| format!("could not write {}: {e}", backup_path().display()))
}

// --- apply / restore -----------------------------------------------------

/// Returns how many values had to be (re)written.
pub fn apply() -> Result<usize, String> {
    // First assertion of this lock: capture every previous value before
    // touching any of them. Never re-captured afterwards, or a re-assert
    // would record our own "off" as the original.
    if !backup_path().exists() {
        let mut entries = Vec::new();
        for p in POLICIES {
            // A value of a type we don't manage is left strictly alone.
            let prev = read(p.key, p.name).map_err(|_| format!("{}\\{} has an unsupported type", p.key, p.name))?;
            entries.push(json!({ "key": p.key, "name": p.name, "prev": encode(prev) }));
        }
        write_backup(&entries)?;
    }

    let mut changed = 0;
    for p in POLICIES {
        if read(p.key, p.name) != Ok(Some(p.value.data())) {
            write(p.key, p.name, p.value.data())?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Puts every value back as recorded, then removes the record. A value the
/// user (or an admin) changed to something other than ours since is left
/// alone — we only undo what we did.
pub fn restore() -> Result<usize, String> {
    let Some(entries) = read_backup() else { return Ok(0) };
    let mut restored = 0;
    for e in &entries {
        let (Some(key), Some(name)) = (
            e.get("key").and_then(Value::as_str),
            e.get("name").and_then(Value::as_str),
        ) else {
            continue;
        };
        let Some(policy) = POLICIES.iter().find(|p| p.key == key && p.name == name) else {
            continue;
        };
        if read(key, name) != Ok(Some(policy.value.data())) {
            continue;
        }
        match e.get("prev").and_then(decode) {
            Some(prev) => write(key, name, prev)?,
            None => delete(key, name)?,
        }
        restored += 1;
    }
    let _ = std::fs::remove_file(backup_path());
    Ok(restored)
}

/// Human-readable snapshot for `dns-status`.
pub fn status() -> String {
    let mut out = String::new();
    for p in POLICIES {
        let cur = match read(p.key, p.name) {
            Ok(Some(d)) => format!("{d:?}"),
            Ok(None) => "(not set)".to_string(),
            Err(()) => "(unsupported type)".to_string(),
        };
        out += &format!("{}\\{} = {cur}\n", p.key.trim_start_matches(r"SOFTWARE\Policies\"), p.name);
    }
    out += &format!("browser policy backup present: {}\n", backup_path().exists());
    out
}

/// Call every service tick. Applies/re-asserts while a lock is active,
/// restores the first tick none is.
pub struct BrowserPolicy {
    applied: bool,
}

impl BrowserPolicy {
    pub fn new() -> Self {
        BrowserPolicy { applied: backup_path().exists() }
    }

    pub fn sync(&mut self, conn: &Connection) {
        let lock_active = db::lock::get_current(conn).map(|l| l.is_some()).unwrap_or(false);
        if lock_active {
            match apply() {
                Ok(0) => {}
                Ok(n) => {
                    log_line(&format!("browser_policy: (re)asserted secure-DNS-off on {n} value(s)."));
                    self.applied = true;
                }
                Err(e) => log_line(&format!("browser_policy: apply failed: {e}")),
            }
        } else if self.applied || backup_path().exists() {
            match restore() {
                Ok(n) => {
                    log_line(&format!("browser_policy: restored {n} value(s)."));
                    self.applied = false;
                }
                Err(e) => log_line(&format!("browser_policy: restore failed: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_encoding_roundtrips() {
        for d in [None, Some(Data::Sz("automatic".into())), Some(Data::Dword(7))] {
            assert_eq!(decode(&encode(d.clone())), d);
        }
    }

    #[test]
    fn every_policy_targets_a_machine_policy_path() {
        for p in POLICIES {
            assert!(p.key.starts_with(r"SOFTWARE\Policies\"), "{}", p.key);
        }
    }

    #[test]
    fn covers_chromium_family_and_firefox() {
        for needle in ["Google\\Chrome", "Microsoft\\Edge", "BraveSoftware", "Mozilla\\Firefox"] {
            assert!(POLICIES.iter().any(|p| p.key.contains(needle)), "{needle}");
        }
    }
}
