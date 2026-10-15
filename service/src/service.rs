use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Duration;

use windows::core::PWSTR;
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_CALL_NOT_IMPLEMENTED, NO_ERROR};
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};

use crate::install::SERVICE_NAME;
use crate::logging::log_line;

static STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Mirrors "is there a current lock" for the control handler's benefit.
/// Updated once per tick from the DB — the control handler itself must
/// stay fast and can't safely do its own DB I/O from that callback context.
static LOCK_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Set by the control handler when it refuses a stop, cleared and logged by
/// the next tick — not logged from inside the handler itself. Kept as
/// defense-in-depth logging for the rare case a stop control still reaches
/// the handler (see the dwControlsAccepted note below for why that should
/// now be uncommon while locked).
static STOP_REFUSED: AtomicBool = AtomicBool::new(false);
/// Mirrors whatever `dwControlsAccepted` was last sent to the SCM, so the
/// tick loop only calls `SetServiceStatus` again when it actually needs to
/// flip (cheap either way, but avoids spamming it every second for no
/// reason).
static CONTROLS_ACCEPTED: AtomicBool = AtomicBool::new(true);

/// Wide, null-terminated service name buffer. Kept alive for the whole
/// process lifetime (`'static`, leaked once) since the SCM holds a pointer
/// into `SERVICE_TABLE_ENTRYW` for as long as the dispatcher is running.
fn service_name_wide() -> &'static mut [u16] {
    let mut wide: Vec<u16> = SERVICE_NAME.encode_utf16().collect();
    wide.push(0);
    Box::leak(wide.into_boxed_slice())
}

/// Blocks for the lifetime of the service — only returns once the SCM has
/// told us to stop (or dispatch itself fails, e.g. not launched by the SCM).
pub fn run() -> windows::core::Result<()> {
    let name_buf = service_name_wide();

    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name_buf.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];

    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let name_buf = service_name_wide();
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(PWSTR(name_buf.as_mut_ptr()), Some(control_handler), None)
    };

    let handle = match handle {
        Ok(h) => h,
        Err(e) => {
            log_line(&format!("RegisterServiceCtrlHandlerExW failed: {e:?}"));
            return;
        }
    };
    STATUS_HANDLE.store(handle.0 as isize, Ordering::SeqCst);

    set_status(SERVICE_START_PENDING, 0);
    log_line("Warden service starting.");

    let db_conn = match crate::db::open() {
        Ok(conn) => {
            log_line(&format!(
                "Database ready at {}.",
                crate::db::db_path().display()
            ));
            Some(conn)
        }
        Err(e) => {
            log_line(&format!("Database open failed: {e}"));
            None
        }
    };

    crate::update_server::spawn();
    crate::blocklist_refresh::spawn();

    // The DNS filter always runs (cheap, and inert until system DNS points
    // at it); `dns_enforce` decides when that happens, tied to the lock.
    let dns_upstreams = crate::dns_filter::new_upstreams(Vec::new());
    let dns_bound = crate::dns_filter::spawn(crate::dns_filter::Config {
        listen: vec![
            std::net::SocketAddr::from(([127, 0, 0, 1], crate::dns_enforce::DNS_PORT)),
            std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, crate::dns_enforce::DNS_PORT)),
        ],
        upstreams: dns_upstreams.clone(),
        allow: crate::dns_filter::new_allow(),
        on_block: crate::dns_filter::db_logging_hook(),
    })
    .unwrap_or_else(|e| {
        log_line(&format!("DNS filter failed to start: {e}"));
        Vec::new()
    });
    let mut dns_enforcer =
        crate::dns_enforce::Enforcer::new(dns_upstreams, crate::dns_enforce::DNS_PORT, &dns_bound);
    let mut browser_policy = crate::browser_policy::BrowserPolicy::new();
    let mut doh_firewall = crate::firewall::DohFirewall::new();

    set_status(SERVICE_RUNNING, SERVICE_ACCEPT_STOP);
    log_line("Warden service running.");

    let mut tick: u64 = 0;
    while !STOP_REQUESTED.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(1));
        tick += 1;

        // Logged here, not from inside control_handler — see STOP_REFUSED's
        // doc comment for why.
        if STOP_REFUSED.swap(false, Ordering::SeqCst) {
            log_line("Refused sc stop: a lock is active.");
        }

        // Expiry check every tick: a timed lock should stop as soon as
        // possible after its expiry, not whenever the next status request
        // happens to arrive.
        if let Some(conn) = &db_conn {
            match crate::lock_logic::maybe_expire(conn) {
                Ok(true) => log_line("Lock expired."),
                Ok(false) => {}
                Err(e) => log_line(&format!("Expiry check failed: {e}")),
            }
            // Also right before the registry sync, same reasoning: applying
            // a due emergency unlock ends the forever lock, and the very
            // next line should see that and relax policy immediately.
            match crate::emergency_unlock_logic::maybe_apply(conn) {
                Ok(true) => log_line("Emergency unlock applied; forever lock ended."),
                Ok(false) => {}
                Err(e) => log_line(&format!("Emergency unlock check failed: {e}")),
            }
            // Runs right after the expiry check, in the same tick, so
            // policy relaxes as soon as possible after a lock actually
            // ends rather than waiting for the next tick to notice. Also
            // self-heals: re-asserts every tick while a lock is active, not
            // just once at the start, so a manual registry edit mid-lock
            // gets corrected within ~1s (Phase 10).
            if let Err(e) = crate::registry_policy::sync(conn) {
                log_line(&format!("Registry policy sync failed: {e}"));
            }

            dns_enforcer.sync(conn, tick);
            browser_policy.sync(conn);
            doh_firewall.sync(conn, tick);

            let lock_active = crate::db::lock::get_current(conn)
                .map(|l| l.is_some())
                .unwrap_or(false);
            LOCK_ACTIVE.store(lock_active, Ordering::SeqCst);

            // Phase 14 root-cause fix for the recurring 1061 finding: don't
            // rely on the control handler's return code to refuse a stop.
            // Returning a non-NO_ERROR code from an ExtendedHandler for a
            // control that's still advertised as accepted isn't a documented
            // Win32 contract, and testing showed it reliably: after the
            // *first* such refusal, the SCM stopped delivering the control
            // to our handler at all for every subsequent stop attempt
            // (confirmed via service.log — only one "Refused sc stop" line
            // ever appeared across 8 rapid `sc stop` calls) and instead
            // rejected them itself with 1061, even minutes later at 5-10s
            // spacing, until the process was restarted. That's the SCM's own
            // documented way of saying "can't accept this control" — which
            // is actually the *correct*, standard mechanism for what we
            // want: stop advertising SERVICE_ACCEPT_STOP in
            // dwControlsAccepted while locked, and let the SCM reject the
            // stop at the door before it ever reaches our handler. The 1061
            // that produces isn't a bug — it's Windows accurately reporting
            // "this service isn't currently accepting that control", which
            // is exactly true while locked. Trade-off: a stop attempt
            // rejected this way never reaches control_handler, so it no
            // longer produces a "Refused sc stop" log line — the security
            // property (can't be stopped) gets more reliable, at the cost of
            // losing a per-attempt audit trail for exactly those attempts.
            let should_accept_stop = !lock_active;
            if CONTROLS_ACCEPTED.swap(should_accept_stop, Ordering::SeqCst) != should_accept_stop {
                set_status(
                    SERVICE_RUNNING,
                    if should_accept_stop { SERVICE_ACCEPT_STOP } else { 0 },
                );
            }
        }

        if tick % 30 == 0 {
            log_line("Warden service alive.");
        }
    }

    set_status(SERVICE_STOP_PENDING, 0);
    log_line("Warden service stopping.");
    dns_enforcer.shutdown();
    doh_firewall.shutdown();
    set_status(SERVICE_STOPPED, 0);
    log_line("Warden service stopped.");
}

/// Refuses `sc stop` (and anything else that sends SERVICE_CONTROL_STOP)
/// while a lock is active — the whole point of Phase 10: the service can't
/// be a lock's weak point just because someone runs `sc stop` from an
/// elevated prompt instead of clicking around the (nonexistent) UI control.
///
/// As of Phase 14, the primary mechanism for this is the tick loop dropping
/// SERVICE_ACCEPT_STOP from dwControlsAccepted while locked (see the
/// CONTROLS_ACCEPTED comment) — the SCM then rejects a stop attempt itself,
/// with 1061, before this handler is even invoked. This branch is now a
/// defense-in-depth fallback for the narrow race where a stop control is
/// already in flight right as a lock starts (dwControlsAccepted hasn't been
/// updated yet); it stays correct either way and costs nothing to keep.
unsafe extern "system" fn control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    if control == SERVICE_CONTROL_STOP {
        if LOCK_ACTIVE.load(Ordering::SeqCst) {
            STOP_REFUSED.store(true, Ordering::SeqCst);
            return ERROR_ACCESS_DENIED.0;
        }
        STOP_REQUESTED.store(true, Ordering::SeqCst);
        set_status(SERVICE_STOP_PENDING, 0);
        NO_ERROR.0
    } else {
        ERROR_CALL_NOT_IMPLEMENTED.0
    }
}

fn set_status(
    state: windows::Win32::System::Services::SERVICE_STATUS_CURRENT_STATE,
    controls_accepted: u32,
) {
    let handle_val = STATUS_HANDLE.load(Ordering::SeqCst);
    if handle_val == 0 {
        return;
    }
    let handle = SERVICE_STATUS_HANDLE(handle_val as _);

    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: controls_accepted,
        dwWin32ExitCode: NO_ERROR.0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 3000,
    };

    unsafe {
        let _ = SetServiceStatus(handle, &status);
    }
}
