# Lyra Mail Multi-Select Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apple Mail–style multi-selection in the mail conversation list (cmd/ctrl-click toggle, shift-click ranges, ⌘A / Esc / shift+↑↓, card-stack reading pane, bulk actions), frontend-only.

**Architecture:** Selection state (conversation keys + anchor + focus) lives in the Zustand UI store; pure selection logic lives in a new `lib/multi-select.ts` with colocated vitest coverage. Bulk execution reuses the existing per-message batch loop in `lib/conversation-actions.ts` — no backend changes.

**Tech Stack:** React 19, Zustand, vitest, Tailwind, shadcn/ui, dnd-kit.

**Spec:** `docs/superpowers/specs/2026-09-09-lyra-mail-multi-select-design.md`
**Worktree:** `/Users/huangcheng/Projects/Lyra/.worktrees/mail-multi-select` (branch `feat/mail-multi-select`). All commands run from `<worktree>/frontend` unless noted.

**Baseline:** 266 frontend tests passing (37 files) on a clean checkout.

---

### Task 1: Pure selection logic (`lib/multi-select.ts`)

**Files:**
- Create: `frontend/src/lib/multi-select.ts`
- Test: `frontend/src/lib/multi-select.test.ts`

- [ ] **Step 1: Write the failing test**

Create `frontend/src/lib/multi-select.test.ts`:

```ts
import { describe, expect, it } from 'vitest';

import {
  applyCmdShiftClick,
  applyShiftClick,
  EMPTY_SELECTION,
  extendSelection,
  isMultiSelection,
  rangeKeys,
  selectAll,
  singleSelect,
  toggleKey,
} from '@/lib/multi-select';

const V = ['a', 'b', 'c', 'd', 'e'];

describe('singleSelect', () => {
  it('selects exactly one key as anchor and focus', () => {
    expect(singleSelect('b')).toEqual({ keys: ['b'], anchor: 'b', focus: 'b' });
  });
});

describe('toggleKey', () => {
  it('adds an unselected key and makes it anchor+focus', () => {
    expect(toggleKey(singleSelect('a'), 'c')).toEqual({
      keys: ['a', 'c'],
      anchor: 'c',
      focus: 'c',
    });
  });
  it('removes a selected non-anchor key, keeping the anchor', () => {
    const sel = { keys: ['a', 'c'], anchor: 'c', focus: 'c' };
    expect(toggleKey(sel, 'a')).toEqual({ keys: ['c'], anchor: 'c', focus: 'c' });
  });
  it('falls back to the last remaining key when the anchor is toggled off', () => {
    const sel = { keys: ['a', 'c'], anchor: 'c', focus: 'c' };
    expect(toggleKey(sel, 'c')).toEqual({ keys: ['a'], anchor: 'a', focus: 'a' });
  });
  it('empties the selection when the last key is toggled off', () => {
    expect(toggleKey(singleSelect('a'), 'a')).toEqual(EMPTY_SELECTION);
  });
});

describe('rangeKeys', () => {
  it('returns the inclusive slice in visible order, either direction', () => {
    expect(rangeKeys(V, 'b', 'd')).toEqual(['b', 'c', 'd']);
    expect(rangeKeys(V, 'd', 'b')).toEqual(['b', 'c', 'd']);
  });
  it('returns empty when either endpoint is not visible', () => {
    expect(rangeKeys(V, 'b', 'zzz')).toEqual([]);
    expect(rangeKeys([], 'a', 'b')).toEqual([]);
  });
});

describe('applyShiftClick', () => {
  it('replaces the selection with the anchor-to-target range', () => {
    const sel = { keys: ['a'], anchor: 'a', focus: 'a' };
    expect(applyShiftClick(sel, V, 'd')).toEqual({
      keys: ['a', 'b', 'c', 'd'],
      anchor: 'a',
      focus: 'd',
    });
  });
  it('single-selects when there is no anchor', () => {
    expect(applyShiftClick(EMPTY_SELECTION, V, 'c')).toEqual(singleSelect('c'));
  });
  it('single-selects when the anchor is no longer visible', () => {
    const sel = { keys: ['zzz'], anchor: 'zzz', focus: 'zzz' };
    expect(applyShiftClick(sel, V, 'c')).toEqual(singleSelect('c'));
  });
});

describe('applyCmdShiftClick', () => {
  it('unions the current selection with the anchor-to-target range', () => {
    const sel = { keys: ['e'], anchor: 'b', focus: 'b' };
    expect(applyCmdShiftClick(sel, V, 'd').keys).toEqual(['e', 'b', 'c', 'd']);
    expect(applyCmdShiftClick(sel, V, 'd').anchor).toBe('b');
  });
});

describe('extendSelection', () => {
  it('moves focus one row and keeps the anchor fixed', () => {
    const sel = { keys: ['b'], anchor: 'b', focus: 'b' };
    expect(extendSelection(V, sel, 1)).toEqual({
      keys: ['b', 'c'],
      anchor: 'b',
      focus: 'c',
    });
  });
  it('shrinks back toward the anchor', () => {
    const sel = { keys: ['b', 'c', 'd'], anchor: 'b', focus: 'd' };
    expect(extendSelection(V, sel, -1).keys).toEqual(['b', 'c']);
  });
  it('clamps at the list bounds', () => {
    const sel = { keys: ['a'], anchor: 'a', focus: 'a' };
    expect(extendSelection(V, sel, -1)).toEqual(sel);
    const last = { keys: ['e'], anchor: 'e', focus: 'e' };
    expect(extendSelection(V, last, 1)).toEqual(last);
  });
  it('starts from the edge when nothing is selected', () => {
    expect(extendSelection(V, EMPTY_SELECTION, 1)).toEqual(singleSelect('a'));
    expect(extendSelection(V, EMPTY_SELECTION, -1)).toEqual(singleSelect('e'));
  });
});

describe('selectAll', () => {
  it('selects every visible key, first as anchor and last as focus', () => {
    expect(selectAll(V)).toEqual({ keys: V, anchor: 'a', focus: 'e' });
    expect(selectAll([])).toEqual(EMPTY_SELECTION);
  });
});

describe('isMultiSelection', () => {
  it('is true only with more than one key', () => {
    expect(isMultiSelection(singleSelect('a'))).toBe(false);
    expect(isMultiSelection({ keys: ['a', 'b'], anchor: 'a', focus: 'b' })).toBe(true);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/multi-select.test.ts`
Expected: FAIL — cannot resolve `@/lib/multi-select`.

- [ ] **Step 3: Write the implementation**

Create `frontend/src/lib/multi-select.ts`:

```ts
/**
 * Apple Mail–style conversation multi-selection.
 *
 * Pure state transitions over conversation keys; the UI store
 * (`stores/ui.ts`) holds the current `{ keys, anchor, focus }` and the
 * components keep `selectedMessageId` synced to the anchor conversation's
 * target message. `visibleKeys` is always the current on-screen
 * conversation order (day-group header rows excluded).
 */

import type { Conversation } from '@/lib/conversation';

export interface ConversationSelection {
  /** Selected conversation keys, in click order. */
  keys: string[];
  /** Range anchor; also the reading pane's front card. */
  anchor: string | null;
  /** Moving edge for shift+↑/↓ extension. */
  focus: string | null;
}

export const EMPTY_SELECTION: ConversationSelection = { keys: [], anchor: null, focus: null };

export function isMultiSelection(sel: ConversationSelection): boolean {
  return sel.keys.length > 1;
}

export function singleSelect(key: string): ConversationSelection {
  return { keys: [key], anchor: key, focus: key };
}

/** Cmd/Ctrl+click: toggle one key. Toggling off the anchor falls back to the last remaining key. */
export function toggleKey(sel: ConversationSelection, key: string): ConversationSelection {
  if (sel.keys.includes(key)) {
    const keys = sel.keys.filter((k) => k !== key);
    const anchor = sel.anchor === key ? (keys[keys.length - 1] ?? null) : sel.anchor;
    const focus = sel.focus === key ? anchor : sel.focus;
    return { keys, anchor, focus };
  }
  return { keys: [...sel.keys, key], anchor: key, focus: key };
}

/** Inclusive slice of the visible order between two keys, either direction. */
export function rangeKeys(visibleKeys: string[], from: string, to: string): string[] {
  const a = visibleKeys.indexOf(from);
  const b = visibleKeys.indexOf(to);
  if (a === -1 || b === -1) return [];
  const [lo, hi] = a <= b ? [a, b] : [b, a];
  return visibleKeys.slice(lo, hi + 1);
}

/** Shift+click: selection becomes range(anchor…key); the anchor is unchanged. */
export function applyShiftClick(
  sel: ConversationSelection,
  visibleKeys: string[],
  key: string,
): ConversationSelection {
  if (!sel.anchor) return singleSelect(key);
  const range = rangeKeys(visibleKeys, sel.anchor, key);
  if (range.length === 0) return singleSelect(key);
  return { keys: range, anchor: sel.anchor, focus: key };
}

/** Cmd+Shift+click: union of the current selection and range(anchor…key). */
export function applyCmdShiftClick(
  sel: ConversationSelection,
  visibleKeys: string[],
  key: string,
): ConversationSelection {
  if (!sel.anchor) return singleSelect(key);
  const range = rangeKeys(visibleKeys, sel.anchor, key);
  if (range.length === 0) return toggleKey(sel, key);
  return { keys: [...new Set([...sel.keys, ...range])], anchor: sel.anchor, focus: key };
}

/** Shift+↑/↓: move the focus one row and select range(anchor…focus). */
export function extendSelection(
  visibleKeys: string[],
  sel: ConversationSelection,
  dir: 1 | -1,
): ConversationSelection {
  if (visibleKeys.length === 0) return sel;
  if (!sel.anchor || !sel.focus) return singleSelect(dir === 1 ? visibleKeys[0] : visibleKeys[visibleKeys.length - 1]);
  const idx = visibleKeys.indexOf(sel.focus);
  if (idx === -1) return singleSelect(sel.focus);
  const nextIdx = Math.min(Math.max(idx + dir, 0), visibleKeys.length - 1);
  if (nextIdx === idx) return sel;
  const focus = visibleKeys[nextIdx];
  return { keys: rangeKeys(visibleKeys, sel.anchor, focus), anchor: sel.anchor, focus };
}

export function selectAll(visibleKeys: string[]): ConversationSelection {
  if (visibleKeys.length === 0) return EMPTY_SELECTION;
  return {
    keys: [...visibleKeys],
    anchor: visibleKeys[0],
    focus: visibleKeys[visibleKeys.length - 1],
  };
}

/** The message a conversation opens to: first unread, else the latest. */
export function targetMessageId(convo: Conversation): string {
  return (convo.messages.find((m) => !m.isRead) ?? convo.latest).id;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/multi-select.test.ts`
Expected: PASS (8 test groups).

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/multi-select.ts frontend/src/lib/multi-select.test.ts
git commit -m "feat(mail): pure conversation multi-select state transitions"
```

---

### Task 2: UI-store selection state

**Files:**
- Modify: `frontend/src/stores/ui.ts`
- Test: `frontend/src/stores/ui.test.ts` (exists; extend)

- [ ] **Step 1: Write the failing test**

Read `frontend/src/stores/ui.test.ts` first and match its style. Append:

```ts
import { singleSelect } from '@/lib/multi-select';

describe('conversation multi-select', () => {
  it('applyConversationSelection sets keys, anchor, focus, and message together', () => {
    useUIStore.getState().applyConversationSelection(
      { keys: ['a', 'b'], anchor: 'a', focus: 'b' },
      'msg-1',
    );
    const s = useUIStore.getState();
    expect(s.selectedConversationKeys).toEqual(['a', 'b']);
    expect(s.selectionAnchorKey).toBe('a');
    expect(s.selectionFocusKey).toBe('b');
    expect(s.selectedMessageId).toBe('msg-1');
  });

  it('clearConversationSelection empties keys/anchor/focus but keeps the message', () => {
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().clearConversationSelection();
    const s = useUIStore.getState();
    expect(s.selectedConversationKeys).toEqual([]);
    expect(s.selectionAnchorKey).toBeNull();
    expect(s.selectionFocusKey).toBeNull();
    expect(s.selectedMessageId).toBe('msg-1');
  });

  it('folder/account switches clear the conversation selection', () => {
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().setSelectedFolder('f1');
    expect(useUIStore.getState().selectedConversationKeys).toEqual([]);
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().setSelectedAccount('acc-1');
    expect(useUIStore.getState().selectedConversationKeys).toEqual([]);
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().setSelectedFolderRole('inbox');
    expect(useUIStore.getState().selectedConversationKeys).toEqual([]);
  });
});
```

(Wrap in the file's existing `describe` for the UI store if there is one; ensure the store is reset between tests the way the existing test does.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/stores/ui.test.ts`
Expected: FAIL — `applyConversationSelection is not a function`.

- [ ] **Step 3: Implement**

In `frontend/src/stores/ui.ts`:

Add the import at the top:

```ts
import type { ConversationSelection } from '@/lib/multi-select';
```

Add to the `UIState` interface (after `selectedMessageId: string | null;`):

```ts
  /** Conversation-level multi-select (empty = single-select mode). Not persisted. */
  selectedConversationKeys: string[];
  selectionAnchorKey: string | null;
  selectionFocusKey: string | null;
```

Add to the actions block of the interface (after `setSelectedMessage`):

```ts
  /** Set the conversation selection and the reader's message in one update. */
  applyConversationSelection: (sel: ConversationSelection, messageId: string | null) => void;
  /** Empty the conversation selection (keeps `selectedMessageId`). */
  clearConversationSelection: () => void;
```

Add initial state (after `selectedMessageId: null,`):

```ts
  selectedConversationKeys: [],
  selectionAnchorKey: null,
  selectionFocusKey: null,
```

Update the three view-switch setters to also clear the selection:

```ts
  setSelectedAccount: (id) =>
    set({
      selectedAccountId: id,
      selectedFolderId: null,
      selectedFolderRole: id === ALL_ACCOUNTS ? 'inbox' : null,
      selectedMessageId: null,
      selectedConversationKeys: [],
      selectionAnchorKey: null,
      selectionFocusKey: null,
    }),

  setSelectedFolder: (id) =>
    set({
      selectedFolderId: id,
      selectedFolderRole: null,
      selectedMessageId: null,
      selectedConversationKeys: [],
      selectionAnchorKey: null,
      selectionFocusKey: null,
    }),

  setSelectedFolderRole: (role) =>
    set({
      selectedFolderRole: role,
      selectedFolderId: null,
      selectedMessageId: null,
      selectedConversationKeys: [],
      selectionAnchorKey: null,
      selectionFocusKey: null,
    }),
```

Add the actions (after `setSelectedMessage`):

```ts
  applyConversationSelection: (sel, messageId) =>
    set({
      selectedConversationKeys: sel.keys,
      selectionAnchorKey: sel.anchor,
      selectionFocusKey: sel.focus,
      selectedMessageId: messageId,
    }),

  clearConversationSelection: () =>
    set({ selectedConversationKeys: [], selectionAnchorKey: null, selectionFocusKey: null }),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/stores/ui.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/stores/ui.ts frontend/src/stores/ui.test.ts
git commit -m "feat(mail): conversation selection state in the UI store"
```

---

### Task 3: Keyboard matchers

**Files:**
- Modify: `frontend/src/lib/keyboard.ts`
- Test: `frontend/src/lib/keyboard.test.ts` (create if absent — check first; the repo colocates lib tests)

- [ ] **Step 1: Write the failing test**

Append to `frontend/src/lib/keyboard.test.ts` (create the file with matching imports if missing):

```ts
import { matchMailListShortcut, matchMailSelectionShortcut } from '@/lib/keyboard';

describe('matchMailSelectionShortcut', () => {
  const noMod = { metaKey: false, ctrlKey: false };

  it('maps mod+A to select-all', () => {
    expect(
      matchMailSelectionShortcut({ key: 'a', ...noMod, metaKey: true, shiftKey: false }, null),
    ).toBe('select-all');
    expect(
      matchMailSelectionShortcut({ key: 'a', ...noMod, ctrlKey: true, shiftKey: false }, null, false),
    ).toBe('select-all');
  });

  it('maps shift+arrows and shift+J/K to extend', () => {
    const ev = (key: string) => ({ key, ...noMod, shiftKey: true });
    expect(matchMailSelectionShortcut(ev('ArrowDown'), null)).toBe('extend-next');
    expect(matchMailSelectionShortcut(ev('ArrowUp'), null)).toBe('extend-prev');
    expect(matchMailSelectionShortcut(ev('J'), null)).toBe('extend-next');
    expect(matchMailSelectionShortcut(ev('K'), null)).toBe('extend-prev');
  });

  it('ignores plain keys, mod+shift+A, and editable targets', () => {
    expect(
      matchMailSelectionShortcut({ key: 'a', ...noMod, shiftKey: false }, null),
    ).toBeNull();
    expect(
      matchMailSelectionShortcut({ key: 'a', metaKey: true, ctrlKey: false, shiftKey: true }, null),
    ).toBeNull();
    const input = document.createElement('input');
    expect(
      matchMailSelectionShortcut({ key: 'a', metaKey: true, ctrlKey: false, shiftKey: false }, input),
    ).toBeNull();
  });

  it('plain navigation ignores shift (handled by the selection matcher)', () => {
    expect(matchMailListShortcut({ key: 'j', shiftKey: true }, null)).toBeNull();
    expect(matchMailListShortcut({ key: 'ArrowDown', shiftKey: true }, null)).toBeNull();
    expect(matchMailListShortcut({ key: 'j', shiftKey: false }, null)).toBe('next');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/keyboard.test.ts`
Expected: FAIL — `matchMailSelectionShortcut is not a function`.

- [ ] **Step 3: Implement**

In `frontend/src/lib/keyboard.ts`, change the navigation matcher to ignore shift (shift variants belong to the selection matcher):

```ts
export function matchMailListShortcut(
  event: { key: string; shiftKey?: boolean },
  target: EventTarget | null,
): MailListShortcut {
  if (isEditableTarget(target) || event.shiftKey) return null;
  // ...rest unchanged
}
```

Append the selection matcher:

```ts
/**
 * Multi-select chords: ⌘A/Ctrl+A select-all, shift+↑/↓ (or shift+J/K)
 * extend. Returns null for anything else (or when typing).
 */
export type MailSelectionShortcut = 'select-all' | 'extend-next' | 'extend-prev' | null;

export function matchMailSelectionShortcut(
  event: { key: string; metaKey: boolean; ctrlKey: boolean; shiftKey: boolean },
  target: EventTarget | null,
  isMac = true,
): MailSelectionShortcut {
  if (isEditableTarget(target)) return null;
  const mod = isMac ? event.metaKey : event.ctrlKey;
  const other = isMac ? event.ctrlKey : event.metaKey;
  if (mod && !other && !event.shiftKey && (event.key === 'a' || event.key === 'A')) {
    return 'select-all';
  }
  if (event.shiftKey && !mod && !other) {
    if (event.key === 'ArrowDown' || event.key === 'J') return 'extend-next';
    if (event.key === 'ArrowUp' || event.key === 'K') return 'extend-prev';
  }
  return null;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/keyboard.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/keyboard.ts frontend/src/lib/keyboard.test.ts
git commit -m "feat(mail): multi-select keyboard matchers"
```

---

### Task 4: i18n strings

**Files:**
- Modify: `frontend/src/i18n/en.json`
- Modify: `frontend/src/i18n/zh.json`
- (parity enforced by `frontend/src/i18n/i18n.test.ts`)

- [ ] **Step 1: Add keys to both locales**

In `frontend/src/i18n/en.json`, inside the `mail` object (alphabetical-ish, next to existing keys — match surrounding style):

```json
    "selectionCount": "{{count}} selected",
    "clearSelection": "Clear selection",
    "skippedOtherAccounts": "{{count}} conversations from other accounts were not moved",
```

and inside the `shortcuts` object:

```json
    "selectAll": "Select all conversations",
    "extendSelection": "Extend selection",
```

In `frontend/src/i18n/zh.json`, same locations:

```json
    "selectionCount": "已选 {{count}} 项",
    "clearSelection": "清除选择",
    "skippedOtherAccounts": "{{count}} 个其他账户的会话未移动",
```

```json
    "selectAll": "全选会话",
    "extendSelection": "扩展选择",
```

- [ ] **Step 2: Run the i18n parity test**

Run: `cd frontend && npx vitest run src/i18n/i18n.test.ts`
Expected: PASS (fails if a key exists in only one locale).

- [ ] **Step 3: Commit**

```bash
git add frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "feat(mail): i18n strings for multi-select"
```

---

### Task 5: Mail list — modifier clicks, highlight, keyboard, pruning

**Files:**
- Modify: `frontend/src/components/mail/mail-list.tsx`

- [ ] **Step 1: Wire the store and visible-key order**

In `MailList()`, add selectors next to the existing `selectedMessageId` selector (~line 119):

```ts
  const selectedConversationKeys = useUIStore((s) => s.selectedConversationKeys);
  const applyConversationSelection = useUIStore((s) => s.applyConversationSelection);
```

Add imports:

```ts
import {
  applyCmdShiftClick,
  applyShiftClick,
  extendSelection,
  selectAll,
  singleSelect,
  targetMessageId,
  type ConversationSelection,
} from '@/lib/multi-select';
import { matchMailListShortcut, matchMailSelectionShortcut } from '@/lib/keyboard';
```

(Replace the existing `matchMailListShortcut` import.)

After `const conversations = useMemo(...)` (~line 278), add:

```ts
  const visibleKeys = useMemo(() => conversations.map((c) => c.key), [conversations]);

  /** Current selection snapshot from the store (handlers read it lazily). */
  const currentSelection = (): ConversationSelection => {
    const s = useUIStore.getState();
    return {
      keys: s.selectedConversationKeys,
      anchor: s.selectionAnchorKey,
      focus: s.selectionFocusKey,
    };
  };

  /** Apply a new selection and point the reader at the anchor conversation. */
  const commitSelection = (sel: ConversationSelection) => {
    const anchorConvo = sel.anchor
      ? conversations.find((c) => c.key === sel.anchor)
      : undefined;
    applyConversationSelection(sel, anchorConvo ? targetMessageId(anchorConvo) : null);
  };
```

- [ ] **Step 2: Replace the row click/context-menu/Enter handlers**

Replace the three handlers on the row div (currently ~lines 436-450):

```tsx
                    onClick={(e) => {
                      const sel = currentSelection();
                      if (e.shiftKey && (e.metaKey || e.ctrlKey)) {
                        commitSelection(applyCmdShiftClick(sel, visibleKeys, convo.key));
                      } else if (e.shiftKey) {
                        commitSelection(applyShiftClick(sel, visibleKeys, convo.key));
                      } else if (e.metaKey || e.ctrlKey) {
                        commitSelection(toggleKey(sel, convo.key));
                      } else {
                        commitSelection(singleSelect(convo.key));
                      }
                    }}
                    onContextMenu={() => {
                      // Apple Mail: right-click inside the selection keeps it;
                      // right-click elsewhere collapses the selection to that row.
                      if (!currentSelection().keys.includes(convo.key)) {
                        commitSelection(singleSelect(convo.key));
                      }
                    }}
                    onKeyDown={(e) => {
                      if ((e.key === 'Enter' || e.key === ' ') && !e.shiftKey && !e.metaKey && !e.ctrlKey) {
                        e.preventDefault();
                        commitSelection(singleSelect(convo.key));
                      }
                    }}
```

Add `toggleKey` to the multi-select import.

Update the highlight check (~line 395):

```ts
            const isSelected =
              selectedConversationKeys.length > 0
                ? selectedConversationKeys.includes(convo.key)
                : convo.messages.some((m) => m.id === selectedMessageId);
```

- [ ] **Step 3: Extend the keyboard effect**

Replace the navigation effect (~lines 282-311) with:

```tsx
  // Gmail-style list navigation (j/k, o/Enter, u/Esc) plus multi-select
  // chords (⌘A, shift+↑/↓). Esc collapses an active multi-selection to its
  // anchor before falling back to the plain back behavior.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const isMac = /Mac|iPhone|iPad/.test(navigator.platform || '');
      const selAction = matchMailSelectionShortcut(e, e.target, isMac);
      if (selAction && conversations.length > 0) {
        e.preventDefault();
        if (selAction === 'select-all') {
          commitSelection(selectAll(visibleKeys));
        } else {
          commitSelection(extendSelection(visibleKeys, currentSelection(), selAction === 'extend-next' ? 1 : -1));
        }
        return;
      }
      const action = matchMailListShortcut(e, e.target);
      if (!action || conversations.length === 0) return;
      const currentIdx = Math.max(
        0,
        conversations.findIndex((c) => c.messages.some((m) => m.id === selectedMessageId)),
      );
      if (action === 'next' || action === 'prev') {
        e.preventDefault();
        const idx = action === 'next'
          ? Math.min(currentIdx + 1, conversations.length - 1)
          : Math.max(currentIdx - 1, 0);
        commitSelection(singleSelect(conversations[idx].key));
      } else if (action === 'open') {
        const current = conversations[currentIdx];
        if (current && !current.messages.some((m) => m.id === selectedMessageId)) {
          e.preventDefault();
          commitSelection(singleSelect(current.key));
        }
      } else if (action === 'back') {
        const sel = currentSelection();
        if (sel.keys.length > 1 && sel.anchor) {
          e.preventDefault();
          commitSelection(singleSelect(sel.anchor));
        } else if (selectedMessageId) {
          e.preventDefault();
          setSelectedMessage(null);
        }
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [conversations, visibleKeys, selectedMessageId, setSelectedMessage]);
```

(`commitSelection`/`currentSelection` are stable-enough closures recreated each render; the effect deps cover what they read. If oxlint complains about exhaustive deps, follow its suggestion.)

- [ ] **Step 4: Prune vanished conversations from the selection**

After the navigation effect, add:

```tsx
  // Selection pruning: conversations that left the view (archived, moved,
  // tab/filter change) drop out of the selection; the anchor falls back to
  // the last surviving key.
  useEffect(() => {
    const s = useUIStore.getState();
    if (s.selectedConversationKeys.length === 0) return;
    const visible = new Set(visibleKeys);
    const keys = s.selectedConversationKeys.filter((k) => visible.has(k));
    if (keys.length === s.selectedConversationKeys.length) return;
    const anchor =
      s.selectionAnchorKey && visible.has(s.selectionAnchorKey)
        ? s.selectionAnchorKey
        : (keys[keys.length - 1] ?? null);
    const focus =
      s.selectionFocusKey && visible.has(s.selectionFocusKey) ? s.selectionFocusKey : anchor;
    const anchorConvo = anchor ? conversations.find((c) => c.key === anchor) : undefined;
    s.applyConversationSelection(
      { keys, anchor, focus },
      anchorConvo ? targetMessageId(anchorConvo) : null,
    );
  }, [conversations, visibleKeys]);
```

(If oxlint flags `set-state-in-effect`, add the same `// oxlint-disable-next-line set-state-in-effect` comment used at ~line 245 with a one-line reason.)

- [ ] **Step 5: Typecheck + tests**

Run: `cd frontend && npx tsc --noEmit && npm test 2>&1 | tail -5`
Expected: no type errors; 266+ tests passing.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/components/mail/mail-list.tsx
git commit -m "feat(mail): multi-select interactions in the conversation list"
```

---

### Task 6: Context menu applies to the selection

**Files:**
- Modify: `frontend/src/components/mail/conversation-context-menu.tsx`
- Modify: `frontend/src/components/mail/mail-list.tsx` (pass the selected conversations)

- [ ] **Step 1: Make the menu multi-aware**

In `conversation-context-menu.tsx`:

1. Extend the props:

```ts
export function ConversationContextMenu({
  convo,
  multiConvos,
  onActionError,
  children,
}: {
  convo: Conversation;
  /** Non-null when the right-clicked row is part of a multi-selection. */
  multiConvos?: Conversation[];
  onActionError: (message: string | null) => void;
  children: ReactNode;
}) {
```

2. Below `const ids = convo.messages.map((m) => m.id);` (~line 178), add:

```ts
  const targets = multiConvos && multiConvos.length > 1 ? multiConvos : [convo];
  const targetIds = targets.flatMap((c) => c.messages.map((m) => m.id));
  const anyUnread = targets.some((c) => c.unreadCount > 0);
  const anyUnstarred = targets.some((c) => !c.anyStarred);
  const clearSelection = useUIStore((s) => s.clearConversationSelection);
  /** Removing actions drop the selection once the batch starts. */
  const runRemoving = (p: Promise<{ error: string | null }>) => {
    clearSelection();
    run(p);
  };
```

3. Replace `ids` with `targetIds` in the batch actions: archive, spam/notSpam, trash (both the `confirmMoveToTrash(locale, targetIds.length)` count and the `actOnMessages` call), move, copy, snooze — and switch archive/spam/trash/move/snooze from `run(...)` to `runRemoving(...)`. Copy keeps `run(...)` (it removes nothing).

4. Replace the read/unread and star conditions to use the aggregates:

```tsx
        {anyUnread ? (
          <ContextMenuItem onSelect={() => run(patchMessages(targetIds, { isRead: true }))}>
            <MailOpen />
            {t(locale, 'mail.markRead')}
          </ContextMenuItem>
        ) : (
          <ContextMenuItem onSelect={() => run(patchMessages(targetIds, { isRead: false }))}>
            <Mail />
            {t(locale, 'mail.markUnread')}
          </ContextMenuItem>
        )}
        <ContextMenuItem
          onSelect={() => run(patchMessages(targetIds, { isStarred: anyUnstarred }))}
        >
          {anyUnstarred ? <Star /> : <StarOff />}
          {t(locale, anyUnstarred ? 'mail.star' : 'mail.unstar')}
        </ContextMenuItem>
```

5. Mute: loop the target conversations' thread ids. Replace the mute item's `onSelect` body so that, for each target conversation, it computes that conversation's `notifyThreadId` (same expression as the existing one, per convo) and mutes/unmutes each; the `notifyMuted` toggle state stays driven by the right-clicked convo. Reply/ReplyAll/Forward/Edit-draft stay bound to `latest` (the right-clicked conversation) in all modes.

6. Move/Copy pickers (`FolderPickerSub`) stay account-scoped to the right-clicked convo; when the selection spans accounts, conversations from other accounts are skipped in `onPick`:

```ts
        onPick={(folderId) => {
          const sameAccountIds = targets
            .filter((c) => c.latest.accountId === convo.latest.accountId)
            .flatMap((c) => c.messages.map((m) => m.id));
          const skipped = targets.length - targets.filter((c) => c.latest.accountId === convo.latest.accountId).length;
          if (skipped > 0) report(t(locale, 'mail.skippedOtherAccounts', { count: skipped }));
          runRemoving(moveMessages(sameAccountIds, folderId));
        }}
```

(Copy: same filtering, but `run(copyMessages(...))`.)

- [ ] **Step 2: Pass the selection from the list**

In `mail-list.tsx`, after `visibleKeys`, add:

```ts
  const selectedConvos = useMemo(
    () => conversations.filter((c) => selectedConversationKeys.includes(c.key)),
    [conversations, selectedConversationKeys],
  );
```

Then in the row render, pass the prop:

```tsx
                <ConversationContextMenu
                  convo={convo}
                  multiConvos={
                    selectedConversationKeys.length > 1 && selectedConversationKeys.includes(convo.key)
                      ? selectedConvos
                      : undefined
                  }
                  onActionError={setActionError}
                >
```

- [ ] **Step 3: Typecheck + tests**

Run: `cd frontend && npx tsc --noEmit && npm test 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/components/mail/conversation-context-menu.tsx frontend/src/components/mail/mail-list.tsx
git commit -m "feat(mail): context menu actions apply to the multi-selection"
```

---

### Task 7: Drag & drop moves the selection

**Files:**
- Modify: `frontend/src/components/mail/mail-list.tsx` (`DraggableConversationRow`)
- Modify: `frontend/src/components/mail/mail-dnd.tsx`

- [ ] **Step 1: Multi-conversation drag payload**

In `mail-list.tsx`, change `DraggableConversationRow` (~lines 84-112) to accept the drag conversation set:

```tsx
function DraggableConversationRow({
  convo,
  dragConvos,
  children,
}: {
  convo: Conversation;
  /** Non-null when dragging a row that is part of a multi-selection. */
  dragConvos?: Conversation[];
  children: React.ReactNode;
}) {
  // Cross-account moves are rejected per folder, so a mixed-account drag
  // only carries the dragged row's own account.
  const dragged = (dragConvos ?? [convo]).filter(
    (c) => c.latest.accountId === convo.latest.accountId,
  );
  const messageIds = dragged.flatMap((c) => c.messages.map((m) => m.id));
  const folderIds = [...new Set(dragged.flatMap((c) => c.messages.map((m) => m.folderId)))];
  const { listeners, setNodeRef, isDragging } = useDraggable({
    id: `convo:${convo.key}`,
    data: {
      type: 'conversation',
      accountId: convo.latest.accountId,
      messageIds,
      folderIds,
      subject: convo.latest.subject,
      count: messageIds.length,
      selectionDrag: dragged.length > 1,
    } satisfies ConversationDragData,
  });
  return (
    <div ref={setNodeRef} {...listeners} className={cn(isDragging && 'opacity-40')}>
      {children}
    </div>
  );
}
```

Pass the prop at the row (~line 426):

```tsx
              <DraggableConversationRow
                key={convo.key}
                convo={convo}
                dragConvos={
                  selectedConversationKeys.length > 1 && selectedConversationKeys.includes(convo.key)
                    ? selectedConvos
                    : undefined
                }
              >
```

In `conversation-actions.ts`, extend the drag payload type:

```ts
export interface ConversationDragData {
  type: 'conversation';
  accountId: string;
  messageIds: string[];
  folderIds: string[];
  subject: string;
  count: number;
  /** True when the payload carries a multi-selection (cleared after drop). */
  selectionDrag?: boolean;
}
```

In `mail-dnd.tsx`, at the end of `handleConversationDrop` (after `setProgress(null)`), clear the selection for selection drags:

```ts
    setProgress(null);
    if (data.selectionDrag) useUIStore.getState().clearConversationSelection();
    if (res.error) setError(res.error);
```

(Add `import { useUIStore } from '@/stores/ui';` if not already imported.)

- [ ] **Step 2: Typecheck + tests**

Run: `cd frontend && npx tsc --noEmit && npm test 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add frontend/src/components/mail/mail-list.tsx frontend/src/components/mail/mail-dnd.tsx frontend/src/lib/conversation-actions.ts
git commit -m "feat(mail): drag a multi-selection onto folders"
```

---

### Task 8: Reading pane — card stack + bulk action bar

**Files:**
- Create: `frontend/src/components/mail/multi-select-stack.tsx`
- Modify: `frontend/src/components/mail/mail-display.tsx`

- [ ] **Step 1: Create the stack frame + bulk action bar**

Create `frontend/src/components/mail/multi-select-stack.tsx`:

```tsx
/**
 * Multi-select reading pane: the anchor conversation on a front card with
 * stacked page edges behind it (Apple Mail style), plus the bulk action bar
 * that replaces the normal toolbar while a multi-selection is active.
 */

import {
  Archive,
  ArchiveX,
  ChevronLeft,
  ChevronRight,
  FolderInput,
  Mail,
  MailOpen,
  Star,
  StarOff,
  Trash2,
  X,
} from 'lucide-react';
import { useState, type ReactNode } from 'react';

import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { t } from '@/i18n';
import { confirmMoveToTrash } from '@/lib/confirm-trash';
import { actOnMessages, moveMessages, patchMessages } from '@/lib/conversation-actions';
import type { Conversation } from '@/lib/conversation';
import { buildAccountMoveFolderEntries, type MoveFolderEntry } from '@/lib/folder-tree';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

/** Front card + up to two offset pseudo-cards suggesting the stack. */
export function SelectionStackFrame({ count, children }: { count: number; children: ReactNode }) {
  return (
    <div className="relative min-h-0 flex-1">
      {count > 2 ? (
        <div
          aria-hidden
          className="absolute inset-x-4 top-0 h-3 rounded-t-xl border border-b-0 border-border/50 bg-background/60"
        />
      ) : null}
      <div
        aria-hidden
        className="absolute inset-x-2 top-0.5 h-3 rounded-t-xl border border-b-0 border-border/60 bg-background/80"
      />
      <div className="relative flex h-full flex-col border-t border-border/60 bg-background pt-1">
        {children}
      </div>
    </div>
  );
}

function folderLabel(entry: MoveFolderEntry, locale: 'en' | 'zh'): string {
  return entry.role ? t(locale, `mail.folder.${entry.role}`) : entry.name;
}

/** Toolbar shown while a multi-selection is active; actions hit every selected conversation. */
export function BulkActionBar({
  convos,
  position,
  onStep,
  onError,
}: {
  /** Selected conversations in the current view order. */
  convos: Conversation[];
  /** Anchor position within the selection (for the ‹ i of N › pager). */
  position: { index: number; total: number };
  onStep: (delta: 1 | -1) => void;
  onError: (message: string | null) => void;
}) {
  const locale = useUIStore((s) => s.locale);
  const folders = useMailStore((s) => s.folders);
  const clearConversationSelection = useUIStore((s) => s.clearConversationSelection);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const [busy, setBusy] = useState(false);
  const [moveProgress, setMoveProgress] = useState<{ done: number; total: number } | null>(null);

  const ids = convos.flatMap((c) => c.messages.map((m) => m.id));
  const anchorConvo = convos[position.index] ?? convos[0];
  const anchorAccountId = anchorConvo?.latest.accountId;
  const anyUnread = convos.some((c) => c.unreadCount > 0);
  const anyUnstarred = convos.some((c) => !c.anyStarred);
  const inSpamFolder = anchorConvo
    ? folders[anchorConvo.latest.folderId]?.role === 'spam'
    : false;
  const moveEntries = buildAccountMoveFolderEntries(
    Object.values(folders).filter((f) => f.accountId === anchorAccountId),
    folders,
  );

  /** Removing actions: run, then drop the selection and the reader. */
  const runRemoving = async (p: () => Promise<{ error: string | null }>) => {
    if (busy) return;
    setBusy(true);
    onError(null);
    const res = await p();
    setBusy(false);
    clearConversationSelection();
    setSelectedMessage(null);
    if (res.error) onError(res.error);
  };

  const moveTo = (folderId: string) => {
    const sameAccount = convos.filter((c) => c.latest.accountId === anchorAccountId);
    const skipped = convos.length - sameAccount.length;
    if (skipped > 0) onError(t(locale, 'mail.skippedOtherAccounts', { count: skipped }));
    const moveIds = sameAccount.flatMap((c) => c.messages.map((m) => m.id));
    void runRemoving(async () => {
      setMoveProgress({ done: 0, total: moveIds.length });
      const res = await moveMessages(moveIds, folderId, (done) =>
        setMoveProgress({ done, total: moveIds.length }),
      );
      setMoveProgress(null);
      return res;
    });
  };

  const iconClass = 'shrink-0 rounded-[7px] text-ter-foreground hover:bg-accent hover:text-foreground';

  return (
    <div className="flex shrink-0 items-center gap-1.5 overflow-x-auto border-b border-border/60 p-2">
      <span className="px-1 text-[11px] tabular-nums text-muted-foreground">
        {t(locale, 'mail.selectionCount', { count: convos.length })}
      </span>
      <div className="flex items-center gap-0.5">
        <Button variant="ghost" size="icon" className={iconClass} disabled={busy || position.index <= 0} onClick={() => onStep(-1)} aria-label={t(locale, 'mail.prevConversation')}>
          <ChevronLeft className="h-4 w-4" />
        </Button>
        <span className="text-[11px] tabular-nums text-muted-foreground">
          {position.index + 1} / {position.total}
        </span>
        <Button variant="ghost" size="icon" className={iconClass} disabled={busy || position.index >= position.total - 1} onClick={() => onStep(1)} aria-label={t(locale, 'mail.nextConversation')}>
          <ChevronRight className="h-4 w-4" />
        </Button>
      </div>
      <div className="mx-1 h-4 w-px bg-border/60" />
      <Button variant="ghost" size="icon" className={iconClass} disabled={busy} title={t(locale, 'mail.archive')} aria-label={t(locale, 'mail.archive')} onClick={() => void runRemoving(() => actOnMessages(ids, 'archive'))}>
        <Archive className="h-4 w-4" />
      </Button>
      <Button variant="ghost" size="icon" className={iconClass} disabled={busy} title={t(locale, inSpamFolder ? 'mail.notSpam' : 'mail.moveToJunk')} aria-label={t(locale, inSpamFolder ? 'mail.notSpam' : 'mail.moveToJunk')} onClick={() => void runRemoving(() => actOnMessages(ids, inSpamFolder ? 'notSpam' : 'spam'))}>
        <ArchiveX className="h-4 w-4" />
      </Button>
      <Button variant="ghost" size="icon" className={iconClass} disabled={busy} title={t(locale, 'mail.moveToTrash')} aria-label={t(locale, 'mail.moveToTrash')} onClick={() => {
        void (async () => {
          if (!(await confirmMoveToTrash(locale, ids.length))) return;
          await runRemoving(() => actOnMessages(ids, 'trash'));
        })();
      }}>
        <Trash2 className="h-4 w-4" />
      </Button>
      <Button variant="ghost" size="icon" className={iconClass} disabled={busy} title={t(locale, anyUnread ? 'mail.markRead' : 'mail.markUnread')} aria-label={t(locale, anyUnread ? 'mail.markRead' : 'mail.markUnread')} onClick={() => {
        if (busy) return;
        setBusy(true);
        void patchMessages(ids, { isRead: anyUnread }).then((res) => {
          setBusy(false);
          if (res.error) onError(res.error);
        });
      }}>
        {anyUnread ? <MailOpen className="h-4 w-4" /> : <Mail className="h-4 w-4" />}
      </Button>
      <Button variant="ghost" size="icon" className={iconClass} disabled={busy} title={t(locale, anyUnstarred ? 'mail.star' : 'mail.unstar')} aria-label={t(locale, anyUnstarred ? 'mail.star' : 'mail.unstar')} onClick={() => {
        if (busy) return;
        setBusy(true);
        void patchMessages(ids, { isStarred: anyUnstarred }).then((res) => {
          setBusy(false);
          if (res.error) onError(res.error);
        });
      }}>
        {anyUnstarred ? <Star className="h-4 w-4" /> : <StarOff className="h-4 w-4" />}
      </Button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon" className={iconClass} disabled={busy} title={t(locale, 'mail.moveToFolder')} aria-label={t(locale, 'mail.moveToFolder')}>
            <FolderInput className="h-4 w-4" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent className="max-h-64 w-56 overflow-y-auto">
          {moveEntries.length === 0 ? (
            <DropdownMenuLabel>{t(locale, 'mail.noFolders')}</DropdownMenuLabel>
          ) : (
            moveEntries.map((f) => (
              <DropdownMenuItem key={f.id} onSelect={() => moveTo(f.id)} style={{ paddingLeft: `${0.5 + f.depth * 0.75}rem` }}>
                <span className="truncate">{folderLabel(f, locale)}</span>
              </DropdownMenuItem>
            ))
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      <div className="mx-1 h-4 w-px bg-border/60" />
      <Button variant="ghost" size="icon" className={iconClass} disabled={busy} title={t(locale, 'mail.clearSelection')} aria-label={t(locale, 'mail.clearSelection')} onClick={() => {
        const anchor = useUIStore.getState().selectionAnchorKey;
        if (anchor) {
          const keep = convos.find((c) => c.key === anchor);
          if (keep) {
            const target = keep.messages.find((m) => !m.isRead) ?? keep.latest;
            useUIStore.getState().applyConversationSelection({ keys: [anchor], anchor, focus: anchor }, target.id);
            return;
          }
        }
        clearConversationSelection();
      }}>
        <X className="h-4 w-4" />
      </Button>
      {moveProgress ? (
        <span className="ml-auto text-[11px] tabular-nums text-muted-foreground">
          {t(locale, 'mail.movingMessages', { done: moveProgress.done, total: moveProgress.total })}
        </span>
      ) : null}
    </div>
  );
}
```

Notes for the implementer:
- The pager aria-label keys `mail.prevConversation` / `mail.nextConversation` already exist in both locale files (en.json lines 59-60).
- `frontend/src/components/ui/dropdown-menu.tsx` exists; use it as written.

- [ ] **Step 2: Branch in MailDisplay**

In `frontend/src/components/mail/mail-display.tsx`:

1. Add selectors near the existing ones (~line 88):

```ts
  const selectedConversationKeys = useUIStore((s) => s.selectedConversationKeys);
  const selectionAnchorKey = useUIStore((s) => s.selectionAnchorKey);
```

and derive, after `viewConversations` is defined (~line 142):

```ts
  const multi = selectedConversationKeys.length > 1;
  const selectedConvos = useMemo(
    () => viewConversations.filter((c) => selectedConversationKeys.includes(c.key)),
    [viewConversations, selectedConversationKeys],
  );
  /** Pager position within the selection while multi-selecting. */
  const multiPosition = useMemo(() => {
    const idx = selectedConvos.findIndex((c) => c.key === selectionAnchorKey);
    return { index: Math.max(0, idx), total: selectedConvos.length };
  }, [selectedConvos, selectionAnchorKey]);
```

2. Extend `stepConversation` (~line 150) to cycle within the selection:

```ts
  const stepConversation = (delta: 1 | -1) => {
    if (multi) {
      const next = selectedConvos[multiPosition.index + delta];
      if (!next) return;
      const target = next.messages.find((m) => !m.isRead) ?? next.latest;
      useUIStore.getState().applyConversationSelection(
        {
          keys: selectedConversationKeys,
          anchor: next.key,
          focus: next.key,
        },
        target.id,
      );
      return;
    }
    const next = viewConversations[convoPosition.index + delta];
    if (!next) return;
    const target = next.messages.find((m) => !m.isRead) ?? next.latest;
    setSelectedMessage(target.id);
  };
```

3. Replace the toolbar branch: change `{showToolbar ? (` (~line 462) to render the bulk bar when multi:

```tsx
      {multi && mail ? (
        <BulkActionBar
          convos={selectedConvos}
          position={multiPosition}
          onStep={stepConversation}
          onError={setActionError}
        />
      ) : showToolbar ? (
        <>
          ...existing toolbar JSX unchanged...
        </>
      ) : null}
```

(The existing branch ends with `) : null}`; keep that structure.)

4. Wrap the conversation content with the stack frame. Just before the `return (`, add:

```tsx
  const wrapContent = (node: React.ReactNode) =>
    multi && mail ? (
      <SelectionStackFrame count={selectedConvos.length}>{node}</SelectionStackFrame>
    ) : (
      node
    );
```

Then change the content branch `{mail ? (` (~line 900s) to `{wrapContent(mail ? (` and its closing `)}` (right before the `: (` EmptyState) to `))}`. Imports: add

```ts
import { BulkActionBar, SelectionStackFrame } from '@/components/mail/multi-select-stack';
```

and `import type { ReactNode } ...` only if `React.ReactNode` isn't already available (mail-display imports React types — check the header).

- [ ] **Step 3: Typecheck + tests + lint**

Run: `cd frontend && npx tsc --noEmit && npm test 2>&1 | tail -5 && npx oxlint src/components/mail 2>&1 | tail -5`
Expected: clean (7 pre-existing oxlint warnings elsewhere are the baseline; add none).

- [ ] **Step 4: Commit**

```bash
git add frontend/src/components/mail/multi-select-stack.tsx frontend/src/components/mail/mail-display.tsx
git commit -m "feat(mail): card-stack reading pane and bulk action bar for multi-select"
```

---

### Task 9: Shortcut help + final gates

**Files:**
- Modify: `frontend/src/components/shortcut-help.tsx`

- [ ] **Step 1: Add the new rows**

In `frontend/src/components/shortcut-help.tsx`, extend `rows` (~line 24):

```ts
  const rows: Array<[string, string]> = [
    [`${mod} K`, t(locale, 'palette.action.open')],
    ['/', t(locale, 'shortcuts.search')],
    ['C', t(locale, 'shortcuts.compose')],
    ['J / K', t(locale, 'shortcuts.nextPrev')],
    ['O / Enter', t(locale, 'shortcuts.open')],
    ['U / Esc', t(locale, 'shortcuts.back')],
    [`${mod} A`, t(locale, 'shortcuts.selectAll')],
    [`⇧ ↑ / ↓`, t(locale, 'shortcuts.extendSelection')],
    ['?', t(locale, 'shortcuts.help')],
  ];
```

- [ ] **Step 2: Full verification**

Run from the worktree root:

```bash
cd frontend && npm test 2>&1 | tail -5
npm run check 2>&1 | tail -10
cd .. && make fmt
git status --short
```

Expected: all tests pass; `npm run check` clean (tsc + oxlint, only the 7 known baseline warnings); prettier formats any drift; nothing unexpected in git status after fmt (commit fmt changes if any).

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(mail): document multi-select shortcuts in the help dialog"
```

---

### Task 10: Manual smoke test (dev server)

- [ ] **Step 1: Rebuild and restart the local stack**

From the **main repo** (not the worktree) after merge, or from the worktree for a pre-merge check:

```bash
cd /Users/huangcheng/Projects/Lyra/.worktrees/mail-multi-select
docker compose build --build-arg HTTP_PROXY=http://host.docker.internal:7897 --build-arg HTTPS_PROXY=http://host.docker.internal:7897 --build-arg ALL_PROXY=socks5://host.docker.internal:7897 lyra && docker compose up -d lyra
```

- [ ] **Step 2: Exercise the flows at http://127.0.0.1:3000**

- cmd/click two conversations → both highlighted, stack + bulk bar appear, count correct
- shift-click a third → contiguous range selected
- ‹ › pager cycles the front card
- Archive from the bulk bar → conversations leave the list, selection clears
- ⌘A selects all visible; Esc collapses to the anchor; Esc again clears
- right-click a selected row → menu acts on all; right-click an unselected row → selection collapses
- drag a selected row onto a folder → all selected move, progress pill shows
- single-click a row → back to normal single-select reading

---

## Self-Review Notes

- Spec coverage: state (Task 2), pure logic (Task 1), mouse (Task 5), context menu (Task 6), dnd (Task 7), keyboard (Tasks 3+5), reading pane stack + bulk bar (Task 8), i18n (Task 4), shortcut help (Task 9), pruning (Task 5 Step 4), clearing after removing actions (Tasks 6/7/8), non-persistence (no persist-view-state change — deliberately untouched).
- Spec deviation (intentional): the spec put anchor→`selectedMessageId` sync inside the UI store; the plan does it in callers (`commitSelection`) so `stores/ui.ts` stays free of conversation data and store-to-store imports.
- Type consistency: `ConversationSelection { keys, anchor, focus }` is the single shape threaded through Tasks 1, 2, 3, 5; `ConversationDragData.selectionDrag` is the only new payload field; `multiConvos` / `dragConvos` props share the "undefined unless the row is in a multi-selection" contract.
