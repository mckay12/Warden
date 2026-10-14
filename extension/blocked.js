const params = new URLSearchParams(location.search);
const matched = params.get('matched'); // 'blocklist' | 'keyword'
const domain = params.get('domain');
const matchedValue = params.get('value') || domain;

const matchLabel = matched === 'keyword' ? 'keyword pattern' : 'domain blocklist';
document.getElementById('match-line').textContent = domain
  ? `Matched: ${matchLabel} (${domain})`
  : `Matched: ${matchLabel}`;

document.getElementById('go-back-btn').addEventListener('click', () => {
  if (history.length > 1) {
    history.back();
  } else {
    location.href = 'chrome://newtab';
  }
});

// Phase 7: log the attempt via the service (source of truth), not
// chrome.storage.local — the service owns this data now.
if (matched && domain) {
  chrome.runtime.sendNativeMessage(
    'com.warden.service',
    { type: 'log_attempt', match_type: matched, matched_value: matchedValue, domain },
    (response) => {
      if (chrome.runtime.lastError) {
        console.error('log_attempt failed:', chrome.runtime.lastError.message);
      } else if (!response || response.ok !== true) {
        console.error('log_attempt rejected:', response?.error);
      }
    }
  );
}
