# PIM credentials + calendar read completeness — design

**Date:** 2026-09-04  
**Status:** Approved — ready for implementation  
**Scope:** (1) Per-mail-account encrypted PIM / app password so CardDAV/CalDAV work with JMAP bearer accounts; (2) Calendar/Contacts deep-link into Settings; (3) Calendar read-completeness (still functional, not visual polish). UI chrome redesign is **out of this design**.

**Relates to:**  
- `docs/superpowers/specs/2026-09-04-lyra-pim-subsystems-design.md` (shell + ICS; follow-up was manual DAV)  
- `docs/specs/2026-09-03-caldav-carddav-spec.md` (discovery + sync)  
- `docs/specs/2026-08-20-lyra-data-model-spec.md` (encrypted credentials)

## Goal

Unblock Fastmail-class accounts (mail = JMAP bearer; DAV = app password) without a visual redesign. Then finish calendar **read** behaviors that are still missing. Polish the ugly grid later.

## Confirmed decisions

| Decision | Choice |
|----------|--------|
| Sequence | **A)** PIM credential + discover/sync UX → **B)** calendar read-completeness → **C)** UI polish (separate later design) |
| Credential model | Second encrypted blob on `mail_account`: `pim_credential` |
| Credential UI | Settings owns the field; Calendar/Contacts deep-link and trigger discover/sync |
| URL entry (this slice) | **Password only** — discovery via provider hints + RFC 6764; no manual CardDAV/CalDAV URL fields |
| DAV auth secret | Prefer `pim_credential` if set; else mail password when `auth_type=password`; **never** use JMAP bearer token as HTTP Basic for DAV |
| Standalone DAV sources | Out of scope (no DAV account table) |
| Event create/edit | Out of scope |
| Authenticated ICS | Out of scope |

## Current gaps

- Auto-discover skips `auth_type=bearer`, so Fastmail never gets `carddav_url` / `caldav_url` without a password path.
- Sync decrypts the mail `credential` only — bearer tokens are useless for DAV Basic.
- Settings already has Discover / Sync Contacts / Sync Calendars menu actions; no place to store an app password.
- Calendar shell + ICS work, but lack: event detail, multi-day span rendering, RRULE expansion in the visible window, persisted source visibility for ICS (`is_active`).

## Design

### 1. Data model

Add nullable `pim_credential TEXT` on `mail_account` (SQLite + PostgreSQL migration `0021_pim_credential`).

- Same AES-GCM encrypted JSON shape as `credential` / `smtp_credential` (encrypt under user DEK).
- API never returns the secret; PATCH accepts `pimPassword` (write-only); clearing uses explicit `pimPassword: null` or a dedicated clear flag — pick **empty string rejected; omit = leave unchanged; `clearPimPassword: true` clears**.
- Account list/detail may expose `hasPimCredential: boolean` so UI can show “configured” without leaking material.

### 2. Credential resolution (DAV only)

Central helper (e.g. `pim_dav::dav_password_for_account`):

1. If `pim_credential` present → decrypt → use as Basic password (username = account email unless already specialized).
2. Else if `auth_type` is password (or unset) → decrypt mail `credential`.
3. Else → return actionable error: `pim_password_required` (do not attempt bearer-as-Basic).

Discover and sync (contacts + calendars) **must** use this helper. Account create auto-discover stays password-auth only; after `pim_credential` is set, Settings “Discover” / Calendar deep-link path runs discover with the PIM secret.

### 3. Settings UX

On each mail account card (existing PIM menu area):

- Optional field: **PIM / app password** (password input; placeholder when `hasPimCredential`).
- Help text: needed when mail uses an API token/bearer (e.g. Fastmail); same app password covers CardDAV and CalDAV.
- Save via existing account PATCH (or small dedicated PATCH) → encrypt `pim_credential`.
- Keep existing Discover / Sync Contacts / Sync Calendars actions; Discover uses the resolver above and persists homesets.
- Deep-link target: `/settings?account=<id>&pim=1` (or hash) focuses that account and scrolls/highlights the PIM field.

### 4. Calendar / Contacts entry

When sources are empty or DAV sync returns `pim_password_required`:

- Empty / error CTA: **Connect calendars** / **Connect contacts** → navigate to Settings deep-link for the relevant account (if one mail account: that id; if several: account picker then deep-link).
- After user saves PIM password in Settings, optional auto-run: discover → sync contacts and/or calendars → toast result (reuse existing sync endpoints).

No second credential form on Calendar/Contacts rails.

### 5. Discovery behavior (unchanged algorithm, new secret)

Reuse `discover_homesets` / provider hints (Fastmail fixed hosts) + well-known. Persist `carddav_url` / `caldav_url` on success. Failure messaging distinguishes auth failure vs “no DAV found” when possible (HTTP 401/403 vs empty homeset), without claiming the provider lacks DAV solely on auth errors.

### 6. Phase B — Calendar read-completeness (functional only)

Still **no** visual redesign (colors, typography, density polish deferred).

| Feature | Done when |
|---------|-----------|
| Event detail | Click event → read-only panel/popover (summary, time, location, description, calendar name) |
| Multi-day spans | Month (and week all-day band) shows events spanning `dtstart`–`dtend` across days |
| RRULE window | Expand simple RRULEs for the visible range (±1 month padding); store rule as today; skip exotic rules gracefully |
| ICS `is_active` | Toggle persists via existing PATCH `isActive` on subscriptions (not only local checkbox) |
| Source errors | Show `lastError` per ICS row; Discover/sync errors actionable toward Settings PIM |

CalDAV write (create/edit event) remains deferred.

### 7. API / OpenAPI

- Account DTO: `hasPimCredential`; PATCH: `pimPassword?`, `clearPimPassword?`
- Error code or stable `message`/`code` for missing PIM password on discover/sync
- No new public URL override fields in this slice

### 8. Testing

- Unit: credential resolver (pim set / password fallback / bearer without pim → error)
- Unit: RRULE expansion for daily/weekly in a fixed window; multi-day span helpers
- Integration / mock DAV: discover + sync with `pim_credential` while mail auth is bearer
- Frontend: Settings field + deep-link query handling; calendar detail open/close
- `postgres_live` for new column roundtrip

### 9. i18n

en + zh: PIM password label/help, clear/update, deep-link CTAs, `pim_password_required` copy, event detail labels.

## Phased delivery

| Phase | Deliverable | Done when |
|-------|-------------|-----------|
| **A1** | Schema + resolver + wire discover/sync | Bearer account + app password discovers Fastmail DAV URLs and syncs |
| **A2** | Settings field + deep-link from Calendar/Contacts | User can complete the flow without leaving “settings owns secrets” |
| **B** | Read-completeness | Detail, multi-day, RRULE window, persisted ICS visibility |
| **C** | UI polish | **Not this design** — separate impeccable pass later |

## Non-goals

- Manual CardDAV/CalDAV URL fields  
- Standalone DAV accounts  
- Two-way event/contact edit in this slice  
- Authenticated ICS  
- Visual redesign of the calendar grid  

## Self-review checklist

- [x] No TBD in decisions table  
- [x] Bearer never used as DAV Basic  
- [x] Settings owns secrets; rails only deep-link  
- [x] Password-only discovery; polish explicitly deferred  
- [x] Phase B listed as functional, not cosmetic  
