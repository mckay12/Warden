// Duration picker: separate years / days / hours / minutes / seconds fields,
// shared by the popup and the dashboard. Any element marked
//   <div data-duration-picker="NAME" data-default="Y,D,H,M,S"></div>
// is filled in with the fields and a live "Locks for ..." summary. Used for the
// "Start a lock" form ("start") and the "Extend lock" form ("extend").
//
//   window.wardenDuration.read(NAME)            -> { seconds } | { error }
//   window.wardenDuration.confirmLong(seconds)  -> true unless the user declines
//                                                  the "this is a long lock" warning
(() => {
  const MINUTE = 60;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;
  const YEAR = 365 * DAY; // a "year" here is always 365 days
  // Keep in step with MAX_LOCK_SECONDS in service/src/native_messaging.rs.
  const MAX_SECONDS = 100 * YEAR;
  // Locks (and extensions) at least this long get a typed confirmation first.
  const LONG_LOCK_SECONDS = 7 * DAY;
  // Keep in step with UNDO_WINDOW_SECONDS in service/src/lock_logic.rs.
  const UNDO_WINDOW_MINUTES = 5;

  const UNITS = [
    { key: 'years', label: 'Years', size: YEAR, one: 'year', many: 'years' },
    { key: 'days', label: 'Days', size: DAY, one: 'day', many: 'days' },
    { key: 'hours', label: 'Hours', size: HOUR, one: 'hour', many: 'hours' },
    { key: 'minutes', label: 'Mins', size: MINUTE, one: 'minute', many: 'minutes' },
    { key: 'seconds', label: 'Secs', size: 1, one: 'second', many: 'seconds' },
  ];

  // Non-negative whole number from a field's text; anything else counts as 0.
  function wholeNumber(text) {
    const n = Number(String(text).trim());
    return Number.isFinite(n) && n > 0 ? Math.floor(n) : 0;
  }

  function totalSeconds(values) {
    return values.reduce((sum, v, i) => sum + wholeNumber(v) * UNITS[i].size, 0);
  }

  // 5400 -> "1 hour, 30 minutes"
  function describe(seconds) {
    const parts = [];
    let rest = seconds;
    for (const u of UNITS) {
      const n = Math.floor(rest / u.size);
      rest -= n * u.size;
      if (n > 0) parts.push(`${n.toLocaleString('en-US')} ${n === 1 ? u.one : u.many}`);
    }
    return parts.join(', ');
  }

  function validate(seconds) {
    if (seconds <= 0) return 'Enter a duration greater than zero.';
    if (seconds > MAX_SECONDS) return 'The longest lock is 100 years.';
    return null;
  }

  function isLong(seconds) {
    return seconds >= LONG_LOCK_SECONDS;
  }

  function longLockMessage(seconds, extending, now = Date.now()) {
    const length = describe(seconds);
    if (extending) {
      return `You are adding ${length} to this lock.\n\nExtending cannot be undone, and it ends the ${UNDO_WINDOW_MINUTES}-minute undo window.`;
    }
    const until = new Date(now + seconds * 1000).toLocaleString();
    return `This lock will last ${length} (until ${until}).\n\nYou will have ${UNDO_WINDOW_MINUTES} minutes to undo it. After that it cannot be shortened or cancelled.`;
  }

  // Typing a confirmation phrase has to be a deliberate act, so it ignores case,
  // spaces and punctuation: "183 days", "183Days" and "183 DAYS." all match.
  function normalizePhrase(text) {
    return String(text).toLowerCase().replace(/[^a-z0-9]/g, '');
  }

  function phraseMatches(typed, expected) {
    const want = normalizePhrase(expected);
    return want.length > 0 && normalizePhrase(typed) === want;
  }

  if (typeof module !== 'undefined') {
    module.exports = { totalSeconds, describe, validate, isLong, longLockMessage, normalizePhrase, phraseMatches, MAX_SECONDS, LONG_LOCK_SECONDS, UNDO_WINDOW_MINUTES, YEAR, DAY };
    return;
  }

  // ---- DOM ----

  function container(name) {
    return document.querySelector(`[data-duration-picker="${name}"]`);
  }

  function inputsOf(el) {
    return UNITS.map((u) => el.querySelector(`input[data-unit="${u.key}"]`));
  }

  function valuesOf(name) {
    const el = container(name);
    return el ? inputsOf(el).map((i) => (i ? i.value : '')) : [];
  }

  function refreshSummary(el) {
    const summary = el.querySelector('.duration-summary');
    const seconds = totalSeconds(inputsOf(el).map((i) => i.value));
    summary.textContent = validate(seconds) ? '' : `${el.dataset.summaryPrefix || 'Locks for'} ${describe(seconds)}`;
  }

  function mount(el) {
    const name = el.dataset.durationPicker;
    const defaults = (el.dataset.default || '0,0,1,0,0').split(',').map((v) => wholeNumber(v));

    const row = document.createElement('div');
    row.className = 'duration-picker';
    UNITS.forEach((u, i) => {
      const unit = document.createElement('div');
      unit.className = 'duration-unit';
      const label = document.createElement('label');
      label.htmlFor = `${name}-${u.key}`;
      label.textContent = u.label;
      const input = document.createElement('input');
      Object.assign(input, { type: 'number', id: `${name}-${u.key}`, min: '0', step: '1', value: String(defaults[i] || 0) });
      input.inputMode = 'numeric';
      input.dataset.unit = u.key;
      if (u.key === 'years') input.max = '100';
      unit.append(label, input);
      row.appendChild(unit);
    });
    const summary = document.createElement('div');
    summary.className = 'duration-summary';
    el.replaceChildren(row, summary);
    refreshSummary(el);
  }

  window.wardenDuration = {
    read(name) {
      const seconds = totalSeconds(valuesOf(name));
      const problem = validate(seconds);
      return problem ? { error: problem } : { seconds };
    },
    // Resolves true unless the user declines. Short locks need no confirmation;
    // long ones (and long extensions) make you type the duration you are committing to.
    confirmLong(seconds, { extending = false } = {}) {
      if (!isLong(seconds)) return Promise.resolve(true);
      return confirmTyped({
        title: extending ? 'Add this much time?' : 'Start a long lock?',
        message: longLockMessage(seconds, extending),
        phrase: describe(seconds),
        confirmLabel: extending ? 'Extend' : 'Start lock',
      });
    },
    confirmTyped,
  };

  // In-page dialog (not window.confirm/prompt: those are unreliable in extension
  // popups). Confirm stays disabled until the phrase is typed; Esc or Cancel declines.
  function confirmTyped({ title, message, phrase, confirmLabel = 'Confirm' }) {
    return new Promise((resolve) => {
      const previouslyFocused = document.activeElement;
      const backdrop = document.createElement('div');
      backdrop.className = 'warden-modal-backdrop';
      backdrop.setAttribute('role', 'dialog');
      backdrop.setAttribute('aria-modal', 'true');

      const box = document.createElement('div');
      box.className = 'warden-modal';
      const heading = document.createElement('h3');
      heading.textContent = title;
      const text = document.createElement('p');
      text.className = 'warden-modal-text';
      text.textContent = message;
      const label = document.createElement('label');
      label.append('To confirm, type: ');
      const bold = document.createElement('b');
      bold.textContent = phrase;
      label.appendChild(bold);
      const input = document.createElement('input');
      input.type = 'text';
      input.id = 'warden-modal-input';
      input.autocomplete = 'off';
      input.spellcheck = false;
      label.htmlFor = input.id;

      const actions = document.createElement('div');
      actions.className = 'warden-modal-actions';
      const ok = document.createElement('button');
      ok.className = 'btn-primary';
      ok.textContent = confirmLabel;
      ok.disabled = true;
      const cancel = document.createElement('button');
      cancel.className = 'btn-secondary';
      cancel.textContent = 'Cancel';
      actions.append(ok, cancel);

      box.append(heading, text, label, input, actions);
      backdrop.appendChild(box);

      const finish = (answer) => {
        backdrop.remove();
        resolve(answer);
        // Put focus back on the control that opened the dialog, but a tick later:
        // doing it inside the Enter keydown would let that same keypress activate
        // the restored button (and reopen the dialog).
        if (previouslyFocused && previouslyFocused.focus) setTimeout(() => previouslyFocused.focus(), 0);
      };
      input.addEventListener('input', () => { ok.disabled = !phraseMatches(input.value, phrase); });
      input.addEventListener('keydown', (event) => {
        if (event.key === 'Enter') {
          event.preventDefault(); // also stops the keypress from reaching whatever gets focus next
          if (!ok.disabled) finish(true);
        }
      });
      backdrop.addEventListener('keydown', (event) => {
        if (event.key === 'Escape') {
          event.preventDefault();
          finish(false);
        }
      });
      ok.addEventListener('click', () => finish(true));
      cancel.addEventListener('click', () => finish(false));

      document.body.appendChild(backdrop);
      input.focus();
    });
  }

  document.querySelectorAll('[data-duration-picker]').forEach(mount);
  document.addEventListener('input', (event) => {
    const el = event.target && event.target.closest && event.target.closest('[data-duration-picker]');
    if (el) refreshSummary(el);
  });
})();
