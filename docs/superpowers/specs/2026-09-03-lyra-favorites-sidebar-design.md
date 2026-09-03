# Favorites sidebar (Apple Mail–style) — design

**Date:** 2026-09-03  
**Status:** Implemented (see `docs/superpowers/plans/2026-09-03-lyra-favorites-sidebar.md`)  
**Scope:** Mail sidebar + list selection for Favorites (frontend). Optional additive local-DB list filter for Starred (`GET /api/v1/messages?isStarred=true`). No IMAP/JMAP fetch-on-open, no sync protocol changes.

**Relates to:** `docs/superpowers/specs/2026-08-24-lyra-redesign-v2-design.md` (UNIFIED section → Favorites). Does not supersede redesign v2; narrows the mail sidebar section labeling and Favorites row set.

## Goal

Match Apple Mail’s sidebar shape: a **Favorites** strip at the top (shortcuts into unified / special views), then full **per-account folder trees** that still list Inbox / Drafts / Sent / Junk / Trash / Archive / custom folders — duplicates with Favorites are intentional.

## Constraint: not Apple’s live fetch

Lyra cannot pull every account instantly like Apple Mail. Favorites are **views over already-synced local mail** (same data model as today’s Unified rows). Opening a Favorite must not start a special multi-account fetch. Background sync and existing per-account sync indicators on account rows stay as they are.

## Confirmed decisions

| Decision | Choice |
|----------|--------|
| Layout | Favorites on top + keep role folders under each account |
| All Inboxes | Expandable; children = each account’s Inbox |
| Starred | Cross-account starred (`isStarred`) row in Favorites |
| Smart Mailboxes / Today | Out of scope |
| User-reorder Favorites | Out of scope |
| All Junk in Favorites | Out of scope this pass |
| When account switcher selects one account | Favorites section still visible at top (Apple Mail), then that account’s tree (or all accounts trees — see below) |

## Current state

- Section label `mail.section.unified` (“Unified” / “统一”) + rows for roles `inbox`, `drafts`, `sent`, `trash` via `getUnifiedFolders` / `UNIFIED_ROLE_ORDER`.
- Selecting a unified row sets `selectedAccountId = ALL_ACCOUNTS` and `selectedFolderRole = role`.
- When the account switcher is not “all accounts”, sidebar shows only that account’s `AccountSection` (`bare`) and **hides** the unified strip.
- Star exists on messages; there is no sidebar “Starred” destination yet. List filtering is `accountId` + `folderId` | `folderRole`.
- Account trees already include role folders + custom folders; DnD drop targets exist for unified roles and account folders.

## Design

### Section structure

```
Favorites                    ← rename of Unified
  ▾ All Inboxes              ← unified inbox; expandable
      · {Account A}          ← that account’s Inbox folder
      · {Account B}
  ★ Starred                  ← special local filter view
  · All Drafts
  · All Sent
  · All Trash
Accounts                     ← existing section (when showing all accounts)
  ▾ Personal
      Inbox, Drafts, Sent, …
  …
```

When the switcher selects a **single** account: keep **Favorites** at top (same rows; All Inboxes children may still list all accounts’ inboxes as shortcuts, or only the selected account’s child — prefer **all accounts’ inbox children** so Favorites stay global like Apple Mail). Below Favorites, show that account’s tree only (current single-account sidebar behavior for the accounts area).

### Row behavior

| Row | Selection | List content |
|-----|-----------|--------------|
| All Inboxes (parent) | `ALL_ACCOUNTS` + `folderRole: inbox` | Same as today’s unified inbox |
| All Inboxes → account child | that `accountId` + that account’s inbox `folderId` (or role inbox for that account) | That inbox only |
| Starred | `ALL_ACCOUNTS` + special view flag (e.g. `selectedFolderRole: 'starred'` or `selectedView: 'starred'`) | Messages with `isStarred`, across accounts (respect switcher account filter if single account selected: only that account’s starred) |
| All Drafts / Sent / Trash | `ALL_ACCOUNTS` + corresponding `folderRole` | Unchanged unified role views |

Unread / count badges: keep existing unified aggregation for role rows; Starred badge = count of starred messages in scope (or omit total if expensive — prefer a store-derived count over new API). Account children under All Inboxes show that inbox’s unread like today.

### Expand / collapse

- Persist All Inboxes expansion in existing `uiState` / folder-expansion persistence if a key already exists for similar rows; otherwise add a small boolean (e.g. `favoritesAllInboxesExpanded`) in the same preferences blob. Default: expanded when ≥2 accounts, collapsed when 1 (or always expanded — implementer pick; prefer **expanded by default** when multi-account).
- Chevron affordance matches account / folder disclosure styling.

### Labels (i18n)

| Key | en | zh |
|-----|----|----|
| `mail.section.unified` → rename or alias to favorites | Favorites | 收藏夹 |
| Existing `mail.allInboxes` | All inboxes | 全部收件箱 |
| Starred (reuse `mail.starred` if present) | Starred | 已标星 |
| Drafts / Sent / Trash | keep existing “All …” naming where already used | keep |

Update redesign v2 prose mentally: UNIFIED → Favorites; no need to rewrite the whole redesign doc.

### Non-goals

- Smart Mailboxes, Flagged as a separate server mailbox, Today.
- Drag-reorder Favorites; pin/unpin individual folders.
- Fetch-on-select Favorites, sync progress on Favorite rows (account-row indicators remain enough).
- Changing DnD semantics beyond ensuring Favorites role rows remain valid drop targets like today’s unified rows.
- Provider-side “Flagged” mailbox sync; Starred is Lyra’s local `is_starred` / `$flagged` flag already stored in the DB.

### Starred data path

`GET /api/v1/messages` today accepts `role` + `accountId` only — not `isStarred`. Implementation should add an optional `isStarred=true` query that filters the **local** message table (same seam as role listing). That is not a live multi-account fetch; it reads what sync already wrote. OpenAPI + thin handler/query change allowed for this. Frontend: UI store selection + mail-list params + `getMessagesForView` filter.

### Implementation sketch (for planning)

1. Rename section + i18n.
2. Always render Favorites when mail sidebar is shown; gate only the Accounts multi-tree vs single tree on switcher.
3. All Inboxes expandable row + children from each account’s inbox folder.
4. Starred: `isStarred` list query (local DB) + UI store selection + list/store filter.
5. Tests for expansion helpers / starred filter / query if logic is extracted to `src/lib/` or backend query tests.

## Acceptance

- Sidebar shows **Favorites** above account trees; label not “Unified”.
- All Inboxes expands to per-account Inbox shortcuts; parent and children navigate correctly.
- Starred opens a starred message list from local data.
- Account sections still list canonical folders (duplicates OK).
- Selecting Favorites does not invent a new sync/fetch path.
- en + zh strings updated; existing format/lint/tests pass for touched code.
