// Runs after popup.js / dashboard.js.

// The window hides to the tray instead of closing, and these pages only fetch
// their data when they load — so without this, reopening shows whatever the
// page loaded with last time (stale lock state, stale attempt log). Reload
// whenever the window becomes visible or regains focus (unless a form is open). A short guard after
// each load stops a focus event fired by the reload itself from looping.
(() => {
  const loadedAt = Date.now();
  const MIN_AGE_MS = 2000;
  const refresh = () => {
    if (document.hidden || Date.now() - loadedAt < MIN_AGE_MS) return;
    // Never reload out from under someone who is mid-way through a form
    // (start / extend a lock): that would throw away what they typed, and a
    // native confirmation dialog closing also fires a focus event.
    if (document.querySelector('.start-form:not([hidden])')) return;
    location.reload();
  };
  document.addEventListener('visibilitychange', refresh);
  window.addEventListener('focus', refresh);
})();

// Popup only: replaces the extension-specific "rule count" line (which
// describes in-browser rules that don't exist here).
(() => {
  const el = document.getElementById('rule-count');
  if (!el) return;
  const text = 'System-wide DNS filter · enforced while a lock is active';
  const apply = () => { if (el.textContent !== text) el.textContent = text; };
  new MutationObserver(apply).observe(el, { childList: true, characterData: true, subtree: true });
  apply();
  const status = document.getElementById('status-text');
  if (status && status.textContent === 'Extension loaded') status.textContent = 'Warden desktop';
})();
