# PIM Calendar Shell (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the thin Calendar `SecondaryPage` with a Thunderbird/Notion-style subsystem shell: source rail, month/week/day views, today/now accents, multi-calendar event overlay — using existing CalDAV data only.

**Architecture:** Pure frontend Phase 1. Date-grid math lives in `frontend/src/lib/calendar-grid.ts` with vitest. `calendar-page.tsx` becomes a full-height shell that loads all calendars, fetches events per visible calendar, and renders month/week/day. No new backend routes; visibility toggles are client-side. ICS subscriptions are Phase 2 (separate plan).

**Tech Stack:** React, TanStack Router, Tailwind + shadcn, Lyra CSS tokens (`--unread` for today), en/zh i18n.

**Spec:** `docs/superpowers/specs/2026-09-04-lyra-pim-subsystems-design.md` (Phase 1 only)

## Global Constraints

- Full-height subsystem chrome — no `SecondaryPage` padded card layout.
- Today accent uses `var(--unread)` (amber gold), not indigo/`--primary` fill alone.
- Week starts **Monday** (match Notion CN study + common EU/CN calendars).
- Multi-calendar: show events from all **visible** calendars (client toggle), not one selected calendar only.
- Read-only event detail; no create/edit.
- ICS / Add subscription UI stub only (“coming soon” or disabled) — no API.
- No FullCalendar SDK.

## File map

| File | Role |
|------|------|
| `frontend/src/lib/calendar-grid.ts` | Month cells, week range, day hours, event-in-range helpers |
| `frontend/src/lib/calendar-grid.test.ts` | Unit tests for grid math |
| `frontend/src/components/calendar-page.tsx` | Shell: rail + header + views + detail |
| `frontend/src/i18n/en.json` / `zh.json` | View labels, refresh, sources, stub add |

---

### Task 1: Calendar grid helpers (TDD)

**Files:**
- Create: `frontend/src/lib/calendar-grid.ts`
- Create: `frontend/src/lib/calendar-grid.test.ts`

**Interfaces:**
- Produces:
  - `type CalendarView = 'month' | 'week' | 'day'`
  - `startOfWeekMonday(d: Date): Date` — local midnight Monday
  - `monthGridDays(year: number, monthIndex: number): Date[]` — 35 or 42 days starting Monday
  - `weekDays(anchor: Date): Date[]` — 7 days Mon–Sun containing anchor
  - `addViewOffset(anchor: Date, view: CalendarView, delta: number): Date`
  - `viewTitle(anchor: Date, view: CalendarView, locale: string): string`
  - `eventOccursOnDay(event: { dtstart?: string; dtend?: string; isAllDay: boolean }, day: Date): boolean`
  - `eventsForDay<T>(events: T[], day: Date): T[]` where T has dtstart/dtend/isAllDay
  - `hourSlots(): number[]` — 0..23

- [ ] **Step 1: Write failing tests** in `calendar-grid.test.ts` covering Monday week start, month grid length, event spanning midnight, `addViewOffset` month/week/day.

- [ ] **Step 2: Run** `cd frontend && npm test -- --run src/lib/calendar-grid.test.ts` — expect FAIL (module missing).

- [ ] **Step 3: Implement** `calendar-grid.ts` minimally to pass.

- [ ] **Step 4: Re-run tests** — expect PASS.

- [ ] **Step 5: Commit** (when user asks) — `feat(calendar): add grid date helpers for month/week/day`

---

### Task 2: i18n strings for shell

**Files:**
- Modify: `frontend/src/i18n/en.json` (`calendar` object)
- Modify: `frontend/src/i18n/zh.json` (`calendar` object)

**Produces keys:**
- `calendar.view.day` / `.week` / `.month`
- `calendar.sources`
- `calendar.refresh`
- `calendar.addSubscription` (stub label)
- `calendar.addSubscriptionSoon` (hint)
- `calendar.days.mon` … already exist; ensure Mon-first header can use `calendar.days.mon` … `sun` order

- [ ] Add en + zh keys listed above.
- [ ] No commit required until Task 3 lands with UI.

---

### Task 3: Calendar page shell — layout + month + multi-source

**Files:**
- Modify: `frontend/src/components/calendar-page.tsx` (rewrite)

**Interfaces:**
- Consumes: `calendar-grid` helpers; `GET /calendars`; `GET /calendars/{id}/events?start=&end=`
- Behavior:
  - `h-svh` shell with back link + title in header
  - Left rail: all calendars with color dot + checkbox/toggle; empty → EmptyState + hint to Settings
  - Load events for every visible calendar in the visible range (Promise.all)
  - Month grid Mon–Sun; chips colored by owning calendar; today disc `bg-[var(--unread)]`
  - Header: title, view segmented control (month default), Today, ‹ ›, Refresh
  - Event click → right detail panel (read-only)
  - Stub “+ Add subscription” disabled with soon hint

- [ ] Rewrite `calendar-page.tsx` per above.
- [ ] Run: `cd frontend && npx tsc --noEmit` and `npx oxlint src/components/calendar-page.tsx src/lib/calendar-grid.ts`
- [ ] Manual smoke: open `/calendar` with CalDAV data if available.

---

### Task 4: Week + day views + now-line

**Files:**
- Modify: `frontend/src/components/calendar-page.tsx`

**Behavior:**
- Week: 7 columns, hours 0–23, all-day row on top, timed blocks by start/end
- Day: single column same grid
- Now-line: horizontal rule + time label when viewing today (week: under today’s column)
- `addViewOffset` / title switch with view

- [ ] Implement week/day render paths.
- [ ] `npx tsc --noEmit`; oxlint clean.
- [ ] Smoke: switch 月/周/天, Today jumps correctly.

---

### Task 5: Contacts chrome parity (Phase 1b light)

**Files:**
- Modify: `frontend/src/components/contacts-page.tsx` (minor)

**Behavior:**
- Match calendar header density (back + title + search already present)
- Add disabled “+ Add address book” stub in rail with i18n soon hint (parity with calendar stub)
- No structural rewrite if already three-pane

- [ ] Add stub + i18n `contacts.addBook` / `contacts.addBookSoon`
- [ ] oxlint + tsc clean

---

### Task 6: Spec status + AGENTS (if needed)

**Files:**
- Modify: `docs/superpowers/specs/2026-09-04-lyra-pim-subsystems-design.md` — Status → Phase 1 in progress / Implemented when done

- [ ] Flip status line when Phase 1 UI ships.

---

## Spec coverage (self-review)

| Spec Phase 1 item | Task |
|-------------------|------|
| Full-height shell | 3 |
| Source rail + toggles | 3 |
| Month / week / day | 3–4 |
| Today + now-line | 3–4 |
| CalDAV multi-source | 3 |
| Contacts parity stub | 5 |
| ICS deferred | stub only in 3 |

## Out of plan

- ICS table/API (Phase 2 plan later)
- PATCH calendar `is_active` persistence
- Manual CalDAV credentials
