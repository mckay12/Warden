//! System-DNS enforcement: while a lock is active, every active network
//! adapter is pointed at the local DNS filter (`127.0.0.1` / `::1`), and put
//! back exactly as it was when the lock ends. Same shape as
//! `registry_policy.rs`: back up first, re-assert every tick, restore from
//! the backup rather than trying to "undo" a change.
//!
//! Reads and writes go through the Win32 `Get/SetInterfaceDnsSettings` APIs
//! rather than shelling out to `netsh`, so there's no per-tick process spawn
//! and no dependence on the Windows display language.
//!
//! **Safety — a dead filter means a dead internet connection, so:**
//! - DNS is only ever pointed at the filter if it answered a self-test, and
//!   is restored immediately (not on a delay) when the self-test fails.
//! - The pre-lock settings live in `dns_backup.json`; `dns-restore` (see
//!   `main.rs`) applies them by hand if the service is down.
//! - `Enforcer` restores on a clean service stop as well as on lock end.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{
    FreeInterfaceDnsSettings, GetAdaptersAddresses, GetInterfaceDnsSettings,
    SetInterfaceDnsSettings, DNS_INTERFACE_SETTINGS, DNS_INTERFACE_SETTINGS_VERSION1,
    DNS_SETTING_IPV6, DNS_SETTING_NAMESERVER, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_FRIENDLY_NAME, GAA_FLAG_SKIP_MULTICAST, GAA_FLAG_SKIP_UNICAST,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::AF_UNSPEC;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

use crate::db;
use crate::dns_filter::{self, Upstreams};
use crate::logging::log_line;

const V4_TARGET: &str = "127.0.0.1";
const V6_TARGET: &str = "::1";

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_TYPE_TUNNEL: u32 = 131;

/// Consecutive failed self-tests before DNS is handed back. More than one so
/// a single dropped loopback packet doesn't flap the whole system's DNS.
pub const DNS_PORT: u16 = 53;

const SELFTEST_FAILS_BEFORE_RESTORE: u32 = 3;
const SELFTEST_EVERY_TICKS: u64 = 3;
const UPSTREAM_REFRESH_EVERY_TICKS: u64 = 10;

pub fn backup_path() -> PathBuf {
    db::db_dir().join("dns_backup.json")
}

// ---------------------------------------------------------------------------
// Adapters + DNS settings (Win32)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Adapter {
    /// `{XXXXXXXX-XXXX-...}` — the key used in the backup file and registry.
    pub name: String,
    pub guid: GUID,
}

fn parse_guid(name: &str) -> Option<GUID> {
    GUID::try_from(name.trim_matches(|c| c == '{' || c == '}')).ok()
}

/// Adapters that are currently up, excluding loopback and tunnel pseudo
/// adapters (Teredo / ISATAP etc., which have no DNS of their own to speak of).
pub fn up_adapters() -> Result<Vec<Adapter>, String> {
    let flags = GAA_FLAG_SKIP_UNICAST
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER
        | GAA_FLAG_SKIP_FRIENDLY_NAME;

    let mut size: u32 = 15 * 1024;
    let mut buf: Vec<u8>;
    loop {
        // u64-backed so the buffer is 8-byte aligned for the struct cast below.
        buf = vec![0u8; (size as usize).div_ceil(8) * 8];
        let rc = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC.0 as u32,
                flags,
                None,
                Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut size,
            )
        };
        if rc == ERROR_SUCCESS.0 {
            break;
        }
        if rc != ERROR_BUFFER_OVERFLOW.0 {
            return Err(format!("GetAdaptersAddresses failed: {rc}"));
        }
    }

    let mut out = Vec::new();
    let mut cur = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !cur.is_null() {
        let a = unsafe { &*cur };
        let up = a.OperStatus == IfOperStatusUp;
        if up && a.IfType != IF_TYPE_SOFTWARE_LOOPBACK && a.IfType != IF_TYPE_TUNNEL {
            if let Ok(name) = unsafe { a.AdapterName.to_string() } {
                if let Some(guid) = parse_guid(&name) {
                    out.push(Adapter { name, guid });
                }
            }
        }
        cur = a.Next;
    }
    Ok(out)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The *statically configured* name servers for one family, comma-separated;
/// empty string means "automatic" (DHCP / router advertisement).
fn get_static(guid: GUID, v6: bool) -> Result<String, u32> {
    let mut s = DNS_INTERFACE_SETTINGS {
        Version: DNS_INTERFACE_SETTINGS_VERSION1,
        Flags: (DNS_SETTING_NAMESERVER | if v6 { DNS_SETTING_IPV6 } else { 0 }) as u64,
        ..Default::default()
    };
    let rc = unsafe { GetInterfaceDnsSettings(guid, &mut s) };
    if rc != ERROR_SUCCESS {
        return Err(rc.0);
    }
    let value = if s.NameServer.is_null() {
        String::new()
    } else {
        unsafe { s.NameServer.to_string().unwrap_or_default() }
    };
    unsafe { FreeInterfaceDnsSettings(&mut s) };
    Ok(value)
}

fn set_static(guid: GUID, v6: bool, value: &str) -> Result<(), u32> {
    let mut buf = wide(value);
    let s = DNS_INTERFACE_SETTINGS {
        Version: DNS_INTERFACE_SETTINGS_VERSION1,
        Flags: (DNS_SETTING_NAMESERVER | if v6 { DNS_SETTING_IPV6 } else { 0 }) as u64,
        NameServer: PWSTR(buf.as_mut_ptr()),
        ..Default::default()
    };
    let rc = unsafe { SetInterfaceDnsSettings(guid, &s) };
    if rc == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(rc.0)
    }
}

/// DHCP-provided DNS servers for an adapter. These stay in the registry
/// (updated on every lease renewal) even while a static override is set, which
/// is what lets the filter track the *current* network's resolver after the
/// adapter itself has been pointed at loopback.
fn dhcp_name_servers(adapter_name: &str) -> Vec<IpAddr> {
    let subkey = wide(&format!(
        r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\{adapter_name}"
    ));
    let value = wide("DhcpNameServer");
    let mut buf = vec![0u16; 512];
    let mut bytes = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut bytes),
        )
    };
    if rc != ERROR_SUCCESS {
        return Vec::new();
    }
    let len = (bytes as usize / 2).saturating_sub(1).min(buf.len());
    parse_ip_list(&String::from_utf16_lossy(&buf[..len]))
}

fn parse_ip_list(s: &str) -> Vec<IpAddr> {
    s.split(|c: char| c == ',' || c == ' ' || c == ';')
        .filter_map(|p| p.trim().parse::<IpAddr>().ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

/// adapter name -> (original static v4, original static v6); "" = automatic.
type Backup = BTreeMap<String, (String, String)>;

fn read_backup() -> Option<Backup> {
    let text = std::fs::read_to_string(backup_path()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let mut out = Backup::new();
    for (k, v) in v.get("adapters")?.as_object()? {
        out.insert(
            k.clone(),
            (
                v.get("v4")?.as_str()?.to_string(),
                v.get("v6")?.as_str()?.to_string(),
            ),
        );
    }
    Some(out)
}

fn write_backup(b: &Backup) -> Result<(), String> {
    let mut adapters = Map::new();
    for (k, (v4, v6)) in b {
        adapters.insert(k.clone(), json!({ "v4": v4, "v6": v6 }));
    }
    let text = serde_json::to_string_pretty(&json!({ "adapters": adapters })).unwrap_or_default();
    // Temp + rename so a crash mid-write can't leave a truncated backup,
    // which would be the one file that knows how to put DNS back.
    let tmp = backup_path().with_extension("json.tmp");
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, backup_path()))
        .map_err(|e| format!("could not write {}: {e}", backup_path().display()))
}

// ---------------------------------------------------------------------------
// Apply / restore
// ---------------------------------------------------------------------------

/// Points every up adapter at the filter, recording originals first. Returns
/// how many adapters actually needed a change this call.
fn apply(v6_enabled: bool) -> Result<usize, String> {
    let mut backup = read_backup().unwrap_or_default();
    let mut changed = 0;
    for a in up_adapters()? {
        let cur4 = get_static(a.guid, false).map_err(|e| format!("{}: read v4 failed ({e})", a.name))?;
        let cur6 = get_static(a.guid, true).map_err(|e| format!("{}: read v6 failed ({e})", a.name))?;
        let v6_done = !v6_enabled || cur6 == V6_TARGET;
        if cur4 == V4_TARGET && v6_done {
            continue;
        }
        // Record the original *before* the first write to this adapter, and
        // never overwrite it afterwards (a re-assert after tampering would
        // otherwise record our own loopback value as "the original").
        if !backup.contains_key(&a.name) {
            let orig4 = if cur4 == V4_TARGET { String::new() } else { cur4.clone() };
            let orig6 = if cur6 == V6_TARGET { String::new() } else { cur6.clone() };
            backup.insert(a.name.clone(), (orig4, orig6));
            write_backup(&backup)?;
        }
        if cur4 != V4_TARGET {
            set_static(a.guid, false, V4_TARGET).map_err(|e| format!("{}: set v4 failed ({e})", a.name))?;
        }
        if v6_enabled && cur6 != V6_TARGET {
            set_static(a.guid, true, V6_TARGET).map_err(|e| format!("{}: set v6 failed ({e})", a.name))?;
        }
        changed += 1;
    }
    Ok(changed)
}

/// Puts every adapter in the backup back exactly as recorded, then deletes
/// the backup. An adapter that no longer exists is skipped (nothing to
/// restore); one that fails to restore keeps the backup file in place so the
/// next call retries it.
pub fn restore() -> Result<usize, String> {
    let Some(backup) = read_backup() else {
        return Ok(0);
    };
    let mut restored = 0;
    let mut failures = Vec::new();
    for (name, (v4, v6)) in &backup {
        let Some(guid) = parse_guid(name) else { continue };
        // Only touch families we actually overrode, and only if still ours —
        // if the user changed it themselves meanwhile, respect that.
        let r4 = match get_static(guid, false) {
            Ok(cur) if cur == V4_TARGET => set_static(guid, false, v4),
            _ => Ok(()),
        };
        let r6 = match get_static(guid, true) {
            Ok(cur) if cur == V6_TARGET => set_static(guid, true, v6),
            _ => Ok(()),
        };
        match (r4, r6) {
            (Ok(()), Ok(())) => restored += 1,
            // 1168 = ERROR_NOT_FOUND: adapter was removed since the backup.
            (a, b) if a.err() == Some(1168) || b.err() == Some(1168) => {}
            (a, b) => failures.push(format!("{name}: v4={a:?} v6={b:?}")),
        }
    }
    if failures.is_empty() {
        let _ = std::fs::remove_file(backup_path());
        Ok(restored)
    } else {
        Err(format!("restore incomplete, backup kept: {}", failures.join("; ")))
    }
}

/// The resolvers the network itself would have used: for each up adapter the
/// originally-configured static servers if any, otherwise the DHCP-provided
/// ones. Never includes loopback.
fn network_upstreams() -> Vec<SocketAddr> {
    let backup = read_backup().unwrap_or_default();
    let mut out: Vec<SocketAddr> = Vec::new();
    let Ok(adapters) = up_adapters() else { return out };
    for a in adapters {
        let mut ips: Vec<IpAddr> = Vec::new();
        let (v4, v6) = match backup.get(&a.name) {
            Some((v4, v6)) => (v4.clone(), v6.clone()),
            None => (
                get_static(a.guid, false).unwrap_or_default(),
                get_static(a.guid, true).unwrap_or_default(),
            ),
        };
        ips.extend(parse_ip_list(&v4));
        ips.extend(parse_ip_list(&v6));
        if parse_ip_list(&v4).is_empty() {
            ips.extend(dhcp_name_servers(&a.name));
        }
        for ip in ips {
            let addr = SocketAddr::new(ip, 53);
            if !ip.is_loopback() && !out.contains(&addr) {
                out.push(addr);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

/// Asks the filter (on the real port 53) for its self-test name and checks
/// that it answers. Exercises the actual socket path the system resolver
/// will use, not just "the thread exists".
pub fn selftest(port: u16) -> bool {
    let mut q = vec![0x53, 0x54, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in dns_filter::SELFTEST_NAME.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, 1, 0, 1]); // A, IN

    let Ok(sock) = UdpSocket::bind("127.0.0.1:0") else { return false };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(800)));
    if sock.send_to(&q, ("127.0.0.1", port)).is_err() {
        return false;
    }
    let mut buf = [0u8; 512];
    matches!(sock.recv_from(&mut buf), Ok((n, _)) if n >= 12 && buf[..2] == q[..2] && buf[3] & 0x0F == 0)
}

// ---------------------------------------------------------------------------
// Service-facing driver
// ---------------------------------------------------------------------------

pub struct Enforcer {
    upstreams: Upstreams,
    port: u16,
    /// False if the filter never managed to bind: DNS must never be pointed
    /// at something that isn't listening.
    filter_running: bool,
    /// Whether the filter is also listening on `::1`. If not, IPv6 DNS is
    /// left alone rather than pointed at an address nothing answers on.
    v6_enabled: bool,
    selftest_fails: u32,
    last_upstreams: Vec<SocketAddr>,
    /// Log-spam guards: only log state *changes*, not every tick.
    applied: bool,
    warned_no_filter: bool,
}

impl Enforcer {
    pub fn new(upstreams: Upstreams, port: u16, bound: &[SocketAddr]) -> Self {
        Enforcer {
            upstreams,
            port,
            filter_running: bound.iter().any(|a| a.ip() == IpAddr::from([127, 0, 0, 1])),
            v6_enabled: bound.iter().any(|a| a.ip() == IpAddr::from(std::net::Ipv6Addr::LOCALHOST)),
            selftest_fails: 0,
            last_upstreams: Vec::new(),
            applied: false,
            warned_no_filter: false,
        }
    }

    fn refresh_upstreams(&mut self) {
        let fresh = network_upstreams();
        if fresh != self.last_upstreams {
            log_line(&format!("dns_enforce: upstream resolvers now {fresh:?}."));
            if let Ok(mut guard) = self.upstreams.write() {
                *guard = fresh.clone();
            }
            self.last_upstreams = fresh;
        }
    }

    /// Call every service tick (1s).
    pub fn sync(&mut self, conn: &Connection, tick: u64) {
        // Cheap; keeps the filter forwarding to whatever the current network
        // uses, including after a Wi-Fi switch mid-lock.
        if tick % UPSTREAM_REFRESH_EVERY_TICKS == 1 || self.last_upstreams.is_empty() {
            self.refresh_upstreams();
        }

        let lock_active = db::lock::get_current(conn).map(|l| l.is_some()).unwrap_or(false);

        if !lock_active {
            self.release("lock not active");
            return;
        }
        if !self.filter_running {
            if !self.warned_no_filter {
                log_line("dns_enforce: DNS filter is not listening; NOT enforcing system DNS.");
                self.warned_no_filter = true;
            }
            return;
        }

        if tick % SELFTEST_EVERY_TICKS == 0 || !self.applied {
            if selftest(self.port) {
                self.selftest_fails = 0;
            } else {
                self.selftest_fails += 1;
                // Not yet applied: never apply on an unproven filter.
                // Applied: tolerate a blip, but bail out before long.
                if !self.applied || self.selftest_fails >= SELFTEST_FAILS_BEFORE_RESTORE {
                    log_line(&format!(
                        "dns_enforce: filter self-test failed ({}x); {}.",
                        self.selftest_fails,
                        if self.applied { "restoring system DNS" } else { "not applying" }
                    ));
                    self.release("filter unhealthy");
                    return;
                }
            }
        }

        match apply(self.v6_enabled) {
            Ok(0) => {}
            Ok(n) => {
                log_line(&format!("dns_enforce: (re)asserted filter DNS on {n} adapter(s)."));
                self.applied = true;
                // Windows keeps its own cache of the old answers; clear it so
                // already-resolved blocked domains don't keep working.
                flush_dns_cache();
            }
            Err(e) => log_line(&format!("dns_enforce: apply failed: {e}")),
        }
        if !self.applied && read_backup().is_some() {
            self.applied = true;
        }
    }

    /// Hands DNS back if (and only if) we had taken it.
    fn release(&mut self, why: &str) {
        if read_backup().is_none() {
            self.applied = false;
            return;
        }
        match restore() {
            Ok(n) => {
                log_line(&format!("dns_enforce: restored system DNS on {n} adapter(s) ({why})."));
                flush_dns_cache();
                self.applied = false;
            }
            Err(e) => log_line(&format!("dns_enforce: restore failed ({why}): {e}")),
        }
    }

    /// Clean service stop: never exit leaving DNS pointed at a dead filter.
    pub fn shutdown(&mut self) {
        self.release("service stopping");
    }
}

/// Manual test harness for `dns-serve --enforce`: takes over system DNS for a
/// bounded time with no lock involved, then always restores. Bounded on
/// purpose — this is the path used to try the feature out on a real machine.
pub fn run_manual(bound: &[SocketAddr], upstreams: Upstreams, seconds: u64) -> Result<(), String> {
    let mut e = Enforcer::new(upstreams, DNS_PORT, bound);
    if !e.filter_running {
        return Err("filter is not listening on 127.0.0.1:53".into());
    }
    if !selftest(DNS_PORT) {
        return Err("filter failed its self-test; not touching system DNS".into());
    }
    e.refresh_upstreams();
    let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
    let mut fails = 0;
    let mut tick = 0u64;
    let result = loop {
        if std::time::Instant::now() >= deadline {
            break Ok(());
        }
        match apply(e.v6_enabled) {
            Ok(n) if n > 0 => {
                println!("pointed {n} adapter(s) at the filter");
                flush_dns_cache();
            }
            Ok(_) => {}
            Err(err) => break Err(err),
        }
        match crate::browser_policy::apply() {
            Ok(n) if n > 0 => println!("set secure-DNS-off policy on {n} value(s)"),
            Ok(_) => {}
            Err(err) => break Err(err),
        }
        tick += 1;
        if tick % 5 == 0 {
            e.refresh_upstreams();
        }
        if tick % 3 == 0 {
            fails = if selftest(DNS_PORT) { 0 } else { fails + 1 };
            if fails >= SELFTEST_FAILS_BEFORE_RESTORE {
                break Err("filter stopped answering".into());
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    let restored = restore();
    flush_dns_cache();
    println!("restore: dns={restored:?} browser_policy={:?}", crate::browser_policy::restore());
    result.and(restored.map(|_| ()))
}

/// Human-readable snapshot for the `dns-status` command.
pub fn status() -> String {
    let mut out = String::new();
    match up_adapters() {
        Ok(list) => {
            for a in list {
                out += &format!(
                    "{}  v4={:?} v6={:?}  dhcp={:?}
",
                    a.name,
                    get_static(a.guid, false).unwrap_or_else(|e| format!("err {e}")),
                    get_static(a.guid, true).unwrap_or_else(|e| format!("err {e}")),
                    dhcp_name_servers(&a.name),
                );
            }
        }
        Err(e) => out += &format!("adapter enumeration failed: {e}
"),
    }
    out += &format!("backup file present: {}
", backup_path().exists());
    out += &format!("filter self-test on 127.0.0.1:53: {}
", selftest(DNS_PORT));
    out
}

fn flush_dns_cache() {
    let _ = std::process::Command::new("ipconfig.exe").arg("/flushdns").output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_separators_and_skips_junk() {
        let ips = parse_ip_list("172.20.10.1, 8.8.8.8  fe80::1;bogus");
        assert_eq!(ips.len(), 3);
        assert!(parse_ip_list("").is_empty());
    }

    #[test]
    fn guid_parses_with_and_without_braces() {
        assert!(parse_guid("{12345678-1234-1234-1234-123456789abc}").is_some());
        assert!(parse_guid("12345678-1234-1234-1234-123456789abc").is_some());
        assert!(parse_guid("not-a-guid").is_none());
    }

    #[test]
    fn backup_roundtrips_through_json() {
        let mut b = Backup::new();
        b.insert("{A}".into(), ("1.1.1.1,8.8.8.8".into(), String::new()));
        let mut adapters = Map::new();
        for (k, (v4, v6)) in &b {
            adapters.insert(k.clone(), json!({ "v4": v4, "v6": v6 }));
        }
        let text = json!({ "adapters": adapters }).to_string();
        let v: Value = serde_json::from_str(&text).unwrap();
        let back = v["adapters"]["{A}"]["v4"].as_str().unwrap();
        assert_eq!(back, "1.1.1.1,8.8.8.8");
    }
}
