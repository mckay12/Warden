# Warden — Phase 1 + 2 + 3 + 6 + 7 + 8 + 11 + 12 + 13 + 14

Extension skeleton with keyword/pattern matching (Phase 2), a Native
Messaging connection to the Rust service (Phase 6), the attempt log backed
by the service's SQLite DB instead of local storage (Phase 7), a real popup
UI for starting/extending a lock (Phase 8), the emergency-unlock flow for a
"forever" lock (Phase 11), a real dashboard page (Phase 12), and — as of
Phase 13 — a **dynamic** domain blocklist sourced live from the service
instead of Phase 1's hardcoded static `rules/blocklist.json` (deleted
entirely this phase). Phase 14 hardened the keyword engine's regex to cut a
real false-positive class (see **Known limitations** below) as part of an
end-to-end tamper-testing pass covering the whole system. This is still a
functional shell, not the final visual design — no dial yet, that's a later
polish pass now that the functional core (lock/timer, tamper resistance,
emergency unlock, dashboard, live blocklist) is essentially done.

## Load it

1. `chrome://extensions`
2. Enable **Developer mode** (top right)
3. **Load unpacked** → select this `extension/` folder

This extension's ID is pinned via the `key` field in `manifest.json` (backed
by `dev-signing-key.pem` in this folder) so it's always
`pjnjbihjibbooihkcdmcdjdpbbmkfdkk`, regardless of which machine or path it's
loaded from. That's required for Native Messaging — the host manifest's
`allowed_origins` has to name the extension's exact ID, and without a pinned
key an unpacked extension's ID is derived from its folder path and would
differ per checkout. **Keep `dev-signing-key.pem`** — losing it changes the
ID and breaks the native host registration (`service/src/native_host_install.rs`
would need updating to match, and the host would need re-registering).

## Test it

**Domain blocklist (Phase 13 — now live, not static)**: this used to have a
safe, fake test domain (`warden-test-block.example`) baked into a static
rules file. That file is gone — the blocklist is now ~2000 real domains
fetched live from a public adult-content list by the service
(`service/src/blocklist_refresh.rs`) and pulled into the extension's
dynamic `declarativeNetRequest` rules by `background.js`. There's no safe
placeholder domain to test against anymore; testing this for real means
navigating to an actual listed domain and confirming the redirect
intercepts it *before* any real connection completes (the same way any
ad/content-blocker extension's blocklist gets verified) — deliberately not
something to script or automate repeatedly. See `service/README.md`'s
Phase 13 findings for exactly how far automated verification got and where
it stopped for that reason. To check without picking a domain yourself: `chrome://extensions` →
Warden → "service worker" → Inspect → in that DevTools console, run
`chrome.declarativeNetRequest.getDynamicRules().then(r => console.log(r.length, r.filter(x => x.id >= 2000).length))`
— should show ~2001 total rules, ~2000 of them domain rules (id ≥ 2000).

**Keyword engine**: navigate to `http://example.com/some-porn-video` (a real,
always-safe domain — it ignores the path and normally just shows a generic
placeholder page, but here it should never even load because the redirect
intercepts it first). Should redirect to the blocked page with
`matched=keyword` instead of `matched=blocklist`. As of Phase 14 the regex
is word-boundaried (`\b(...)\b`), so `http://example.com/some-pornography-article`
should now correctly *not* match — see **Known limitations** for what's
still a known gap versus what this fixed.

**Native Messaging (Phase 6) + attempt log (Phase 7)**: for either to work,
the service side needs to be set up first:

1. Build the service (`service/`) and run `warden-service.exe
   register-native-host` once — this writes
   `%ProgramData%\Warden\native-messaging-host.json` and a
   `HKCU\...\NativeMessagingHosts\com.warden.service` registry entry
   pointing at it (see `service/README.md`).
2. Load this extension (its ID must be the pinned one above, matching the
   host manifest's `allowed_origins`).

Then open the popup — it shows "Service: reachable — no active lock" if the
`get_status` round trip worked, or "Service: unreachable (...)" if the host
isn't registered or its registered `path` doesn't point at the exe you
actually built.

**Attempt log**: trigger either blocklist or keyword test above. The blocked
page sends a `log_attempt` message to the service (fire-and-forget — check
the page's console if you want to confirm it didn't error). Then open the
popup: it calls `get_attempts` and should show the entry under "Attempt log
(from service)". Nothing is written to `chrome.storage` anymore — the
`storage` permission was removed from the manifest entirely since nothing
uses it now.

**Lock / timer (Phase 8)**: open the popup with no lock active — it shows
"No lock active" and a "Start a lock" button, which reveals a duration
(seconds) field plus "Start" and "Lock forever". Start a short one (try 10)
and the popup switches to a live countdown ("Xs remaining") ticking down
client-side every second, computed from the `expires_at` the service
returned — no repeated Native Messaging calls needed just to keep the
countdown moving. An "+ Extend lock" button adds a fixed 5 minutes (see
`EXTEND_SECONDS` in `popup.js` — a real duration picker is later polish, not
Phase 8 scope). Let the timer run out and the countdown switches to
"Expiring…", then the popup re-polls shortly after and should fall back to
the idle view once the service's own tick (checked every second,
independent of the popup being open) has actually ended it.

Try "Lock forever" and the popup shows "Locked indefinitely" with a
"Request emergency unlock" button. There is deliberately no cancel or
shorten control for the *lock itself* in either state — not hidden, not
disabled, simply absent, matching the UI/UX spec. The service backs this up
independently: `extend_lock` rejects zero/negative `added_seconds`, a
second `start_lock` while one's already active is rejected, and undefined
message types (`cancel_lock`, etc.) just fall through to "unknown message
type" — so there's no way to shorten or cancel a *lock* through the
protocol even if something other than this popup were driving it.

**Emergency unlock (Phase 11)**: click "Request emergency unlock" on a
forever lock — the popup switches to a pending view: "Emergency unlock
requested / Takes effect: [date], in N days M hours / Cancel request",
matching the spec's §3.8 copy exactly. The countdown ticks down every
minute (a 7-day window doesn't need second-level precision the way the
lock countdown does). Click "Cancel request" and it goes back to the
"Request emergency unlock" button — cancelling is explicitly allowed while
a request is still pending (PRD §7.4), unlike the lock itself. Let a
request's cooldown actually elapse (only really testable by directly
editing the service's DB — the real 7-day wait isn't something you can sit
through) and the service's own tick applies it automatically, ending the
forever lock; the popup re-polls and falls back to the idle view without
you needing to do anything.

**Dashboard (Phase 12)**: click "View dashboard →" at the bottom of the
popup — opens `dashboard.html` as a real browser tab (not inside the tiny
popup window; a plain `<a>` would just navigate the popup itself, so this
goes through `window.open(chrome.runtime.getURL(...))` instead). Two
columns above 720px wide, one column below. The left sidebar has the same
lock card as the popup (idle / active / forever, fully functional — start,
extend, forever, emergency unlock, all work from here too), a streak
number, and a this-week-vs-last-week comparison. The right side has the
attempt log (last 30 days, via `get_attempts` with a `days` field) and an
emergency unlock history section that only appears at all once a request
has ever been made (per spec §3.7 — no empty placeholder card). Badge
styling for `blocklist` vs `keyword` matches the spec exactly
(`accent-steel` fill vs. a quiet muted outline).

**Blocklist refresh (Phase 13)**: `background.js` re-fetches the domain
list and rebuilds its dynamic rules every 30 minutes via `chrome.alarms`
(survives service worker suspension, unlike `setInterval` — the actual
mechanism behind "refresh without a browser restart"). To see it react to a
real service-side change sooner than that: on the service side run
`warden-service.exe refresh-blocklist` (forces an immediate refresh
regardless of the configured interval), then either wait up to 30 minutes
or reload the extension (`chrome://extensions` → Warden → reload icon) to
pick it up immediately. If the service is unreachable when the alarm
fires, `background.js` deliberately leaves the existing dynamic rules
untouched rather than clearing them — a transient hiccup shouldn't
silently widen what gets through.

## What's here

- `manifest.json` — MV3, `declarativeNetRequest` + `nativeMessaging` +
  `alarms` permissions (no `storage` — nothing uses `chrome.storage`
  anymore)
- `background.js` — registers the dynamic keyword-matching rule (Phase 2)
  and, as of Phase 13, one dynamic `urlFilter` rule per domain from
  `get_blocklist_domains` (not one big regex — Chrome's per-rule
  `regexFilter` length cap couldn't hold more than ~100-150 domains that
  way; a plain `urlFilter` rule per domain has no such limit). Re-registers
  on install/startup and every 30 minutes via `chrome.alarms`.
- `popup.html/css/js` — confirms both blocking engines are enabled, lists
  the last 10 attempts via `get_attempts`, renders real lock state from
  `get_status` with three views (idle / active-timed / forever) driven by
  `start_lock` / `extend_lock`, and (Phase 11) the forever view's two
  sub-states — no pending request vs. pending with a live cooldown —
  driven by `request_emergency_unlock` / `cancel_emergency_unlock`
- `blocked.html/css/js` — local page shown on a blocked navigation. Reads
  `matched` (`blocklist` | `keyword`), `domain`, and `value` (the specific
  blocklisted domain or matched keyword) from the redirect's query string,
  and sends a `log_attempt` Native Message with them
- `dashboard.html/css/js` (Phase 12) — the full-page dashboard: a lock card
  (same states/controls as the popup, larger), streak + week-comparison
  cards, the 30-day attempt log table, and emergency unlock history. Opened
  from the popup's "View dashboard →" link via `window.open`, not a plain
  navigation.
- `dev-signing-key.pem` — the RSA key pinning this extension's ID (see
  above). Not used by the extension at runtime — only referenced by the
  `key` field in `manifest.json`.

## Known limitations

Both engines only match `main_frame` (top-level) navigations, and the
keyword regex is anchored to the path/query — it won't flag a keyword that
happens to appear only in the hostname. Content served without a full page
navigation (e.g. embedded iframes) isn't covered yet; that's beyond Phase 1/2
scope.

**Keyword false positives (Phase 14)**: testing the keyword engine against
real, realistic URLs (not just the hardcoded test case above) surfaced a
genuine over-blocking problem the PRD (§11) explicitly warns about:
Wikipedia's `/wiki/Pornography` and `/wiki/Child_pornography_laws` articles,
and similar encyclopedic/legal/recovery-resource pages, were being blocked
outright because `porn` matched as a plain substring inside `pornography`.
Fixed by word-boundarying the regex (`\b(${KEYWORDS.join('|')})\b`) — this
is a strict improvement verified against real URLs (Wikipedia's
"Pornography"/"Child_pornography_laws" articles now correctly load; every
tested real adult-content URL pattern still matches). Two things this did
**not** fix, left as known, accepted gaps rather than glossed over:

- A URL path segment that *is* exactly the bare keyword (e.g. Wikipedia's
  `/wiki/Hentai`) still matches — word boundaries can't distinguish "the
  article about X" from "X itself" using the keyword alone. `nhs.uk`'s
  `/porn-addiction-help/` page is a similar case, but arguably a *correct*
  match rather than a false positive — it's a page about porn, discussing
  porn addiction using that literal word, which is closer to the PRD's
  "recovery resource" concern in spirit than the encyclopedia case, but
  isn't a clean win either way.
- `xxx`-in-clothing-sizes (e.g. `size-xxx-large`) still matches, since
  hyphens aren't word characters so `xxx` is still boundary-matched as a
  standalone "word" there. Not fixed this phase — flagged as a genuine
  precision/recall product trade-off (removing `xxx` from `KEYWORDS` fixes
  this but stops catching a common, unambiguous real adult-content term),
  not something to decide unilaterally.

**Domain blocklist size (Phase 13)**: the real source list has 953,393
entries; only the first 2000 (per the order the source happens to list
them in, not curated or prioritized in any way) become actual browser
rules — a hard Chrome `declarativeNetRequest` rule-count ceiling, not a
choice that could be raised much further even if wanted. See
`service/README.md`'s Phase 13 findings for the exact limits checked and
why. `background.js` and `service/src/blocklist_refresh.rs` both hardcode
this cap independently (`DOMAIN_RULE_ID_MAX_COUNT` / `MAX_DOMAINS`) — they
aren't drift-proof against each other if one changes without the other.

**Not verified in a real browser (Phase 13)**: whether ~2000 dynamic domain
rules actually register successfully and a real listed domain actually
redirects. Attempted via headless Chrome + direct CDP introspection of the
extension's own service worker (not just watching page behavior) — hit the
same "headless can't reliably run unpacked MV3 extensions" wall that's
shown up since Phase 1, this time even at the service-worker-target level
(the CDP target list never showed Warden's own context, only unrelated
ones). See `service/README.md`'s Phase 13 findings for exactly what was
tried, including one attempt that inadvertently reached a real domain from
the blocklist (a bot-check page, not adult content, but a real request
regardless) before switching approaches — flagged there rather than glossed
over.
