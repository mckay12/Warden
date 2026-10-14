// Never show a console window (debug builds included — this is a tray app).
#![windows_subsystem = "windows"]

//! Warden desktop app: a tray icon plus window in front of the Warden Windows
//! service. It owns no lock state — everything goes through the service's
//! native-messaging host (the same length-prefixed JSON protocol the Chrome
//! extension uses), so the service stays the single source of truth and this
//! app can be closed, killed or uninstalled without touching an active lock.

use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SERVICE_NAME: &str = "WardenService";
/// Any `chrome-extension://` origin makes the service run as a native host.
const HOST_ORIGIN: &str = "chrome-extension://warden-desktop/";
const TRAY_ID: &str = "main";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Warden";
/// Passed by the login entry so the app starts in the tray, window hidden.
const MINIMIZED_FLAG: &str = "--minimized";

fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("reg.exe").args(args).creation_flags(CREATE_NO_WINDOW).output()
}

fn autostart_enabled() -> bool {
    reg(&["query", RUN_KEY, "/v", RUN_VALUE]).map(|o| o.status.success()).unwrap_or(false)
}

/// Run-at-login via the per-user Run key: no admin needed, and it only ever
/// starts this tray app — the service starts itself at boot regardless.
fn set_autostart(on: bool) -> Result<(), String> {
    let out = if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command = format!("\"{}\" {MINIMIZED_FLAG}", exe.display());
        reg(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f"])
    } else {
        reg(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])
    }
    .map_err(|e| e.to_string())?;
    // Deleting a value that isn't there is fine.
    if out.status.success() || !on {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Where the installed service's executable is. Asked of the Service Control
/// Manager's own registry entry rather than guessed, so it follows the
/// service wherever it was installed. `WARDEN_SERVICE_EXE` overrides it.
fn service_exe() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("WARDEN_SERVICE_EXE") {
        return Ok(PathBuf::from(p));
    }
    let out = Command::new("reg.exe")
        .args([
            "query",
            &format!(r"HKLM\SYSTEM\CurrentControlSet\Services\{SERVICE_NAME}"),
            "/v",
            "ImagePath",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("could not query the service registration: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    // ImagePath    REG_EXPAND_SZ    "C:\path\warden-service.exe" run
    let path = text
        .split('"')
        .nth(1)
        .ok_or_else(|| "Warden service is not installed (no ImagePath found).".to_string())?;
    Ok(PathBuf::from(path))
}

/// One request/response round trip with the service's native host.
fn talk_to_service(message: &Value) -> Result<Value, String> {
    let exe = service_exe()?;
    let mut child = Command::new(&exe)
        .arg(HOST_ORIGIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;

    let body = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    {
        let stdin = child.stdin.as_mut().ok_or("no stdin")?;
        stdin.write_all(&(body.len() as u32).to_le_bytes()).map_err(|e| e.to_string())?;
        stdin.write_all(&body).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
    }

    let stdout = child.stdout.as_mut().ok_or("no stdout")?;
    let mut len = [0u8; 4];
    stdout.read_exact(&mut len).map_err(|e| format!("no response from service: {e}"))?;
    let mut resp = vec![0u8; u32::from_le_bytes(len) as usize];
    stdout.read_exact(&mut resp).map_err(|e| format!("truncated response: {e}"))?;

    drop(child.stdin.take()); // EOF ends the host's loop
    let _ = child.wait();
    serde_json::from_slice(&resp).map_err(|e| format!("bad response: {e}"))
}

/// Everything the UI asks of the service goes through here.
#[tauri::command]
async fn native_message(message: Value) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || talk_to_service(&message))
        .await
        .map_err(|e| e.to_string())?
}

/// Opens (or focuses) a second window, e.g. the dashboard. `async` on
/// purpose: creating a window from a synchronous command can deadlock on
/// Windows.
#[tauri::command]
async fn open_page(app: AppHandle, page: String) -> Result<(), String> {
    let page = page.trim_start_matches('/').to_string();
    let label = page.trim_end_matches(".html").replace(['.', '/'], "_");
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(page.into()))
        .title("Warden — Dashboard")
        .inner_size(900.0, 720.0)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn tooltip_for(status: &Value) -> String {
    if status.get("ok").and_then(Value::as_bool) != Some(true) {
        return "Warden — service unreachable".to_string();
    }
    let Some(lock) = status.get("lock").filter(|l| !l.is_null()) else {
        return "Warden — no lock active".to_string();
    };
    if lock.get("is_forever").and_then(Value::as_bool) == Some(true) {
        return "Warden — locked (forever)".to_string();
    }
    let until = lock
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Local).format("%b %d, %H:%M").to_string());
    match until {
        Some(t) => format!("Warden — locked until {t}"),
        None => "Warden — locked".to_string(),
    }
}

fn main() {
    // Headless helper for scripting/testing: `warden-desktop.exe --set-autostart on|off`.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--set-autostart") {
        let on = args.get(i + 1).map(String::as_str) == Some("on");
        match set_autostart(on) {
            Ok(()) => println!("run at login: {}", if on { "on" } else { "off" }),
            Err(e) => {
                eprintln!("could not change run-at-login: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let start_hidden = args.iter().any(|a| a == MINIMIZED_FLAG);

    tauri::Builder::default()
        // Must be registered first. A second launch just brings the first
        // instance's window forward instead of starting another copy.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .invoke_handler(tauri::generate_handler![native_message, open_page])
        .on_window_event(|window, event| {
            // Closing the main window hides it to the tray; the service (and
            // any active lock) is unaffected either way.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(move |app| {
            let open = MenuItem::with_id(app, "open", "Open Warden", true, None::<&str>)?;
            let autostart = CheckMenuItem::with_id(
                app,
                "autostart",
                "Start at login",
                true,
                autostart_enabled(),
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit app (lock stays active)", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &autostart, &quit])?;
            let autostart_item = autostart.clone();

            TrayIconBuilder::with_id(TRAY_ID)
                .icon(app.default_window_icon().cloned().ok_or("no window icon")?)
                .tooltip("Warden")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "autostart" => {
                        // The item has already flipped itself; make the
                        // registry match it, or flip it back on failure.
                        let want = autostart_item.is_checked().unwrap_or(false);
                        if set_autostart(want).is_err() {
                            let _ = autostart_item.set_checked(!want);
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // The window is created hidden; show it unless this is a
            // login-time launch that should stay in the tray.
            if !start_hidden {
                show_main(app.handle());
            }

            // Keep the tray tooltip current without the window being open.
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                let status = talk_to_service(&json!({ "type": "get_status" }))
                    .unwrap_or_else(|_| json!({ "ok": false }));
                if let Some(tray) = handle.tray_by_id(TRAY_ID) {
                    let _ = tray.set_tooltip(Some(tooltip_for(&status)));
                }
                std::thread::sleep(Duration::from_secs(5));
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Warden desktop");
}
