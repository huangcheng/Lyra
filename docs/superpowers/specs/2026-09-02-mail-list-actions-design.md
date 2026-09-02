# Mail List Actions — Drag-to-Folder, Context Menu, Sync-All (Design)

Date: 2026-09-02
Status: Approved (design, revised after webmail comparison)

## Prior-art comparison (Outlook / Fastmail / Yandex web)

Investigated live via the user's logged-in sessions, 2026-09-02:

- **Outlook web**: right-click menu is folder-adaptive (in Junk: Forward, Delete,
  Move ▸, Copy ▸, Mark unread, Categories, Flag, Rules, Report, Block, Download,
  Find related, View, Advanced). Move submenu shows a few recent folders +
  "Choose another folder" + "New folder". Both message rows and folder tree
  items are HTML5 draggable.
- **Fastmail**: right-click menu has Open in new window, Copy link, Find all
  conversations with sender, Reply / Reply to all / Forward / Forward as
  attachment, a contextual "Move to inbox", Delete, Move to ▸, Mark unread, Pin,
  Notify/Mute replies, Report spam/phishing. **Move to ▸ is a searchable folder
  picker** (filter input on top, folder tree, "Create folder…" at bottom).
- **Yandex Mail**: right-click selects the row (checkbox) and opens: Open in new
  tab, Reply, Forward, Delete, Archive, Unread, To folder ▸, Label ▸, Spam!,
  Create filter. **To folder ▸ also has a folder-name filter input** + "New
  folder". A circular-refresh icon button sits next to Compose as the manual
  refresh affordance.

Consequences for this design:

1. The standard-set menu is confirmed by all three.
2. Right-click selecting the row (already in this design) matches Yandex.
3. The **Move to… submenu gets a folder-name filter input** (Fastmail/Yandex
   pattern) — important for accounts with deep custom trees like this user's
   Fastmail account.
4. A header-level sync/refresh icon next to the sidebar header matches Yandex's
   Compose-adjacent refresh button.
5. Deferred ideas (not in scope): Open in new window, Copy link, Pin, Mute,
   Unsubscribe, Forward as attachment, Copy to folder, Download .eml.

Three frontend-only additions to the mail list. No backend or API changes; every
action reuses existing `/api/v1` endpoints.

## 1. Drag mail to folder

Users drag a conversation row from the mail list onto a sidebar folder to move it.

- **DnD context**: hoist a new `DndContext` into `mail.tsx` wrapping both the
  `MailList` pane and the `SidebarFolders` pane. The existing account-reorder
  `DndContext` inside `sidebar-folders.tsx` stays nested as-is — dnd-kit scopes a
  drag to the context whose sensor fired, so account reordering is unaffected.
- **Draggable**: each conversation row in `mail-list.tsx` gets `useDraggable` with
  data `{ type: 'conversation', accountId, messageIds: string[], subject }`
  (`messageIds` = all messages in the conversation). A `DragOverlay` renders the
  subject plus a count badge when the conversation has more than one message.
- **Droppable**: every folder row in `sidebar-folders.tsx` (role rows and custom
  folder rows, unified and per-account views) gets `useDroppable` with data
  `{ type: 'folder', folderId, accountId }`. While a conversation drag is active,
  valid targets get a highlight ring; folders belonging to a different account
  (cross-account moves are rejected server-side) and the folder the conversation
  already lives in render dimmed and do not accept the drop.
- **Drop handling**: validate `message.accountId === folder.accountId` and
  target ≠ current folder, then sequentially call
  `POST /api/v1/messages/{id}/move` `{folderId}` for each message id, and
  `removeMessage(id)` locally for each success. If the current view *is* the
  target folder, moved messages appear on the next `sync_complete` refetch —
  identical to today's move-from-reader behavior.
- **Errors**: the move endpoint is synchronous-with-remote. On failure, stop the
  loop, keep remaining messages in place, and show a short inline error line at
  the top of the mail list (mirroring `mail-display.tsx`'s `actionError`
  pattern; the app has no toast system). A subtle "Moving…" indicator shows on
  the list while the loop runs.

## 2. Context menu

Right-clicking a conversation row opens a context menu.

- **Primitive**: add `frontend/src/components/ui/context-menu.tsx` (shadcn
  new-york style). The monolithic `radix-ui` dependency already re-exports
  `ContextMenu`; no new packages.
- **Trigger behavior**: right-click also selects the conversation
  (`setSelectedMessage`) so the reader pane and menu stay consistent.
- **Items** (all act on the whole conversation by looping existing per-message
  endpoints; per-item enablement derives from the conversation's latest message):
  - Reply / Reply All / Forward — build quoted HTML with the existing
    `lib/compose-html` helpers (`quotedReplyHtml`, `forwardHtml`) and call the UI
    store's `openCompose`, fetching the full message first if its body is not
    loaded. These three target the conversation's latest message only.
  - *(divider)* Archive / Spam / Trash — existing `POST /messages/{id}/{action}`.
  - Move to… — `ContextMenuSub` with a folder-name filter input at the top
    (Fastmail/Yandex pattern), followed by the account's role folders + custom
    tree flattened/indented; picking one runs the same move loop as
    drag-and-drop. Filtering matches folder display names case-insensitively.
  - *(divider)* Mark Read/Unread (`PATCH /messages/{id}`), Star/Unstar, Snooze
    (existing `POST /messages/{id}/snooze`).
- **Errors**: same inline-error pattern as drag-and-drop.
- **i18n**: all labels added to en + zh translation tables.

## 3. Sync-all button

A manual "sync every account" trigger in the sidebar.

- **Placement**: `RefreshCw` icon button in the sidebar header next to the
  existing `SyncStatusDot` in `mail.tsx`, with an en/zh tooltip ("Sync all
  accounts" / "同步所有账户").
- **Behavior**: on click, loop `useMailStore.accounts` and call
  `POST /api/v1/accounts/{id}/sync` for each; the backend already dedups
  pending/running jobs and returns 202.
- **State**: the icon spins and the button is disabled while any account is
  syncing, derived from the existing SSE `sync_started` / `sync_complete` /
  `sync_error` events already consumed in the app (fall back to idle if the
  stream disconnects, matching `SyncStatusDot` semantics).

## Non-goals

- Copy-to-folder, download .eml, rules/block/report (QQ-style extras) — need new
  backend endpoints; out of scope.
- Multi-select of list rows; drag and menu act on one conversation at a time.
- Backend changes, new endpoints, or optimistic folder-count updates.

## Testing

- Vitest unit tests for new `lib/` helpers: the conversation-action loop
  (partial failure stops, successes removed locally) and drop validation
  (cross-account / same-folder rejection).
- Existing suites stay green: `make test`, `make lint`, `make fmt`.
- Manual verification on the local Docker stack: drag onto valid/invalid
  targets, every context-menu item, sync-all spinner behavior.
