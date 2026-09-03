# Favorites Sidebar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apple Mail–style Favorites at the top of the mail sidebar (expandable All Inboxes, Starred, unified Drafts/Sent/Trash) over local synced data only.

**Architecture:** Keep role listing as today; add `isStarred` to `GET /api/v1/messages` for local-DB Starred. Frontend always shows Favorites; account trees stay below. Selection uses `folderRole: 'starred'` as a special view flag alongside existing roles.

**Tech Stack:** React sidebar (`sidebar-folders.tsx`), Zustand UI/mail stores, Axum list query, OpenAPI, en/zh i18n.

**Spec:** `docs/superpowers/specs/2026-09-03-lyra-favorites-sidebar-design.md`

## Global Constraints

- Favorites are local views only — no fetch-on-open / multi-account live pull.
- Duplicates under account trees are intentional.
- No Smart Mailboxes, Favorites reorder, or All Junk in Favorites this pass.

---

### Task 1: Starred list query (backend + OpenAPI)

**Files:**
- Modify: `backend/src/sync/queries.rs` (`query_user_messages`)
- Modify: `backend/src/sync/http.rs` (`ListMessagesQuery`, `list_messages_query`)
- Modify: `docs/openapi/api-v1.yaml` (GET `/messages` query params)
- Test: add in `backend/src/sync/queries.rs` or `mod.rs` near existing `query_user_messages` tests

**Interfaces:**
- Produces: `query_user_messages(db, user_id, role, account_id, is_starred: Option<bool>)`
- Produces: `GET /api/v1/messages?isStarred=true` (+ optional `accountId`)

- [ ] Extend `query_user_messages` with `is_starred: Option<bool>`; when `Some(true)`, `AND m.is_starred = true` (still apply not_deleted + snooze_visible).
- [ ] Wire `ListMessagesQuery { is_starred: Option<bool> }` and pass through.
- [ ] Document `isStarred` in OpenAPI.
- [ ] Test: seed starred + unstarred; `is_starred=Some(true)` returns only starred.
- [ ] Run: `cargo test --bin lyra_backend query_user_messages -- --nocapture` (or the new test name).

---

### Task 2: Frontend view selection for Starred + fetch

**Files:**
- Modify: `frontend/src/stores/ui.ts` (allow `selectedFolderRole: 'starred'`)
- Modify: `frontend/src/stores/mail.ts` (`getMessagesForView` / `replaceMessagesForView` filter `isStarred`)
- Modify: `frontend/src/lib/load-mail-messages.ts` (`messagesUrlForView` → `?isStarred=true`)
- Test: `frontend/src/lib/load-mail-messages.test.ts` (create if missing)

**Interfaces:**
- Consumes: Task 1 `isStarred` query
- Produces: `messagesUrlForView({ folderRole: 'starred', accountId })` → `/messages?isStarred=true[&accountId=…]`

- [ ] When `folderRole === 'starred'`, URL uses `isStarred=true` (no `role=`).
- [ ] Store filters: `folderRole === 'starred'` ⇒ `m.isStarred`.
- [ ] Starred badge: count starred messages in store (all accounts or filtered by switcher) — helper in `src/lib/` optional.
- [ ] Tests for URL builder + store filter.

---

### Task 3: Favorites sidebar UI

**Files:**
- Modify: `frontend/src/components/mail/sidebar-folders.tsx`
- Modify: `frontend/src/i18n/en.json`, `zh.json` (`mail.section.unified` → Favorites / 收藏夹; ensure `mail.starred`, All Drafts labels)
- Modify: `frontend/src/stores/ui.ts` + `frontend/src/lib/persist-view-state.ts` for `favoritesAllInboxesExpanded`
- Test: pure helpers for All Inboxes children if extracted

**Interfaces:**
- Consumes: Task 2 selection APIs
- Produces: Favorites always rendered; All Inboxes expandable with per-account inbox children

- [ ] Rename section label to Favorites.
- [ ] Always show Favorites; when single account selected, still Favorites + that account’s tree only.
- [ ] All Inboxes row: chevron + parent selects unified inbox; children = each account’s inbox folder → `setSelectedFolder(inboxId)`.
- [ ] Starred row → `setSelectedAccount(ALL_ACCOUNTS)` + `setSelectedFolderRole('starred')` (or keep switcher account if single — per spec: filter starred to that account when switcher is single).
- [ ] Drafts/Sent/Trash unified rows unchanged (after inbox+starred).
- [ ] Persist `favoritesAllInboxesExpanded` (default expanded when ≥2 accounts).
- [ ] i18n en/zh.
- [ ] Manual smoke: Favorites label, expand All Inboxes, Starred list, account trees still show Inbox etc.

---

### Task 4: Verify

- [ ] `cd frontend && npm test -- --run` (touched tests) + `npx tsc -p tsconfig.app.json --noEmit`
- [ ] `cd backend && cargo test --bin lyra_backend -- is_starred` (or new test filter)
- [ ] Update design status to implemented in the spec header when done.
