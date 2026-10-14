//! Chrome policy enforcement (Phase 9) — tying an active lock to two real
//! Chrome policies, written to `HKLM\SOFTWARE\Policies\Google\Chrome` via
//! `reg.exe` (same shelling-out convention as the rest of this codebase).
//! This *requires* the service's `LocalSystem` privileges: HKLM policy keys
//! aren't writable by a normal user, which is exactly why this can't live
//! in `native_messaging.rs` (that process runs as the logged-in user,
//! inherited from Chrome) — it's driven from `service.rs`'s own tick loop
//! instead, polling DB lock state the same way `lock_logic::maybe_expire`
//! does.
//!
//! Two corrections to the PRD's original plan (§7.2), found while
//! researching the actual current policy schema before writing this:
//!
//! 1. There is no per-extension "allow in Incognito" policy — Chrome only
//!    exposes `IncognitoModeAvailability` (whole-browser: available /
//!    disabled / forced). Disabling Incognito entirely while a lock is
//!    active is blunter than the PRD's wording but actually closes the
//!    bypass more robustly than a hypothetical per-extension toggle would.
//! 2. `installation_mode: force_installed` requires a packaged, versioned
//!    `.crx` plus an update-manifest URL Chrome polls — it cannot target a
//!    "Load unpacked" dev extension. See `extension.crx` and
//!    `update_manifest.xml` in this same `%ProgramData%\Warden` directory
//!    (built via `msedge --pack-extension`; see `service/README.md`).
//!
//! Safety: before ever touching either registry value, the pre-existing
//! value (if any) is saved to `policy_backup.json`. Relaxing always
//! restores that exact prior value (or deletes the value if there wasn't
//! one) rather than trying to "un-merge" — correct regardless of which
//! `registry_policy_merge_mode` was used to apply it.
//!
//! Phase 10: `sync()` doesn't just apply once at the start of a lock — it
//! re-asserts every tick for as long as the lock is active. A direct
//! registry edit (or delete) of either value mid-lock gets corrected within
//! about a second, the same cadence `lock_logic::maybe_expire` already
//! polls at, rather than silently staying tampered-with until the lock
//! happens to end on its own.
//!
//! Known limitation, not fixed by any of this: `force_installed` itself
//! doesn't currently take effect at all on this (unmanaged) machine — see
//! **Phase 9 findings** in `service/README.md`. Self-healing a policy value
//! that Chrome isn't acting on yet doesn't block extension removal; it just
//! means whichever real fix gets chosen later (Web Store publish, or Chrome
//! Browser Cloud Management enrollment) will have this self-healing already
//! in place for free.

use std::path::PathBuf;
use std::process::Command;

use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::db;
use crate::db::service_config;
use crate::extension_id::EXTENSION_ID;

const CHROME_POLICY_KEY: &str = r"HKLM\SOFTWARE\Policies\Google\Chrome";
const EXTENSION_SETTINGS_VALUE: &str = "ExtensionSettings";
const INCOGNITO_VALUE: &str = "IncognitoModeAvailability";
/// `file://` was tried first and Chrome silently rejected it (Warning
/// status, never showed up in "Extension Install Policies") — see
/// `update_server.rs` for the local HTTP server this now points at instead.
fn update_manifest_url() -> String {
    format!("http://127.0.0.1:{}/update_manifest.xml", crate::update_server::PORT)
}

fn backup_path() -> PathBuf {
    db::db_dir().join("policy_backup.json")
}

fn run_reg(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("reg.exe").args(args).output()
}

/// Raw text of a registry value via `reg query`, or `None` if the key/value
/// doesn't exist. Works for REG_SZ and REG_DWORD alike — a DWORD comes back
/// as `"0x1"`, which `reg add /t REG_DWORD /d` also happily accepts, so it
/// round-trips through `apply`/`relax` without needing to parse it further.
fn query_value(value_name: &str) -> Option<String> {
    let output = run_reg(&["query", CHROME_POLICY_KEY, "/v", value_name]).ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(value_name) {
            let rest = rest.trim();
            if let Some(data) = rest.strip_prefix("REG_SZ") {
                return Some(data.trim().to_string());
            }
            if let Some(data) = rest.strip_prefix("REG_DWORD") {
                return Some(data.trim().to_string());
            }
        }
    }
    None
}

fn write_sz(value_name: &str, data: &str) -> Result<(), String> {
    let output = run_reg(&[
        "add",
        CHROME_POLICY_KEY,
        "/v",
        value_name,
        "/t",
        "REG_SZ",
        "/d",
        data,
        "/f",
    ])
    .map_err(|e| format!("failed to launch reg.exe: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "reg add {value_name} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn write_dword(value_name: &str, data: &str) -> Result<(), String> {
    let output = run_reg(&[
        "add",
        CHROME_POLICY_KEY,
        "/v",
        value_name,
        "/t",
        "REG_DWORD",
        "/d",
        data,
        "/f",
    ])
    .map_err(|e| format!("failed to launch reg.exe: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "reg add {value_name} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn delete_value(value_name: &str) -> Result<(), String> {
    let output = run_reg(&["delete", CHROME_POLICY_KEY, "/v", value_name, "/f"])
        .map_err(|e| format!("failed to launch reg.exe: {e}"))?;
    // A "not found" failure is fine when deleting — there's nothing to clean up.
    if output.status.success() || String::from_utf8_lossy(&output.stderr).contains("unable to find") {
        Ok(())
    } else {
        Err(format!(
            "reg delete {value_name} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn read_backup() -> Option<Value> {
    let text = std::fs::read_to_string(backup_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_backup(lock_id: i64, extension_settings_prev: Option<&str>, incognito_prev: Option<&str>) -> Result<(), String> {
    let backup = json!({
        "lock_id": lock_id,
        "extension_settings_prev": extension_settings_prev,
        "incognito_prev": incognito_prev,
    });
    std::fs::write(
        backup_path(),
        serde_json::to_string_pretty(&backup).unwrap_or_default(),
    )
    .map_err(|e| format!("could not write {}: {e}", backup_path().display()))
}

/// Applies the policy, and — this is the point of Phase 10 — keeps
/// re-applying it every tick for as long as the lock stays active, not just
/// once. A direct registry edit deleting or altering either value mid-lock
/// gets corrected within one tick (~1s), rather than only being noticed
/// (and never fixed) whenever the lock happens to end.
fn apply_or_reassert(lock_id: i64, merge_mode: &str) -> Result<(), String> {
    if !backup_path().exists() {
        // First tick this lock is active: capture what was there *before*
        // touching anything, so relax() can restore it exactly later,
        // regardless of merge_mode or how many times we've since
        // re-asserted over top of it.
        let existing_settings = query_value(EXTENSION_SETTINGS_VALUE);
        let existing_incognito = query_value(INCOGNITO_VALUE);
        write_backup(lock_id, existing_settings.as_deref(), existing_incognito.as_deref())?;
    }

    // Re-read "current" every call (not the original backup) so merge mode
    // still picks up other extensions' entries added *during* the lock —
    // our own entry always wins if something tries to remove or alter it.
    let existing_settings = query_value(EXTENSION_SETTINGS_VALUE);

    let our_entry = json!({
        "installation_mode": "force_installed",
        "update_url": update_manifest_url(),
    });

    let new_settings = if merge_mode == "overwrite" {
        let mut map = Map::new();
        map.insert(EXTENSION_ID.to_string(), our_entry);
        Value::Object(map)
    } else {
        // "merge" (default, per service_config): preserve any existing
        // entries for other extensions/sources, just add/replace ours.
        let mut map = match existing_settings
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        map.insert(EXTENSION_ID.to_string(), our_entry);
        Value::Object(map)
    };

    write_sz(EXTENSION_SETTINGS_VALUE, &new_settings.to_string())?;
    write_dword(INCOGNITO_VALUE, "1")?;
    Ok(())
}

fn relax_policy() -> Result<(), String> {
    let Some(backup) = read_backup() else {
        return Ok(()); // nothing applied, nothing to relax
    };

    match backup.get("extension_settings_prev").and_then(Value::as_str) {
        Some(prev) => write_sz(EXTENSION_SETTINGS_VALUE, prev)?,
        None => delete_value(EXTENSION_SETTINGS_VALUE)?,
    }
    match backup.get("incognito_prev").and_then(Value::as_str) {
        Some(prev) => write_dword(INCOGNITO_VALUE, prev)?,
        None => delete_value(INCOGNITO_VALUE)?,
    }

    let _ = std::fs::remove_file(backup_path());
    Ok(())
}

/// Call every service tick. Applies policy the first tick a lock is seen
/// active with none applied yet; relaxes it the first tick no lock is
/// active but policy is still applied. Idempotent otherwise.
pub fn sync(conn: &Connection) -> Result<(), String> {
    let current = db::lock::get_current(conn).map_err(|e| e.to_string())?;

    match current {
        Some(lock) => {
            let config = service_config::get(conn).map_err(|e| e.to_string())?;
            apply_or_reassert(lock.id, &config.registry_policy_merge_mode)
        }
        None if backup_path().exists() => relax_policy(),
        None => Ok(()),
    }
}
