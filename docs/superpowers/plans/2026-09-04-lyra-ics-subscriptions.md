# ICS / webcal subscriptions — implementation plan

> **For agentic workers:** Phase 2 of `docs/superpowers/specs/2026-09-04-lyra-pim-subsystems-design.md`.

**Goal:** Users can add a public ICS/webcal URL on Calendar, see events on the grid, refresh/toggle/remove, with background refresh.

**Status:** Implemented (2026-09-04).

## Delivered

- [x] Migration `0020_calendar_subscription` (sqlite + postgres): `calendar_subscription`, `subscription_event`
- [x] `backend/src/ics.rs` — normalize (webcal→https, public-only), parse, fetch (SSRF), refresh upsert
- [x] `backend/src/pim_subscriptions.rs` — CRUD + refresh + list events under `/api/v1/calendar-subscriptions`
- [x] Scheduler tick calls `refresh_due_subscriptions` (~6h)
- [x] Calendar UI: source rail ICS rows, add modal, delete, merge events with CalDAV
- [x] OpenAPI + design status updated
- [x] Unit tests: webcal normalize, private host reject, parse fixtures
