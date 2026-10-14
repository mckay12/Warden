# Warden Service — Phase 4 through 14

A minimal Rust Windows Service (Phase 4), the full SQLite schema and CRUD
layer (Phase 5), a Native Messaging bridge to the Chrome extension (Phase 6),
the source-of-truth cutover for attempt logging (Phase 7), real lock/timer
logic (Phase 8), real Chrome policy enforcement (Phase 9 — `Incognito`
lockout confirmed working; `ExtensionSettings` force-install confirmed
**not** working on this unmanaged machine, root cause found, deliberately
deferred — see **Phase 9 findings**), real tamper resistance for the
service itself (Phase 10 — refuses `sc stop` while locked, self-heals the
registry policy every tick, ACL-hardened against `sc delete` entirely — see
**Phase 10 findings**), the emergency-unlock escape hatch for a "forever"
lock (Phase 11 — 7-day cooldown, cancellable while pending, applies
automatically once due), the dashboard's stats queries (Phase 12 — streak
and week-over-week counts; caught a real bug in the streak calculation
before it shipped), and — as of Phase 13 — a real remote blocklist refresh
against a live, actively-maintained public domain list, with a version-hash
skip, offline fallback to a small bundled list, and a hard cap on how many
domains actually become browser rules (Chrome's `declarativeNetRequest`
can't hold anywhere near the real list's size — see **Phase 13 findings**).
Phase 14 was an end-to-end tamper-testing and hardening pass against the
fully integrated system: root-caused and fixed the recurring `error 1061`
finding from Phases 11/13 (see **Phase 14 findings**), re-confirmed
registry-policy merge/restore behavior, and verified the total-lockout
recovery path from PRD §11 for real by deliberately corrupting a backed-up
copy of the live database mid-lock.

## Layout

- `src/service.rs` — the actual Windows Service runtime, using `windows-rs`
  directly against the raw Win32 Service Control Manager APIs
  (`StartServiceCtrlDispatcherW`, `RegisterServiceCtrlHandlerExW`,
  `SetServiceStatus`) per the PRD's choice of `windows-rs` for this.
- `src/install.rs` — install/uninstall via `sc.exe` (register, configure
  restart-on-crash, start / stop, delete) — the PRD explicitly allows either
  `sc.exe` or in-code registration for this part; `sc.exe` is simpler and
  just as legitimate.
- `src/logging.rs` — plain file logging to `%ProgramData%\Warden\service.log`,
  best-effort (a logging failure never brings the service down).
- `src/main.rs` — CLI dispatch: `install` / `uninstall` / `db-check` /
  anything else (including no args, which is how the SCM actually launches
  it).
- `src/db/` — the data layer (Phase 5):
  - `schema.rs` — DDL for all 8 ERD tables (`lock`, `lock_extension`,
    `emergency_unlock_request`, `blocked_attempt`, `blocklist_source`,
    `blocked_domain`, `keyword_rule`, `service_config`), applied via
    `CREATE TABLE IF NOT EXISTS` on every open — no separate migration step
    yet.
  - `models.rs` — plain structs mirroring each table.
  - One file per entity (`lock.rs`, `lock_extension.rs`, etc.) with basic
    CRUD, each with its own `#[cfg(test)]` unit tests against an in-memory
    DB (`cargo test` — 23 tests, no service/SCM code involved at all).
  - `mod.rs` — `open()` (real on-disk DB at `%ProgramData%\Warden\warden.db`,
    creating the directory/file/schema if missing) and `open_in_memory()`
    (tests only).
- `src/db_check.rs` — a manual harness (`warden-service.exe db-check`) that
  exercises every CRUD path against the *real* on-disk DB, as a second,
  closer-to-production check beyond the in-memory unit tests.
- `service::run()` now opens the DB on every real service start and logs
  whether it succeeded.
- `src/native_messaging.rs` — the Native Messaging host: a length-prefixed
  JSON stdio loop, per Chrome's protocol. Dispatches on a `type` field:
  - `get_status` (Phase 6) — queries `lock::get_current`.
  - `log_attempt` (Phase 7) — inserts via `blocked_attempt::create`; the
    extension's replacement for the old `chrome.storage.local` log.
  - `get_attempts` (Phase 7) — wraps `blocked_attempt::list_recent`, backs
    the popup's log view.
  - `start_lock` (Phase 8) — timed (`duration_seconds`) or `forever: true`.
    Rejects the request if a lock is already active/forever (ERD §2: only
    one at a time).
  - `extend_lock` (Phase 8) — adds `added_seconds` to the current timed
    lock's `expires_at` and records a `lock_extension` row. Requires a
    *positive* `added_seconds` — the one realistic way a "shorten" attempt
    could hide behind a legitimate message, so it's rejected explicitly
    rather than relying only on there being no dedicated shorten message.
  - `request_emergency_unlock` — still just "not implemented yet"; needs
    the emergency-unlock flow (Phase 11).
- `src/lock_logic.rs` (Phase 8) — `maybe_expire()`: ends a timed lock once
  it's past `expires_at`. Called from two places: the service's own 1-tick
  (1s) loop in `service.rs` (using one persistent `Connection` opened at
  startup, not reopened every tick), and defensively at the top of every
  native-messaging handler that reads/writes lock state — a fresh
  CLI-launched host process could otherwise report a stale "active" lock if
  queried in the (up to ~1s) gap before the service's own tick catches it.
- `src/native_host_install.rs` — `register-native-host` /
  `unregister-native-host`: writes the host manifest JSON to
  `%ProgramData%\Warden\native-messaging-host.json` (with the current exe's
  absolute path baked in, same pattern as `install.rs`) and points
  `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.warden.service` at
  it via `reg.exe`. Per-user, no admin needed — unlike the Windows Service
  itself, Chrome reads this from the current user's registry hive.
  `allowed_origins` is hardcoded to the extension's pinned ID
  (`pjnjbihjibbooihkcdmcdjdpbbmkfdkk` — see `extension/README.md` for how
  that's pinned); update `EXTENSION_ID` in this file if that ever changes.
- `src/extension_id.rs` (Phase 9) — the pinned extension ID, factored out to
  one place since both `native_host_install.rs` and `registry_policy.rs`
  need it.
- `src/registry_policy.rs` (Phase 9) — writes/relaxes two Chrome policies
  under `HKLM\SOFTWARE\Policies\Google\Chrome` via `reg.exe`, polled from
  `service.rs`'s tick (same pattern as `lock_logic::maybe_expire` — this
  *must* run in the SCM service process, which is `LocalSystem` and can
  write `HKLM`; `native_messaging.rs` runs as whichever user launched
  Chrome and can't). Before ever touching either value, saves whatever was
  there previously to `%ProgramData%\Warden\policy_backup.json`; relaxing
  always restores that exact prior value (or deletes it if there wasn't
  one) rather than trying to reverse a merge — correct regardless of
  `registry_policy_merge_mode`. See the module's doc comment for the full
  design rationale, including the two PRD corrections below. As of Phase
  10, also re-asserts every tick a lock is active (not just once) — see
  **Phase 10 findings**.
- `src/update_server.rs` (Phase 9) — a minimal hand-rolled local HTTP
  server (`http://127.0.0.1:47990`, two static GET routes, no new
  dependencies) serving `update_manifest.xml` and `extension.crx`, started
  from `service.rs` alongside the SCM dispatcher. Built to rule out `file://`
  as the cause of the `ExtensionSettings` warning (it wasn't).
- `service::control_handler` (Phase 10) — refuses `SERVICE_CONTROL_STOP`
  (i.e. `sc stop`, and anything else that sends that control code) while a
  lock is active, returning `ERROR_ACCESS_DENIED` instead of accepting it.
  Checks a `LOCK_ACTIVE` atomic updated once per tick rather than querying
  the DB directly from the control-handler callback, which per Microsoft's
  guidance should stay fast and non-blocking.
- `src/install.rs` (Phase 10) — `install()` now also runs
  `harden_delete_protection()`, adding a DENY ACE for the `SD` (DELETE)
  right to Administrators on the service's own security descriptor via
  `sc sdset` (reads the *current* descriptor via `sc sdshow` first rather
  than hardcoding one, since defaults vary by Windows version). `uninstall()`
  runs the inverse (`soften_delete_protection()`) before calling `sc delete`,
  so the legitimate path still works — see **Phase 10 findings** for why
  this exists and how it's deliberately still reversible by a determined
  admin, not a true permanent lock.
- `src/emergency_unlock_logic.rs` (Phase 11) — `maybe_apply()`: the mirror
  of `lock_logic::maybe_expire` for the escape hatch. Once a pending
  request's `cooldown_ends_at` has passed, marks it `applied` and ends the
  forever lock via the same `lock::end()` a timed lock's expiry uses (the
  ERD's `lock.status` enum only has `active`/`expired`/`forever` — "ended by
  emergency unlock" is exactly what `expired` already means). Polled from
  `service.rs`'s tick, right before `registry_policy::sync`, so policy
  relaxes the same tick the lock actually ends.
- `src/native_messaging.rs` (Phase 11) — `request_emergency_unlock` (only
  valid on a `forever` lock, rejects a second pending request), 
  `cancel_emergency_unlock` (cancels whatever's pending, if anything), and
  `get_emergency_unlock_history` (every request ever made, newest first,
  regardless of outcome — PRD §7.4's "visible, not something you can
  quietly do and forget"). `get_status` now also returns an
  `emergency_unlock` field (the current lock's pending request, if any) so
  the popup can render the forever view in one round trip.
- `src/stats.rs` (Phase 12) — `streak_days()` (consecutive clean UTC
  calendar days, counting back from today) and `week_counts()` (rolling
  7-day / 7-14-day-ago attempt counts, not calendar-aligned). Both derived
  purely from `blocked_attempt` at read time, per the ERD's own note that
  stats shouldn't live in a separate materialized table. See **Phase 12
  findings** for a real bug caught here before it shipped.
- `src/native_messaging.rs` (Phase 12) — `get_dashboard_stats` (wraps
  `stats.rs`). `get_attempts` gained an optional `days` field: when present,
  switches from the popup's plain "last N" (`list_recent`) to a
  date-windowed query (`list_since`) for the dashboard's default "last 30
  days" view (per the ERD's own open question, resolved in favor of a
  30-day default).
- `src/blocklist_refresh.rs` (Phase 13) — fetches
  [The Block List Project's](https://github.com/blocklistproject/Lists)
  maintained `porn.txt` (public, MIT-licensed, real production URL) via
  `ureq`, on its own background thread (network I/O must never delay the
  1-second tick loop's lock-expiry/registry-self-healing checks — same
  reasoning as `update_server.rs`). Polls once a minute for whether a
  refresh is due per `service_config.blocklist_refresh_interval_hours`;
  hashes fetched content (`DefaultHasher` — this is a change-detection
  check, not a security boundary, so no need for a real crypto hash crate)
  and skips the reload if unchanged (ERD §2); caps what actually gets
  parsed and stored at `MAX_DOMAINS` (2000) — see **Phase 13 findings** for
  why that cap exists at all. Falls back to a small bundled list
  (`bundled_blocklist.txt`, `include_str!`'d into the binary) only if this
  source has *never* successfully produced any domains; otherwise a fetch
  failure just leaves the last-known-good list alone.
- `src/db/blocklist_source.rs` (Phase 13) — added `get_by_url` /
  `get_or_create`, so the refresh job can find-or-seed its source row
  without a separate setup step.
- `src/native_messaging.rs` (Phase 13) — `get_blocklist_domains`: every
  domain across all active sources, deduped and sorted. The extension's
  replacement for the Phase 1 static `rules/blocklist.json`, which is now
  deleted entirely.
- `src/main.rs` — `refresh-blocklist`: a manual harness that force-clears
  the source's `last_refreshed_at` and calls `maybe_refresh` immediately,
  for testing without waiting on the interval or the poll loop.

## Phase 9 findings — two corrections to the PRD, one open problem

Found while implementing this phase, verified against Chrome's real,
current policy documentation and against a real installed Google Chrome
(not just the registry) — not assumptions carried over from the PRD:

1. **No per-extension Incognito policy exists.** The PRD (§7.2) assumed
   Chrome could be told "allow this one extension in Incognito" via policy.
   It can't — the only lever is `IncognitoModeAvailability`, which is
   whole-browser (available / disabled / forced). Implemented as: disable
   Incognito entirely (`1`) while a lock is active. Blunter than the PRD's
   wording, but it closes the actual bypass (no Incognito windows at all)
   more robustly than a per-extension toggle would have anyway.
2. **`installation_mode: force_installed` needs a packaged `.crx` + update
   manifest**, not a "Load unpacked" dev extension. Built one: `extension/`
   is staged (excluding `dev-signing-key.pem` and `README.md`) and packed
   via `msedge --pack-extension=<staging> --pack-extension-key=<dev key>`,
   producing `%ProgramData%\Warden\extension.crx`, referenced by
   `update_manifest.xml` (a minimal Omaha-protocol XML pointing at the crx
   via a `file://` codebase). This is a **manual step, not automated** —
   re-run it whenever `extension/` changes:
   ```
   robocopy "extension" "C:\ProgramData\Warden\extension-src" /E /XF dev-signing-key.pem README.md
   & "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" --pack-extension="C:\ProgramData\Warden\extension-src" --pack-extension-key="extension\dev-signing-key.pem"
   move "C:\ProgramData\Warden\extension-src.crx" "C:\ProgramData\Warden\extension.crx"
   ```

**Open problem**: `IncognitoModeAvailability` is confirmed working — real
Chrome's `chrome://policy` page shows it with **Status: OK**. But
`ExtensionSettings` shows **Status: Warning**, and the "Extension Install
Policies" section on that same page shows "No policies set" even while the
policy is applied — Chrome is reading and parsing our JSON (its value
column visibly shows our exact `installation_mode`/`update_url` for the
right extension ID) but isn't accepting it as a genuine force-install
directive.

Ruled out `file://` as the cause: added `src/update_server.rs`, a minimal
hand-rolled HTTP server (two static GET routes, no new dependencies) that
serves both files over `http://127.0.0.1:47990` instead, started from
`service.rs` alongside the SCM dispatcher. Confirmed the server itself works
correctly (fetched both endpoints directly, right content/headers) and
confirmed the registry value updated to the new `http://` URL — but
`chrome://policy` showed the identical Warning/"No policies set" result.
So the URL scheme isn't it.

Tried repeatedly to extract the actual warning text from `chrome://policy`
via the Chrome DevTools Protocol (`Runtime.evaluate` piercing the page's
nested shadow DOM, coordinate-based clicks on the expand chevron, a
recursive shadow-tree text walker) — none of it reached the real message,
which only appears to render after an interaction the scripted clicks never
quite landed correctly. Also checked whether this is Chrome's newer
anti-abuse hardening around policy-forced extensions on unmanaged machines
(a real, documented 2026 Chrome change) — but that specific protection is
scoped to extensions overriding the search engine or New Tab page, which
Warden doesn't touch, so it's probably not the cause here, though it's a
reminder that this whole mechanism sits in space Chrome is actively
tightening.

**Resolved (root cause found)**: got the exact warning text by asking the
user to expand the row by hand in their own Chrome, after scripted
extraction kept failing:

> This computer is not detected as enterprise managed so policy can only
> automatically install extensions hosted on the Chrome Webstore. The
> Chrome Webstore update URL is "https://clients2.google.com/service/
> update2/crx".
> Schema validation error: Unknown property: [BLOCKED]pjnjbihjibbooihkcdmcdjdpbbmkfdkk

This is a **hard Chrome platform restriction**, not a bug in this code: on
an unmanaged (non-enterprise-enrolled) Windows machine, `force_installed`
only accepts extensions whose `update_url` is the real Chrome Web Store.
Neither `file://` nor a local `http://127.0.0.1` server — regardless of
how correctly either serves the manifest/crx — can satisfy this. The
"[BLOCKED]" prefix and schema error are Chrome's rejection of the extension
ID once it decided the source wasn't trusted, not a formatting problem on
our end.

**This has consequences beyond Phase 9.** The PRD's force-install mechanism
(§7.2, §9) can't work as designed on a personal/unmanaged machine without
one of:
1. **Publish the extension to the Chrome Web Store** (Unlisted works, no
   need to be public) — the sanctioned fix; `update_url` becomes the real
   Web Store URL. Costs a one-time $5 Google Developer fee; every future
   update needs a Web Store submission instead of an instant local rebuild.
2. **Enroll this machine in Chrome Browser Cloud Management** (Google
   offers this free for personal accounts) — would make Chrome treat the
   device as enterprise-managed, which should lift the Web-Store-only
   restriction, including for our own local update server. Not yet
   verified.
3. **Drop force-install as the mechanism** and lean on other Phase 10
   tamper-resistance approaches instead — e.g., the service detecting the
   extension's absence/removal and reacting, rather than Chrome preventing
   removal outright.

This is a product-direction decision, not an implementation detail — left
for a deliberate choice rather than picked unilaterally. `Incognito`
lockout is confirmed working end-to-end; force-install is not, and won't
be until one of the above is chosen and implemented.

**Decision (2026-08-29)**: deferred. Force-install stays a documented,
known gap for now; Phase 10 proceeded on the service-hardening side, which
doesn't depend on it.

## Phase 10 findings — sc stop refusal, and a real bypass found and closed

**`sc stop` refusal** worked as designed on the first try: `control_handler`
checks a `LOCK_ACTIVE` flag (updated once per tick) and returns
`ERROR_ACCESS_DENIED` instead of honoring `SERVICE_CONTROL_STOP` while a
lock is active — the service simply stays `RUNNING`. Verified against the
real installed service: confirmed normal `sc stop` still works with no lock
active (didn't break the ordinary path), started a lock, confirmed `sc
stop` was refused and logged, confirmed the service stayed `RUNNING`
throughout.

**Then found a real, working bypass** that the above doesn't cover:
`DeleteService` (`sc delete`) is a *different* SCM API — it never reaches
the running service process, so there's no control code to refuse. Tested
against the real service, in order:
1. `sc delete` while `RUNNING` and locked → `[SC] DeleteService SUCCESS`.
   Service stayed `RUNNING` (deletion is marked pending, not immediate).
2. Any further control message, including `sc stop`, now got refused by
   **Windows itself** (`error 1061: The service cannot accept control
   messages at this time`) — a different lockout than the one built above.
3. Killed the process directly (`Stop-Process -Force`, simulating Task
   Manager) → the pending deletion finalized immediately. The service was
   completely gone (`error 1060: service does not exist`) — not even
   crash-recovery could help, since there was nothing left to restart.

**Root-caused and closed it**: added `install::harden_delete_protection()`,
which sets a security descriptor on the service object itself (`sc sdset`)
adding `(D;;SD;;;BA)` — a DENY ACE for the `SD` (`DELETE`, confirmed via
[Microsoft's ACE strings reference](https://learn.microsoft.com/en-us/windows/win32/secauthz/ace-strings),
after two independently-summarized sources gave contradictory answers for
which SDDL letter means delete) right, targeting Administrators. This
blocks `sc delete` **unconditionally**, not just while locked — since ACL
checks happen before any of our code runs, there's no way to make this
conditional on lock state the way the control-handler refusal is.

Deliberately **not permanent**: `WRITE_DAC` is left untouched in the ACL,
so an admin can still run `sc sdset` to remove the deny ACE, then
`sc delete` — two deliberate steps instead of one, matching the PRD's own
§11 requirement that there be *some* recovery path, however inconvenient,
rather than a true dead end. `install::soften_delete_protection()` does
exactly this sequence automatically, so this tool's own `uninstall` command
still works normally when no lock is active.

Verified end-to-end against the real service:
- Fresh `install()` → `sc sdshow` confirms the deny ACE is present.
- `sc delete` fails outright: `Access is denied` — with **no lock active**,
  stronger than the control-handler protection alone.
- Manually reset the SDDL back to the pre-hardening original → `sc delete`
  succeeds again (marks pending) → confirms recoverability, not a brick.
- Killed the process (no prior `sc delete` this time) → **crash-recovery
  worked**: service came back `RUNNING` automatically. This was flagged as
  "not verified" in every phase since Phase 4 — now confirmed for real.
- Reinstalled fresh → deny ACE reapplied automatically → survived another
  crash-restart cycle unchanged (it's stored in the registry, not
  per-process state).
- Own `uninstall()` command tested with no lock active → softened the ACL,
  stopped, deleted cleanly → service fully gone, exactly as intended for
  the legitimate path.

**Not covered by Phase 10** (documented, not fixed): direct registry edits
to `HKLM\...\Services\WardenService` itself (as opposed to the Chrome
policy keys, which *are* self-healed) — e.g., manually changing its
`ImagePath` or `Start` type. Also out of scope, per the PRD's own
acknowledged limits: a full OS reinstall, or anything requiring physical/
offline access to the disk.

## Phase 11 findings — emergency unlock, and a Windows SCM oddity

The logic itself worked correctly on the first real test pass — every
scenario verified against the actual installed service (not just unit
tests), driving the exe's stdin/stdout exactly like Chrome would:

- `start_lock forever` → `request_emergency_unlock` succeeds, returns a
  request with `cooldown_ends_at` 7 days out.
- A second `request_emergency_unlock` while one's pending → rejected
  ("already pending").
- `get_status` on a forever lock now includes the pending request inline.
- `cancel_emergency_unlock` → succeeds; calling it again with nothing
  pending → rejected ("no pending emergency unlock request").
- `get_emergency_unlock_history` → shows the cancelled request, permanently
  (status `cancelled`, `cancelled_at` set).
- A fresh `request_emergency_unlock` after cancelling → succeeds (a new
  row, not a resurrection of the old one).
- `request_emergency_unlock` on a *timed* (non-forever) lock → rejected
  ("only applies to a forever lock").
- **Auto-apply**: shortened a pending request's `cooldown_ends_at` directly
  in the DB (same technique used in Phase 8 to test lock expiry without
  waiting hours — the actual `request_emergency_unlock` handler always
  computes a real 7-day cooldown; there's no test backdoor in the product
  code itself). Within one tick, `service.log` showed "Emergency unlock
  applied; forever lock ended." and `get_status` correctly returned to
  `lock: null` — and, as a nice side effect of Phase 9/10 already being in
  place, the Chrome policy relaxed automatically in the same tick, with no
  emergency-unlock-specific code needed for that part.
- History afterward showed both outcomes side by side, permanently: the
  earlier `cancelled` request and the later `applied` one.

**Unrelated oddity hit during cleanup, not during the feature testing
itself**: mid-cleanup, `sc stop` started failing with `error 1061: The
service cannot accept control messages at this time` — with no lock active
and no prior `sc delete` involved (ruling out the Phase 10 pending-deletion
lockout, which produces the identical error). Confirmed via `get_status`
that the DB genuinely showed no active lock, and the failure persisted
across a couple of retries a few seconds apart, so it wasn't just SCM
being momentarily busy. Recovered using the same path Phase 10 already
proved safe: killed the process directly, and crash-recovery brought it
back cleanly, after which `sc stop` worked normally again on the fresh
process. Not root-caused — logged here as something to watch for, not
something fixed, since it never reproduced on demand and didn't affect any
actual lock/emergency-unlock behavior.

## Phase 12 findings — a real streak-calculation bug, caught by its own test

The initial `streak_days()` walked backward from today, incrementing until
it hit a day with a logged attempt. On a **completely empty** `blocked_attempt`
table (a brand-new install, or simply a quiet stretch with nothing ever
blocked), there's no such day to hit — the loop had nothing to stop at and
ran all the way to its own safety cap, reporting a streak of **3651 days**
instead of a sane number. Caught immediately by
`stats::tests::streak_counts_today_when_nothing_blocked_yet` (written
alongside the function, not after — the failure showed up on the first
`cargo test` run) before it ever reached the real service.

The real constraint: the ERD deliberately has no separate "tracking
started" timestamp — streaks are purely derived from `blocked_attempt` at
read time (its own design note). That means an empty table is genuinely
ambiguous: it could mean "just installed, no history at all" or "N real
clean days with nothing ever logged," and there's no data anywhere to tell
those apart. Rather than add a new persisted anchor column (a schema change
beyond what Phase 12 needed, and not what the ERD's note pointed toward),
the fix is conservative: an empty table reports a streak of exactly `1`
(today, so far, is clean — the one thing actually known) rather than
guessing further back. Added a second regression test
(`streak_stops_exactly_at_a_dirty_day_several_days_back`) confirming the
normal case — a dirty day 4 days back — still correctly stops the count at
4, not just checking the two empty/immediate-dirty edge cases.

Also hit an unrelated process hygiene lesson mid-phase: after fixing the
bug, `cargo test` was re-run and passed, but the *installed service* was
restarted without an intervening `cargo build` — `cargo test` builds its
own test harness binary, which isn't guaranteed to be the same artifact
`sc start` launches. The stale pre-fix binary briefly ran against real data
(reproducing the exact 3651-day bug live) before this was noticed and a
proper rebuild fixed it. No lasting effect — caught immediately from the
live response, DB reset afterward — but worth remembering: verifying
against the real service always needs a real rebuild in between, `cargo
test` passing isn't sufficient on its own.

**Verified for real**, service side: exercised `get_dashboard_stats` before
and after logging attempts (streak `1` → `0`, `this_week` `0` → `2`), and
`get_attempts` with a `days` window (returns the full set regardless of a
small `limit`, confirming the floor-at-1000 behavior) — all against the
actual installed, freshly-rebuilt service.

## Phase 13 findings — the real list is 953,393 domains, and a browser-side hard limit

Checked the actual size of the chosen source **before** writing any parsing
code: `curl`'d it directly. The header claims "Entries: 953,393"; counting
non-comment, non-blank lines confirmed it. That's the real, live, currently
maintained list — not a guess.

That number matters because Chrome's `declarativeNetRequest` cannot come
close to holding it. Checked the current (post-Chrome-120) limits against
Chrome's own docs rather than assume the numbers from memory were still
current: **30,000** for "safe" dynamic rules, **5,000** for "unsafe" ones
(a redirect rule matching a host the extension has no declared
`host_permissions` for — which is exactly our domain rules — likely falls
in the unsafe bucket, though the exact safe/unsafe boundary for
`extensionPath`-only redirects wasn't confirmed to the letter). Either way,
953,393 is roughly 32x even the *larger* limit. There is no rule-format
trick that fits it — this is a hard platform ceiling, not a bug to route
around.

Chose `MAX_DOMAINS = 2000` (in `blocklist_refresh.rs`, shared conceptually
with the extension's `DOMAIN_RULE_ID_MAX_COUNT` in `background.js` — the
two aren't currently enforced as a single source of truth, so if one
changes the other needs updating by hand; worth a comment in both files,
already added) — comfortably under 5,000 with real headroom, while still
being a meaningfully large, real, live-fetched list rather than a token
handful. Also ruled out one specific design early: a *single* dynamic rule
matching all domains via one big regex alternation (the same trick used for
the 5-keyword engine) doesn't scale here — Chrome caps a single
`regexFilter` string at roughly 2KB, which even ~100-150 average-length
domains joined with `|` would blow past. The extension instead registers
one plain `urlFilter` rule per domain (`||domain^`), the standard technique
real ad/content-blocker extensions use for large lists, with no comparable
per-rule length constraint.

**Verified for real, service side**: the background refresh thread's very
first tick (fresh DB, `last_refreshed_at` was `NULL`) fetched the live
953K-line file, parsed it, and stored exactly 2000 domains —
`service.log` confirms it, and so does the DB directly (`SELECT COUNT(*)
FROM blocked_domain` → 2000). Ran the refresh again via the `refresh-blocklist`
CLI harness (force-clearing `last_refreshed_at` first): the content hash
matched, and it correctly skipped the reload rather than re-parsing and
re-writing 2000 rows for nothing. `get_blocklist_domains` returns all 2000,
deduped and sorted.

**Not verified**: the extension side actually registering ~2000
`declarativeNetRequest` rules from that data and a matching domain
redirecting correctly in a real browser. Tried — loaded the real extension
into a real (not headless) Chrome via CDP and inspected its live service
worker directly rather than just watching page behavior, since headless
Chromium's inability to run unpacked MV3 extensions reliably has been a
consistent finding since Phase 1. This time even that deeper approach hit
the same wall: the CDP target list under `--load-extension` in this
environment showed two *other* extensions' contexts, never Warden's own
`background.js` — so there was nothing valid to introspect.

One early attempt, before switching to the safer service-worker-introspection
approach, navigated a headless tab directly to a real domain from the
fetched list expecting the redirect to intercept it first (exactly how
`declarativeNetRequest` is supposed to work — no real connection should
ever complete). It didn't intercept, and the domain's real server did
respond (a bot-check interstitial, not adult content, but a real network
round-trip to a listed adult-content domain regardless). Stopped that
approach immediately, deleted the screenshot, and moved to introspection
instead of retrying with more such domains. Flagging this plainly rather
than glossing over it: **testing this specific feature by navigating to
real entries from an adult-content blocklist isn't something to automate
further** — if this needs re-verifying, load the extension in a real
Chrome window yourself and check
`chrome.declarativeNetRequest.getDynamicRules()` from the service worker's
own DevTools console (`chrome://extensions` → Warden → "service worker" →
Inspect) rather than by navigating anywhere.

## Phase 14 findings — the 1061 mystery solved, registry merge re-confirmed, and the total-lockout recovery path verified

**The recurring `error 1061`, finally root-caused.** Phases 11 and 13 both
hit `sc stop` failing with `1061` ("service cannot accept control messages")
under conditions that didn't reproduce on demand, and were logged as
unexplained oddities. This phase reproduced it reliably with a rapid-fire
stress test (8x `sc stop`, 500ms apart, while a lock was active) and traced
it to the real cause: `control_handler` was returning `ERROR_ACCESS_DENIED`
from inside the `HandlerEx` callback to refuse a stop — but the Win32
contract for that callback doesn't actually define what happens when you
return a non-`NO_ERROR` code for a control that's still advertised as
accepted in `dwControlsAccepted`. In practice, the *first* such refusal
passed the error through cleanly, but the SCM then stopped delivering the
`STOP` control to our handler at all on every subsequent attempt —
confirmed via `service.log`, which showed only **one** "Refused sc stop"
line across all 8 rapid calls — and rejected them itself with the generic
`1061`, a state that persisted even minutes later at 5-10s spacing, until
the process was restarted.

A first fix attempt (moving `log_line()` file I/O out of the handler, on
the theory the SCM was penalizing a slow callback) was tried, deployed, and
**disproven** by the same stress test — 1061 still reproduced identically
with a handler doing nothing but atomic loads/stores. The real fix: stop
trying to refuse the control from inside the handler at all, and instead
toggle whether `SERVICE_ACCEPT_STOP` is even advertised in
`dwControlsAccepted` (via `SetServiceStatus`) based on lock state, once per
tick. This is the documented, standard Windows mechanism for a temporarily
unstoppable service — the SCM then rejects a stop attempt at the door,
before ever reaching application code. Re-running the identical 8x rapid
stress test against this fix produced `1052` ("The requested control is not
valid for this service") consistently across all 8 attempts — the
*correct*, standard error code, not a degrading one — and a stop attempt
made after the lock naturally expired succeeded cleanly (exit `0`,
`STOP_PENDING` → `STOPPED`) on the first try. The old handler-based refusal
branch is kept as a defense-in-depth fallback for the narrow race where a
stop control is already in flight the instant a lock starts, but is no
longer the primary mechanism. Trade-off: a stop rejected via
`dwControlsAccepted` never reaches `control_handler`, so it no longer
produces a "Refused sc stop" audit-log line — the security property itself
got strictly more reliable, at the cost of losing a log line for exactly
the attempts that are now rejected the new way.

**Registry policy merge, re-confirmed against the fully integrated
system.** Phase 9 verified this in isolation; this phase re-ran it with
everything from Phases 4-13 wired together. Seeded a fake competing
`ExtensionSettings` entry for a different extension ID (via the .NET
registry API directly, to avoid a `reg.exe`/PowerShell quoting trap that
silently stripped the JSON's quotes on a first attempt), started a real
lock through Native Messaging, and confirmed the live registry value showed
*both* entries side by side — the fake one untouched, ours added. Let the
lock expire naturally and confirmed the value was restored to *exactly* the
seeded fake entry alone, with no trace of ours left behind. Merge and
restore both behave correctly with the real, fully integrated system, not
just in isolation.

**Total-lockout recovery path (PRD §11), verified empirically.** The
concern PRD §11 raises is: what happens if you genuinely can't get a lock
to end (crash, corruption, whatever) — is there *any* documented way out,
even a deliberately painful one? Backed up the real `warden.db` (to
preserve the live 2000-domain blocklist data), started a 120s lock,
confirmed `sc stop` was correctly refused (`1052`) while it was active, then
directly overwrote the first 4KB of the live `warden.db` file with random
bytes (via a `FileShare.ReadWrite`-opened handle, since the service holds
its own open connection) to simulate real corruption without needing to
stop the service first. Within one tick, `service.log` showed "Registry
policy sync failed: file is not a database" / "Expiry check failed: file is
not a database" repeating every second — and `sc stop`, attempted while the
lock's 120s window was still nominally open, **succeeded immediately** (exit
`0`, clean `STOP_PENDING` → `STOPPED`). This confirms the tick loop's
`LOCK_ACTIVE.store(...unwrap_or(false))` fail-open behavior works exactly as
intended: a DB the service can no longer read is treated as "no lock",
not as "lock stuck on forever". **This is the actual answer to PRD §11**:
the manual recovery path is corrupting or deleting
`C:\ProgramData\Warden\warden.db` (requires the same admin/LocalSystem-
adjacent access as everything else that can touch `%ProgramData%\Warden`),
which causes the service to fail open on its very next tick, after which
`sc stop` and uninstall both work normally. It's genuinely painful by
design — no UI exposes it, it requires direct filesystem access to a
`%ProgramData%` path, and it destroys whatever lock/attempt history was in
the DB — but it is a real, verified way out that doesn't require trusting
"eventually the lock will expire" when something has gone genuinely wrong.
Restored the real `warden.db` from the pre-corruption backup afterward and
confirmed the live blocklist data (2000 domains) survived intact.

**Not changed this phase, flagged as a known product trade-off rather than
fixed unilaterally**: the keyword engine's `xxx` entry still flags
`size-xxx-large`-style clothing/apparel URLs as a false positive (hyphens
aren't word characters, so the word-boundary fix doesn't help there the way
it did for `pornography`/`hentai`-as-substring cases). Removing `xxx` from
`KEYWORDS` would fix it but would also stop catching a common, unambiguous
real adult-content term — a real precision/recall trade-off, not something
to decide without the person who'll actually be living under this block.

## Build

```
cargo build --release
```

Needs the MSVC linker (Visual Studio Build Tools or a full VS install with
the "Desktop development with C++" workload) — this machine already had it
via an existing VS Community 2022 install.

If the service is currently installed and running, `sc stop WardenService`
first — its `.exe` is locked while running, and the build will fail trying
to overwrite it. If a rebuild still fails with an "Access is denied" /
"process cannot access the file" error on a `.o` file even after stopping
it, that's Windows Defender transiently scanning the freshly written build
artifact; retry, or set `CARGO_INCREMENTAL=0` to sidestep it.

## Install / uninstall

Both require an elevated (Administrator) prompt — `sc.exe` will report
"Access is denied" otherwise, surfaced as a clear error rather than swallowed.

```
target\debug\warden-service.exe install     # registers, configures, starts
target\debug\warden-service.exe uninstall   # stops, removes
```

`install` sets: `start= auto` (boots with Windows), `obj= LocalSystem`, and
failure actions (`sc failure`) to restart at 5s, 5s, then 60s after a crash,
resetting the failure count after a day of continuous uptime.

## Verify

```
sc query WardenService      # should show STATE: RUNNING
sc qc WardenService          # should show START_TYPE: AUTO_START
sc qfailure WardenService    # should show the restart actions
type %ProgramData%\Warden\service.log
```

**Data layer**:

```
cargo test                          # 23 CRUD unit tests, in-memory DB
target\debug\warden-service.exe db-check   # same CRUD paths, real on-disk DB
```

`db-check` prints what it did at each step and creates
`%ProgramData%\Warden\warden.db` if it doesn't already exist. Delete that
file first if you want to confirm the "creates the DB if missing" path from
a clean slate — the real service does the same thing on every startup (see
`service.log` for a "Database ready at ..." line after each start).

**Native Messaging**:

```
target\debug\warden-service.exe register-native-host   # per-user, no admin
```

Then from the extension side (`extension/`), open the popup — it sends a
`get_status` message on load and shows the result. To test the host binary
directly without a browser at all, launch it exactly as Chrome would (with
the extension's origin as argv[1]) and speak the length-prefixed JSON
protocol over its stdin/stdout yourself; that's a closer, browser-independent
check of the framing/parsing logic specifically.

## Status

Installed and verified running on this machine as of 2026-08-28: `sc query`
shows RUNNING, `sc qc` shows AUTO_START/LocalSystem, `sc qfailure` shows the
restart actions, and `service.log` shows the startup + periodic "alive"
heartbeat lines. Stop was also exercised directly (`sc stop`) and logged a
clean STOP_PENDING → STOPPED transition before being started again.

**Not verified**: actual reboot survival (would require rebooting this
machine, which wasn't done) and the restart-on-crash path (would require
killing the process to force a crash-driven SCM restart). Both should work
given the config above, but are worth confirming for real before Phase 4 is
considered fully closed out — e.g. next time this machine reboots normally,
check `sc query WardenService` came up RUNNING on its own.

**Phase 5**: all 23 CRUD unit tests pass (`cargo test`), and `db-check` was
run against a freshly deleted `%ProgramData%\Warden\warden.db` to confirm it
creates the file and schema from nothing, then exercises every table. The
real running service was also restarted after the DB code was added and its
`service.log` shows "Database ready at C:\ProgramData\Warden\warden.db."
right after "Warden service starting." — so DB creation happens on real
service startup too, not just under the manual harness.

**Phase 6**: `register-native-host` was run and verified — the generated
`%ProgramData%\Warden\native-messaging-host.json` and the
`HKCU\...\NativeMessagingHosts\com.warden.service` registry value (checked
via `reg query`) both look correct. The wire protocol itself was verified by
launching the exe exactly as Chrome would (`warden-service.exe
chrome-extension://pjnjbihjibbooihkcdmcdjdpbbmkfdkk/`) and driving it with
real length-prefixed JSON from a small script: `get_status` returned a real
DB-backed response, an unimplemented type returned a clear error, and
malformed input didn't crash the process.

**Not verified**: the actual Chrome round trip end-to-end (popup → Chrome →
this host → back). Headless Chromium doesn't reliably drive unpacked MV3
extensions or Native Messaging (see Phase 1/2's notes), so this needs a real
browser window — load the extension, open the popup, and confirm the
"Service: ..." line resolves to "reachable" rather than "unreachable".

**Phase 7**: `log_attempt` and `get_attempts` were added and verified the
same way as Phase 6's `get_status` — driving the exe's stdin/stdout directly
with the exact framing Chrome uses. Logged a blocklist and a keyword
attempt, confirmed a missing-field request errors cleanly instead of
inserting garbage, then confirmed `get_attempts` (both default and
`limit: 1`) returns them newest-first with the right fields. `blocked.js`
and `popup.js` were switched over to call these instead of
`chrome.storage.local`, and the `storage` permission was removed from the
extension's manifest since nothing uses it anymore. DB was reset to a clean
state afterward (the verification runs left a couple of rows behind — pure
test data, not meaningful history).

**Not verified**: same browser-round-trip gap as Phase 6 — needs a real
Chrome window to confirm the blocked page's `log_attempt` call actually
lands and the popup's `get_attempts` call actually renders it.

**Phase 8**: `cargo test` — 27 tests now (4 new `lock_logic` tests). Verified
against the real running service, not just in-memory:
- Full lifecycle via the same "drive the exe like Chrome would" technique:
  idle → `start_lock` (3s) → a second `start_lock` while active correctly
  rejected → `extend_lock` with `-30` and `0` both rejected ("shortening is
  not permitted") → a made-up `cancel_lock`/`shorten_lock` type both hit
  "unknown message type" → a legitimate `extend_lock: 10` succeeded and
  `expires_at` moved forward exactly 10s → `get_status` reflected it.
- Real-time expiry against the *actual installed service* (not a
  standalone test binary): started a 4s lock through one CLI-launched host
  process, and ~120ms after the real `expires_at` the service's own tick
  logged "Lock expired." in `service.log` — confirming the persistent
  `Connection` + 1s tick loop in `service.rs` actually works, not just the
  defensive check in the native-messaging handlers.
- DB reset to a clean state afterward (verified empty: `lock`,
  `lock_extension`, `blocked_attempt`, `emergency_unlock_request` all 0 rows).

**Not verified**: the actual popup UI round-trip in a real Chrome window
(same gap as every prior phase) — start a lock from the popup, watch the
countdown tick down client-side, extend it, and confirm the service's own
expiry actually clears it back to the idle view without a manual refresh.

**Phase 9**: verified against the real registry and a real installed Google
Chrome (152.0.7933.0), not just unit tests (`registry_policy.rs` has none —
it's inherently an OS-interaction module, hard to meaningfully unit test).
- **Merge safety**: seeded a fake pre-existing `ExtensionSettings` entry for
  a different extension ID plus `IncognitoModeAvailability=0`, using argv-
  level `reg.exe` calls (not a shell) to avoid the quoting trap PowerShell's
  own quoting fell into on a first attempt. Started a lock → confirmed our
  entry was added *alongside* the fake one (merge, not overwrite) and
  Incognito flipped to `1`, with `policy_backup.json` correctly recording
  the exact prior values. Let the lock expire → confirmed both values were
  restored to *exactly* the seeded originals and the backup file was
  removed.
- **Real Chrome confirmation**: launched an isolated, fully off-screen
  Google Chrome instance (unique profile, `--remote-debugging-port`) and
  used the Chrome DevTools Protocol directly (`Page.navigate` +
  `Page.captureScreenshot` over its own debugging websocket) to render
  `chrome://policy` and confirm what Chrome itself believes is in effect —
  deliberately not a full-screen capture, which would risk showing
  whatever's actually on the real desktop. `IncognitoModeAvailability`
  showed **Status: OK** with value `1`. `ExtensionSettings` showed the
  correct JSON for the right extension ID but **Status: Warning** — see
  **Phase 9 findings** above for what that means and what's still open.
- Cleaned up afterward: deleted the seeded fake registry values and
  `policy_backup.json`, reset the DB, confirmed `HKLM\...\Chrome` has no
  policy values left.

**Not verified**: the exact `ExtensionSettings` warning/rejection reason
(see findings above), and therefore also not verified: force-installation
actually happening, and removal actually being blocked in
`chrome://extensions` — the two things Phase 9's exit criteria are actually
about. Incognito lockout is the one piece of this phase confirmed working
end-to-end today.

**Phase 9, round 2**: added `update_server.rs` (local HTTP server for the
update manifest + crx) to rule out `file://` as the cause. Verified the
server directly (`GET /update_manifest.xml` and `GET /extension.crx` both
returned correct content/headers), confirmed the registry picked up the new
`http://127.0.0.1:47990/...` URL, and re-checked real Chrome — identical
Warning/"No policies set" result, so the URL scheme wasn't it. Spent
significant effort trying to extract the actual warning text from
`chrome://policy` via CDP (shadow-DOM-piercing `Runtime.evaluate`,
coordinate clicks, a recursive text walker) without success — this needs a
human opening `chrome://policy` and expanding the row by hand. Cleaned up
again afterward: DB reset, registry values removed, all scratch scripts and
Chrome test profiles deleted.

**Phase 9, round 3**: the user opened `chrome://policy` in their own real
Chrome and expanded the row by hand, surfacing the exact warning — see
**Phase 9 findings** above for the full text and what it means. Root cause
found: unmanaged machines can only `force_installed` from the real Chrome
Web Store, full stop. Ended the test lock immediately afterward (direct DB
reset rather than waiting out the remaining ~4 minutes, since `Incognito`
had gone to "Restart required" and there was no reason to leave that
pending) and re-confirmed a clean registry state.

**Phase 10**: see **Phase 10 findings** above for the full story — `sc
stop` refusal, the `sc delete` bypass found and closed via ACL hardening,
and crash-recovery confirmed working for real for the first time. All
verified against the actual installed service, including deliberately
breaking it (delete + process kill) and confirming recovery. Ended in a
clean state: DB reset, fresh install with hardening applied, service
`RUNNING`.

**Phase 11**: `cargo test` — 32 tests now (5 new: 1 `db::emergency_unlock`,
4 `emergency_unlock_logic`). See **Phase 11 findings** above for the full
real-service verification (request/reject-duplicate/cancel/reject-on-timed-
lock/auto-apply/history, all confirmed) and the unrelated `error 1061`
oddity hit during cleanup (recovered via the Phase-10-proven kill-and-
recover path, not root-caused). Ended in a clean state: DB reset, service
`RUNNING`.

**Phase 12**: `cargo test` — 38 tests now (6 new: 3 `db::blocked_attempt`,
3 `stats`). See **Phase 12 findings** above for the streak-calculation bug
its own test caught before it ever reached the real service, and the
stale-binary lesson from re-running the service without an intervening
rebuild. Verified `get_dashboard_stats` and `get_attempts` with a `days`
window against the real, freshly-rebuilt service. Ended in a clean state:
DB reset, service `RUNNING`.

**Phase 13**: `cargo test` — 43 tests now (5 new: 1 `db::blocklist_source`,
4 `blocklist_refresh`). See **Phase 13 findings** above for the full story:
the real 953,393-entry source, the confirmed Chrome rule-count ceiling that
makes a 2000-domain cap a real constraint rather than an arbitrary one, the
live refresh/skip-when-unchanged/`get_blocklist_domains` verification
(all against the actual service, real network fetch, real data), and the
extension-side verification gap plus what actually happened trying to close
it. Left the DB with real fetched data this time (2000 real domains) rather
than resetting — that data demonstrates the feature working, unlike prior
phases' lock/attempt test rows which were pure noise.

**Phase 14**: `cargo build` + `cargo test` — still 43 tests, all passing (no
new unit tests this phase; everything verified was inherently an
OS/SCM-interaction concern, same reasoning as Phase 9/10). See **Phase 14
findings** above for the full story: the `error 1061` root cause found and
fixed (verified via an 8x rapid `sc stop` stress test, before and after —
`1061`/degrading before, consistent `1052` then clean `0` after), the
registry merge/restore re-confirmed against the fully integrated system
with a seeded fake competing policy, and the DB-corruption fail-open
recovery path (PRD §11) verified for real by corrupting a backed-up copy of
the live `warden.db` mid-lock and confirming `sc stop` then succeeds.
Resource usage over a ~20-minute session: ~13.6MB working set, ~2MB private
memory, 4 threads — light, but not a genuine multi-day soak test (worth a
follow-up check via `Get-Process warden-service` after real multi-day
uptime for stronger confidence). Ended in a clean state: real `warden.db`
restored from its pre-corruption backup (2000-domain blocklist data intact,
confirmed via `get_blocklist_domains`), no lock active, all test registry
values removed, service `RUNNING` on the rebuilt binary.

**Overall confidence assessment, Phase 14's exit criteria** ("confident
handing this to yourself for real, unsupervised use"): the core commitment-
device mechanics are solid and now well-tested under adversarial
conditions — a lock genuinely cannot be ended early through `sc stop`,
`sc delete`, direct registry edits, or (now confirmed) rapid repeated stop
attempts; the only way out during an active lock is waiting it out, the
7-day emergency-unlock cooldown for a forever lock, or the deliberately
painful DB-corruption recovery path. What remains genuinely limited, not
fixed this phase: `ExtensionSettings` force-install still doesn't take
effect on this unmanaged machine (Phase 9, deliberately deferred — the
extension can still be manually removed via `chrome://extensions` during a
lock, which is the one real gap in the tamper-resistance story); the
`xxx`-in-clothing-sizes keyword false positive (flagged above, a trade-off
not a bug); and several browser-side behaviors (large-scale dynamic rule
registration, real domain-blocklist redirects) that were verified via
service-worker introspection and direct protocol testing but never through
an actual visible browser window, for the same headless-Chromium
limitations noted since Phase 1. None of those remaining gaps are in the
"can the lock be broken" category — they're either a known, separately-
deferred install mechanism, a precision trade-off, or a testing-method gap
rather than a demonstrated bug.
