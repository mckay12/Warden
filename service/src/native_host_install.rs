//! Registers/unregisters this exe as a Chrome Native Messaging host, so
//! `chrome.runtime.sendNativeMessage` can reach `native_messaging::run()`.
//!
//! Per-user (`HKCU`), via `reg.exe` — same "shell out rather than pull in a
//! raw Win32 registry API" choice as `install.rs` makes for the service
//! itself. Unlike the Windows Service, this doesn't need admin: Chrome reads
//! the native messaging host list from the current user's own registry
//! hive, so this can run from an ordinary (non-elevated) prompt.

use std::env;
use std::path::PathBuf;
use std::process::Command;

use crate::db::db_dir;
use crate::extension_id::EXTENSION_ID;

pub const HOST_NAME: &str = "com.warden.service";

fn manifest_path() -> PathBuf {
    db_dir().join("native-messaging-host.json")
}

fn registry_key() -> String {
    format!(r"HKCU\Software\Google\Chrome\NativeMessagingHosts\{HOST_NAME}")
}

pub fn install() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| format!("could not determine current exe path: {e}"))?;

    // Windows paths need their backslashes escaped for JSON.
    let exe_json_safe = exe.display().to_string().replace('\\', "\\\\");

    let manifest = format!(
        r#"{{
  "name": "{HOST_NAME}",
  "description": "Warden native messaging host",
  "path": "{exe_json_safe}",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://{EXTENSION_ID}/"]
}}
"#
    );

    std::fs::create_dir_all(db_dir()).map_err(|e| format!("could not create {}: {e}", db_dir().display()))?;
    std::fs::write(manifest_path(), manifest)
        .map_err(|e| format!("could not write {}: {e}", manifest_path().display()))?;

    let output = Command::new("reg.exe")
        .args([
            "add",
            &registry_key(),
            "/ve",
            "/t",
            "REG_SZ",
            "/d",
            &manifest_path().display().to_string(),
            "/f",
        ])
        .output()
        .map_err(|e| format!("failed to launch reg.exe: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "reg add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub fn uninstall() -> Result<(), String> {
    let output = Command::new("reg.exe")
        .args(["delete", &registry_key(), "/f"])
        .output()
        .map_err(|e| format!("failed to launch reg.exe: {e}"))?;

    let _ = std::fs::remove_file(manifest_path());

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "reg delete failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
