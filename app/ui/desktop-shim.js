// Stands in for the handful of chrome.* APIs the extension's popup/dashboard
// use, so those pages run unchanged inside the desktop app. Loaded before
// popup.js / dashboard.js.
(() => {
  const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

  window.chrome = {
    runtime: {
      lastError: undefined,
      // Same shape as the extension API: callback(response), lastError on failure.
      sendNativeMessage(_host, message, callback) {
        invoke('native_message', { message }).then(
          (response) => { window.chrome.runtime.lastError = undefined; callback(response); },
          (error) => {
            window.chrome.runtime.lastError = { message: String(error) };
            callback(undefined);
            window.chrome.runtime.lastError = undefined;
          },
        );
      },
      getURL: (path) => path,
    },
    // Blocking happens in the system-wide DNS filter, not in-browser rules.
    declarativeNetRequest: { getDynamicRules: () => Promise.resolve([]) },
    storage: { local: { get: () => Promise.resolve({}), set: () => Promise.resolve() } },
  };

  // The popup opens the dashboard with window.open; do that as a real window.
  window.open = (url) => { invoke('open_page', { page: String(url) }); return null; };
})();
