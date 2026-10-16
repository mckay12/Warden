const NATIVE_HOST = 'com.warden.service';

function sendMessage(message) {
  return new Promise((resolve) => {
    chrome.runtime.sendNativeMessage(NATIVE_HOST, message, (response) => {
      if (chrome.runtime.lastError) {
        resolve({ ok: false, error: chrome.runtime.lastError.message, unreachable: true });
      } else {
        resolve(response ?? { ok: false, error: 'no response' });
      }
    });
  });
}

// ---- Lock card (mirrors popup.js's lock rendering, at a larger size —
// spec §3.7: "Current lock status card (mirrors popup 3.1–3.3 states,
// larger dial)". No dial yet here either, same as the popup — later
// polish pass.) ----

const lockViews = {
  loading: document.getElementById('lock-loading'),
  error: document.getElementById('lock-error'),
  idle: document.getElementById('lock-idle'),
  active: document.getElementById('lock-active'),
  forever: document.getElementById('lock-forever'),
};

function showLockView(name) {
  for (const [key, el] of Object.entries(lockViews)) {
    el.hidden = key !== name;
  }
}

let countdownTimer = null;
let euCountdownTimer = null;

function stopCountdown() {
  if (countdownTimer) {
    clearInterval(countdownTimer);
    countdownTimer = null;
  }
}

function stopEuCountdown() {
  if (euCountdownTimer) {
    clearInterval(euCountdownTimer);
    euCountdownTimer = null;
  }
}

function formatRemaining(ms) {
  if (ms <= 0) return '0s';
  const totalSeconds = Math.floor(ms / 1000);
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = totalSeconds % 60;
  if (h > 0) return `${h}h ${m}m remaining`;
  if (m > 0) return `${m}m ${s}s remaining`;
  return `${s}s remaining`;
}

function formatRemainingLong(ms) {
  if (ms <= 0) return 'any moment now';
  const totalMinutes = Math.floor(ms / 60000);
  const days = Math.floor(totalMinutes / 1440);
  const hours = Math.floor((totalMinutes % 1440) / 60);
  const minutes = totalMinutes % 60;
  if (days > 0) return `in ${days} day${days === 1 ? '' : 's'} ${hours} hour${hours === 1 ? '' : 's'}`;
  if (hours > 0) return `in ${hours} hour${hours === 1 ? '' : 's'} ${minutes} minute${minutes === 1 ? '' : 's'}`;
  return `in ${minutes} minute${minutes === 1 ? '' : 's'}`;
}

function renderActive(lock) {
  showLockView('active');
  const expiresAt = new Date(lock.expires_at).getTime();
  document.getElementById('active-until').textContent =
    `Locked until ${new Date(lock.expires_at).toLocaleTimeString()}`;

  const tick = () => {
    const remaining = expiresAt - Date.now();
    const el = document.getElementById('active-countdown');
    if (remaining <= 0) {
      el.textContent = 'Expiring…';
      stopCountdown();
      setTimeout(refreshLock, 1500);
    } else {
      el.textContent = formatRemaining(remaining);
    }
  };
  tick();
  stopCountdown();
  countdownTimer = setInterval(tick, 1000);
}

function renderForever(emergencyUnlock) {
  showLockView('forever');
  const euIdle = document.getElementById('eu-idle');
  const euPending = document.getElementById('eu-pending');
  document.getElementById('emergency-unlock-result').textContent = '';
  document.getElementById('cancel-emergency-unlock-error').textContent = '';

  stopEuCountdown();

  if (emergencyUnlock && emergencyUnlock.status === 'pending') {
    euIdle.hidden = true;
    euPending.hidden = false;

    const cooldownEndsAt = new Date(emergencyUnlock.cooldown_ends_at).getTime();
    const dateLabel = new Date(emergencyUnlock.cooldown_ends_at).toLocaleString();
    const tick = () => {
      const remaining = cooldownEndsAt - Date.now();
      const el = document.getElementById('eu-takes-effect');
      el.textContent = `Takes effect: ${dateLabel}, ${formatRemainingLong(remaining)}`;
      if (remaining <= 0) {
        stopEuCountdown();
        setTimeout(refreshLock, 1500);
      }
    };
    tick();
    euCountdownTimer = setInterval(tick, 60000);
  } else {
    euIdle.hidden = false;
    euPending.hidden = true;
  }
}

function renderLock(lock, emergencyUnlock) {
  stopCountdown();
  updateUndo(lock);
  if (!lock) {
    stopEuCountdown();
    showLockView('idle');
    document.getElementById('start-form').hidden = true;
    document.getElementById('start-error').textContent = '';
  } else if (lock.is_forever) {
    renderForever(emergencyUnlock);
  } else {
    stopEuCountdown();
    renderActive(lock);
  }
}

async function refreshLock() {
  const response = await sendMessage({ type: 'get_status' });
  if (!response.ok) {
    showLockView('error');
    document.getElementById('lock-error-text').textContent = response.unreachable
      ? `Service unreachable (${response.error})`
      : `Service error: ${response.error}`;
    return;
  }
  renderLock(response.lock, response.emergency_unlock);
}

document.getElementById('show-start-form-btn').addEventListener('click', () => {
  document.getElementById('start-form').hidden = false;
});

document.getElementById('start-timed-btn').addEventListener('click', async () => {
  const errorEl = document.getElementById('start-error');
  const { seconds: duration, error } = window.wardenDuration.read('start');

  if (error) {
    errorEl.textContent = error;
    return;
  }
  errorEl.textContent = '';

  // Long locks get an "are you sure" first (LONG_LOCK_SECONDS in duration-picker.js).
  if (!(await window.wardenDuration.confirmLong(duration))) {
    return;
  }

  const response = await sendMessage({ type: 'start_lock', duration_seconds: duration });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  renderLock(response.lock, null);
});

document.getElementById('start-forever-btn').addEventListener('click', async () => {
  const errorEl = document.getElementById('start-error');
  if (!(await window.wardenDuration.confirmTyped({
    title: 'Lock forever?',
    message: 'This lock has no end date. You will have 5 minutes to undo it. After that, the only way out is the 7-day emergency unlock.',
    phrase: 'forever',
    confirmLabel: 'Lock forever',
  }))) {
    return;
  }
  const response = await sendMessage({ type: 'start_lock', forever: true });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  renderLock(response.lock, null);
});

// ---- Undo window (slip protection) ----
// For a few minutes after a lock starts the service lets it be undone; the
// service decides (lock.undo_seconds_left) and enforces it. This only shows
// the button and counts down.

let undoTimer = null;

function stopUndo() {
  if (undoTimer) {
    clearInterval(undoTimer);
    undoTimer = null;
  }
  document.getElementById('undo-box').hidden = true;
}

function updateUndo(lock) {
  stopUndo();
  const secondsLeft = lock && lock.undo_seconds_left > 0 ? lock.undo_seconds_left : 0;
  if (!secondsLeft) return;

  const deadline = Date.now() + secondsLeft * 1000;
  const button = document.getElementById('undo-btn');
  const tick = () => {
    const s = Math.ceil((deadline - Date.now()) / 1000);
    if (s <= 0) {
      stopUndo();
      return;
    }
    button.textContent = `Undo this lock (${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')})`;
  };
  document.getElementById('undo-error').textContent = '';
  document.getElementById('undo-box').hidden = false;
  tick();
  undoTimer = setInterval(tick, 1000);
}

document.getElementById('undo-btn').addEventListener('click', async () => {
  const errorEl = document.getElementById('undo-error');
  if (!confirm('Undo this lock? It will end right away.')) {
    return;
  }
  const response = await sendMessage({ type: 'cancel_lock' });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  stopUndo();
  refreshLock();
});

// "+ Extend lock" opens an inline form (same picker as starting a lock);
// the Extend button inside it sends the request.
document.getElementById('extend-btn').addEventListener('click', () => {
  document.getElementById('extend-form').hidden = false;
});

document.getElementById('extend-cancel-btn').addEventListener('click', () => {
  document.getElementById('extend-form').hidden = true;
  document.getElementById('extend-error').textContent = '';
});

document.getElementById('extend-confirm-btn').addEventListener('click', async () => {
  const errorEl = document.getElementById('extend-error');
  const { seconds: added, error } = window.wardenDuration.read('extend');

  if (error) {
    errorEl.textContent = error;
    return;
  }
  errorEl.textContent = '';

  if (!(await window.wardenDuration.confirmLong(added, { extending: true }))) {
    return;
  }

  const response = await sendMessage({ type: 'extend_lock', added_seconds: added });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  document.getElementById('extend-form').hidden = true;
  renderLock(response.lock, null);
});

document.getElementById('emergency-unlock-btn').addEventListener('click', async () => {
  const resultEl = document.getElementById('emergency-unlock-result');
  const response = await sendMessage({ type: 'request_emergency_unlock' });
  if (!response.ok) {
    resultEl.textContent = response.error;
    return;
  }
  renderForever(response.request);
  refreshEmergencyUnlockHistory();
});

document.getElementById('cancel-emergency-unlock-btn').addEventListener('click', async () => {
  const errorEl = document.getElementById('cancel-emergency-unlock-error');
  const response = await sendMessage({ type: 'cancel_emergency_unlock' });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  renderForever(null);
  refreshEmergencyUnlockHistory();
});

refreshLock();

// ---- Streak + week comparison ----

sendMessage({ type: 'get_dashboard_stats' }).then((response) => {
  if (!response.ok) {
    document.getElementById('streak-number').textContent = '—';
    return;
  }
  document.getElementById('streak-number').textContent = response.streak_days;
  document.getElementById('this-week-value').textContent = response.this_week;
  document.getElementById('last-week-value').textContent = response.last_week;

  const deltaEl = document.getElementById('week-delta');
  const diff = response.this_week - response.last_week;
  if (diff === 0) {
    deltaEl.textContent = 'Same as last week';
  } else if (diff < 0) {
    deltaEl.textContent = `${Math.abs(diff)} fewer than last week`;
  } else {
    deltaEl.textContent = `${diff} more than last week`;
  }
});

// ---- Attempt log (last 30 days) ----

sendMessage({ type: 'get_attempts', days: 30 }).then((response) => {
  const table = document.getElementById('log-table');
  const tbody = document.getElementById('log-tbody');
  const empty = document.getElementById('log-empty');

  if (!response.ok) {
    empty.textContent = response.unreachable
      ? `Service unreachable (${response.error})`
      : `Could not load attempt log (${response.error}).`;
    empty.hidden = false;
    return;
  }

  const attempts = response.attempts;
  if (attempts.length === 0) {
    empty.hidden = false;
    return;
  }
  table.hidden = false;

  for (const entry of attempts) {
    const tr = document.createElement('tr');

    const tsTd = document.createElement('td');
    tsTd.className = 'ts';
    tsTd.textContent = new Date(entry.occurred_at).toLocaleString();

    const domainTd = document.createElement('td');
    domainTd.className = 'domain';
    domainTd.textContent = entry.domain;

    const matchTd = document.createElement('td');
    const badge = document.createElement('span');
    badge.className = `badge ${entry.match_type}`;
    badge.textContent = entry.match_type;
    matchTd.appendChild(badge);

    const actionTd = document.createElement('td');
    actionTd.className = 'row-action';
    const notAdult = document.createElement('button');
    notAdult.className = 'link-btn';
    notAdult.textContent = 'Not adult?';
    notAdult.title = `Ask to unblock ${entry.domain}`;
    notAdult.addEventListener('click', () => requestException(entry.domain));
    actionTd.appendChild(notAdult);

    tr.append(tsTd, domainTd, matchTd, actionTd);
    tbody.appendChild(tr);
  }
});

// ---- Exceptions (false positives) ----
// Asking only starts a cooldown (see service/src/exceptions.rs); cancelling or
// removing is instant, because that only ever makes blocking stricter.

let exceptionCooldownHours = 24;

function describeTimeLeft(seconds) {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${Math.max(m, 1)}m`;
}

async function requestException(rawDomain) {
  const errorEl = document.getElementById('exc-error');
  const domain = String(rawDomain || '').trim();
  if (!domain) {
    errorEl.textContent = 'Enter a domain, like example.com.';
    return;
  }
  if (!confirm(
    `Ask to unblock ${domain}?\n\nIt will stay blocked for ${exceptionCooldownHours} hours first, and you can cancel before then. ` +
    `Only do this if it isn't an adult site.`,
  )) {
    return;
  }
  const response = await sendMessage({ type: 'request_exception', domain });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  errorEl.textContent = '';
  document.getElementById('exc-domain').value = '';
  refreshExceptions();
}

function refreshExceptions() {
  return sendMessage({ type: 'list_exceptions' }).then((response) => {
    const list = document.getElementById('exc-list');
    const empty = document.getElementById('exc-empty');
    const form = document.getElementById('exc-form');
    list.innerHTML = '';

    if (!response.ok) {
      // An older service doesn't know these messages; the form would only error.
      form.hidden = true;
      empty.hidden = false;
      empty.textContent = response.unreachable
        ? `Service unreachable (${response.error})`
        : 'Exceptions need a newer version of the Warden service.';
      return;
    }
    form.hidden = false;
    exceptionCooldownHours = response.cooldown_hours || exceptionCooldownHours;
    document.getElementById('exc-hours').textContent = exceptionCooldownHours;
    empty.hidden = response.exceptions.length > 0;
    empty.textContent = 'No exceptions.';

    for (const ex of response.exceptions) {
      const li = document.createElement('li');

      const domain = document.createElement('span');
      domain.className = 'exc-domain';
      domain.textContent = ex.domain;

      const state = document.createElement('span');
      state.className = `exc-state ${ex.state}`;
      state.textContent = ex.state === 'pending'
        ? `unblocks in ${describeTimeLeft(ex.seconds_left)}`
        : 'unblocked';

      const action = document.createElement('button');
      action.className = 'link-btn';
      action.textContent = ex.state === 'pending' ? 'Cancel' : 'Remove';
      action.addEventListener('click', async () => {
        const result = await sendMessage({ type: 'remove_exception', id: ex.id });
        document.getElementById('exc-error').textContent = result.ok ? '' : result.error;
        refreshExceptions();
      });

      const meta = document.createElement('span');
      meta.className = 'exc-meta';
      meta.append(state, action);
      li.append(domain, meta);
      list.appendChild(li);
    }
  });
}

document.getElementById('exc-request-btn').addEventListener('click', () => {
  requestException(document.getElementById('exc-domain').value);
});
document.getElementById('exc-domain').addEventListener('keydown', (event) => {
  if (event.key === 'Enter') requestException(event.target.value);
});

refreshExceptions();
// Keep the countdowns honest while the page stays open.
setInterval(refreshExceptions, 30000);

// ---- Emergency unlock history ----

function refreshEmergencyUnlockHistory() {
  sendMessage({ type: 'get_emergency_unlock_history' }).then((response) => {
    const card = document.getElementById('eu-history-card');
    const list = document.getElementById('eu-history-list');
    list.innerHTML = '';

    if (!response.ok || response.requests.length === 0) {
      card.hidden = true;
      return;
    }
    card.hidden = false;

    for (const req of response.requests) {
      const row = document.createElement('div');
      row.className = 'eu-row';

      const left = document.createElement('div');
      const requestedLine = document.createElement('div');
      requestedLine.textContent = `Requested ${new Date(req.requested_at).toLocaleDateString()}`;
      const datesLine = document.createElement('div');
      datesLine.className = 'eu-dates';
      datesLine.textContent = `Cooldown ends ${new Date(req.cooldown_ends_at).toLocaleDateString()}`;
      left.append(requestedLine, datesLine);

      const outcome = document.createElement('span');
      outcome.className = `eu-outcome ${req.status}`;
      outcome.textContent = req.status;

      row.append(left, outcome);
      list.appendChild(row);
    }
  });
}

refreshEmergencyUnlockHistory();
