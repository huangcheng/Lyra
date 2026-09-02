# Same-account Copy to Folder — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Add same-account `Copy to…` (API + IMAP/JMAP + list/reader UI) matching Move’s folder picker; reject cross-account.

**Architecture:** Mirror `POST …/move` with `POST …/copy`. IMAP uses `UID COPY` only; JMAP unions mailbox IDs. Frontend reuses the move folder tree helper and adds `copyMessages` beside `moveMessages`.

**Tech Stack:** Axum/Rust sync HTTP + imap/jmap seams; React context menu / popover; OpenAPI `api-v1.yaml`.

---

### Task 1: Backend copy endpoint + protocol

**Files:**
- Modify: `backend/src/imap.rs` — public `copy_uid`
- Modify: `backend/src/sync/jmap_client.rs` — `add_email_mailbox` (union)
- Modify: `backend/src/sync/http.rs` — `copy_message`, route, tests
- Modify: `docs/openapi/api-v1.yaml`

- [x] Add failing handler/integration test for copy (same-account + cross-account 400)
- [x] Implement IMAP `copy_uid`, JMAP mailbox union, `apply_message_copy` (remote only; no local folder_id change)
- [x] Mount route; update OpenAPI
- [x] `cargo test --bin lyra_backend` for touched tests

### Task 2: Frontend Copy to UI

**Files:**
- Modify: `frontend/src/lib/conversation-actions.ts` (+ tests)
- Modify: `frontend/src/components/mail/conversation-context-menu.tsx`
- Modify: `frontend/src/components/mail/mail-display.tsx`
- Modify: `frontend/src/i18n/en.json`, `zh.json`

- [x] `copyMessages` posts `/messages/{id}/copy`; does **not** `removeMessage`
- [x] Copy submenu twin of Move (shared tree)
- [x] Reader toolbar Copy popover
- [x] `npm test` + `npm run check`
