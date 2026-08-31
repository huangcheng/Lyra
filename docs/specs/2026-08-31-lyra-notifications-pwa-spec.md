# Lyra — Notifications & PWA Spec

**Date:** 2026-08-31
**Status:** Shipped (v1 as described here); Push API explicitly deferred
**Scope:** Browser notifications for new mail + installable PWA shell
**Out of scope:** Push API/VAPID, notification content actions (reply/archive from the notification), per-account notification rules

---

## 1. What shipped

### Notifications (frontend-driven v1)

| Aspect | Decision |
|---|---|
| Trigger | SSE `sync_complete` (and `incremental_complete` when the backend starts emitting it) per account |
| New-mail detection | Client diff: `GET /api/v1/messages?role=inbox&accountId=…` (limit 15, newest first) vs a per-account id baseline in `localStorage` (`lyra.notify.baseline.v1`) |
| First run | Seeds the baseline **silently** — no notification storm on login/first sync |
| Suppression | No notifications while the tab is visible; no notifications when disabled or permission ≠ granted |
| Volume cap | Max 3 notifications per sync; extras fold into an "N more new messages" summary |
| Display | `ServiceWorkerRegistration.showNotification` when the SW is registered (background-tab safe), page-level `Notification` fallback |
| Click behavior | Focus the Lyra window, load the message if absent from the store, select it, navigate to the mail view (`lyra:open-message` SW → page message) |
| Toggle | Settings → General; persisted in `localStorage` (`lyra.notifications`) — browser `Notification.permission` remains the real gate |
| Permission UX | Requested from the settings toggle (a user gesture), never on page load |

**Known limitation (deliberate):** delivery requires Lyra to be **running** — an open
tab (foreground or background) or a launched installed app. When the app is fully
closed, nothing is connected to SSE, so nothing fires. Closed-app delivery is the
Push API workstream below.

### PWA

| Aspect | Decision |
|---|---|
| Manifest | `frontend/public/manifest.webmanifest` — `display: standalone`, `id: "/"`, theme/background `#ffffff` (paper canvas), stamp icons 192/512 + maskable + `apple-touch-icon` |
| Service worker | Hand-rolled `frontend/public/sw.js` (~150 lines) — **no new frontend dependency** |
| SW caching | Precache app shell; `/assets/*` (Vite content-hashed) cache-first; navigations network-first with cached `index.html` offline fallback; other same-origin static stale-while-revalidate; **`/api/*` never cached** (auth, mutations, SSE) |
| SW updates | Version-bumped cache names; old caches dropped on activate; `SKIP_WAITING` message supported |
| Registration | Production builds only (`import.meta.env.PROD`) — a caching SW over the Vite dev server breaks HMR |
| Backend serving | Explicit `/manifest.webmanifest` + `/sw.js` routes in `main.rs` with deterministic `Content-Type` (`application/manifest+json`, `text/javascript`) and `Cache-Control: no-cache`; everything else unchanged through `ServeDir` |
| Install UX | Settings → General "Install Lyra" card: deferred-prompt button where `beforeinstallprompt` fires (Chromium); Share → Add to Home Screen instructions on iOS Safari; "Installed" badge when running standalone |
| CSP | Unchanged — `script-src 'self'`, `connect-src 'self'` already cover SW registration and same-origin fetches |

## 2. Why frontend-driven detection (not a backend event)

The SSE bus today carries only account-level lifecycle events (`sync_started` /
`sync_complete` / `sync_error`); no message-level event exists. Two options:

1. **Client diff** (shipped): zero wire/schema changes, one cheap indexed query
   per completed sync, robust against missed events (baseline is state, not a stream).
2. **Backend `MessagesNew` event**: the persist seam (`persist_imap_folder_batch` /
   `persist_jmap_folder_batch`) knows exactly which rows are new and could carry
   `{id, from, subject}` on the event. Better precision, no extra fetch — but an
   `AppEvent` + SSE shape change and plumbing through the job worker.

Option 2 is the right v2 if the client diff ever shows false positives/negatives
or the per-sync inbox fetch matters at scale.

## 3. Deferred: Push API (closed-app delivery)

Real push (VAPID + `PushSubscription` + backend `POST` on new mail) would deliver
when the app is closed. Cost: key management, a subscription store + migration,
per-account push triggers in the sync engine, and per-platform web-push client
libs in Rust. Deferred until the running-app limitation is felt in practice.

## 4. Testing

- Backend: `pwa_assets_serve_explicit_content_type_and_no_cache`,
  `pwa_assets_missing_file_is_404` (in `main.rs` tests).
- Frontend: `notifications.test.ts` (prefs round-trip + corrupted-blob
  robustness, sender-label extraction); i18n parity test covers the new keys.

## 5. Files

| File | Role |
|---|---|
| `frontend/public/manifest.webmanifest` | PWA manifest |
| `frontend/public/sw.js` | Service worker: shell cache + notification display/click |
| `frontend/public/icons/*`, `apple-touch-icon.png` | Stamp icons (rendered from `favicon.svg`) |
| `frontend/src/lib/pwa.ts` | SW registration, install-prompt capture, platform detection |
| `frontend/src/lib/notifications.ts` | Permission, prefs, inbox diff, notification display, click routing |
| `frontend/src/lib/use-mail-notifications.ts` | SSE → notifications tap (root layout) |
| `frontend/src/components/notification-settings.tsx` | Settings cards (notifications + install) |
| `backend/src/main.rs` | `/manifest.webmanifest` + `/sw.js` explicit routes |
