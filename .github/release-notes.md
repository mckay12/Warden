## 🛡️ Warden {{TAG}}

> ✏️ **This is an auto-created draft.** Describe what's new in this version below (replace this line), then press **Publish**.

A lock you set for yourself and can't talk yourself out of. Warden blocks adult content across **every browser on your Windows PC** (Incognito included), and once a lock has settled in, **nobody can shorten or cancel it** — not even by uninstalling or killing a process. (A **5-minute undo window** right after starting catches slips.)

> ⚠️ **Read this before you click Start.** The lock is real. Try a **short one first** (say 1–2 minutes) to see how it behaves. You can undo a lock for its first 5 minutes; after that it can be *extended* but never cut short, and long locks make you type a confirmation. After the first 5 minutes, "Lock forever" can only be undone through a 7-day emergency unlock.

### 📦 Install

1. Download **`{{FILENAME}}`** below.
2. Run it. Windows will ask for admin rights (it registers a background service).
3. **SmartScreen warning?** This build is **unsigned**, so Windows says "Unknown publisher". Click **More info → Run anyway**.
4. Open **Warden** from the Start menu or the tray icon, click **Start a lock**, set a duration, press **Start**.

**Requirements:** Windows 10/11 (64-bit) with WebView2 (already included on Windows 11).

🔁 **Updating?** Run the newer installer. It refuses while a lock is active, on purpose.

### ⚠️ Known limitations

- Windows only, **browsers only** (DNS-level)
- Opera, Vivaldi and Tor Browser ignore machine policy; VPNs and proxy extensions can carry DNS past the filter
- No URL/keyword matching (DNS only sees domain names)
- Unsigned installer

Full details in the [README](https://github.com/mckay12/Warden#readme). 🙂

### 🔐 Checksum

```
SHA-256  {{SHA256}}
```

Check it in PowerShell: `Get-FileHash .\{{FILENAME}}`
