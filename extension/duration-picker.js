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
  // Locks (and extensions) at least this long get a confirmation first.
  const LONG_LOCK_SECONDS = 7 * DAY;

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
      return `You are adding ${length} to this lock.\n\nOnce added, it cannot be shortened or cancelled. Extend it?`;
    }
    const until = new Date(now + seconds * 1000).toLocaleString();
    return `This lock will last ${length} (until ${until}).\n\nOnce started, it cannot be shortened or cancelled. Start it?`;
  }

  if (typeof module !== 'undefined') {
    module.exports = { totalSeconds, describe, validate, isLong, longLockMessage, MAX_SECONDS, LONG_LOCK_SECONDS, YEAR, DAY };
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
    confirmLong(seconds, { extending = false } = {}) {
      return !isLong(seconds) || window.confirm(longLockMessage(seconds, extending));
    },
  };

  document.querySelectorAll('[data-duration-picker]').forEach(mount);
  document.addEventListener('input', (event) => {
    const el = event.target && event.target.closest && event.target.closest('[data-duration-picker]');
    if (el) refreshSummary(el);
  });
})();
