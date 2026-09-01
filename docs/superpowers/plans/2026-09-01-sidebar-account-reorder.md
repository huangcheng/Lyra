# Sidebar Account Reordering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users drag account sections in the mail sidebar's ACCOUNTS area into a custom order, persisted server-side in the existing `uiState` blob so it restores across sessions and devices.

**Architecture:** Frontend-only. A pure `orderAccounts` helper maps the mail store's `accounts` array + a persisted `accountOrder: string[]` (in `useUIStore`) to display order; both `SidebarFolders` and `AccountSwitcher` consume it. Drag-and-drop via `@dnd-kit` on account header rows in the unified view writes the new order back through the existing debounced `PATCH /api/v1/auth/preferences { uiState }` seam. No backend changes.

**Tech Stack:** React 19, Zustand 5, vitest 4, `@dnd-kit/core` + `@dnd-kit/sortable` + `@dnd-kit/utilities` (new dependencies).

**Spec:** `docs/superpowers/specs/2026-09-01-sidebar-account-reorder-design.md`

**Key existing files (read before editing):**
- `frontend/src/components/mail/sidebar-folders.tsx` — unified rows + `AccountSection` per account
- `frontend/src/components/mail/account-switcher.tsx` — account dropdown
- `frontend/src/stores/ui.ts` — UI chrome store (`folderExpansion` pattern to mirror)
- `frontend/src/lib/persist-view-state.ts` — `applyViewState` / `startViewStatePersistence`
- `frontend/src/types/index.ts` — `MailAccount` (line 94)
- Restore path already wired: `frontend/src/lib/session.ts:29` calls `applyViewState(me.ui_state)`; `frontend/src/main.tsx:19` calls `startViewStatePersistence()`.

**Verification commands:**
- `cd frontend && npm test` — vitest
- `cd frontend && npm run check` — oxlint + tsc + format
- `make check` from repo root before the final commit

---

### Task 1: Add @dnd-kit dependencies

**Files:**
- Modify: `frontend/package.json` (via npm)

- [ ] **Step 1: Install the packages**

Run:

```bash
cd frontend && npm install @dnd-kit/core @dnd-kit/sortable @dnd-kit/utilities
```

Expected: three new entries in `frontend/package.json` `dependencies`; `package-lock.json` updated.

- [ ] **Step 2: Verify the toolchain still passes**

Run: `cd frontend && npm run check`
Expected: PASS (no type or lint regressions from the install).

- [ ] **Step 3: Commit**

```bash
git add frontend/package.json frontend/package-lock.json
git commit -m "feat(frontend): add @dnd-kit for sortable sidebar accounts"
```

---

### Task 2: Pure ordering helpers (`orderAccounts`, `moveId`)

**Files:**
- Create: `frontend/src/lib/account-order.ts`
- Test: `frontend/src/lib/account-order.test.ts`

- [ ] **Step 1: Write the failing test**

Create `frontend/src/lib/account-order.test.ts`:

```ts
import { describe, expect, it } from 'vitest';

import { moveId, orderAccounts } from '@/lib/account-order';
import type { MailAccount } from '@/types';

function account(id: string): MailAccount {
  return {
    id,
    displayName: id,
    emailAddress: `${id}@example.com`,
    protocol: 'imap',
    isActive: true,
    syncEnabled: true,
  };
}

describe('orderAccounts', () => {
  it('returns server order when accountOrder is empty', () => {
    const accounts = [account('a'), account('b'), account('c')];
    expect(orderAccounts(accounts, []).map((a) => a.id)).toEqual(['a', 'b', 'c']);
  });

  it('honors the persisted id order', () => {
    const accounts = [account('a'), account('b'), account('c')];
    expect(orderAccounts(accounts, ['c', 'a', 'b']).map((a) => a.id)).toEqual(['c', 'a', 'b']);
  });

  it('appends accounts missing from accountOrder in server order', () => {
    const accounts = [account('a'), account('b'), account('c')];
    expect(orderAccounts(accounts, ['b']).map((a) => a.id)).toEqual(['b', 'a', 'c']);
  });

  it('ignores stale ids that match no account', () => {
    const accounts = [account('a'), account('b')];
    expect(orderAccounts(accounts, ['ghost', 'b', 'a']).map((a) => a.id)).toEqual(['b', 'a']);
  });

  it('does not mutate the input array', () => {
    const accounts = [account('a'), account('b')];
    orderAccounts(accounts, ['b', 'a']);
    expect(accounts.map((a) => a.id)).toEqual(['a', 'b']);
  });
});

describe('moveId', () => {
  it('moves an entry to a later position', () => {
    expect(moveId(['a', 'b', 'c', 'd'], 'a', 'c')).toEqual(['b', 'c', 'a', 'd']);
  });

  it('moves an entry to an earlier position', () => {
    expect(moveId(['a', 'b', 'c', 'd'], 'd', 'b')).toEqual(['a', 'd', 'b', 'c']);
  });

  it('moves to the top and to the end', () => {
    expect(moveId(['a', 'b', 'c'], 'c', 'a')).toEqual(['c', 'a', 'b']);
    expect(moveId(['a', 'b', 'c'], 'a', 'c')).toEqual(['b', 'c', 'a']);
  });

  it('is a no-op for identical or unknown ids', () => {
    expect(moveId(['a', 'b'], 'a', 'a')).toEqual(['a', 'b']);
    expect(moveId(['a', 'b'], 'x', 'a')).toEqual(['a', 'b']);
    expect(moveId(['a', 'b'], 'a', 'x')).toEqual(['a', 'b']);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/account-order.test.ts`
Expected: FAIL — module `@/lib/account-order` not found.

- [ ] **Step 3: Write the implementation**

Create `frontend/src/lib/account-order.ts`:

```ts
/**
 * Custom display order for sidebar accounts.
 *
 * `accountOrder` is the user-persisted id list (uiState blob); accounts not
 * listed (e.g. newly added) keep their server order at the end. Stale ids
 * (deleted accounts) never match and are effectively dropped.
 */

import type { MailAccount } from '@/types';

export function orderAccounts(accounts: MailAccount[], accountOrder: string[]): MailAccount[] {
  if (accountOrder.length === 0) return accounts;
  const rank = new Map(accountOrder.map((id, index) => [id, index]));
  // Array.prototype.sort is stable: unranked accounts keep server order.
  return [...accounts].sort((a, b) => {
    const ra = rank.get(a.id);
    const rb = rank.get(b.id);
    if (ra === undefined && rb === undefined) return 0;
    if (ra === undefined) return 1;
    if (rb === undefined) return -1;
    return ra - rb;
  });
}

/** Move `activeId` onto `overId`'s position within the rendered id order. */
export function moveId(ids: string[], activeId: string, overId: string): string[] {
  const from = ids.indexOf(activeId);
  const to = ids.indexOf(overId);
  if (from === -1 || to === -1 || from === to) return ids;
  const next = [...ids];
  next.splice(from, 1);
  next.splice(to, 0, activeId);
  return next;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/account-order.test.ts`
Expected: PASS (all 9 tests).

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/account-order.ts frontend/src/lib/account-order.test.ts
git commit -m "feat(frontend): account ordering helpers with tests"
```

---

### Task 3: `accountOrder` in the UI store

**Files:**
- Modify: `frontend/src/stores/ui.ts`
- Test: `frontend/src/stores/ui.test.ts` (create)

- [ ] **Step 1: Write the failing test**

Create `frontend/src/stores/ui.test.ts`:

```ts
import { describe, expect, it } from 'vitest';

import { useUIStore } from '@/stores/ui';

describe('accountOrder', () => {
  it('defaults to empty and setAccountOrder replaces it', () => {
    expect(useUIStore.getState().accountOrder).toEqual([]);
    useUIStore.getState().setAccountOrder(['b', 'a']);
    expect(useUIStore.getState().accountOrder).toEqual(['b', 'a']);
    useUIStore.getState().setAccountOrder([]);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/stores/ui.test.ts`
Expected: FAIL — `accountOrder` is undefined / `setAccountOrder` is not a function.

- [ ] **Step 3: Add state + mutation to the store**

In `frontend/src/stores/ui.ts`, in the `UIState` interface, add next to the `folderExpansion` entries:

```ts
  /** Custom sidebar account order (account ids; persisted server-side). */
  accountOrder: string[];
```

and to the mutations list (next to `setFolderExpansion`):

```ts
  setAccountOrder: (ids: string[]) => void;
```

In the `create<UIState>` initializer, add next to `folderExpansion: {}`:

```ts
  accountOrder: [],
```

and next to the `setFolderExpansion` implementation:

```ts
  setAccountOrder: (ids) => set({ accountOrder: ids }),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/stores/ui.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/stores/ui.ts frontend/src/stores/ui.test.ts
git commit -m "feat(frontend): accountOrder state in UI store"
```

---

### Task 4: Persist `accountOrder` in the uiState blob

**Files:**
- Modify: `frontend/src/lib/persist-view-state.ts`
- Test: `frontend/src/lib/persist-view-state.test.ts` (create)

- [ ] **Step 1: Write the failing test**

Create `frontend/src/lib/persist-view-state.test.ts`:

```ts
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', () => ({ api: vi.fn().mockResolvedValue({}) }));

import { api } from '@/lib/api-client';
import { applyViewState, startViewStatePersistence } from '@/lib/persist-view-state';
import { useAuthStore } from '@/stores/auth';
import { useUIStore } from '@/stores/ui';

const mockedApi = vi.mocked(api);

beforeEach(() => {
  vi.useFakeTimers();
  useAuthStore.getState().setToken('test-token');
});

afterEach(() => {
  vi.useRealTimers();
  useAuthStore.getState().clearSession();
  useUIStore.getState().setAccountOrder([]);
});

describe('applyViewState accountOrder', () => {
  it('restores a valid accountOrder array', () => {
    applyViewState({ accountOrder: ['b', 'a'] });
    expect(useUIStore.getState().accountOrder).toEqual(['b', 'a']);
  });

  it('drops non-string entries', () => {
    applyViewState({ accountOrder: ['b', 42, 'a', null] });
    expect(useUIStore.getState().accountOrder).toEqual(['b', 'a']);
  });

  it('ignores a malformed accountOrder', () => {
    useUIStore.getState().setAccountOrder(['x']);
    applyViewState({ accountOrder: 'not-an-array' });
    expect(useUIStore.getState().accountOrder).toEqual(['x']);
  });
});

describe('startViewStatePersistence accountOrder', () => {
  it('includes accountOrder in the PATCH payload when it changes', async () => {
    const stop = startViewStatePersistence();
    useUIStore.getState().setAccountOrder(['b', 'a']);
    await vi.advanceTimersByTimeAsync(500);
    stop();
    expect(mockedApi).toHaveBeenCalledWith('/auth/preferences', {
      method: 'PATCH',
      body: expect.stringContaining('"accountOrder":["b","a"]'),
    });
  });

  it('does not PATCH when only unrelated state changes', async () => {
    mockedApi.mockClear();
    const stop = startViewStatePersistence();
    useUIStore.getState().setSearchQuery('hello');
    await vi.advanceTimersByTimeAsync(500);
    stop();
    expect(mockedApi).not.toHaveBeenCalled();
    useUIStore.getState().setSearchQuery('');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/persist-view-state.test.ts`
Expected: FAIL — `accountOrder` never appears in payload / is not restored.

- [ ] **Step 3: Implement restore + save**

In `frontend/src/lib/persist-view-state.ts`:

(a) In `applyViewState`, after the `folderExpansion` block (before the closing brace of the function), add:

```ts
  if (Array.isArray(uiState.accountOrder)) {
    ui.setAccountOrder(uiState.accountOrder.filter((x): x is string => typeof x === 'string'));
  }
```

(b) In `startViewStatePersistence`, extend the early-return comparison:

```ts
    if (
      state.selectedAccountId === prev.selectedAccountId &&
      state.selectedFolderId === prev.selectedFolderId &&
      state.selectedFolderRole === prev.selectedFolderRole &&
      state.folderExpansion === prev.folderExpansion &&
      state.accountOrder === prev.accountOrder
    ) {
      return;
    }
```

(c) Extend the PATCH body:

```ts
        body: JSON.stringify({
          uiState: {
            selectedAccountId: s.selectedAccountId,
            selectedFolderId: s.selectedFolderId,
            selectedFolderRole: s.selectedFolderRole,
            folderExpansion: s.folderExpansion,
            accountOrder: s.accountOrder,
          },
        }),
```

Also update the file header comment to mention account order:

```ts
 * Persist mail view-state (selected account/folder, sidebar folder
 * expansion, sidebar account order) to the server so the sidebar restores
 * identically after a reload — and on any other device.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/persist-view-state.test.ts`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/persist-view-state.ts frontend/src/lib/persist-view-state.test.ts
git commit -m "feat(frontend): persist sidebar account order in uiState blob"
```

---

### Task 5: Apply the order in `AccountSwitcher` and `SidebarFolders`

**Files:**
- Modify: `frontend/src/components/mail/account-switcher.tsx`
- Modify: `frontend/src/components/mail/sidebar-folders.tsx`

No new unit tests here — both components become thin consumers of the already-tested `orderAccounts`. Verified by typecheck/lint and the Task 7 manual smoke test.

- [ ] **Step 1: Order the switcher dropdown**

In `frontend/src/components/mail/account-switcher.tsx`:

Add the import:

```ts
import { orderAccounts } from '@/lib/account-order';
```

In `AccountSwitcher`, after the existing `const accounts = useMailStore((s) => s.accounts);` line, add:

```ts
  const accountOrder = useUIStore((s) => s.accountOrder);
```

Change the `options` array's account spread from `...accounts.map(...)` to:

```ts
    ...orderAccounts(accounts, accountOrder).map((account) => ({
```

- [ ] **Step 2: Order the sidebar ACCOUNTS section (render order only for now)**

In `frontend/src/components/mail/sidebar-folders.tsx`:

Add the import:

```ts
import { orderAccounts } from '@/lib/account-order';
```

In `SidebarFolders`, after `const accounts = useMailStore((s) => s.accounts);`, add:

```ts
  const accountOrder = useUIStore((s) => s.accountOrder);
  const orderedAccounts = orderAccounts(accounts, accountOrder);
```

In the ACCOUNTS section at the bottom of `SidebarFolders`, change `accounts.map((account) =>` to `orderedAccounts.map((account) =>`.

Note: the single-account early return (`accounts.find((a) => a.id === selectedAccountId)`) stays on the raw array — ordering is irrelevant for a lookup.

- [ ] **Step 3: Verify types and existing tests**

Run: `cd frontend && npm run check && npm test`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/components/mail/account-switcher.tsx frontend/src/components/mail/sidebar-folders.tsx
git commit -m "feat(frontend): render accounts in persisted custom order"
```

---

### Task 6: Drag-and-drop on account sections

**Files:**
- Modify: `frontend/src/components/mail/sidebar-folders.tsx`

- [ ] **Step 1: Make `AccountSection` accept a drag handle**

In `frontend/src/components/mail/sidebar-folders.tsx`, add to the imports at the top:

```ts
import type { HTMLAttributes } from 'react';
```

Change the `AccountSection` signature from:

```ts
function AccountSection({
  account,
  selectedFolderId,
  bare = false,
}: {
  account: MailAccount;
  selectedFolderId: string | null;
  /** Single-account view: header omitted (the switcher already names the account). */
  bare?: boolean;
}) {
```

to:

```ts
function AccountSection({
  account,
  selectedFolderId,
  bare = false,
  dragHandleProps,
}: {
  account: MailAccount;
  selectedFolderId: string | null;
  /** Single-account view: header omitted (the switcher already names the account). */
  bare?: boolean;
  /** dnd-kit listeners/attributes for the account header (unified view only). */
  dragHandleProps?: HTMLAttributes<HTMLButtonElement>;
}) {
```

Spread the props onto the account header button (the one with `onClick={() => setAccountExpanded(...)}`):

```tsx
        <button
          type="button"
          onClick={() => setAccountExpanded(account.id, !expanded)}
          aria-expanded={expanded}
          {...dragHandleProps}
          className="flex h-8 w-full items-center gap-1.5 rounded-[7px] px-2.5 hover:bg-accent/60"
        >
```

- [ ] **Step 2: Add the sortable wrapper component**

In `frontend/src/components/mail/sidebar-folders.tsx`, add the dnd-kit imports:

```ts
import {
  DndContext,
  PointerSensor,
  closestCenter,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import { SortableContext, useSortable, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
```

Add `moveId` to the `@/lib/account-order` import from Task 5:

```ts
import { moveId, orderAccounts } from '@/lib/account-order';
```

Add these two components after `AccountSection`:

```tsx
function SortableAccountSection({
  account,
  selectedFolderId,
}: {
  account: MailAccount;
  selectedFolderId: string | null;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: account.id,
  });
  return (
    <div
      ref={setNodeRef}
      style={{
        transform: CSS.Transform.toString(transform),
        transition,
        opacity: isDragging ? 0.6 : undefined,
      }}
    >
      <AccountSection
        account={account}
        selectedFolderId={selectedFolderId}
        dragHandleProps={{ ...attributes, ...listeners }}
      />
    </div>
  );
}

/** ACCOUNTS section with drag-to-reorder; drop persists via the UI store. */
function SortableAccountSections({
  accounts,
  selectedFolderId,
}: {
  accounts: MailAccount[];
  selectedFolderId: string | null;
}) {
  const accountOrder = useUIStore((s) => s.accountOrder);
  const setAccountOrder = useUIStore((s) => s.setAccountOrder);
  const ordered = orderAccounts(accounts, accountOrder);
  const ids = ordered.map((a) => a.id);
  // 4px movement threshold: plain clicks still toggle expand/collapse.
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 4 } }));

  const onDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    setAccountOrder(moveId(ids, String(active.id), String(over.id)));
  };

  return (
    <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onDragEnd}>
      <SortableContext items={ids} strategy={verticalListSortingStrategy}>
        <div className="grid gap-0.5">
          {ordered.map((account) => (
            <SortableAccountSection
              key={account.id}
              account={account}
              selectedFolderId={selectedFolderId}
            />
          ))}
        </div>
      </SortableContext>
    </DndContext>
  );
}
```

- [ ] **Step 3: Use the sortable section in `SidebarFolders`**

In `SidebarFolders`, replace the final ACCOUNTS block:

```tsx
      <SectionLabel>{t(locale, 'mail.section.accounts')}</SectionLabel>
      <div className="grid gap-0.5">
        {orderedAccounts.map((account) => (
          <AccountSection key={account.id} account={account} selectedFolderId={selectedFolderId} />
        ))}
      </div>
```

with:

```tsx
      <SectionLabel>{t(locale, 'mail.section.accounts')}</SectionLabel>
      <SortableAccountSections accounts={accounts} selectedFolderId={selectedFolderId} />
```

(`orderedAccounts` from Task 5 Step 2 is no longer used in `SidebarFolders` — remove that local and keep only the `accountOrder` selector if still referenced; otherwise remove both to keep oxlint clean. The sortable component computes the order itself.)

- [ ] **Step 4: Verify types, lint, and tests**

Run: `cd frontend && npm run check && npm test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/components/mail/sidebar-folders.tsx
git commit -m "feat(frontend): drag to reorder sidebar accounts"
```

---

### Task 7: Full verification

- [ ] **Step 1: Format everything**

Run: `make fmt`
Expected: exits 0; commit any formatting deltas if files changed.

- [ ] **Step 2: Full check**

Run: `make check`
Expected: format check, oxlint, tsc, clippy, vitest, cargo tests, secret scan — all PASS.

- [ ] **Step 3: Manual smoke test**

Run: `make dev` (or the repo's usual dev command), sign in, and verify:

1. ACCOUNTS sections render in server order initially.
2. Drag an account header to a new position → sections reorder; siblings slide during drag.
3. Plain click on an account header still expands/collapses it.
4. Reload the page → custom order survives.
5. Account switcher dropdown shows the same custom order.
6. Folders within accounts are unaffected (not reorderable).

- [ ] **Step 4: Final commit (only if formatting produced changes)**

```bash
git add -A
git commit -m "chore: format after account reordering feature"
```

---

## Self-review notes

- Spec coverage: ordering model → Tasks 2/3/5; drag interaction → Task 6; persistence → Task 4; edge cases (new/deleted/malformed/cap/collapsed) → handled by `orderAccounts` semantics + `applyViewState` validation, tested in Tasks 2/4; testing requirements → Tasks 2–4 unit tests + Task 7 smoke test.
- Out-of-scope items (folder reordering, unified mailbox reordering, server write-back, settings list) have no tasks — intentionally.
- Type consistency: `orderAccounts(accounts, accountOrder)`, `moveId(ids, activeId, overId)`, `setAccountOrder(ids: string[])`, `dragHandleProps?: HTMLAttributes<HTMLButtonElement>` are used identically across all tasks.
