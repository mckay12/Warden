# 🛡️ Warden

**A lock you set for yourself — and can't talk yourself out of.**

[![CI](https://github.com/mckay12/Warden/actions/workflows/ci.yml/badge.svg)](https://github.com/mckay12/Warden/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/mckay12/Warden?include_prereleases&label=release)](https://github.com/mckay12/Warden/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6)

Warden blocks adult content across **every browser on your Windows PC**, and the clever (or maybe slightly evil 😈) part is the timer: once a lock has settled in, *nobody can shorten or cancel it. Not you, not future-you at 2 a.m., not even by uninstalling it or killing the process.* (There's a deliberate **5-minute undo window** right after you start one, just to catch slips — more on that below.)

It's a **commitment device**, not a parental-control suite. You install it on your own machine, for your own reasons, and you hand the keys to the version of you who made the decision on a good day. 💪

> 🙋 **Who is this for?** Adults who want to stop visiting adult sites and know that willpower alone loses at 2 a.m. It is *not* designed (or okay) to use for monitoring someone else's device.

---

## ✨ What it does

| | |
|---|---|
| 🔒 **Unshortenable lock** | Pick a duration (years, days, hours, minutes, seconds — up to 100 years) or lock forever. You can *extend* a lock any time; you can never shorten it. Cancelling is only possible in the first **5 minutes**, to catch a slip. |
| ⌨️ **Typed confirmation** | Long locks (7+ days), long extensions and "lock forever" make you **type what you're committing to** (e.g. `183 days`) before they start. |
| 🌐 **Blocks every browser** | A local DNS filter covers Chrome, Edge, Brave, Firefox and friends — **including Incognito / private windows**. |
| 🧱 **Hard to wriggle out of** | System DNS is re-pointed at the filter *every second* while locked. Delete a setting? Back in about a second. |
| 🕵️ **Closes the sneaky doors** | Secure-DNS (DoH) is switched off in browsers, known DoH/DoT servers are firewalled, Brave's Tor windows and VPN are disabled. |
| 🩹 **Fix false positives** | The adult list isn't perfect. Ask for an **exception** for a site that was blocked by mistake — it takes effect after a deliberate **24-hour cooldown**, so it can't be used on impulse. |
| 📊 **Streaks & stats** | A dashboard with your clean-day streak, weekly counts and an attempt log. One visit = one attempt (not 40 CDN lookups). |
| 🚨 **An honest escape hatch** | "Lock forever" isn't a dead end: an emergency unlock exists, with a deliberate **7-day cooldown** (cancellable while pending, and always logged). |
| 🖥️ **Proper desktop app** | Tray icon, dashboard window, start-at-login, single-instance, and a real installer. |

---

## 🧠 How it works

Warden is a few small pieces that each do one job:

```
┌──────────────────────────┐   native messaging   ┌────────────────────────────────┐
│  🖥️  Desktop app (Tauri)  │ ───────────────────► │  ⚙️  Windows service (Rust)     │
│  tray • dashboard • UI   │                      │  the source of truth           │
└──────────────────────────┘                      │                                │
                                                  │  🔒 lock & timer (SQLite)       │
┌──────────────────────────┐                      │  🌐 DNS filter  127.0.0.1:53    │
│  🧩 Chrome extension     │ ───────────────────► │  🧷 re-asserts system DNS       │
│  (optional companion)    │                      │  📜 browser policies            │
└──────────────────────────┘                      │  🔥 firewall rules              │
                                                  │  🛡️ refuses to be stopped       │
                                                  └────────────────────────────────┘
```

The **service** owns everything that matters: the lock, the timer, the blocking. The desktop app and the extension are just *windows* into it — you can close, kill or uninstall them and the lock doesn't care. 😎

### 🧅 The layers of "no"

When a lock is active, Warden stacks several defences, because any single one has a bypass:

1. **🌐 DNS filter** — a tiny local resolver. Blocked domains (and all their subdomains) get a dead-end answer (`0.0.0.0` / `::`); everything else is forwarded untouched to your real DNS. It loads the full [Block List Project](https://github.com/blocklistproject/Lists) adult list (~950,000 domains, using ~15 MB of RAM) and refreshes daily, with a small bundled list as an offline fallback.
2. **🧷 System DNS takeover** — every active network adapter is pointed at the filter, and put back *exactly* as it was when the lock ends. A self-test runs constantly: if the filter ever stops answering, Warden hands DNS back rather than leaving you offline.
3. **📜 Browser policies** — machine-level policy turns off secure DNS (DoH) in **Chrome, Edge, Brave and Firefox**, so a browser can't quietly bypass the filter. For Brave it also disables **Tor windows** and the **built-in VPN**.
4. **🔥 Firewall rules** — outbound connections to ~70 well-known public DoH/DoT resolver addresses (and DNS-over-TLS on port 853) are blocked, which covers browsers that ignore policy.
5. **🚫 DoH hostnames refused** — the filter won't resolve names like `dns.google` or `cloudflare-dns.com`, and tells Firefox's "canary domain" that secure DNS isn't welcome here.
6. **🛡️ The service protects itself** — it refuses `sc stop` while locked, ACL-hardens itself against `sc delete`, re-applies its settings every second, restarts after a crash, and survives reboots (the lock keeps its original expiry).

---

## 🚀 Getting started

### 📦 Install (the easy way)

Build or grab `Warden_0.2.0_x64-setup.exe` (see [Building](#-building-from-source)) and run it.

- It asks for admin rights (it needs to register a Windows service).
- It's **unsigned** (this is a personal project for now), so Windows SmartScreen will say "Unknown publisher" — click **More info → Run anyway**.
- It installs to `C:\Program Files\Warden`, registers the **Warden** service (starts at boot, restarts on crash) and adds a Start-menu shortcut.
- 🔁 **Updating?** Just run the new installer. It stops the service, swaps the files, and starts it again — **but it refuses while a lock is active**, because replacing the service mid-lock would be a way around it. 😉

### 🎮 Using it

1. Open **Warden** (tray icon → *Open Warden*, or the Start menu).
2. Click **Start a lock**, set a duration in the **Years / Days / Hours / Mins / Secs** boxes, press **Start**.
   - A lock of **7 days or more** (and "Lock forever") asks you to **type what you're committing to** — it tells you exactly when it ends.
3. Live your life. 🌱 Blocked lookups show up in the **attempt log** and your streak resets only if you actually try.
4. Started the wrong one? For the **first 5 minutes** there's an **Undo this lock** button (with a live countdown) — a slip-catcher, not a way out. Once the 5 minutes pass, or as soon as you extend the lock, it's gone for good.
5. Need more time? **+ Extend lock** lets you add any amount. There's deliberately **no** button to cut a lock short once it has settled — that's the whole point.

> 🖱️ Closing the window with **✕** only hides it to the tray. To fully quit the app, use the tray menu → *Quit app (lock stays active)*. The lock never depends on the app being open.

---

## 🩹 When the blocklist gets it wrong

The downloaded adult-domain list is community-compiled, so it occasionally blocks something innocent (a directory of anime sites, a placeholder-image service for developers…). Warden has a way to deal with that **without** giving you an instant "unblock" button to use at 2 a.m.:

1. In the dashboard, click **Not adult?** next to the attempt, or type a domain in the **Exceptions** card. *(Pasting a whole URL is fine — it gets tidied up.)*
2. Confirm. The exception is now **pending** and the site **stays blocked** for **24 hours**.
3. After the cooldown it's allowed — the exception covers that domain and its subdomains. Exempted sites also stop counting as attempts.
4. **Cancelling** a pending exception or **removing** an active one is *instant* — that only ever makes blocking stricter. 🔒

A few guard rails: a site's exception can't override a *more specific* listing (excepting `tumblr.com` won't unblock a listed `bad.tumblr.com`), you can have at most 25 open at once, IPs and overly broad suffixes like `co.uk` are refused, and it all works **during a lock** with no reinstall.

---

## 🔧 Handy commands

The service doubles as a little CLI (run from an admin prompt in `C:\Program Files\Warden`):

```powershell
warden-service dns-status      # 🔍 what's DNS / policy / firewall doing right now?
warden-service dns-restore     # 🆘 emergency: hand DNS back (refused while a lock is active & the service is running)
warden-service dns-serve --port 5300   # 🧪 run the DNS filter on a spare port to poke at it (touches nothing)
warden-service refresh-blocklist       # 🔄 force a blocklist refresh now
warden-service install | uninstall     # ⚙️ (un)register the service (uninstall refuses during a lock)
```

📁 Runtime data lives in `%ProgramData%\Warden` (`warden.db`, `service.log`, `dns_blocklist.txt`).

---

## 🏗️ Building from source

You'll need **Rust** (stable), **Node.js** + npm, and **WebView2** (already on Windows 11).

```powershell
# 1) the service (release build gets bundled into the installer)
cd service
cargo test
cargo build --release

# 2) the desktop app + installer
cd ../app
npm install
npx tauri build          # → app/src-tauri/target/release/bundle/nsis/Warden_0.2.0_x64-setup.exe
```

For quick iteration: `npx tauri build --debug --no-bundle` gives you a debug `warden-desktop.exe`.

### 🚢 Releasing a new version (GitHub does the building)

Pushing a version tag makes GitHub Actions build the installer on a clean Windows machine and attach it to a **draft pre-release** — you review it and press *Publish*. 🎯

```powershell
# 1) bump the version in app/src-tauri/tauri.conf.json AND app/src-tauri/Cargo.toml (and service/Cargo.toml)
# 2) commit, then:
git tag v0.2.0
git push origin v0.2.0
```

- The workflow checks the tag matches the app version, builds the service and the installer, and adds the SHA-256 checksum to the draft's notes. 🔐
- Want a **dry run** first? *Actions → Release → Run workflow* with the tag left empty just builds the installer and keeps it as a downloadable workflow artifact — nothing gets published.
- Every push and pull request also runs the **CI** workflow: the service's Rust tests plus the UI logic tests (`node --test`). ✅

<details>
<summary>🐛 <b>Build gotchas I hit (so you don't have to)</b></summary>

- **"The process cannot access the file" mid-build** — antivirus nibbling fresh build artifacts. Just retry, or build with `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`.
- **Silent installs from Git Bash** — Git Bash rewrites `/S` into a path. Run `Warden_..._setup.exe /S` from **PowerShell** instead.
- **`app/ui/` is generated.** `npm run sync-ui` copies the popup/dashboard from `extension/`. Edit the UI in `extension/`, never in `app/ui/` (except `desktop-shim.js` and `desktop-late.js`, which are hand-written).
- **Replacing the service exe** — it can't be overwritten while running; the installer stops it first. Doing it by hand? `sc stop WardenService` (only possible when no lock is active).

</details>

---

## 🗂️ What's where

```
Warden/
├── 🦀 service/     Rust Windows service — lock, DNS filter, enforcement, SQLite, native-messaging host
├── 🖥️ app/         Tauri desktop app (tray + dashboard) and the NSIS installer config
├── 🧩 extension/   Chrome MV3 extension + the shared popup/dashboard UI (the single source of truth for UI)
├── 🎨 prototype/   Static HTML mockups from the design phase
└── 📚 Warden_*.md  PRD, ERD, UI/UX spec and build plan
```

Each folder has its own README with the deep dive — the [service README](service/README.md) in particular is a very honest build diary. 📓

---

## 🧪 Tested for real

This wasn't just "it compiles". On a real Windows 11 machine I verified: DNS takeover and exact restore · blocking through the normal Windows resolver (0 leaks in 40 lookups) · real Chrome, Edge and Brave reporting the policies as applied · a lock surviving a **reboot** · tamper attempts (`sc stop`, `sc delete`, resetting DNS by hand, deleting policies and firewall rules, running the uninstaller mid-lock, running the installer mid-lock) · an in-place upgrade · and a full uninstall → reinstall. The service has 84 unit tests (`cargo test`), the UI logic has its own (`node --test`), and the exception flow was exercised end to end against an isolated service instance (block → request → cooldown → unblock → remove).

---

## ⚠️ Honest limitations

Warden is a *commitment device*, not an impenetrable fortress. Please know what it isn't:

- 🪟 **Windows only**, and **browsers only** (by design — it's DNS-level, so other apps aren't covered).
- 🌍 **Browsers that ignore machine policy** (Opera, Vivaldi, Tor Browser…) can still use their own secure DNS. The firewall layer helps, but only for the resolvers it knows about.
- 🔌 **VPNs and proxy extensions** (outside Brave's, which is disabled) can carry DNS past the filter.
- 🦊 **Firefox** policy is written but I couldn't test it (no Firefox on the dev machine).
- 🔗 **No URL/keyword matching.** DNS only sees domain names, so a page like `/wiki/Hentai` on a normal site isn't caught. The Chrome extension can still do keyword matching as an optional extra.
- 🔥 If the **Windows Firewall service itself is off**, the firewall layer is inert.
- 🧑‍💻 Someone with admin rights, enough time and bad intentions can eventually defeat *any* local software. Warden's job is to make that hard enough that the impulse passes. 🌊
- ✍️ **Unsigned installer** for now — SmartScreen will nag you.

And a gentle note: Warden is a tool, not therapy. If this is a struggle, talking to someone you trust (or a professional) is a strength, not a failure. 💙

---

## 🛣️ What's next

- 🧾 A signed installer (before sharing it widely)
- 🔔 Extra friction / reminders around the "Lock forever" flow
- 🧹 Smarter grouping and nicer stats on the dashboard
- 📮 One-click "report this false positive" to the upstream blocklist
- 🌍 Firefox verification and more browser coverage

---

## 🤝 Contributing & license

Issues and ideas are very welcome! This started as a personal project, so things are opinionated. 🙂

📄 **License:** [MIT](LICENSE) — free to use, change and share, just keep the copyright notice. 🎉

> 🧾 The domain blocklist Warden downloads is the community-maintained [Block List Project](https://github.com/blocklistproject/Lists); it's fetched at runtime and **not** bundled here (apart from a 5-domain offline fallback), so it keeps its own license.

<p align="center">Built with 🦀 Rust, ⚡ Tauri, and a stubborn belief that the best willpower is the kind you set up in advance.</p>
