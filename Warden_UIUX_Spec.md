# Warden — UI/UX Specification

## 1. Design Direction

**Subject**: a self-imposed commitment device. The honest reference point isn't a typical "productivity app" — it's closer to a bank vault's time-lock, or a ship's log. The tool doesn't negotiate, doesn't moralize, and doesn't perform concern. It states facts: what's blocked, how much time is left, what happened. The user is the one who set the terms; the interface's job is just to hold them to it and show the record honestly.

**Tone constraint (non-negotiable)**: no shaming, no encouragement-as-decoration, no exclamation marks, no "you've got this!" copy. This is a tool for a serious personal commitment, not a gamified habit-tracker. Every string should read like it was written by the lock itself, not a cheerleader. See §5 for specific copy.

**Signature element**: a circular time-lock dial — a ring that fills clockwise as the current lock period elapses, rendered wherever the countdown appears (popup, dashboard, blocked page). It's the one recognizably "Warden" visual, used consistently rather than reinvented per screen. Everything else stays quiet around it.

## 2. Design Tokens

**Color** (dark surface — this tool lives in a place that should feel deliberate and low-stimulation, not bright or app-store-cheerful):

| Token | Hex | Use |
|---|---|---|
| `bg-base` | `#14171C` | App/page background |
| `bg-surface` | `#1D2128` | Cards, panels |
| `border-hairline` | `#2A2F38` | Dividers, card borders |
| `text-primary` | `#E7E9EC` | Primary text |
| `text-muted` | `#8B93A0` | Secondary text, timestamps, labels |
| `accent-steel` | `#5B7A99` | Primary actions, the lock dial's default state, links |
| `signal-amber` | `#D98E3B` | Active lock countdown, warnings, pending emergency-unlock |
| `signal-green` | `#5FA777` | Streak counter, "clean day" indicators, success states |
| `signal-red` | `#B8564B` | Blocked-page indicator only — used sparingly, never for routine UI |

**Typography**:
- **Numerals/timer/data** (`Warden Mono` role): `JetBrains Mono` — used for the countdown dial's number, attempt counts, dates in the log. Monospace reinforces the "instrument panel" feel and makes numbers easy to scan.
- **UI/body** (`Warden Sans` role): `Inter` — labels, buttons, body copy, dashboard headings.
- Scale: 12 / 14 / 16 / 20 / 28 / 40px. The 40px size is reserved for the dial's central countdown number only — nothing else competes with it.

**Layout**:
- Popup: fixed 380×560px (Chrome popup constraints) — single column, no horizontal scrolling ever.
- Dashboard: opens as a full extension page (`chrome-extension://.../dashboard.html`), responsive from 480px up, single-column below 720px, two-column (stats sidebar + log) above.
- Radius: 6px on cards and buttons — enough to soften, not enough to feel playful. No pill-shaped buttons.
- Spacing scale: 4 / 8 / 12 / 16 / 24 / 32px.

**Motion**: minimal, and only where it reinforces meaning:
- The dial's fill animates smoothly in real time (it's a live countdown, not a static ring).
- Lock activation: a single deliberate fill-in of the dial from empty, ~600ms, on confirm — this is the one moment worth a flourish.
- Everything else (popup open, dashboard load, log entries appearing): no animation, instant render. Respect `prefers-reduced-motion` by freezing the dial to tick-only updates (no smooth interpolation) rather than removing it.

## 3. Screens

### 3.1 Popup — Idle (no active lock)
```
┌──────────────────────────────┐
│  WARDEN                       │  ← wordmark, text-muted, small caps
│                                │
│     ┌───────────────┐         │
│     │               │         │
│     │   (empty      │         │  ← dial, unfilled, accent-steel outline
│     │    dial)      │         │
│     │               │         │
│     └───────────────┘         │
│      No lock active           │  ← text-muted
│                                │
│  [ Start a lock ]              │  ← primary button, accent-steel
│                                │
│  ── Today ──                   │
│  0 attempts blocked            │  ← text-primary, small
│  Streak: 4 days                │  ← signal-green
│                                │
│  View dashboard →              │  ← text link, opens full page
└──────────────────────────────┘
```

### 3.2 Popup — Active lock (timed)
```
┌──────────────────────────────┐
│  WARDEN                       │
│                                │
│     ┌───────────────┐         │
│     │   2h 14m       │         │  ← 40px mono, signal-amber
│     │   remaining    │         │  ← dial filled proportionally
│     └───────────────┘         │
│                                │
│  Locked until 4:30 PM          │  ← text-muted, exact timestamp
│                                │
│  [ + Extend lock ]              │  ← secondary button — only action available
│                                │
│  ── Today ──                   │
│  2 attempts blocked            │
│  Streak: 4 days                │
│                                │
│  View dashboard →              │
└──────────────────────────────┘
```
Note: no cancel/shorten control exists anywhere in this state — not hidden, not disabled-with-tooltip, simply absent. Absence communicates "this isn't a control you have" more honestly than a greyed-out button would.

### 3.3 Popup — Active lock (forever)
Same layout as 3.2, but the dial is fully filled and static (no countdown), center text reads "Locked" in signal-amber, and below the "Extend lock" position is instead:
```
  Locked indefinitely
  [ Request emergency unlock ]   ← tertiary/text-style button, not primary —
                                    this shouldn't visually compete with "normal" actions
```

### 3.4 Lock setup flow (triggered by "Start a lock")
Single screen, replaces popup content temporarily:
```
  Set lock duration

  [ Days ] [ Hours ] [ Minutes ] [ Seconds ]   ← segmented control, one active unit at a time
  [   input field   ]

  ─── or ───

  [ Lock forever ]   ← distinct button, signal-red-adjacent seriousness via a
                        confirmation step (3.5), not color — keep it in accent-steel
                        family, don't dramatize it with red

  [ Cancel ]  [ Confirm lock ]
```

### 3.5 Forever-lock confirmation (extra step, deliberately friction-adding)
A single modal, plain language, no dramatic iconography:
```
  This cannot be undone from here.

  Locking forever removes the option to stop blocking from
  this interface. The only way back is an emergency unlock
  request, which takes 7 days to take effect once requested.

  [ Cancel ]  [ Confirm — lock forever ]
```
This is the one place where slowing the user down is explicitly the design goal — no auto-focus on "Confirm," and "Cancel" is visually first (left) since that's the safer default.

### 3.6 Blocked page (full tab, shown on navigation to a blocked site)
```
┌────────────────────────────────────────┐
│                                          │
│              [ dial icon ]              │
│                                          │
│              This site is blocked        │
│                                          │
│        Matched: domain blocklist         │  ← or "keyword pattern", per §7.5 of PRD
│                                          │
│        Lock active — 2h 14m remaining    │  ← only shown if a lock is active;
│                                          │     if no lock, omit this line entirely
│                                          │
│              [ Go back ]                 │
└────────────────────────────────────────┘
```
Deliberately plain — no red/alarm styling, no lecture text. `signal-red` is used only for the small dial-icon accent here, nowhere else in the product. The page states what happened and offers exactly one action.

### 3.7 Dashboard (full extension page)
Two-column above 720px width:

**Left column (stats sidebar)**:
- Current lock status card (mirrors popup 3.1–3.3 states, larger dial)
- Streak counter, large mono numeral, signal-green
- This week's attempt count vs. last week (simple comparison, no chart needed for v1 — a chart is a v2 nice-to-have, not required)

**Right column (log)**:
- Attempt log, most recent first, default window: last 30 days (per ERD open question — confirm this before build)
- Each row: timestamp (mono), domain, match type badge (`blocklist` / `keyword`)
- Emergency unlock history section below the log, if any requests exist — each entry shows requested date, cooldown end date, and outcome (applied/cancelled)

Empty state (first run, zero attempts logged): don't show an empty table with "no data" — instead: *"No blocked attempts yet. This log fills in as Warden catches something."* — factual, not celebratory.

### 3.8 Emergency unlock — pending state
Shown in the dashboard and popup (3.3) once requested:
```
  Emergency unlock requested
  Takes effect: [date], in 6 days 4 hours

  [ Cancel request ]
```

## 4. Component States Checklist
For Claude Code to reference during build — every interactive element needs these states defined before implementation, not discovered during it:
- Buttons: default, hover, focus-visible (visible keyboard outline — required, not optional), disabled, pressed
- Dial: empty, filling (live), full/forever, reduced-motion variant (tick-only, no interpolation)
- Log rows: default, and a subtle distinction for `keyword`-matched vs `blocklist`-matched rows (badge color: `accent-steel` for blocklist, `text-muted` outline for keyword — keep it quiet, this is metadata not a warning)

## 5. Copy Guidelines

Voice: plain, present-tense, states facts. Never apologizes, never congratulates in exclamation points, never uses second-person guilt framing ("you failed," "you gave in").

| Situation | Don't write | Do write |
|---|---|---|
| Blocked navigation | "Oops! This site isn't allowed 🚫" | "This site is blocked." |
| Lock active, user checks popup | "Stay strong! You've got this!" | "Locked until 4:30 PM." |
| Streak broken | "You broke your streak :(" | "Streak reset. New streak starts today." |
| Emergency unlock pending | "Are you sure you want to give up?" | "Emergency unlock requested. Takes effect [date]." |
| Empty log | "Nothing here yet, get blocking!" | "No blocked attempts yet." |

Button labels are always the action itself, present tense, no cleverness: "Start a lock," "Extend lock," "Request emergency unlock," "Cancel request" — never "Let's go," "I'm ready," etc.

## 6. Accessibility & Responsiveness
- Full keyboard navigation through popup and dashboard; visible focus outline in `accent-steel`, 2px, never removed via `outline: none` without a replacement.
- Color is never the only signal — the amber/green/red states are always paired with text ("Locked," "Streak: 4 days," "Blocked").
- Dashboard reflows to single-column below 720px; popup has a fixed size by Chrome's own constraints, so no responsive work needed there beyond ensuring text never truncates awkwardly at 380px width.
- `prefers-reduced-motion` respected as described in §2 (Motion).
