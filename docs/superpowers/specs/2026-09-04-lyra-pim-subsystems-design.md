# PIM subsystems (Calendar + Contacts) — design

**Date:** 2026-09-04  
**Status:** Approved — Phase 1 shell + Phase 2 ICS subscriptions implemented  
**Scope:** First-class Calendar and Contacts apps (Thunderbird-style), Notion/Fastmail-inspired chrome, then ICS/webcal subscriptions. Manual CalDAV/CardDAV account wizards are **out of this design** (follow-up).

**Relates to:**  
- `docs/specs/2026-09-03-caldav-carddav-spec.md` (DAV sync — unchanged wire protocol)  
- `docs/specs/2026-08-20-lyra-data-model-spec.md` (PIM tables; extended by subscriptions)  
- Notion Calendar + Fastmail Contacts UI study (2026-09-04 agent session)  
- Contacts three-pane polish already started (`contacts-page.tsx`, `contacts-ui.ts`)

## Goal

Make **Calendar** and **Contacts** feel like Thunderbird subsystems: peer destinations to Mail, each with its own source rail, refresh, and empty states — not thin `SecondaryPage` grids bolted onto mail accounts.

Then add **ICS / webcal subscriptions** as calendar sources that do not require a mail account.

## Confirmed decisions

| Decision | Choice |
|----------|--------|
| Product shape | Calendar + Contacts = first-class PIM subsystems (Thunderbird-like) |
| UI chrome | In-house (no FullCalendar SDK); Notion Calendar + Fastmail patterns |
| Sequence | **1)** Calendar shell on existing CalDAV data → **1b)** Contacts chrome parity → **2)** ICS subscriptions |
| ICS storage | New `calendar_subscription` table owned by `user_id` (not a synthetic mail account) |
| ICS refresh | Fetch on add + periodic background refresh; cache events locally |
| ICS auth | Public `https` / `webcal`→`https` only; no Basic-auth feeds in this slice |
| Manual CalDAV/CardDAV wizard | Deferred (needed for Fastmail JMAP-bearer + separate app password) |
| Event create/edit | Out of scope this design (read + subscribe first) |
| Notion right-hand help rail | Out of scope |

## Current state

- App rail already routes to `/contacts` and `/calendar`.
- Calendar: `SecondaryPage` + simple month grid; calendars/events only via CalDAV on `mail_account`.
- Contacts: three-pane shell started (All / address books / A–Z / detail); still light on subsystem chrome.
- Fastmail JMAP uses **bearer** for mail; CardDAV/CalDAV need a separate **app password** — auto-discover currently skips bearer. Manual DAV / PIM credential is a **later** slice, not this design.

## Design

### 1. Subsystem framing

| Surface | Owns | Does not own |
|---------|------|----------------|
| **Mail** | Folders, sync of mail accounts | Calendar toggles, address-book toggles |
| **Calendar** | Calendar list (CalDAV collections + ICS subs), view mode, event grid | Mail compose (Compose from event is later) |
| **Contacts** | Address books / filters, contact list + detail | Mail account CRUD (stays in Settings) |

Settings remains the place for **mail account** credentials (JMAP/IMAP/OAuth). Calendar/Contacts pages manage **sources** (visibility, add subscription, refresh).

Shared patterns with Mail:

- Full-height `h-svh` shell (no padded secondary card)
- Left source rail + main workspace
- App rail peer navigation (already present)
- en + zh; Lyra stamp tokens (amber today/unread, hairlines, no indigo)

### 2. Phase 1 — Calendar shell (CalDAV data only)

**Layout**

```
┌──────────────────────────────────────────────────────────┐
│ [rail]  Calendar · 九月 2026 · [月▾] [今天] [‹ ›]  [↻]   │
├────────┬─────────────────────────────────────────────────┤
│ Sources│  Month | Week | Day grid                        │
│ □ Work │                                                 │
│ □ Hol. │                                                 │
│ + Add… │  (Add disabled or “coming soon” until Phase 2)  │
└────────┴─────────────────────────────────────────────────┘
```

**Header**

- Collapse-friendly source rail (optional later; v1: always visible on desktop)
- Title: month / week range / day date (locale-aware)
- View switcher: Day · Week · Month (shortcuts later: D / W / M)
- Today + prev/next (unit follows active view)
- Manual refresh (re-sync CalDAV for visible accounts; Phase 2 also refreshes ICS)

**Month view**

- Mon–Sun columns (locale week-start follows existing i18n preference if any; else Monday to match Notion CN study)
- Dense pastel event chips by calendar color; multi-day bars when `dtstart`/`dtend` span days
- Today: amber/red disc on day number (Lyra status color — prefer amber unread/today token over Notion red unless DESIGN.md says otherwise; **use `--unread` / stamp amber**)

**Week / Day views**

- Hour grid; all-day band above
- Current-time **now-line** with time badge
- Timed events as blocks; click → read-only detail panel or popover

**Source rail**

- One row per `calendar` (CalDAV), color dot + name + visibility toggle (`is_active` or client-side filter — prefer persisting `is_active` via existing calendar API if present, else local UI state until API exists)
- Group by mail account email when multiple accounts
- Empty state: “Sync calendars from Settings” + link to Settings accounts

**Non-goals in Phase 1**

- ICS add form, drag-create events, invites, mini-month widget (nice-to-have; can land with Phase 1b if cheap)

### 3. Phase 1b — Contacts subsystem parity

Align Contacts chrome with Calendar:

- Same full-height density, rail typography, header search placement
- Rail: All contacts · address books (Personal/Shared labels already) · future “Add address book” stub
- Keep Compose / Call actions; no CardDAV credential UI yet

### 4. Phase 2 — ICS / webcal subscriptions

**Data model** (dual-DB migrations)

`calendar_subscription`

| Column | Notes |
|--------|--------|
| `id` | PK |
| `user_id` | FK → `lyra_user` |
| `url` | Canonical `https://…` (webcal rewritten on write) |
| `name` | Display name (from feed `X-WR-CALNAME` or user override) |
| `color` | UI color |
| `etag` / `last_modified` | Conditional fetch when server sends them |
| `last_fetched_at` | |
| `last_error` | Nullable; surface in rail |
| `is_active` | Visibility toggle |
| `created_at` / `updated_at` | |

Events: either

- **Preferred:** `calendar_event` gains nullable `subscription_id` (XOR with `account_id` / `calendar_id` path), **or**
- New thin `subscription_event` table mirrored into the same list API

Recommendation: extend `calendar_event` with nullable `subscription_id` and make `account_id` nullable when `subscription_id` is set (ownership via subscription → user). Document the XOR invariant. Calendar list API returns a unified source DTO: `{ kind: 'caldav' \| 'ics', … }`.

**HTTP**

- `GET/POST /api/v1/calendar-subscriptions`
- `PATCH/DELETE /api/v1/calendar-subscriptions/{id}`
- `POST /api/v1/calendar-subscriptions/{id}/refresh`
- List calendars/events already used by UI must include ICS sources/events

**Fetch rules**

- SSRF: reuse `netsec` URL validation; block private/link-local; HTTPS only after `webcal://` → `https://`
- Size/time limits on download; parse RFC 5545 VEVENT into cache
- RRULE: store string; expansion for display can be naive window (visible month ± 1) — full recurrence engine not required in v1 of ICS
- Background: job or scheduler tick (reuse `jobs` / `scheduler`) every N hours + on add

**UI**

- “+ Add subscription” in Calendar rail → URL + optional name/color
- Toggle / refresh / remove on the row
- Read-only events (cannot PUT back to an ICS URL)

### 5. API / client boundary

- No web-only shortcuts: subscription CRUD on `/api/v1`
- OpenAPI updated with Phase 2
- Web is a peer client

### 6. i18n

All new chrome strings en + zh (view names, Today, Add subscription, empty hints, fetch errors).

### 7. Testing

- Phase 1: unit helpers for month/week grid date math; smoke render
- Phase 2: ICS parse fixtures; SSRF reject; refresh upsert/tombstone; postgres_live for new columns

## Phased delivery

| Phase | Deliverable | Done when |
|-------|-------------|-----------|
| **1** | Calendar subsystem shell | Notion-like month/week/day + source rail on CalDAV data |
| **1b** | Contacts chrome parity | Same subsystem density as Calendar |
| **2** | ICS subscriptions | Add URL → events on grid; background refresh; toggle/remove |

## Follow-ups (explicitly not this design)

- Manual CalDAV/CardDAV source with **separate PIM password** (Fastmail JMAP bearer + app password) — **see** `docs/superpowers/specs/2026-09-04-lyra-pim-credentials-design.md`
- Authenticated ICS
- Two-way event edit, scheduling (RFC 5546)
- Mini-month navigator, Agenda/Schedule outline view
- Calendar UI polish (density, chips, typography) — after credentials + read-completeness

## Self-review checklist

- [x] No TBD placeholders in decisions table
- [x] Sequence matches user approval (UI then ICS)
- [x] Ownership model A (`user_id` subscriptions) recorded
- [x] Non-goals include manual DAV and auth ICS
- [x] XOR event ownership called out for implementers
