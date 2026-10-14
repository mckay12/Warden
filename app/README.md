# Warden desktop app

Tray icon + window in front of the Warden Windows service (Tauri v2).

It owns **no lock state**. Every request goes to the installed service through
its native-messaging host (the same length-prefixed JSON protocol the Chrome
extension uses), so the app can be closed, killed or uninstalled without
affecting an active lock.

## How it fits together

- `ui/` is **generated** from `../extension/` by `scripts/sync-ui.mjs` (popup +
  dashboard), with `desktop-shim.js` injected. The shim fakes the few `chrome.*`
  APIs those pages use and forwards messages to Rust. Edit the UI in
  `../extension/`, not here.
- `src-tauri/src/main.rs`:
  - `native_message` — spawns the service exe as a native host for one
    request/response round trip. The exe path is read from the service's own
    registration (`HKLM\...\Services\WardenService\ImagePath`); set
    `WARDEN_SERVICE_EXE` to override.
  - `open_page` — opens the dashboard as a second window.
  - Tray: left-click opens the window; tooltip shows the lock state (refreshed
    every 5s). Closing the window hides it to the tray; "Quit app" exits the
    app only — the service and any lock keep running.

## Build / run

```
npm install
npm run build        # runs sync-ui, then `tauri build --no-bundle`
```

Use `tauri build --debug --no-bundle` for a faster debug build. If cargo fails
with "process cannot access the file" (antivirus touching fresh build
artifacts), retry, or build with `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`.

The Warden service must be installed and running (`warden-service install`).

## Behaviour

- The main window **is the dashboard** (lock card, streak, weekly counts, attempt log).
  The old popup page is no longer shown in the app; `open_page` remains for opening
  extra windows.
- **Single instance:** launching a second copy just brings the first one's window forward.
- **Start at login:** tray menu -> "Start at login" (per-user Run key, no admin). Login
  launches pass `--minimized`, so the app starts in the tray with the window hidden.
  Scriptable: `warden-desktop.exe --set-autostart on|off`.

## Installer

`cargo build --release` in `../service`, then `npx tauri build` here, produces
`src-tauri/target/release/bundle/nsis/Warden_0.1.0_x64-setup.exe` (unsigned, so
SmartScreen shows "Run anyway"). It is per-machine (UAC), bundles
`warden-service.exe` next to the app, and its hooks (`src-tauri/installer-hooks.nsh`):

- after install: run `warden-service.exe install` — registers the service, or
  re-points an existing registration at the installed exe (an upgrade). Refuses
  while a lock is active.
- before uninstall: run `warden-service.exe uninstall`, which refuses while a lock
  is active; the uninstaller aborts if it does.

Not yet run end to end on a real machine.

## Not done yet

- A native duration picker for "Start a lock" (the extension's seconds field is reused).
- Code signing (deliberately skipped while this is for personal use).
