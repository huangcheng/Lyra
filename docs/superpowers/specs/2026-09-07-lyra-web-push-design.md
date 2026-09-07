# Lyra Web Push Design

Date: 2026-09-07
Status: Approved (design), pending implementation

## Goal

Deliver OS-level "new mail" notifications when Lyra is **not open in any
browser tab** — the gap the current SSE-driven, page-side notification path
(`frontend/src/lib/notifications.ts`) cannot cover. Uses the Web Push stack:

- **RFC 8030** — Web Push protocol (POST to a per-browser push-service endpoint)
- **RFC 8292** — VAPID (voluntary application server identification; ES256 JWT)
- **RFC 8291** — message encryption (aes128gcm; payload is unreadable to the
  push service — only metadata like endpoint and timing is exposed)

## Non-goals

- Replacing the in-page notification path. Page banners and push coexist; the
  shared notification `tag` (`lyra-{messageId}`) collapses duplicates on a
  device where both fire.
- Per-account or per-folder push *schedules*, rich actions (archive/reply from
  the banner), badge counts. Possible follow-ups, not v1.
- Push for calendar reminders or sync errors. Mail only.
- Changing sync cadence. Push rides existing sync triggers (IMAP IDLE, JMAP
  push, `SYNC_POLL_SECS` polling) and adds ~1s of delivery latency.

## Current state (what we build on)

- `backend/src/kernel/events.rs` — `EventBus` broadcast with
  `SyncStarted` / `SyncComplete { account_id }` / `SyncError`. The SSE route
  (`sync/http.rs::sync_events`) subscribes; the push fan-out subscribes the
  same way. No changes to the sync engine.
- `backend/src/kv/mod.rs` — `KvStore` (Redis + in-memory impls), already used
  for captcha settings with AES-GCM encryption under an HKDF-derived master-key
  subkey (`auth/captcha.rs::settings_key`). Push reuses both patterns.
- `frontend/public/sw.js` — app-shell caching + `notificationclick` handler
  that focuses/opens Lyra and posts `lyra:open-message` with
  `data.messageId`. Works unchanged for push-triggered notifications.
- `frontend/src/lib/notifications.ts` — incoming-role filter
  (`NON_INCOMING_ROLES`), Message-ID diff identity, per-account baseline,
  mute lists, `MAX_PER_SYNC = 3` + summary. The backend fan-out **mirrors**
  this logic exactly so open-app and closed-app behavior agree.
- `frontend/src/components/notification-settings.tsx` — existing Settings card
  where the push controls live.

## Architecture

```
IMAP IDLE / JMAP push / poller
        │
        ▼
  sync engine ──emit──▶ EventBus ──┬──▶ SSE ──▶ open tabs (existing banners)
                                   │
                                   └──▶ push fan-out task (new)
                                            │ diff vs kv baseline,
                                            │ filter mutes (server copy)
                                            ▼
                                     web-push crate builds
                                     encrypted RFC 8291 message
                                            │ reqwest POST
                                            ▼
                              browser push service (FCM/Mozilla/Apple)
                                            ▼
                                    sw.js `push` event
                                            ▼
                              registration.showNotification()
```

### Backend: new module `backend/src/push.rs`

**VAPID identity.** An ES256 keypair is generated lazily on first use,
serialized as PKCS#8 PEM, encrypted with `crypto::encrypt` under an
HKDF-derived subkey (same pattern as captcha settings), and stored in kv at
`server:push-vapid`. The public key (base64url uncompressed point) is served
to clients. The JWT `sub` is a contact URI: `LYRA_VAPID_SUBJECT` env var if
set, else `mailto:admin@<request host>`.

**Storage (kv only — no DB migration):**

| Key | Value |
|---|---|
| `server:push-vapid` | encrypted VAPID private key (PEM) |
| `push:subs:{user_id}` | JSON array of `{ endpoint, p256dh, auth, created_at }`, upserted by endpoint, capped at 10 per user |
| `push:baseline:{account_id}` | JSON array of ≤15 recent Message-ID identities (mirrors the frontend baseline) |
| `push:prefs:{user_id}` | JSON `{ mutedFolderIds, mutedThreadIds }` — the server-side copy used to filter before sending |

**Endpoints** (all under existing bearer auth, registered like
`/api/v1/settings/captcha`, documented in `docs/openapi/api-v1.yaml`):

| Endpoint | Purpose |
|---|---|
| `GET /api/v1/push/vapid-key` | `{ publicKey }` for `pushManager.subscribe` |
| `GET /api/v1/push/status` | `{ devices }` — subscription count for the Settings UI |
| `PUT /api/v1/push/subscription` | upsert `{ endpoint, keys: { p256dh, auth } }` (https endpoints; http allowed for loopback dev) |
| `DELETE /api/v1/push/subscription` | remove by `{ endpoint }` |
| `PUT /api/v1/push/prefs` | store the mute-list copy `{ mutedFolderIds, mutedThreadIds, locale }` |
| `POST /api/v1/push/test` | send a test push to all of the user's subscriptions (full-path parity with the in-page test banner) |

**Fan-out task.** Spawned in `main.rs` next to `jobs::spawn_workers`.
Subscribes to `EventBus`; on `SyncComplete { account_id }`:

1. Resolve the account's owner (`user_id`). No subscriptions → done.
2. Query the account's newest messages, filter to incoming folder roles (the
   same `NON_INCOMING_ROLES` set: sent/drafts/trash/spam/junk/outbox excluded;
   archive and custom folders included).
3. Diff by RFC 5322 Message-ID against `push:baseline:{account_id}`
   (row-id fallback for messages without one), take the newest 15. **First
   run seeds the baseline silently** — enabling push must never fire a
   notification storm.
4. Drop messages in muted folders / muted threads (from `push:prefs:{user_id}`).
5. Send ≤3 individual pushes + 1 summary push (`{n} more new messages`) per
   sync, matching the frontend's `MAX_PER_SYNC` behavior.
6. Advance the baseline regardless of send outcome (push is best-effort;
   the mail is in the app regardless).

**Payload** (JSON, ≤4KB, RFC 8291 encrypted):
`{ title: senderLabel, body: subject, tag: "lyra-{messageId}", data: { messageId } }`,
TTL 3600s, urgency `normal`. The `tag` matches the in-page banner tag so a
device showing both collapses them into one.

**Sending.** `web-push = "0.11"` with `default-features = false` (builds the
VAPID JWT and encrypts the payload; brings no HTTP client). The POST itself
goes through the existing `reqwest` 0.12 client. Pure-Rust crypto (`ece`,
`jwt-simple/pure-rust`) — no `ring`/`aws-lc-rs` conflicts with the jmap-client
or x509-parser chains.

**Failure handling.**

| Push-service response | Action |
|---|---|
| 201/202 | ok |
| 404 / 410 | subscription is dead — delete it from `push:subs` |
| 401 / 403 | VAPID rejected — log an error (operator misconfiguration), keep subscription |
| 413 | payload too big — log; payload design stays ≪4KB |
| 429 / 5xx | drop this send (next sync supersedes) |
| network error | drop; logged at debug |

### Frontend

**`frontend/public/sw.js`** — add:

- `push` listener: parse the JSON payload, call
  `event.waitUntil(registration.showNotification(title, { body, tag, icon,
  badge, data }))`. The existing `notificationclick` handler deep-links via
  `data.messageId` unchanged.
- `pushsubscriptionchange` listener: re-subscribe with the same
  `applicationServerKey`. In v1 the SW re-subscribes locally only; the server
  record is healed by `reconcilePushSubscription` on the next app open (the
  SW cannot reach the auth token in localStorage).
- Bump `VERSION` so the new worker rolls out.

**`frontend/src/lib/push.ts`** (new, vitest-covered with a mocked
`PushManager`):

- `getVapidKey()` → `GET /api/v1/push/vapid-key`
- `subscribePush()` — `pushManager.subscribe({ userVisibleOnly: true,
  applicationServerKey: urlBase64ToUint8Array(publicKey) })`, then `PUT` the
  subscription; returns the endpoint
- `unsubscribePush()` — `DELETE` on the backend, then `subscription.unsubscribe()`
- `pushStatus()` — `'unsupported' | 'denied' | 'subscribed' | 'unsubscribed'`

**Settings → Notifications card** (`notification-settings.tsx`): add a
"Background push / 后台推送" section — toggle (subscribe/unsubscribe), count of
registered devices, a "send test push" button hitting
`POST /api/v1/push/test`, and explicit states for `unsupported` and
`denied` permission. iOS hint: push requires installing Lyra to the Home
Screen (Safari 16.4+); reuse `isIos()`/`isStandalone()` from `lib/pwa.ts`.
New i18n keys in `en.json`/`zh.json`.

**Mute write-through.** When push is subscribed, `writeNotificationPrefs`
additionally `PUT`s `{ mutedFolderIds, mutedThreadIds }` to
`/api/v1/push/prefs` so the server filters before sending — muted mail never
wakes the device. (Chosen over SW-side IndexedDB filtering: per-user prefs,
no wasted push traffic.)

### Security & privacy

- Payload is end-to-end encrypted (RFC 8291); push services see only the
  endpoint, timing, and size. Title/body stay private — matching Lyra's
  self-hosted privacy posture.
- Subscription endpoints and VAPID key material live in kv; the private key
  is encrypted at rest under the master key.
- Subscriptions are scoped per user; all mutation endpoints require the
  bearer session. Unsubscribe on logout is client-side best-effort.
- VAPID `sub` contact is configurable for operators who want push services
  to reach them about abuse.

### Testing

- **Backend unit tests** (SQLite, existing harness): kv round-trips for
  subscriptions/baseline/prefs; VAPID key generation+persistence; the diff
  function (incoming-role filter, Message-ID identity, first-run seeding,
  mute filtering, MAX_PER_SYNC cap) extracted as a pure function and tested
  against the same cases as `notifications.test.ts`; endpoint auth (401
  without session); 410 → subscription deletion against a local mock
  push-service TCP listener.
- **Frontend vitest**: `push.ts` with mocked `PushManager`/`api`;
  write-through in `writeNotificationPrefs`.
- **OpenAPI**: new endpoints added to `docs/openapi/api-v1.yaml`.
- **Manual E2E**: on the Vultr deploy (HTTPS), enable push, close all tabs,
  send mail, expect an OS banner within seconds; click-through opens the
  message. Verify tag-collapse with the app open (no double banner).

### Rollout notes

- Works only over HTTPS in practice (push services require it; localhost is
  exempt) — the Vultr deployment is already behind Caddy/HTTPS.
- `LYRA_VAPID_SUBJECT` documented in `.env.example`; sensible default
  otherwise.
- No migration, no new infrastructure: kv (Redis) is already deployed.
