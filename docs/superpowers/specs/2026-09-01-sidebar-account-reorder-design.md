# Sidebar account reordering — design

**Date:** 2026-09-01
**Status:** Approved (design), pending implementation plan

## Goal

Let users drag the account sections in the mail sidebar's ACCOUNTS area into a
custom order, and persist that order server-side so it restores identically
after reload and on other devices.

Confirmed scope decisions:

- Reorderable: **account sections only** (QQ, Outlook, … in the unified
  sidebar's ACCOUNTS area).
- Interaction: **drag-and-drop directly in the sidebar**.
- Persistence: **existing `uiState` JSON blob** on `lyra_user.ui_state` via
  `PATCH /api/v1/auth/preferences` (frontend-only change, no migration).

## Current state

- `frontend/src/components/mail/sidebar-folders.tsx` renders the unified
  mailbox rows, then one `AccountSection` per account in `accounts` order
  (the order the mail store received from `/api/v1`).
- `frontend/src/components/mail/account-switcher.tsx` lists the same
  `accounts` array in its dropdown.
- View state (selected account/folder, folder expansion) already persists
  through `frontend/src/lib/persist-view-state.ts`: a debounced,
  fire-and-forget `PATCH /api/v1/auth/preferences { uiState: {…} }`, restored
  by `applyViewState`. Backend stores the blob in `lyra_user.ui_state`
  (JSON, 16KB cap, must be an object). **No backend changes are needed.**
- No drag-and-drop library is currently a frontend dependency.

## Design

### Ordering model

- `useUIStore` gains `accountOrder: string[]` (account ids) and
  `setAccountOrder(ids: string[])`.
- New pure helper in `frontend/src/lib/` (colocated with tests):

  ```ts
  orderAccounts(accounts: MailAccount[], accountOrder: string[]): MailAccount[]
  ```

  Semantics: accounts whose id appears in `accountOrder` come first, in that
  order; remaining accounts (e.g. newly added) are appended in server order;
  stale ids in `accountOrder` (deleted accounts) are dropped. Empty or absent
  `accountOrder` yields the server order unchanged.

- `SidebarFolders` and `AccountSwitcher` both consume `orderAccounts`. The
  mail store's raw `accounts` array is left untouched — ordering is view
  state, not data.

### Drag interaction

- Add `@dnd-kit/core` + `@dnd-kit/sortable` dependencies (standard,
  accessible sortable-list library; hand-rolled HTML5 drag events were
  considered and rejected as more error-prone around nested expandable rows).
- In the **unified view only** (`AccountSection` with `bare=false`, where
  multiple account headers render), the account header row becomes a
  sortable item. The single-account view (`bare=true`) has one section and
  no reordering.
- A vertical insertion indicator appears between account sections while
  dragging.
- dnd-kit activation constraint (~4px pointer movement) keeps a plain click
  on the header toggling expand/collapse as today; only a drag gesture
  reorders.
- Array surgery lives in a pure, unit-tested helper:

  ```ts
  moveId(order: string[], activeId: string, overId: string): string[]
  ```

  On drop, the component computes the new full id order with `moveId` and
  calls `setAccountOrder`.

### Persistence

- `persist-view-state.ts`:
  - change-detection comparison gains `state.accountOrder !== prev.accountOrder`;
  - the PATCH payload gains `accountOrder: s.accountOrder` inside `uiState`.
- `applyViewState` validates `uiState.accountOrder`: apply only when it is an
  array; keep string entries, drop non-strings; ignore anything else.
- Saves remain debounced (400ms) and fire-and-forget. Restore happens through
  the existing `/auth/me` → `applyViewState` path on session start.

### Edge cases

- **New account added** — not in `accountOrder` → appended at the end.
- **Account deleted** — its id is stale → dropped from the rendered order;
  cleaned out of `accountOrder` on the next reorder save.
- **Malformed `accountOrder` in the blob** — ignored; server order used.
- **16KB blob cap** — account ids are short; impact negligible alongside the
  existing fields.
- **Collapsed sidebar** — icon-only unified rows; no account sections, no
  reordering affordance. Order still applies when the sidebar expands.

## Testing

- `orderAccounts`: known order honored; unknown accounts appended in server
  order; stale ids pruned; empty/absent order returns server order.
- `moveId`: move to top/middle/end; same-id and unknown-id no-ops.
- `applyViewState`: valid `accountOrder` restored; malformed values dropped.
- `startViewStatePersistence`: payload includes `accountOrder` when it
  changes (existing debounce pattern).
- vitest, colocated in `frontend/src/lib/`, matching existing conventions.
- DnD wiring in the component is thin glue over these tested helpers.

## Out of scope (YAGNI)

- Reordering folders within an account (explicitly declined by user).
- Reordering the unified smart mailboxes.
- Writing order back to JMAP/IMAP servers.
- Keyboard-accessible reorder controls (dnd-kit provides keyboard sorting
  primitives; considered a bonus if free, not a requirement).
- Settings-page ordered list.
