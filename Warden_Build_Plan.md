# Warden — Staged Build Plan

Each phase has a clear exit criteria before moving to the next. Phases 1–3 build a working (but not yet tamper-resistant) extension in isolation, so there's something testable early. Phases 4–7 bring the service online and switch it to being the source of truth. Phases 8–11 add the actual lock/tamper-resistance mechanics — the core value of the tool. Phases 12–14 add the dashboard, blocklist maintenance, and hardening pass.

---

### Phase 1 — Extension skeleton + static domain blocking
**Goal**: A loadable, working Chrome extension that blocks a hardcoded list of domains.
- Manifest V3 setup (`manifest.json`, permissions: `declarativeNetRequest`, `storage`)
- Bundle a small static test blocklist as `declarativeNetRequest` static rules
- Basic popup UI shell (no functionality yet, just confirms extension loads)
- Blocked navigations redirect to a local "blocked" page

**Exit criteria**: Loading the extension unpacked and visiting a test domain from the hardcoded list redirects to the blocked page.

---

### Phase 2 — Keyword/pattern matching engine
**Goal**: Catch adult content on domains not in the static blocklist.
- Implement keyword/regex matching against URL path/query (via dynamic `declarativeNetRequest` rules or a `webNavigation` listener, whichever proves more reliable in testing)
- Curate an initial conservative keyword list (favor false negatives over false positives at this stage)
- Blocked page distinguishes which engine caught it (for later logging)

**Exit criteria**: A test URL not on the domain blocklist, but matching a keyword rule, gets blocked.

---

### Phase 3 — Local attempt logging (temporary)
**Goal**: Get a working attempt log before the service exists, using `chrome.storage.local`.
- Log timestamp, match_type, matched_value, domain on every block
- Minimal dashboard view in the popup (just a plain list, no styling priority yet)

**Exit criteria**: Blocked attempts appear in a visible log inside the extension. This storage is throwaway — it gets replaced in Phase 7, so don't over-invest in its schema.

---

### Phase 4 — Windows service skeleton
**Goal**: A minimal Rust Windows Service that installs, starts, stops, and logs to a file.
- Project setup with `windows-rs`
- Service install/uninstall via `sc.exe` or in-code service registration
- Runs at boot, restart-on-crash configured
- Basic file logging to confirm it's alive

**Exit criteria**: Service installs, survives a reboot, and produces log output confirming it's running — no business logic yet.

---

### Phase 5 — SQLite schema + service-side data layer
**Goal**: Implement the ERD schema inside the service.
- `rusqlite` integration, DB created at `%ProgramData%\Warden\warden.db`
- Create all tables from the ERD (`lock`, `lock_extension`, `emergency_unlock_request`, `blocked_attempt`, `blocklist_source`, `blocked_domain`, `keyword_rule`, `service_config`)
- Basic CRUD functions for each table, unit-testable in isolation from the Windows Service wrapper

**Exit criteria**: Service starts, creates the DB if missing, and CRUD operations can be exercised via a simple internal test harness.

---

### Phase 6 — Native Messaging bridge
**Goal**: Extension and service can talk to each other.
- Register the Native Messaging host manifest, pointing to the service's messaging endpoint
- Define the message protocol (e.g., `get_status`, `start_lock`, `extend_lock`, `request_emergency_unlock`, `log_attempt`)
- Extension popup can successfully round-trip a `get_status` call and display the (still-static/default) response

**Exit criteria**: A message sent from the extension reaches the service and a response renders in the popup.

---

### Phase 7 — Switch attempt logging to the service (source-of-truth cutover)
**Goal**: Retire the Phase 3 local log; the service becomes authoritative.
- Extension sends `log_attempt` messages instead of writing to `chrome.storage.local`
- Extension's log/dashboard view reads from the service via `get_attempts` instead of local storage
- Remove the Phase 3 local storage log code

**Exit criteria**: Blocking a test domain results in an entry in the service's SQLite DB, visible in the extension's log view, with no local-storage log remaining.

---

### Phase 8 — Lock/timer core logic (no enforcement yet)
**Goal**: Implement start/extend/expire lock logic in the service, reflected in the extension UI, but without registry enforcement yet — this isolates the timer logic from the tamper-resistance mechanism so each can be tested independently.
- `start_lock` (duration or forever), `extend_lock`, automatic expiry check (background tick in the service)
- Extension UI: countdown display, "extend" control, disabled cancel/shorten controls while active
- Enforce in service code that only one `lock` row can be active/forever at a time

**Exit criteria**: Starting a lock counts down accurately and expires on schedule; attempting to shorten/cancel via any exposed message is rejected by the service.

---

### Phase 9 — Registry policy enforcement
**Goal**: Tie the lock state to actual Chrome policy enforcement.
- Service writes `ExtensionSettings` registry policy on `start_lock`: force-install Warden's extension ID, force-allow Incognito, block removal
- Respect `service_config.registry_policy_merge_mode` — merge with any existing `ExtensionSettings` policy rather than blindly overwriting (per PRD §11 risk)
- Policy is removed/relaxed automatically on lock expiry (not before)

**Exit criteria**: While a lock is active, attempting to remove the extension via `chrome://extensions` fails or is blocked; Incognito windows have the extension active without manual toggling. After expiry, normal removal is possible again.

---

### Phase 10 — Tamper-resistance hardening
**Goal**: Make the service itself resistant to being stopped or removed mid-lock.
- Service refuses/delays `sc stop`/uninstall requests while a lock is active (or requires the emergency-unlock path)
- Re-assert registry policy and lock state on every service start (covers crash/reboot recovery)
- Test against: Task Manager process kill, `sc stop` from an elevated prompt, direct registry edits, extension force-removal attempts

**Exit criteria**: None of the above bypass an active lock in manual testing. Document any remaining bypass (e.g., full OS reinstall) as an accepted limitation, not a bug to fix.

---

### Phase 11 — Emergency unlock flow
**Goal**: Implement the "lock forever" escape hatch.
- `request_emergency_unlock` message → creates `emergency_unlock_request` row with 7-day cooldown
- Enforce single-pending-request-at-a-time in service logic
- Background check applies the unlock automatically once `cooldown_ends_at` passes
- Extension UI: request button (only visible when `is_forever` is true), visible countdown, cancel-during-cooldown option
- All requests/outcomes visible permanently in the dashboard log (Phase 12 dependency — can stub the log entry format here and finish the view in Phase 12)

**Exit criteria**: Requesting an emergency unlock during a "forever" lock correctly waits the full cooldown before releasing the lock, and is visible in the log regardless of outcome.

---

### Phase 12 — Dashboard & stats UI
**Goal**: Build the real dashboard view (replacing the plain list from Phase 3/7).
- Daily/weekly attempt counts (computed via query, not a separate stored table — per ERD §1)
- Streak counter (consecutive clean days)
- Attempt log view (default: last 30 days, per ERD open question — confirm before building pagination)
- Emergency unlock history section

**Exit criteria**: Dashboard accurately reflects real attempt data and streaks across a multi-day manual test.

---

### Phase 13 — Blocklist remote refresh
**Goal**: Keep the domain blocklist current without manual updates.
- Service fetches from the configured `blocklist_source.source_url` on the configured interval
- Bulk replace strategy per source (wipe-and-reload, per ERD open question) rather than row-by-row diffing
- Version hash comparison to skip unnecessary rule reloads
- Fallback to bundled list if fetch fails (offline resilience)

**Exit criteria**: A scheduled refresh updates `blocked_domain` and the extension's active `declarativeNetRequest` rules without requiring a browser restart.

---

### Phase 14 — End-to-end tamper testing & hardening pass
**Goal**: Adversarial testing pass against the finished system, treating yourself as the "attacker."
- Attempt every bypass from Phase 10 again now that all pieces are integrated
- Test registry-policy merge behavior against a machine with a pre-existing `ExtensionSettings` policy
- Confirm keyword/pattern false-positive rate is acceptable on a normal day of browsing (non-adult sites)
- Confirm service resource usage is negligible over a multi-day run
- Final review of the "recovery from total lockout" scenario flagged in PRD §11 — document the manual recovery path even if it's intentionally painful

**Exit criteria**: You're confident handing this to yourself for real, unsupervised use.
