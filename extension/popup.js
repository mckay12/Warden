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

// Both engines are dynamic rules now (Phase 13 moved the domain blocklist
// off the old static rules/blocklist.json) — distinguish them by ID range:
// keyword is a single rule at id 1000, domain rules start at 2000 (see
// background.js).
chrome.declarativeNetRequest.getDynamicRules().then((dynamicRules) => {
  const el = document.getElementById('rule-count');
  const keywordOn = dynamicRules.some((r) => r.id === 1000);
  const domainCount = dynamicRules.filter((r) => r.id >= 2000).length;
  el.textContent = `Domain blocklist: ${domainCount > 0 ? `${domainCount} domains` : 'off'} · Keyword engine: ${keywordOn ? 'enabled' : 'off'}`;
});

// ---- Lock section ----

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

// Longer-scale version for the emergency-unlock cooldown (up to 7 days),
// matching the spec's "in 6 days 4 hours" wording rather than the
// second-level precision the active-lock countdown uses.
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
      // The service's own tick will actually end it; re-poll shortly after
      // so the popup doesn't sit showing a stale "expiring" state.
      setTimeout(refreshStatus, 1500);
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
        // The service's own tick applies it; re-poll shortly after so the
        // popup doesn't sit showing a stale pending state.
        setTimeout(refreshStatus, 1500);
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

async function refreshStatus() {
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
  if (!window.wardenDuration.confirmLong(duration)) {
    return;
  }

  const response = await sendMessage({ type: 'start_lock', duration_seconds: duration });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  renderLock(response.lock);
});

document.getElementById('start-forever-btn').addEventListener('click', async () => {
  const errorEl = document.getElementById('start-error');
  // TODO(later UI phase): the spec's friction-adding confirmation modal
  // (§3.5) — this is a stand-in so "lock forever" isn't a single click.
  if (!confirm('This cannot be undone from here. Locking forever removes the option to stop blocking from this interface. Continue?')) {
    return;
  }
  const response = await sendMessage({ type: 'start_lock', forever: true });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  renderLock(response.lock);
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

  if (!window.wardenDuration.confirmLong(added, { extending: true })) {
    return;
  }

  const response = await sendMessage({ type: 'extend_lock', added_seconds: added });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  document.getElementById('extend-form').hidden = true;
  renderLock(response.lock, response.emergency_unlock);
});

document.getElementById('emergency-unlock-btn').addEventListener('click', async () => {
  const resultEl = document.getElementById('emergency-unlock-result');
  const response = await sendMessage({ type: 'request_emergency_unlock' });
  if (!response.ok) {
    resultEl.textContent = response.error;
    return;
  }
  renderForever(response.request);
});

document.getElementById('cancel-emergency-unlock-btn').addEventListener('click', async () => {
  const errorEl = document.getElementById('cancel-emergency-unlock-error');
  const response = await sendMessage({ type: 'cancel_emergency_unlock' });
  if (!response.ok) {
    errorEl.textContent = response.error;
    return;
  }
  renderForever(null);
});

refreshStatus();

// ---- Attempt log (Phase 7: reads from the service, not chrome.storage) ----

sendMessage({ type: 'get_attempts', limit: 10 }).then((response) => {
  const list = document.getElementById('log-list');
  const empty = document.getElementById('log-empty');

  if (!response.ok) {
    empty.textContent = response.unreachable
      ? `Service: unreachable (${response.error})`
      : `Could not load attempt log (${response.error}).`;
    empty.style.display = 'block';
    return;
  }

  const attempts = response.attempts;
  if (attempts.length === 0) {
    empty.style.display = 'block';
    return;
  }
  empty.style.display = 'none';

  for (const entry of attempts) {
    const li = document.createElement('li');

    const domainSpan = document.createElement('span');
    domainSpan.className = 'log-domain';
    const time = new Date(entry.occurred_at).toLocaleString();
    domainSpan.textContent = `${time} — ${entry.domain}`;

    const typeSpan = document.createElement('span');
    typeSpan.className = 'log-type';
    typeSpan.textContent = entry.match_type;

    li.append(domainSpan, typeSpan);
    list.appendChild(li);
  }
});

// ---- Dashboard link (Phase 12) ----
// A plain <a> inside a popup navigates the popup's own tiny window, not a
// real tab — window.open on an extension-page URL is what actually opens
// it as a full browser tab.
document.getElementById('dashboard-link').addEventListener('click', (event) => {
  event.preventDefault();
  window.open(chrome.runtime.getURL('dashboard.html'), '_blank');
});
