// Copies the extension's popup + dashboard UI into ui/ so there is one source
// of truth, and injects the desktop shim (which fakes the few chrome.* APIs
// those pages use) into each page.
import { copyFileSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const src = join(root, '..', 'extension');
const dst = join(root, 'ui');
mkdirSync(dst, { recursive: true });

for (const f of ['popup.css', 'popup.js', 'dashboard.css', 'dashboard.js', 'duration-picker.js']) {
  copyFileSync(join(src, f), join(dst, f));
}
for (const f of ['popup.html', 'dashboard.html']) {
  let html = readFileSync(join(src, f), 'utf8');
  html = html.replace('</head>', '<script src="desktop-shim.js"></script>\n</head>');
  html = html.replace('</body>', '<script src="desktop-late.js"></script>\n</body>');
  writeFileSync(join(dst, f), html);
}
console.log('ui/ synced from extension/');
