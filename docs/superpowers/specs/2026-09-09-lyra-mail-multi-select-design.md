# Lyra Mail Multi-Select — Design

Date: 2026-09-09
Status: Approved (design), pending implementation

## Goal

Apple Mail–style multi-selection in the mail conversation list: cmd/ctrl-click
to toggle individual conversations, shift-click for ranges, keyboard support
(⌘A, Esc, shift+↑/↓), a card-stack reading pane while multiple conversations
are selected, and bulk actions (archive, trash, read/unread, star, move,
spam, snooze, labels) applied to the whole selection.

## Non-goals (YAGNI)

- No backend changes. Bulk execution reuses the established frontend batch
  loop in `frontend/src/lib/conversation-actions.ts` over the per-message
  `/api/v1/messages/{id}/...` endpoints.
- No mobile/touch multi-select (tap stays plain single-select; the mobile
  list↔reader swap keyed on `selectedMessageId` is untouched).
- No select-all beyond the currently loaded list (backend caps list queries
  at 500; selection only ever covers visible conversations).
- No list virtualization (plain map today; 500-row cap keeps it fine).
- Multi-selection is **not** persisted server-side; only the existing single
  `selectedMessageId` continues to round-trip via `persist-view-state.ts`.

## Concepts

- Selection is **conversation-level** (the list renders `Conversation`s,
  identified by `Conversation.key`; actions expand to member message ids via
  `convo.messages.map((m) => m.id)`).
- `selectedMessageId` stays the single-focus concept the reader, pager, and
  persistence already use. Multi-select adds conversation-key state alongside
  it; the two are kept in sync by the selection helpers.

## State (frontend/src/stores/ui.ts)

New UI-store state next to `selectedMessageId`:

- `selectedConversationKeys: string[]` — ordered; length ≤ 1 means "not in
  multi-select" and behavior is exactly today's.
- `selectionAnchorKey: string | null` — anchor for shift ranges; the anchor
  conversation is also the front card of the reading-pane stack.
- `selectionFocusKey: string | null` — moving edge for shift+↑/↓ extension.

Actions: `setConversationSelection(keys, anchor, focus)`,
`clearConversationSelection()` (collapses to the anchor if one exists,
else clears), and internal sync so that changing the anchor updates
`selectedMessageId` to that conversation's target message
(first unread ?? latest — the same rule as today's click handler).

`selectedConversationKeys` is cleared in the same places that already reset
`selectedMessageId` (folder/account switch, `ui.ts` setters). It is never
written into the persisted `ui_state` blob.

## Pure selection logic (frontend/src/lib/multi-select.ts, new)

Pure, fully unit-tested functions (colocated vitest per AGENTS.md):

- `toggleKey(selected, key)` — cmd/ctrl-click semantics; returns new
  selection + anchor/focus update.
- `rangeKeys(visibleKeys, from, to)` — contiguous slice of the **visible
  conversation key order** (day-header rows excluded) between two keys,
  inclusive; direction-agnostic; empty if either key not visible.
- `applyShiftClick(selected, visibleKeys, anchor, key)` — Apple Mail
  semantics: selection becomes `range(anchor…key)`; anchor unchanged.
- `applyCmdShiftClick(selected, visibleKeys, anchor, key)` — union of
  current selection with `range(anchor…key)`.
- `extendSelection(visibleKeys, selected, anchor, focus, dir)` — one-step
  shift+↑/↓: moves focus one row and returns `range(anchor…focus)`.
- `targetMessageId(convo)` — first unread ?? latest (extracted from the
  current click handler so list and store share one rule).

## Interactions

### Mouse (frontend/src/components/mail/mail-list.tsx)

Row click handler gains modifier branches (visible order = `listRows`
conversation order):

| Input | Effect |
|---|---|
| plain click | selection = [key]; anchor = focus = key (today's behavior) |
| cmd/ctrl+click | `toggleKey`; if added, anchor = focus = key; if the removed key was the anchor, anchor/focus fall back to the last remaining key (or null) |
| shift+click | `applyShiftClick` |
| cmd+shift+click | `applyCmdShiftClick` |

Highlight: a row is highlighted when `selectedConversationKeys.includes(key)`
(or, when not multi-selecting, today's `selectedMessageId` membership check).

### Context menu (conversation-context-menu.tsx)

- Right-click on a conversation **in** the selection: the menu's actions
  (archive, spam/not-spam, trash with existing count-aware confirm, move/copy,
  read/unread, star, mute, snooze) apply to **all** selected conversations —
  the menu receives the union of member message ids and the existing helpers
  already loop `string[]`.
- Right-click on an **unselected** conversation: selection collapses to that
  conversation first, then today's single-conversation menu applies.

### Drag & drop (mail-dnd.tsx)

`ConversationDragData` gains the selected keys when the dragged row is in the
selection; `handleConversationDrop` then moves every selected conversation
(reusing its existing done/total progress toast). Dragging an unselected row
behaves as today.

### Keyboard (lib/keyboard.ts + mail-list effect)

- **⌘A / Ctrl+A** (list context, `isEditableTarget` guard): select all visible
  conversation keys; anchor/focus = first/last visible.
- **Esc**: while multi-select is active, collapse to the anchor conversation;
  otherwise today's back-navigation behavior is unchanged.
- **Shift+↑ / Shift+↓** (and shift+j/k): `extendSelection` one row.
- Plain j/k, o/Enter, u unchanged (plain navigation collapses to single).
- `shortcut-help.tsx` gains rows for the new shortcuts.

## Reading pane: card stack (mail-display.tsx + new multi-select-stack.tsx)

When `selectedConversationKeys.length > 1`, the reading pane renders
`MultiSelectStack` instead of the single-conversation view:

- **Front card**: the anchor conversation, rendered with the existing
  `MessageCard` pipeline inside a framed card.
- **Stack edges**: 1–2 pseudo-cards behind the front card (offset ≈10px,
  `scale(0.97)`, soft shadow, pure CSS) for the Apple Mail layered look.
- **Count badge**: "N selected" (`mail.conversationCount` with `{count}`).
- **Pager**: `‹ i of N ›` cycles the anchor through the selection (updates
  anchor + `selectedMessageId`; selection itself unchanged). Reuses the
  `stepConversation` pattern.
- **Bulk action bar** above the stack: Archive, Trash (count-aware
  `confirmMoveToTrash`), Mark read/unread, Star/unstar, Move to folder
  (existing `FolderPickerSub`), Spam. Read/star buttons resolve mixed state
  as "any unread → mark read", "any unstarred → star". Progress feedback
  reuses the `mail.movingMessages` `{done, total}` toast pattern.

After a bulk action that removes conversations from the current view
(archive/trash/move/spam/snooze), the selection is cleared; for in-place
patches (read/star/labels) the selection is kept.

## Error handling

Follows `conversation-actions.ts`: sequential per-message loop, stop on first
error, `BatchResult { done, error }` surfaced as a toast naming how many
succeeded. Trash keeps the existing `confirmMoveToTrash(locale, count)`
dialog. No new error surface is introduced.

## i18n

New keys under `mail.*` in both `frontend/src/i18n/en.json` and `zh.json`
(parity enforced by `i18n.test.ts`): selection count label, bulk-action
labels where not already present, pager label, new shortcut-help strings.

## Testing

- `frontend/src/lib/multi-select.test.ts`: toggle, range (both directions,
  unseen keys, empty visible list), shift-click replace, cmd+shift union,
  anchor fallback after toggle-off, `extendSelection` at list bounds,
  select-all.
- UI-store selection actions: sync between anchor and `selectedMessageId`,
  clear-on-folder-switch.
- Existing suites must stay green: `npm test`, `npm run check`
  (oxlint + tsc), `make fmt`.

## Files touched

| File | Change |
|---|---|
| `frontend/src/lib/multi-select.ts` (+test) | new pure selection logic |
| `frontend/src/stores/ui.ts` | selection state + actions |
| `frontend/src/components/mail/mail-list.tsx` | modifier clicks, highlight, keyboard |
| `frontend/src/components/mail/multi-select-stack.tsx` | new card-stack reader |
| `frontend/src/components/mail/mail-display.tsx` | branch to stack when N>1 |
| `frontend/src/components/mail/conversation-context-menu.tsx` | multi-aware dispatch |
| `frontend/src/components/mail/mail-dnd.tsx` | drag selected set |
| `frontend/src/lib/keyboard.ts` | ⌘A / Esc / shift-arrow matchers |
| `frontend/src/components/shortcut-help.tsx` | new shortcut rows |
| `frontend/src/i18n/en.json`, `zh.json` | new strings |
