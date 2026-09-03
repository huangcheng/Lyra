# Account Sync Error Log — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans.

**Goal:** Persist scrubbed sync failure detail and show a per-account error log on Settings → Accounts.

**Architecture:** Extend `jobs` with `last_error_detail`; scrub `error_chain` at write time; list failed `SyncAccount` jobs via a new account-scoped API; expand UI under each account row.

**Tech Stack:** sqlx migrations (SQLite+PG), Axum, React settings page, en/zh i18n.

---

### Task 1: Schema + scrub + persist

- [x] Dual-DB migration `jobs.last_error_detail`
- [x] Entity + `mark_failed` / `mark_completed` / scrub unit tests
- [x] Wire sync failure path to store scrubbed detail

### Task 2: API

- [x] `GET /api/v1/accounts/{id}/sync-errors`
- [x] OpenAPI + HTTP tests (auth, ownership, shape)

### Task 3: Settings UI

- [x] Fetch/expand recent errors under account row
- [x] i18n; `npm run check` + backend tests
