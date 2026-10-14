//! Local DNS filter — the system-wide blocking engine for the desktop-app
//! direction. A small stub resolver: blocked domains (and their subdomains)
//! get a sinkhole answer (`0.0.0.0` / `::`), everything else is forwarded
//! untouched to the real upstream resolvers.
//!
//! This module only *answers* DNS. Pointing Windows at it (and putting that
//! back) is a separate concern — see the system-DNS enforcement work — so
//! this can be run and tested on a spare port without touching the machine's
//! network settings (`warden-service dns-serve --port 5353`).
//!
//! The blocklist is held as a sorted `Vec<u64>` of domain hashes rather than
//! a `HashSet<String>`: the real source list has ~950K entries, which would
//! cost tens of MB as strings but is under 8 MB as hashes. The hashes only
//! live in memory (never persisted), so the hasher needn't be stable across
//! builds, and a 64-bit collision across ~1M entries is negligible.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::logging::log_line;

const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);
const SINKHOLE_TTL: u32 = 60;
const RELOAD_CHECK_INTERVAL: Duration = Duration::from_secs(30);

const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;

const BUNDLED: &str = include_str!("bundled_blocklist.txt");

/// Where the blocklist refresh drops the *full* domain list (one per line).
/// Separate from the SQLite `blocked_domain` table on purpose: that table is
/// capped at 2000 rows because it feeds Chrome's `declarativeNetRequest`,
/// whereas DNS has no such ceiling.
pub fn list_path() -> PathBuf {
    crate::db::db_dir().join("dns_blocklist.txt")
}

// ---------------------------------------------------------------------------
// Blocklist
// ---------------------------------------------------------------------------

fn hash_domain(domain: &str) -> u64 {
    let mut h = DefaultHasher::new();
    domain.hash(&mut h);
    h.finish()
}

fn normalize(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[derive(Default)]
pub struct Blocklist {
    hashes: Vec<u64>,
}

impl Blocklist {
    pub fn from_domains<I, S>(domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut hashes: Vec<u64> = domains
            .into_iter()
            .map(|d| normalize(d.as_ref()))
            .filter(|d| !d.is_empty())
            .map(|d| hash_domain(&d))
            .collect();
        hashes.sort_unstable();
        hashes.dedup();
        Blocklist { hashes }
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    /// Returns the listed domain that matched (the name itself or its
    /// nearest listed parent), so a hit on `cdn.evil.com` caused by `evil.com`
    /// can be reported as `evil.com`. Single-label suffixes (bare TLDs) are
    /// never checked: a stray `com` entry must not blackhole the internet.
    pub fn matching_entry(&self, name: &str) -> Option<String> {
        let name = normalize(name);
        let mut candidate = name.as_str();
        loop {
            if !candidate.contains('.') {
                return None;
            }
            if self.hashes.binary_search(&hash_domain(candidate)).is_ok() {
                return Some(candidate.to_string());
            }
            candidate = candidate.split_once('.')?.1;
        }
    }
}

/// Loads the full list from disk, falling back to the small bundled list so
/// blocking still works before the first successful refresh (PRD §7.1).
pub fn load_blocklist() -> Blocklist {
    match std::fs::read_to_string(list_path()) {
        Ok(text) if !text.trim().is_empty() => Blocklist::from_domains(text.lines()),
        _ => Blocklist::from_domains(BUNDLED.lines()),
    }
}

fn list_mtime() -> Option<SystemTime> {
    std::fs::metadata(list_path()).ok()?.modified().ok()
}

// ---------------------------------------------------------------------------
// DNS wire format (just enough for a stub resolver)
// ---------------------------------------------------------------------------

struct Question {
    name: String,
    qtype: u16,
    /// Offset just past the question section's single entry.
    end: usize,
}

/// Parses the first question of a standard query. Returns `None` for
/// anything that isn't a plain single-question query (responses, other
/// opcodes, compressed names in the question, truncated packets).
fn parse_question(packet: &[u8]) -> Option<Question> {
    if packet.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([packet[2], packet[3]]);
    let is_response = flags & 0x8000 != 0;
    let opcode = (flags >> 11) & 0x0F;
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]);
    if is_response || opcode != 0 || qdcount != 1 {
        return None;
    }

    let mut pos = 12;
    let mut name = String::new();
    loop {
        let len = *packet.get(pos)? as usize;
        pos += 1;
        if len == 0 {
            break;
        }
        if len & 0xC0 != 0 {
            return None; // compression pointers aren't valid in a question
        }
        let label = packet.get(pos..pos + len)?;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&String::from_utf8_lossy(label));
        pos += len;
    }
    let qtype = u16::from_be_bytes([*packet.get(pos)?, *packet.get(pos + 1)?]);
    let end = pos + 4; // qtype + qclass
    if end > packet.len() {
        return None;
    }
    Some(Question { name, qtype, end })
}

/// Builds the sinkhole reply: A -> 0.0.0.0, AAAA -> ::, anything else
/// (HTTPS/SVCB, MX, TXT, ...) -> NOERROR with an empty answer so clients
/// fall straight through to the A/AAAA lookups, which are sinkholed.
fn build_blocked_response(query: &[u8], q: &Question) -> Vec<u8> {
    let mut resp = Vec::with_capacity(q.end + 28);
    resp.extend_from_slice(&query[..q.end]);

    // Header: QR=1, keep opcode + RD from the query, RA=1, RCODE=0.
    let rd = query[2] & 0x01;
    resp[2] = 0x80 | (query[2] & 0x78) | rd;
    resp[3] = 0x80;
    // QDCOUNT stays 1; clear NS/AR counts, set ANCOUNT below.
    resp[6] = 0;
    resp[7] = 0;
    resp[8] = 0;
    resp[9] = 0;
    resp[10] = 0;
    resp[11] = 0;

    let rdata: Option<&[u8]> = match q.qtype {
        TYPE_A => Some(&[0, 0, 0, 0]),
        TYPE_AAAA => Some(&[0u8; 16]),
        _ => None,
    };
    if let Some(rdata) = rdata {
        resp[7] = 1; // ANCOUNT = 1
        resp.extend_from_slice(&[0xC0, 0x0C]); // name: pointer to the question
        resp.extend_from_slice(&q.qtype.to_be_bytes());
        resp.extend_from_slice(&1u16.to_be_bytes()); // class IN
        resp.extend_from_slice(&SINKHOLE_TTL.to_be_bytes());
        resp.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        resp.extend_from_slice(rdata);
    }
    resp
}

// ---------------------------------------------------------------------------
// Forwarding
// ---------------------------------------------------------------------------

fn forward_udp(query: &[u8], upstreams: &[SocketAddr]) -> Option<Vec<u8>> {
    for upstream in upstreams {
        let bind: SocketAddr = if upstream.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let Ok(sock) = UdpSocket::bind(bind) else { continue };
        let _ = sock.set_read_timeout(Some(UPSTREAM_TIMEOUT));
        if sock.send_to(query, upstream).is_err() {
            continue;
        }
        let mut buf = vec![0u8; 4096];
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            // Only accept the reply from the server we asked.
            if from.ip() == upstream.ip() && n >= 2 && buf[..2] == query[..2] {
                buf.truncate(n);
                return Some(buf);
            }
        }
    }
    None
}

fn forward_tcp(query: &[u8], upstreams: &[SocketAddr]) -> Option<Vec<u8>> {
    for upstream in upstreams {
        let Ok(mut stream) = TcpStream::connect_timeout(upstream, UPSTREAM_TIMEOUT) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(UPSTREAM_TIMEOUT));
        let _ = stream.set_write_timeout(Some(UPSTREAM_TIMEOUT));
        let mut framed = (query.len() as u16).to_be_bytes().to_vec();
        framed.extend_from_slice(query);
        if stream.write_all(&framed).is_err() {
            continue;
        }
        let mut len = [0u8; 2];
        if stream.read_exact(&mut len).is_err() {
            continue;
        }
        let mut resp = vec![0u8; u16::from_be_bytes(len) as usize];
        if stream.read_exact(&mut resp).is_ok() {
            return Some(resp);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Called once per blocked A/AAAA lookup with `(queried_name, matched_entry)`.
/// Not debounced here — the caller decides what counts as an "attempt".
pub type BlockHook = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// The network's real resolvers, updated live by the system-DNS enforcer as
/// adapters come and go (e.g. moving between Wi-Fi networks). Shared so the
/// filter always forwards to whatever the current network actually uses.
pub type Upstreams = Arc<RwLock<Vec<SocketAddr>>>;

pub fn new_upstreams(initial: Vec<SocketAddr>) -> Upstreams {
    Arc::new(RwLock::new(initial))
}

#[derive(Clone)]
pub struct Config {
    pub listen: Vec<SocketAddr>,
    pub upstreams: Upstreams,
    pub on_block: BlockHook,
}

/// Name the enforcer's health check asks for. Answered locally, never
/// forwarded, never reported as a block.
pub const SELFTEST_NAME: &str = "selftest.warden.invalid";

/// Firefox asks for this before enabling DoH by default; an NXDOMAIN tells it
/// the network opts out. Only a hint (the policy in `browser_policy.rs` is the
/// enforcement), but free, and covers Firefox-family browsers that don't read
/// Firefox's policy keys.
pub const DOH_CANARY: &str = "use-application-dns.net";

/// Hostnames of well-known public DoH/DoT resolvers. A browser has to look one
/// of these up through the system resolver before it can use it, so refusing
/// to resolve them (NXDOMAIN) stops hostname-configured secure DNS. Subdomains
/// are covered too (`chrome.cloudflare-dns.com`, `mozilla.cloudflare-dns.com`).
/// The firewall rules in `firewall.rs` cover the same providers by address.
const DOH_HOSTS: &[&str] = &[
    "dns.google", "dns.google.com", "cloudflare-dns.com", "one.one.one.one",
    "dns.quad9.net", "dns9.quad9.net", "dns10.quad9.net", "dns11.quad9.net",
    "doh.opendns.com", "doh.familyshield.opendns.com", "dns.adguard.com", "dns.adguard-dns.com",
    "unfiltered.adguard-dns.com", "family.adguard-dns.com", "dns.nextdns.io",
    "doh.cleanbrowsing.org", "dns.mullvad.net", "doh.mullvad.net", "doh.libredns.gr",
    "doh.applied-privacy.net", "dns.switch.ch", "dns.sb", "doh.dns.sb", "doh.pub",
    "dns.alidns.com", "doh.360.cn", "dns.twnic.tw", "dnsforge.de", "doh.ffmuc.net",
    "doh.li", "dns.digitale-gesellschaft.ch", "odvr.nic.cz", "public.dns.iij.jp",
    "doh.tiarap.org", "freedns.controld.com", "dns.controld.com", "doh.dnslify.com",
];

fn doh_hosts() -> &'static Blocklist {
    static LIST: std::sync::OnceLock<Blocklist> = std::sync::OnceLock::new();
    LIST.get_or_init(|| Blocklist::from_domains(DOH_HOSTS.iter().copied()))
}

fn build_nxdomain(query: &[u8], q: &Question) -> Vec<u8> {
    let mut resp = query[..q.end].to_vec();
    resp[2] = 0x80 | (query[2] & 0x78) | (query[2] & 0x01);
    resp[3] = 0x83; // RA + RCODE=3 (NXDOMAIN)
    resp[6..12].fill(0);
    resp
}

/// The network's resolvers first, then public ones as a last resort so a
/// stale or unreachable network resolver can't leave the machine without DNS.
/// Loopback is always excluded so the filter can never forward to itself.
fn forward_chain(upstreams: &Upstreams) -> Vec<SocketAddr> {
    let mut chain: Vec<SocketAddr> = upstreams
        .read()
        .map(|u| u.iter().copied().filter(|a| !a.ip().is_loopback()).collect())
        .unwrap_or_default();
    for fallback in default_upstreams() {
        if !chain.contains(&fallback) {
            chain.push(fallback);
        }
    }
    chain
}

/// Public resolvers appended after the network's own (see `forward_chain`).
pub fn default_upstreams() -> Vec<SocketAddr> {
    vec![
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 53),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 53),
    ]
}

/// Second-level public suffixes under which the registrable domain has three
/// labels (`bbc.co.uk`, not `co.uk`). A short built-in list rather than the
/// full Public Suffix List: it only decides how an attempt is *labelled*, and
/// a miss just shows the shorter name.
const TWO_LEVEL_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "me.uk", "com.au", "net.au", "org.au", "co.nz",
    "co.jp", "ne.jp", "or.jp", "com.br", "com.mx", "com.ar", "co.za", "co.in", "com.tr",
    "com.cn", "com.hk", "com.sg", "com.tw", "co.kr", "com.ua", "com.pl", "co.il",
];

/// The site a lookup belongs to: `www.pornhub.com` -> `pornhub.com`,
/// `pix-fl.phncdn.com` -> `phncdn.com`, `x.bbc.co.uk` -> `bbc.co.uk`.
pub fn site_of(name: &str) -> String {
    let name = normalize(name);
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() <= 2 {
        return name;
    }
    let last_two = labels[labels.len() - 2..].join(".");
    let keep = if TWO_LEVEL_SUFFIXES.contains(&last_two.as_str()) { 3 } else { 2 };
    labels[labels.len().saturating_sub(keep)..].join(".")
}

/// Turns a stream of blocked lookups into *attempts*. One page visit makes
/// many lookups (the site, its CDN subdomains, A + AAAA, retries), so a burst
/// counts once: a new attempt starts only after `QUIET` with no blocked
/// lookups, or once the current burst has run for `MAX_BURST` (someone who
/// keeps hammering reload shouldn't collapse into a single attempt forever).
struct AttemptSessions {
    started: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
}

impl AttemptSessions {
    const QUIET: Duration = Duration::from_secs(30);
    const MAX_BURST: Duration = Duration::from_secs(300);

    fn new() -> Self {
        AttemptSessions { started: None, last: None }
    }

    /// Records a blocked lookup at `now`; returns whether it begins a new attempt.
    fn observe(&mut self, now: std::time::Instant) -> bool {
        let new_attempt = match (self.started, self.last) {
            (Some(started), Some(last)) => {
                now.duration_since(last) >= Self::QUIET || now.duration_since(started) >= Self::MAX_BURST
            }
            _ => true,
        };
        if new_attempt {
            self.started = Some(now);
        }
        self.last = Some(now);
        new_attempt
    }
}

/// A block hook that records attempts in the service database, one per burst
/// of blocked lookups (see `AttemptSessions`), labelled with the site of the
/// burst's first lookup.
pub fn db_logging_hook() -> BlockHook {
    let sessions = std::sync::Mutex::new(AttemptSessions::new());
    Arc::new(move |name, entry| {
        let begins_attempt = match sessions.lock() {
            Ok(mut s) => s.observe(std::time::Instant::now()),
            Err(_) => return,
        };
        if !begins_attempt {
            return;
        }
        let site = site_of(name);
        match crate::db::open() {
            Ok(conn) => {
                if let Err(e) = crate::db::blocked_attempt::create(&conn, "blocklist", entry, &site) {
                    log_line(&format!("dns_filter: could not log blocked attempt: {e}"));
                }
            }
            Err(e) => log_line(&format!("dns_filter: db open failed while logging attempt: {e}")),
        }
    })
}

fn answer(
    query: &[u8],
    list: &RwLock<Arc<Blocklist>>,
    cfg: &Config,
    tcp: bool,
) -> Option<Vec<u8>> {
    let q = parse_question(query)?;
    if normalize(&q.name) == SELFTEST_NAME {
        return Some(build_blocked_response(query, &q));
    }
    if normalize(&q.name) == DOH_CANARY || doh_hosts().matching_entry(&q.name).is_some() {
        return Some(build_nxdomain(query, &q));
    }
    let current = list.read().ok()?.clone();
    if let Some(entry) = current.matching_entry(&q.name) {
        if q.qtype == TYPE_A || q.qtype == TYPE_AAAA {
            (cfg.on_block)(&q.name, &entry);
        }
        return Some(build_blocked_response(query, &q));
    }
    let chain = forward_chain(&cfg.upstreams);
    if tcp {
        forward_tcp(query, &chain)
    } else {
        forward_udp(query, &chain)
    }
}

/// Starts the UDP + TCP listeners and the list-reload watcher, each on its
/// own thread, and returns immediately. Binding failures on individual
/// addresses are logged and skipped (e.g. `::1` on a machine without IPv6),
/// but if *nothing* could be bound that is an error — the caller must not
/// point system DNS at a filter that isn't listening.
pub fn spawn(cfg: Config) -> Result<Vec<SocketAddr>, String> {
    let list = Arc::new(RwLock::new(Arc::new(load_blocklist())));
    log_line(&format!(
        "dns_filter: loaded {} blocked domains.",
        list.read().map(|l| l.len()).unwrap_or(0)
    ));

    let mut bound: Vec<SocketAddr> = Vec::new();
    for addr in &cfg.listen {
        let udp = match UdpSocket::bind(addr) {
            Ok(s) => s,
            Err(e) => {
                log_line(&format!("dns_filter: UDP bind {addr} failed: {e}"));
                continue;
            }
        };
        let tcp = match TcpListener::bind(addr) {
            Ok(l) => l,
            Err(e) => {
                log_line(&format!("dns_filter: TCP bind {addr} failed: {e}"));
                continue;
            }
        };
        bound.push(*addr);
        log_line(&format!("dns_filter: listening on {addr} (udp+tcp)."));

        {
            let (list, cfg) = (list.clone(), cfg.clone());
            thread::spawn(move || udp_loop(udp, list, cfg));
        }
        {
            let (list, cfg) = (list.clone(), cfg.clone());
            thread::spawn(move || tcp_loop(tcp, list, cfg));
        }
    }
    if bound.is_empty() {
        return Err("could not bind any listen address".to_string());
    }

    thread::spawn(move || reload_loop(list));
    Ok(bound)
}

fn udp_loop(sock: UdpSocket, list: Arc<RwLock<Arc<Blocklist>>>, cfg: Config) {
    let mut buf = vec![0u8; 4096];
    loop {
        let (n, client) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let query = buf[..n].to_vec();
        let (sock, list, cfg) = (match sock.try_clone() {
            Ok(s) => s,
            Err(_) => continue,
        }, list.clone(), cfg.clone());
        // One short-lived thread per query keeps a slow upstream from
        // stalling every other lookup behind it.
        thread::spawn(move || {
            if let Some(resp) = answer(&query, &list, &cfg, false) {
                let _ = sock.send_to(&resp, client);
            }
        });
    }
}

fn tcp_loop(listener: TcpListener, list: Arc<RwLock<Arc<Blocklist>>>, cfg: Config) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let (list, cfg) = (list.clone(), cfg.clone());
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let mut len = [0u8; 2];
            while stream.read_exact(&mut len).is_ok() {
                let mut query = vec![0u8; u16::from_be_bytes(len) as usize];
                if stream.read_exact(&mut query).is_err() {
                    return;
                }
                let Some(resp) = answer(&query, &list, &cfg, true) else {
                    return;
                };
                let mut framed = (resp.len() as u16).to_be_bytes().to_vec();
                framed.extend_from_slice(&resp);
                if stream.write_all(&framed).is_err() {
                    return;
                }
            }
        });
    }
}

/// Hot-swaps the in-memory list when the refresh job rewrites the file.
fn reload_loop(list: Arc<RwLock<Arc<Blocklist>>>) {
    let mut last = list_mtime();
    loop {
        thread::sleep(RELOAD_CHECK_INTERVAL);
        let now = list_mtime();
        if now != last {
            last = now;
            let fresh = Arc::new(load_blocklist());
            log_line(&format!("dns_filter: reloaded {} blocked domains.", fresh.len()));
            if let Ok(mut guard) = list.write() {
                *guard = fresh;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_packet(name: &str, qtype: u16) -> Vec<u8> {
        let mut p = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            p.push(label.len() as u8);
            p.extend_from_slice(label.as_bytes());
        }
        p.push(0);
        p.extend_from_slice(&qtype.to_be_bytes());
        p.extend_from_slice(&1u16.to_be_bytes());
        p
    }

    #[test]
    fn matches_exact_and_subdomains_but_not_lookalikes() {
        let bl = Blocklist::from_domains(["evil.com", "Other.Example.org."]);
        assert_eq!(bl.matching_entry("evil.com").as_deref(), Some("evil.com"));
        assert_eq!(bl.matching_entry("WWW.Evil.com.").as_deref(), Some("evil.com"));
        assert_eq!(bl.matching_entry("a.b.evil.com").as_deref(), Some("evil.com"));
        assert_eq!(bl.matching_entry("other.example.org").as_deref(), Some("other.example.org"));
        assert!(bl.matching_entry("notevil.com").is_none());
        assert!(bl.matching_entry("evil.com.au").is_none());
        assert!(bl.matching_entry("example.org").is_none());
    }

    #[test]
    fn bare_tld_entry_never_blackholes_everything() {
        let bl = Blocklist::from_domains(["com"]);
        assert!(bl.matching_entry("google.com").is_none());
    }

    #[test]
    fn parses_a_standard_query() {
        let p = query_packet("www.example.com", TYPE_A);
        let q = parse_question(&p).unwrap();
        assert_eq!(q.name, "www.example.com");
        assert_eq!(q.qtype, TYPE_A);
        assert_eq!(q.end, p.len());
    }

    #[test]
    fn rejects_garbage_and_responses() {
        assert!(parse_question(&[]).is_none());
        assert!(parse_question(&[0; 11]).is_none());
        let mut p = query_packet("a.com", TYPE_A);
        p[2] |= 0x80; // QR=1 -> a response, not a query
        assert!(parse_question(&p).is_none());
        let p = query_packet("a.com", TYPE_A);
        assert!(parse_question(&p[..p.len() - 3]).is_none()); // truncated
    }

    #[test]
    fn blocked_a_query_gets_zero_address() {
        let p = query_packet("evil.com", TYPE_A);
        let q = parse_question(&p).unwrap();
        let r = build_blocked_response(&p, &q);
        assert_eq!(&r[..2], &p[..2]); // same transaction id
        assert_ne!(r[2] & 0x80, 0); // QR set
        assert_eq!(r[3] & 0x0F, 0); // RCODE = NOERROR
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 1); // one answer
        assert_eq!(&r[r.len() - 4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn blocked_aaaa_query_gets_zero_v6_address() {
        let p = query_packet("evil.com", TYPE_AAAA);
        let q = parse_question(&p).unwrap();
        let r = build_blocked_response(&p, &q);
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 1);
        assert_eq!(&r[r.len() - 16..], &[0u8; 16]);
    }

    #[test]
    fn other_record_types_get_empty_noerror() {
        let p = query_packet("evil.com", 65); // HTTPS
        let q = parse_question(&p).unwrap();
        let r = build_blocked_response(&p, &q);
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 0);
        assert_eq!(r.len(), q.end);
        assert_eq!(r[3] & 0x0F, 0);
    }

    #[test]
    fn known_doh_hostnames_and_subdomains_are_refused() {
        let hosts = doh_hosts();
        for name in ["dns.google", "mozilla.cloudflare-dns.com", "chrome.cloudflare-dns.com", "DNS.Quad9.NET."] {
            assert!(hosts.matching_entry(name).is_some(), "{name}");
        }
        for name in ["google.com", "www.google.com", "cloudflare.com", "example.com"] {
            assert!(hosts.matching_entry(name).is_none(), "{name} must resolve normally");
        }
    }

    #[test]
    fn site_of_collapses_subdomains_to_the_registrable_domain() {
        assert_eq!(site_of("www.pornhub.com"), "pornhub.com");
        assert_eq!(site_of("PIX-FL.phncdn.com."), "phncdn.com");
        assert_eq!(site_of("a.b.c.example.org"), "example.org");
        assert_eq!(site_of("pornhub.com"), "pornhub.com");
        assert_eq!(site_of("localhost"), "localhost");
        assert_eq!(site_of("x.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(site_of("bbc.co.uk"), "bbc.co.uk");
    }

    #[test]
    fn a_burst_of_lookups_is_one_attempt() {
        use std::time::Instant;
        let t0 = Instant::now();
        let mut s = AttemptSessions::new();
        assert!(s.observe(t0), "first lookup starts an attempt");
        for secs in [0, 1, 1, 2, 5, 9, 20] {
            assert!(!s.observe(t0 + Duration::from_secs(secs)), "same visit at +{secs}s");
        }
    }

    #[test]
    fn a_new_attempt_starts_after_a_quiet_gap() {
        use std::time::Instant;
        let t0 = Instant::now();
        let mut s = AttemptSessions::new();
        assert!(s.observe(t0));
        assert!(!s.observe(t0 + Duration::from_secs(29)));
        // 30s of silence since the last lookup (at +29s) -> a new attempt.
        assert!(s.observe(t0 + Duration::from_secs(60)));
        assert!(!s.observe(t0 + Duration::from_secs(61)));
    }

    #[test]
    fn continuous_hammering_still_becomes_multiple_attempts() {
        use std::time::Instant;
        let t0 = Instant::now();
        let mut s = AttemptSessions::new();
        let mut attempts = 0;
        // A lookup every 10s for 11 minutes: never a 30s gap.
        for i in 0..=66 {
            if s.observe(t0 + Duration::from_secs(i * 10)) {
                attempts += 1;
            }
        }
        assert_eq!(attempts, 3, "one per 5-minute burst cap");
    }

    #[test]
    fn doh_canary_gets_nxdomain() {
        let p = query_packet(DOH_CANARY, TYPE_A);
        let q = parse_question(&p).unwrap();
        let r = build_nxdomain(&p, &q);
        assert_eq!(r[3] & 0x0F, 3);
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 0);
        assert_eq!(r.len(), q.end);
    }

    #[test]
    fn bundled_list_is_non_empty() {
        let bl = Blocklist::from_domains(BUNDLED.lines());
        assert!(bl.matching_entry("www.pornhub.com").is_some());
    }
}
