// Keyword/pattern engine (Phase 2): catches adult content on domains not in
// the domain blocklist, by matching against the URL path + query string.
//
// Conservative on purpose (favor false negatives over false positives, per
// build plan) — these are compound/unambiguous terms unlikely to appear on
// legitimate (health, art, education) sites, not broad single words like
// "sex" or "adult" that would over-block.
const KEYWORD_RULE_ID = 1000;
const KEYWORDS = ['porn', 'xxx', 'hentai', 'nsfw', 'sextape'];

function keywordRegex() {
  // Anchored past the domain so a keyword has to appear in the path/query,
  // not just be a substring of the hostname itself. Word-boundaried on both
  // sides (Phase 14 hardening) — without it, "porn" also matched inside
  // "pornography", which meant Wikipedia's own "Pornography" and
  // "Child_pornography_laws" articles, and similar encyclopedic/legal/
  // recovery-resource pages, were getting blocked outright (confirmed via
  // direct regex testing against real URLs before this fix) — exactly the
  // "health, art, education" over-blocking risk the PRD (§11) calls out.
  // Word boundaries don't catch every case (a URL path segment that IS
  // exactly the bare keyword, e.g. Wikipedia's "/wiki/Hentai", still
  // matches — there's no way to tell "the article about X" from "X itself"
  // from the keyword alone) — see extension/README.md's Phase 14 findings
  // for what's still a known, accepted gap versus what this fixed.
  return `^https?://([^/]+)/[^\\s]*\\b(${KEYWORDS.join('|')})\\b`;
}

async function registerKeywordRules() {
  await chrome.declarativeNetRequest.updateDynamicRules({
    removeRuleIds: [KEYWORD_RULE_ID],
    addRules: [
      {
        id: KEYWORD_RULE_ID,
        priority: 1,
        action: {
          type: 'redirect',
          redirect: { regexSubstitution: '/blocked.html?matched=keyword&domain=\\1&value=\\2' },
        },
        condition: {
          regexFilter: keywordRegex(),
          isUrlFilterCaseSensitive: false,
          resourceTypes: ['main_frame'],
        },
      },
    ],
  });
}

// Domain blocklist (Phase 1: static rules/blocklist.json; Phase 13: dynamic,
// sourced live from the service via Native Messaging instead — the whole
// point being the list can now change without a new extension version or a
// browser restart). One dynamic rule per domain, not one big regex — Chrome
// caps a single regexFilter's length (~2KB) well below what even a few
// hundred domains joined with "|" would need, but a plain `urlFilter` rule
// per domain doesn't have that problem and this is the standard way
// large-blocklist extensions actually do it.
const NATIVE_HOST = 'com.warden.service';
const REFRESH_ALARM = 'warden-blocklist-refresh';
const DOMAIN_RULE_ID_START = 2000;
// Matches the service's own MAX_DOMAINS cap (see service/src/blocklist_refresh.rs)
// — no reason for the two to drift, so this range always covers whatever
// get_blocklist_domains can possibly return.
const DOMAIN_RULE_ID_MAX_COUNT = 2000;

function sendNativeMessage(message) {
  return new Promise((resolve) => {
    chrome.runtime.sendNativeMessage(NATIVE_HOST, message, (response) => {
      if (chrome.runtime.lastError) {
        resolve({ ok: false, error: chrome.runtime.lastError.message });
      } else {
        resolve(response ?? { ok: false, error: 'no response' });
      }
    });
  });
}

async function registerDomainRules() {
  const response = await sendNativeMessage({ type: 'get_blocklist_domains' });

  if (!response.ok) {
    // Deliberately don't touch existing rules here — same "last known
    // good beats nothing" philosophy the service itself uses when its own
    // fetch fails. A transient native-messaging hiccup shouldn't silently
    // widen what gets through just because this one refresh attempt failed.
    console.error('Warden: could not fetch blocklist domains, keeping existing rules:', response.error);
    return;
  }

  const domains = response.domains.slice(0, DOMAIN_RULE_ID_MAX_COUNT);
  const removeRuleIds = Array.from(
    { length: DOMAIN_RULE_ID_MAX_COUNT },
    (_, i) => DOMAIN_RULE_ID_START + i
  );
  const addRules = domains.map((domain, i) => ({
    id: DOMAIN_RULE_ID_START + i,
    priority: 1,
    action: {
      type: 'redirect',
      redirect: {
        extensionPath: `/blocked.html?matched=blocklist&domain=${encodeURIComponent(domain)}&value=${encodeURIComponent(domain)}`,
      },
    },
    condition: {
      urlFilter: `||${domain}^`,
      resourceTypes: ['main_frame'],
    },
  }));

  await chrome.declarativeNetRequest.updateDynamicRules({ removeRuleIds, addRules });
  console.log(`Warden: domain blocklist active — ${addRules.length} domain(s).`);
}

async function registerAllRules() {
  await registerKeywordRules();
  await registerDomainRules();
}

chrome.runtime.onInstalled.addListener(async (details) => {
  await registerAllRules();
  chrome.alarms.create(REFRESH_ALARM, { periodInMinutes: 30 });
  console.log(`Warden installed (${details.reason}). Keyword + domain blocklist engines active.`);
});

chrome.runtime.onStartup.addListener(async () => {
  await registerAllRules();
  chrome.alarms.create(REFRESH_ALARM, { periodInMinutes: 30 });
});

// The actual mechanism behind "refresh without a browser restart" (Phase
// 13's exit criteria): chrome.alarms survives service worker suspension,
// unlike a plain setInterval, which MV3 can and does kill between events.
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === REFRESH_ALARM) {
    registerDomainRules();
  }
});
