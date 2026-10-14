# Warden — Product Requirements Document

*Working title. Rename freely — used here for reference only.*

## 1. Overview

Warden is a two-part system that blocks access to pornographic and hentai content in Chrome, including in Incognito mode, with a self-imposed lock timer that cannot be bypassed or shortened once activated — even by uninstalling the extension or killing a process.

It consists of:
- A **Chrome extension (Manifest V3)** — blocking engine, popup UI, and stats dashboard.
- A **Windows background service** — authoritative lock/timer state, registry-level enforcement, and tamper resistance.

The two communicate via Chrome Native Messaging.

## 2. Problem Statement

Browser-only content blockers are trivially defeated by disabling the extension, using Incognito (which extensions don't run in by default), or simply uninstalling. A tool meant to support a genuine behavior-change commitment needs to remove the user's own in-the-moment ability to undo it — the lock has to survive attempts to defeat it, including by the same person who set it.

## 3. Goals

- Block known pornographic/hentai domains and keyword/pattern-matched content across all Chrome browsing modes, including Incognito.
- Let the user set a lock duration (seconds/minutes/hours/days) or an indefinite "lock forever" state.
- Once active, the lock cannot be shortened, cancelled, or removed by disabling/uninstalling the extension, killing the service process, or editing registry policy through normal (non-admin-recovery) means.
- Provide a stats dashboard so the user can see attempt history and track progress (streaks).
- Provide a single, deliberately slow escape hatch for "lock forever" so the tool isn't a permanent dead-end if circumstances genuinely change.

## 4. Non-Goals (v1)

- Cross-platform support (Mac/Linux) — Windows only for v1.
- On-page image/content analysis (ML-based visual detection) — deferred; v1 relies on domain blocklist + URL/keyword pattern matching only.
- Blocking other browsers (Firefox, Edge, etc.) — Chrome only for v1.
- Multi-user/family management (separate profiles, parental dashboards for a child's device) — this is a single-user self-control tool, not a parental control suite.
- Mobile support.

## 5. Users

Single persona: an adult user installing this on their own Windows machine to support a personal commitment to stop visiting adult content sites. Not designed or intended as covert monitoring of another person's device.

## 6. System Architecture

```
┌─────────────────────────┐        Native Messaging       ┌──────────────────────────┐
│   Chrome Extension       │ ◄──────────────────────────► │  Windows Background       │
│   (Manifest V3)          │                                │  Service (Rust)           │
│                          │                                │                          │
│  - declarativeNetRequest │                                │  - Authoritative lock     │
│    (blocklist)           │                                │    state + timer          │
│  - webNavigation +       │                                │  - Writes/enforces        │
│    pattern matching      │                                │    ExtensionSettings      │
│    (keywords/regex)      │                                │    registry policy        │
│  - Popup UI              │                                │  - Blocks extension       │
│  - Dashboard/stats view  │                                │    removal while locked   │
│  - Local attempt log     │                                │  - Runs at boot, survives │
│    (synced from service) │                                │    logoff                 │
└─────────────────────────┘                                └──────────────────────────┘
```

**Why two components:** Chrome extensions run sandboxed with no registry or OS-level access, and their storage is wiped on uninstall. Genuine tamper resistance — surviving an attempt to uninstall the extension or disable Incognito access — requires a component outside the browser sandbox with elevated privileges. The service is the source of truth; the extension is a thin, replaceable front end.

## 7. Functional Requirements

### 7.1 Blocking Engine
- **Domain blocklist**: bundled list of known adult/hentai domains, checked via `declarativeNetRequest` static rules.
  - Refresh strategy: periodic fetch from a maintained remote list (interval configurable, default daily) with bundled fallback list for offline use.
- **Keyword/pattern matching**: regex/keyword rules against URL path and query string (via `declarativeNetRequest` dynamic rules or `webNavigation` listener) to catch domains not yet in the blocklist.
  - v1 scope: URL only. Page title/meta content matching (requiring content script injection) is a possible v2 addition, not v1.
- Blocked navigation redirects to a local "blocked" page inside the extension (not a generic Chrome error page) so the attempt can be logged and the user sees remaining lock time.

### 7.2 Incognito Enforcement
- Service forces the registry `ExtensionSettings` policy to auto-grant Incognito access for Warden's extension ID, so it applies even in private windows without the user manually toggling it.
- Same policy configuration force-installs the extension and blocks its removal via `chrome://extensions` while a lock is active.

### 7.3 Timer & Lock System
- User sets a duration in seconds/minutes/hours/days, or selects "Lock forever."
- Once started:
  - **Extending** an active lock is always allowed (adding time carries no risk to the tool's purpose).
  - **Shortening or cancelling** an active lock is not possible through any UI while running.
  - Timer countdown is held authoritatively in the service (not extension storage), so removing/reinstalling the extension does not reset or affect it.
- **Lock forever** is permanent through normal use. The only path off it is the emergency unlock (7.4).

### 7.4 Emergency Unlock (escape hatch for "Lock forever")
- Requestable from within the extension UI or a service-level fallback interface.
- Does not take effect immediately — fixed cooldown (default 7 days, not user-configurable while a lock is active) before the unlock applies.
- Once requested, it's timestamped and irreversible-in-timing (can't be sped up), but can be cancelled during the cooldown if the user changes their mind.
- Every emergency unlock request and its outcome is recorded permanently in the dashboard log — it should be visible, not something you can quietly do and forget.

### 7.5 Dashboard & Stats
- **Attempt log**: timestamp, matched domain or keyword/pattern, which engine caught it.
- **Daily/weekly attempt counts** with simple trend view.
- **Streak counter**: consecutive days with zero blocked attempts.
- **Top attempted domains** (if recurring) — v1, low priority, can ship after the above.
- Explicitly excluded from v1: full browsing history, time-of-day heatmaps, or any data beyond what's needed to see attempts and progress — avoiding turning the tool into a surveillance log of yourself.

### 7.6 Native Windows Service
- Built in Rust, using `windows-rs` for Windows Service API and registry access.
- Installed as a proper Windows Service (auto-start at boot, restart-on-crash, requires admin privileges to install/uninstall).
- Responsibilities:
  - Own and persist lock/timer state (outside the extension's reach).
  - Write and continuously re-assert the `ExtensionSettings` registry policy (force-install, force-incognito, block-removal) for the duration of any active lock.
  - Refuse to stop itself or accept an uninstall request while a lock is active (service uninstall attempts during an active lock should fail or require the same emergency-unlock path).
  - Expose a local Native Messaging host for the extension to query status and submit requests (start lock, extend lock, request emergency unlock).

## 8. Non-Functional Requirements

- **Tamper resistance is the primary quality bar.** Every feature should be evaluated against: "can a user in the moment of temptation undo this?" If yes, redesign.
- **Low footprint**: the service should be near-invisible in resource usage — this is meant to run permanently in the background.
- **Fail-safe on crash**: if the service crashes or the machine loses power, blocking policy should re-assert itself on restart, not silently disable.
- **No cloud dependency for enforcement**: lock state and blocking should work fully offline; only the domain-blocklist refresh needs network access.
- **Privacy**: no attempt log or stats data leaves the local machine.

## 9. Data Model (high level — full ERD to follow)

- `LockState`: status (inactive/active/forever), start time, duration, expiry time (nullable for forever), extension history
- `EmergencyUnlockRequest`: requested_at, cooldown_ends_at, status (pending/cancelled/applied)
- `BlockedAttempt`: timestamp, matched_value, match_type (blocklist/keyword), url_redacted_domain_only
- `BlocklistSource`: last_refreshed_at, source_url, version/hash

## 10. Success Metrics

- Zero successful bypasses of an active lock during internal testing (uninstall attempts, Incognito access, registry edits without admin recovery, process kill attempts).
- Streak counter and attempt log accurately reflect real navigation attempts with no false negatives on the bundled blocklist.
- Service resource usage stays negligible (target: comparable to a typical lightweight background utility).

## 11. Risks & Open Questions

- **Registry policy conflicts**: if the user's machine already has `ExtensionSettings` policies configured (e.g., from another tool or corporate management), Warden's policy writes could conflict. Needs a merge strategy, not a blind overwrite.
- **False positives from keyword matching**: overly broad keyword rules could block legitimate sites (health, art, education). Needs a curated, conservative keyword list plus a way to review false positives after the fact (not during, to avoid a bypass vector).
- **Admin/UAC prompts**: installing a service that survives uninstall attempts inherently requires admin rights at install time — the PRD assumes the user installs this themselves with full awareness of what it does.
- **Recovery from total lockout**: if the service state becomes corrupted during a "lock forever" period with no emergency unlock requested, there needs to be a documented (if painful) manual recovery path — this should be decided before build, not discovered during one.

## 12. Out of Scope / Future Considerations

- Cross-platform (Mac/Linux) support
- On-page visual/ML content detection
- Multi-browser support beyond Chrome
- Mobile companion
