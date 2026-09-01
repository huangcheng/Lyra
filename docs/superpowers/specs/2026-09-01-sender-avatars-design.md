# Sender avatars — design

**Date:** 2026-09-01
**Status:** Approved (design), pending implementation plan

## Goal

Show real sender/contact avatars in place of the current monogram initials,
from privacy-ordered sources: **contacts-module photos → BIMI DNS logos →
opt-in Gravatar**. No third party ever learns the user's IP, and Gravatar
learns nothing unless the user explicitly opts in.

Confirmed scope decisions:

- Sources: **all three** — contacts, BIMI, Gravatar (opt-in, default off).
- BIMI: **DNS record logo, no VMC validation** (Thunderbird-style; the logo
  is fetched server-side and sniffed like any proxied image).
- Architecture: **one backend resolver endpoint** hides the source chain
  (rejected: frontend-side resolution — leaks; per-payload `avatar_url`
  fields — wasteful and still needs an image endpoint).

## Current state

- Monogram initials render via `getInitials` + `avatarTone` in
  `frontend/src/components/mail/message-card.tsx` (expanded cards; an
  `AvatarImage` with no `src` is already mounted), `mail-list.tsx` rows,
  and the contacts page.
- `contact.photo_path` column exists (`backend/src/entities/contact.rs`) and
  is read by the contacts API (`pim.rs`), but **nothing ever writes it** —
  CardDAV sync does not extract the vCard `PHOTO` property.
- The HMAC-gated image proxy is fully implemented in `backend/src/media.rs`:
  SSRF guard (`netsec::filter_public_addrs`), redirect cap, 10s timeout,
  10MB cap, `image/*` enforcement + byte sniffing, and a persistent
  media-cache under `DATA_DIR/media-cache/`. The avatar feature reuses this
  pipeline for all upstream fetches.
- Privacy settings seam: `GET/PATCH /api/v1/settings/privacy`
  (`backend/src/privacy.rs`).

## Design

### Backend: avatar resolver (`avatars.rs`, new deep module)

`GET /api/v1/avatars/{email}` (bearer auth) resolves `{email}` (percent-
decoded, trimmed, lowercased) through the chain and returns the image bytes
with `Cache-Control: private, max-age=…`, or **404** when no source has one.

Resolution order:

1. **Contact photo** — find any `contact` row whose `email_addresses` JSON
   contains the address; if it has a `photo_path`, stream that blob. Freshest
   source wins implicitly: contact sync rewrites `photo_path`.
2. **BIMI** — TXT lookup for `default._bimi.{domain}`; parse `v=BIMI1` and
   the `l=` logo URL. Fetch the logo through the media pipeline (SSRF guard,
   caps, sniffing). BIMI logos are SVG — accept `image/svg+xml` in addition
   to the raster types, and verify it against the sniffed bytes.
3. **Gravatar** — only when the user's `gravatar_avatars` setting is on:
   server-side `GET https://www.gravatar.com/avatar/{md5(email)}?d=404&s=128`.
   A 404 from Gravatar is a miss, not an error.

Caching (in the media-cache, keyed `sha256("avatar:" + email)`):

- Positive hits: cached 7 days (BIMI/Gravatar). Contact photos are read from
  the blob store directly — already durable, no extra copy.
- Misses: negative-cached 24h, so reading a folder of stranger mail does not
  re-query DNS or Gravatar on every render.
- Errors (DNS timeout, upstream 5xx): treated as miss, but negative-cached
  for only 10 minutes so transient failures heal quickly.

Non-goal: VMC certificate validation, favicon heuristics, user-uploaded
avatars, avatar editing UI.

### Backend: CardDAV PHOTO extraction

During contact sync (`pim.rs` CardDAV path), parse the vCard `PHOTO`
property:

- `PHOTO;VALUE=URI:https://…` → fetch through the media pipeline, store as a
  blob under `DATA_DIR/blobs/`, set `photo_path`.
- `PHOTO;ENCODING=b;TYPE=JPEG:…` (inline base64) → decode, sniff, store blob.
- Missing/unparseable/fetch-failed → leave `photo_path` null; sync must not
  fail over an avatar.

### Backend: privacy setting

`gravatar_avatars: boolean` (default **false**) added to
`GET/PATCH /api/v1/settings/privacy`. When off, the resolver never contacts
gravatar.com. Toggling it on takes effect immediately: the settings update
clears the user's negative avatar-cache entries, so addresses that missed
while Gravatar was off get a fresh lookup on next render.

### Frontend

- New `frontend/src/lib/avatar.ts`:
  - `loadAvatar(email): Promise<string | null>` — `apiBlob` fetch of
    `/api/v1/avatars/{encodeURIComponent(email)}` (same auth pattern as
    `attachments.ts` / `resolveInlineImages` — `<img>` can't send bearer
    headers, so no raw URL in `src`), returning an object URL, or `null` on
    404.
  - A module-level `Map<email, string | null>` memoizes both hits and misses
    for the session, so re-rendering a list never refetches.
- Wire into the existing monogram sites: `message-card.tsx` (the mounted
  `AvatarImage` finally gets a `src`), `mail-list.tsx` rows, contacts page.
  Initials stay as `AvatarFallback`; on `null` nothing changes visually.
- Settings → Privacy: "Gravatar avatars" switch with a one-line privacy note
  (en + zh), wired to the privacy settings seam.

### Data flow

```
render card/row
  → loadAvatar(sender) [session memo]
    → GET /api/v1/avatars/{email} (bearer)
      → contact.photo_path? → stream blob
      → else cached avatar? → stream cache
      → else BIMI TXT → fetch logo via media pipeline → cache → stream
      → else gravatar_avatars? → fetch md5 URL via media pipeline → cache → stream
      → else 404 → initials
```

### Error handling

- Every upstream failure degrades to 404 → initials. No avatar fetch may
  surface as a user-visible error.
- Contact sync is never failed by PHOTO problems.
- The resolver enforces the same caps as the proxy: 10s timeout, 10MB,
  redirect cap 3, image content-type + sniffing.

### Testing

- `avatars.rs` unit tests (SQLite + `postgres_live` roundtrip for the
  contact-photo lookup, per repo convention for new SQL seams):
  - chain order: contact photo beats BIMI beats Gravatar;
  - Gravatar skipped when setting off;
  - 404 when no source has anything;
  - negative-cache behavior (second resolve does not re-hit upstream);
  - BIMI record parsing: valid `v=BIMI1;l=…`, missing `l=`, wrong version.
- vCard PHOTO parsing tests: URI form, inline base64 form, garbage input.
- Frontend `avatar.ts`: memoization (one fetch per email per session),
  404 → `null`, object-URL lifecycle (revoke on eviction).
- Upstream fetches in tests go through the existing media-pipeline test
  doubles (see `media.rs` tests) — no real network.

## Out of scope (YAGNI)

- VMC / certificate validation for BIMI.
- User-uploaded avatars; avatar editing UI.
- Favicon or Clearbit-style heuristics.
- Contact-photo sync for providers without CardDAV.
