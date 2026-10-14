use std::env;
use std::path::PathBuf;
use std::process::{Command, Output};

pub const SERVICE_NAME: &str = "WardenService";
const DISPLAY_NAME: &str = "Warden";
const DESCRIPTION: &str = "Warden self-imposed lock enforcement service.";

/// The specific ACE Phase 10 adds/removes: denies Administrators the
/// DELETE ("SD" in SDDL) right on the service object. Deliberately leaves
/// WRITE_DAC alone, so an admin *can* still reset the security descriptor
/// and then delete — two deliberate steps instead of one `sc delete`,
/// raising the bar rather than making the service permanently unremovable
/// (see PRD §11 "recovery from total lockout" — a real, if inconvenient,
/// path out is required, not a true dead end).
const DENY_DELETE_ACE: &str = "(D;;SD;;;BA)";

fn exe_path() -> Result<PathBuf, String> {
    env::current_exe().map_err(|e| format!("could not determine current exe path: {e}"))
}

fn run_sc(args: &[&str]) -> Result<Output, String> {
    Command::new("sc.exe")
        .args(args)
        .output()
        .map_err(|e| format!("failed to launch sc.exe: {e}"))
}

fn check(output: Output, action: &str) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{action} failed (exit {:?}): {} {}",
            output.status.code(),
            stdout.trim(),
            stderr.trim()
        ))
    }
}

/// Adds `DENY_DELETE_ACE` as the first ACE in the service's DACL — Windows
/// evaluates DACL entries in order, so a DENY listed before the existing
/// ALLOW (which grants Administrators DELETE among other rights) wins for
/// that specific right. Reads the *current* descriptor via `sc sdshow`
/// rather than hardcoding one, since the default SDDL can vary across
/// Windows versions.
///
/// Found empirically (Phase 10) that this is necessary at all: `sc stop`
/// can be refused from inside the running service (`service::control_handler`
/// checks lock state), but `sc delete` is a *different* SCM API that never
/// reaches the service process — marking a running service "pending
/// deletion" succeeds regardless of lock state, and finalizes the moment
/// the process next stops for any reason (including a plain `taskkill /F`,
/// which was tested and confirmed: the service was gone afterward, no
/// crash-recovery possible since there was nothing left to recover). This
/// ACL is what actually closes that gap.
fn harden_delete_protection() -> Result<(), String> {
    let sdshow = run_sc(&["sdshow", SERVICE_NAME])?;
    if !sdshow.status.success() {
        return Err(format!(
            "sc sdshow failed: {}",
            String::from_utf8_lossy(&sdshow.stderr).trim()
        ));
    }
    let current = String::from_utf8_lossy(&sdshow.stdout).trim().to_string();

    if current.contains(DENY_DELETE_ACE) {
        return Ok(()); // already hardened
    }

    // Insert our whole ACE (both its own parens) right after "D:" — the
    // existing DACL's first ACE keeps its own "(...)" untouched immediately
    // after ours.
    let Some(dacl_start) = current.find("D:") else {
        return Err(format!("could not find a DACL in security descriptor: {current}"));
    };
    let mut hardened = current.clone();
    hardened.insert_str(dacl_start + "D:".len(), DENY_DELETE_ACE);

    let sdset = run_sc(&["sdset", SERVICE_NAME, &hardened])?;
    check(sdset, "sc sdset")
}

/// The inverse: strips `DENY_DELETE_ACE` if present, so `sc delete` (called
/// right after this, in `uninstall()`) can actually succeed. This is the
/// "deliberate two-step" — a script or casual `sc delete` alone still
/// fails; only this tool's own `uninstall` command (or the same manual
/// `sc sdset` + `sc delete` sequence) can complete it.
fn soften_delete_protection() -> Result<(), String> {
    let sdshow = run_sc(&["sdshow", SERVICE_NAME])?;
    if !sdshow.status.success() {
        return Ok(()); // service doesn't exist (or already gone) — nothing to soften
    }
    let current = String::from_utf8_lossy(&sdshow.stdout).trim().to_string();
    let softened = current.replace(DENY_DELETE_ACE, "");
    if softened == current {
        return Ok(()); // wasn't hardened
    }
    let sdset = run_sc(&["sdset", SERVICE_NAME, &softened])?;
    check(sdset, "sc sdset (soften)")
}

fn service_exists() -> bool {
    // `sc query` exits non-zero (1060) when the service isn't registered.
    run_sc(&["query", SERVICE_NAME]).map(|o| o.status.success()).unwrap_or(false)
}

/// Waits (briefly) for the service to report STOPPED after an `sc stop`.
fn wait_until_stopped() -> Result<(), String> {
    for _ in 0..40 {
        let out = run_sc(&["query", SERVICE_NAME])?;
        if !out.status.success() || String::from_utf8_lossy(&out.stdout).contains("STOPPED") {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err("the service did not stop in time".to_string())
}

/// Registers the service with the SCM (auto-start, restart-on-crash),
/// hardens it against `sc delete` (Phase 10), and starts it. Requires an
/// elevated (Administrator) process — `sc.exe` surfaces "Access is denied"
/// via `check()` otherwise.
///
/// Safe to run again over an existing install (an upgrade, or moving the
/// service to a new path): it re-points the registration at this exe and
/// restarts it. It refuses while a lock is active, for the same reason the
/// service refuses `sc stop` then — replacing the binary would be a way
/// around the lock.
pub fn install() -> Result<(), String> {
    let exe = exe_path()?;
    let bin_path = format!("\"{}\" run", exe.display());

    let already_installed = service_exists();
    if already_installed {
        if let Ok(conn) = crate::db::open() {
            if let Ok(Some(lock)) = crate::db::lock::get_current(&conn) {
                return Err(format!(
                    "cannot replace the service: a lock is active (status: {}).",
                    lock.status
                ));
            }
        }
        // Best-effort: it may already be stopped.
        let _ = run_sc(&["stop", SERVICE_NAME]);
        wait_until_stopped()?;
    }

    let create = run_sc(&[
        if already_installed { "config" } else { "create" },
        SERVICE_NAME,
        "binPath=",
        bin_path.as_str(),
        "start=",
        "auto",
        "obj=",
        "LocalSystem",
        "DisplayName=",
        DISPLAY_NAME,
    ])?;
    check(create, if already_installed { "sc config" } else { "sc create" })?;

    let desc = run_sc(&["description", SERVICE_NAME, DESCRIPTION])?;
    check(desc, "sc description")?;

    // Restart-on-crash: 5s, 5s, then 60s backoff; reset the failure count
    // after a day of continuous uptime so old crashes don't linger.
    let failure = run_sc(&[
        "failure",
        SERVICE_NAME,
        "reset=",
        "86400",
        "actions=",
        "restart/5000/restart/5000/restart/60000",
    ])?;
    check(failure, "sc failure")?;

    // Without this, SCM only treats a non-zero *process crash* as a
    // failure, not a clean-exit-with-error-code — flip it on so both count.
    let flag = run_sc(&["failureflag", SERVICE_NAME, "1"])?;
    check(flag, "sc failureflag")?;

    harden_delete_protection()?;

    let start = run_sc(&["start", SERVICE_NAME])?;
    check(start, "sc start")?;

    Ok(())
}

/// Phase 10: refuses outright if a lock is active. This is the friendly
/// path's guard — the real protection is `service::control_handler`
/// refusing `SERVICE_CONTROL_STOP` directly, and `harden_delete_protection`
/// blocking `sc delete` unconditionally (both cover raw `sc.exe` use that
/// bypasses this command entirely).
pub fn uninstall() -> Result<(), String> {
    // Nothing registered (e.g. already removed by hand): that's the state
    // the caller wants, and failing here would wedge an uninstaller forever.
    if !service_exists() {
        return Ok(());
    }

    if let Ok(conn) = crate::db::open() {
        if let Ok(Some(lock)) = crate::db::lock::get_current(&conn) {
            return Err(format!(
                "cannot uninstall: a lock is active (status: {}). Wait for it to expire, or use the emergency unlock flow.",
                lock.status
            ));
        }
    }

    soften_delete_protection()?;

    // Best-effort: fails harmlessly if the service isn't currently running.
    let _ = run_sc(&["stop", SERVICE_NAME]);

    let delete = run_sc(&["delete", SERVICE_NAME])?;
    check(delete, "sc delete")
}
