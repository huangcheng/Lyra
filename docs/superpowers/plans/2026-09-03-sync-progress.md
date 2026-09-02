# Sync Progress Details Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show per-account sync progress — a popover with live folder/fetched/total/error per account, plus a spinner on the syncing account's sidebar header.

**Architecture:** Pure reducer + React hook over the existing SSE `syncEvents$` stream (`frontend/src/lib/sync-progress.ts`), consumed by a new `SyncStatusPopover` in the sidebar footer and by `AccountSection` in `sidebar-folders.tsx`. Frontend-only; no backend changes.

**Tech Stack:** React, Zustand stores, RxJS (`syncEvents$`), shadcn Popover, date-fns, vitest.

Spec: `docs/superpowers/specs/2026-09-02-sync-progress-design.md`

---

### Task 1: `reduceSyncProgress` + `useSyncProgress` in `src/lib/sync-progress.ts`

**Files:**
- Create: `frontend/src/lib/sync-progress.ts`
- Test: `frontend/src/lib/sync-progress.test.ts`

- [ ] **Step 1: Write the failing test**

Create `frontend/src/lib/sync-progress.test.ts`:

```ts
import { describe, expect, it } from 'vitest';

import { reduceSyncProgress, type SyncProgressMap } from '@/lib/sync-progress';
import type { SyncEvent } from '@/types';

const started: SyncEvent = { type: 'sync_started', accountId: 'a1' };

describe('reduceSyncProgress', () => {
  it('sync_started adds a zeroed syncing entry', () => {
    const next = reduceSyncProgress(new Map(), started);
    expect(next.get('a1')).toEqual({
      state: 'syncing',
      currentFolderId: null,
      fetched: 0,
      total: 0,
      error: null,
    });
  });

  it('folder_progress updates folder and counters', () => {
    const withStart = reduceSyncProgress(new Map(), started);
    const next = reduceSyncProgress(withStart, {
      type: 'folder_progress',
      accountId: 'a1',
      folderId: 'f1',
      fetched: 120,
      total: 500,
    });
    expect(next.get('a1')).toMatchObject({
      state: 'syncing',
      currentFolderId: 'f1',
      fetched: 120,
      total: 500,
    });
  });

  it('folder_progress without a prior sync_started still creates a syncing entry', () => {
    const next = reduceSyncProgress(new Map(), {
      type: 'folder_progress',
      accountId: 'a1',
      folderId: 'f1',
      fetched: 3,
      total: 9,
    });
    expect(next.get('a1')).toMatchObject({ state: 'syncing', currentFolderId: 'f1' });
  });

  it('folder_complete clears the current folder but keeps syncing state', () => {
    let map: SyncProgressMap = reduceSyncProgress(new Map(), started);
    map = reduceSyncProgress(map, {
      type: 'folder_progress',
      accountId: 'a1',
      folderId: 'f1',
      fetched: 10,
      total: 10,
    });
    map = reduceSyncProgress(map, { type: 'folder_complete', accountId: 'a1', folderId: 'f1' });
    expect(map.get('a1')).toMatchObject({ state: 'syncing', currentFolderId: null });
  });

  it('incremental_complete clears the current folder', () => {
    let map: SyncProgressMap = reduceSyncProgress(new Map(), started);
    map = reduceSyncProgress(map, {
      type: 'folder_progress',
      accountId: 'a1',
      folderId: 'f1',
      fetched: 1,
      total: 2,
    });
    map = reduceSyncProgress(map, {
      type: 'incremental_complete',
      accountId: 'a1',
      folderId: 'f1',
      changes: 2,
    });
    expect(map.get('a1')?.currentFolderId).toBeNull();
  });

  it('sync_error sets error state, keeps counters, clears current folder', () => {
    let map: SyncProgressMap = reduceSyncProgress(new Map(), started);
    map = reduceSyncProgress(map, {
      type: 'folder_progress',
      accountId: 'a1',
      folderId: 'f1',
      fetched: 5,
      total: 50,
    });
    map = reduceSyncProgress(map, { type: 'sync_error', accountId: 'a1', error: 'boom' });
    expect(map.get('a1')).toEqual({
      state: 'error',
      currentFolderId: null,
      fetched: 5,
      total: 50,
      error: 'boom',
    });
  });

  it('sync_started after an error clears the error', () => {
    let map: SyncProgressMap = reduceSyncProgress(new Map(), {
      type: 'sync_error',
      accountId: 'a1',
      error: 'boom',
    });
    map = reduceSyncProgress(map, started);
    expect(map.get('a1')).toMatchObject({ state: 'syncing', error: null });
  });

  it('sync_complete removes the account (idle = absent)', () => {
    const map = reduceSyncProgress(new Map(), started);
    const next = reduceSyncProgress(map, { type: 'sync_complete', accountId: 'a1' });
    expect(next.has('a1')).toBe(false);
  });

  it('does not touch other accounts', () => {
    let map: SyncProgressMap = reduceSyncProgress(new Map(), started);
    map = reduceSyncProgress(map, { type: 'sync_started', accountId: 'a2' });
    map = reduceSyncProgress(map, { type: 'sync_error', accountId: 'a1', error: 'x' });
    expect(map.get('a2')).toMatchObject({ state: 'syncing', error: null });
  });

  it('does not mutate the input map', () => {
    const input = new Map();
    reduceSyncProgress(input, started);
    expect(input.size).toBe(0);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/sync-progress.test.ts`
Expected: FAIL — module `@/lib/sync-progress` does not exist.

- [ ] **Step 3: Write the implementation**

Create `frontend/src/lib/sync-progress.ts`:

```ts
/**
 * Per-account sync progress derived from the SSE sync event stream.
 * Pure reducer + hook; mirrors use-syncing-accounts.ts (no Zustand writes).
 * Idle is represented by the account being absent from the map.
 */

import { useEffect, useState } from 'react';

import { syncEvents$ } from '@/rxjs/sync-events';
import type { SyncEvent } from '@/types';

export interface AccountSyncStatus {
  state: 'syncing' | 'error';
  currentFolderId: string | null;
  fetched: number;
  total: number;
  error: string | null;
}

export type SyncProgressMap = ReadonlyMap<string, AccountSyncStatus>;

/** Pure reducer: apply one sync event to the per-account progress map. */
export function reduceSyncProgress(
  prev: SyncProgressMap,
  ev: SyncEvent,
): Map<string, AccountSyncStatus> {
  const next = new Map(prev);
  switch (ev.type) {
    case 'sync_started':
      next.set(ev.accountId, {
        state: 'syncing',
        currentFolderId: null,
        fetched: 0,
        total: 0,
        error: null,
      });
      break;
    case 'folder_progress':
      next.set(ev.accountId, {
        state: 'syncing',
        currentFolderId: ev.folderId,
        fetched: ev.fetched,
        total: ev.total,
        error: next.get(ev.accountId)?.error ?? null,
      });
      break;
    case 'folder_complete':
    case 'incremental_complete': {
      const existing = next.get(ev.accountId);
      if (existing) next.set(ev.accountId, { ...existing, currentFolderId: null });
      break;
    }
    case 'sync_error': {
      const existing = next.get(ev.accountId);
      next.set(ev.accountId, {
        state: 'error',
        currentFolderId: null,
        fetched: existing?.fetched ?? 0,
        total: existing?.total ?? 0,
        error: ev.error,
      });
      break;
    }
    case 'sync_complete':
      next.delete(ev.accountId);
      break;
  }
  return next;
}

/** Live per-account sync status (absent account = idle). */
export function useSyncProgress(): SyncProgressMap {
  const [progress, setProgress] = useState<SyncProgressMap>(new Map());
  useEffect(() => {
    const sub = syncEvents$.subscribe((ev) => setProgress((prev) => reduceSyncProgress(prev, ev)));
    return () => sub.unsubscribe();
  }, []);
  return progress;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/sync-progress.test.ts`
Expected: PASS — 10 tests.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/sync-progress.ts frontend/src/lib/sync-progress.test.ts
git commit -m "feat(frontend): per-account sync progress reducer + hook"
```

---

### Task 2: i18n strings (en + zh)

**Files:**
- Modify: `frontend/src/i18n/en.json` (inside the `"sync"` object, after line 194 `"lastSync"`)
- Modify: `frontend/src/i18n/zh.json` (same location)

- [ ] **Step 1: Add the keys**

In `frontend/src/i18n/en.json`, inside `"sync": { ... }` after `"lastSync": "Last synced"` add:

```json
    "lastSync": "Last synced",
    "details": "Sync details",
    "starting": "Starting…",
    "syncingFolder": "Syncing {{folder}} — {{fetched}}/{{total}}",
    "folderFallback": "folder",
    "notSyncedYet": "Not synced yet",
    "syncAccount": "Sync now"
```

In `frontend/src/i18n/zh.json`, same position:

```json
    "lastSync": "上次同步",
    "details": "同步详情",
    "starting": "正在启动…",
    "syncingFolder": "正在同步 {{folder}} — {{fetched}}/{{total}}",
    "folderFallback": "文件夹",
    "notSyncedYet": "尚未同步",
    "syncAccount": "立即同步"
```

(Keep the existing trailing commas/structure valid JSON — the existing `"lastSync"` line gains a comma.)

- [ ] **Step 2: Verify i18n tests pass**

Run: `cd frontend && npx vitest run src/i18n`
Expected: PASS (key-parity test between en/zh must stay green).

- [ ] **Step 3: Commit**

```bash
git add frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "feat(frontend): i18n strings for sync status popover"
```

---

### Task 3: `SyncStatusPopover` component

**Files:**
- Create: `frontend/src/components/mail/sync-status-popover.tsx`
- Modify: `frontend/src/components/mail/mail.tsx:94-101` (sidebar footer)

- [ ] **Step 1: Create the component**

Create `frontend/src/components/mail/sync-status-popover.tsx`:

```tsx
/**
 * Sync status popover for the sidebar footer.
 * Per-account live status from the SSE stream: current folder + fetched/total
 * while syncing, the error text on failure, lastSyncAt when idle.
 * Each idle/errored account row has a "Sync now" action.
 */

import { formatDistanceToNow } from 'date-fns';
import { ChevronUp, Loader2, RefreshCw } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import { useSyncProgress } from '@/lib/sync-progress';
import { cn } from '@/lib/utils';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

export function SyncStatusPopover() {
  const locale = useUIStore((s) => s.locale);
  const accounts = useMailStore((s) => s.accounts);
  const folders = useMailStore((s) => s.folders);
  const progress = useSyncProgress();
  const [requested, setRequested] = useState<ReadonlySet<string>>(new Set());

  const syncNow = async (accountId: string) => {
    setRequested((prev) => new Set(prev).add(accountId));
    try {
      await api(`/accounts/${accountId}/sync`, { method: 'POST' });
    } catch {
      // The SSE sync_error event surfaces the failure in this same popover.
    } finally {
      setRequested((prev) => {
        const next = new Set(prev);
        next.delete(accountId);
        return next;
      });
    }
  };

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-5 shrink-0"
          aria-label={t(locale, 'sync.details')}
        >
          <ChevronUp className="h-3.5 w-3.5" />
        </Button>
      </PopoverTrigger>
      <PopoverContent side="top" align="end" className="w-80 p-1.5">
        {accounts.map((account) => {
          const status = progress.get(account.id);
          const folderName = status?.currentFolderId
            ? folders[status.currentFolderId]?.name
            : undefined;
          return (
            <div key={account.id} className="flex items-center gap-2 rounded-md px-2 py-1.5">
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px] font-medium">
                  {account.displayName || account.emailAddress}
                </div>
                <div
                  className={cn(
                    'truncate text-[12px]',
                    status?.state === 'error' ? 'text-destructive' : 'text-muted-foreground',
                  )}
                >
                  {status?.state === 'syncing' ? (
                    status.currentFolderId ? (
                      t(locale, 'sync.syncingFolder', {
                        folder: folderName ?? t(locale, 'sync.folderFallback'),
                        fetched: status.fetched,
                        total: status.total,
                      })
                    ) : (
                      t(locale, 'sync.starting')
                    )
                  ) : status?.state === 'error' ? (
                    (status.error ?? t(locale, 'sync.syncFailed'))
                  ) : account.lastSyncAt ? (
                    `${t(locale, 'sync.lastSync')} ${formatDistanceToNow(new Date(account.lastSyncAt), { addSuffix: true })}`
                  ) : (
                    t(locale, 'sync.notSyncedYet')
                  )}
                </div>
              </div>
              {status?.state === 'syncing' ? (
                <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground" />
              ) : (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7 shrink-0"
                  disabled={requested.has(account.id)}
                  onClick={() => void syncNow(account.id)}
                  aria-label={t(locale, 'sync.syncAccount')}
                >
                  <RefreshCw
                    className={cn('h-3.5 w-3.5', requested.has(account.id) && 'animate-spin')}
                  />
                </Button>
              )}
            </div>
          );
        })}
      </PopoverContent>
    </Popover>
  );
}
```

- [ ] **Step 2: Render it in the sidebar footer**

In `frontend/src/components/mail/mail.tsx`, add the import (next to the `SyncAllButton` import at line 16):

```ts
import { SyncStatusPopover } from '@/components/mail/sync-status-popover';
```

and render it after `<SyncAllButton />` (line 99):

```tsx
          <SyncAllButton />
          <SyncStatusPopover />
```

- [ ] **Step 3: Typecheck + lint**

Run: `cd frontend && npm run check`
Expected: PASS (oxlint warnings in `ui/*`, `router.tsx`, `avatar.ts` are pre-existing and allowed).

- [ ] **Step 4: Commit**

```bash
git add frontend/src/components/mail/sync-status-popover.tsx frontend/src/components/mail/mail.tsx
git commit -m "feat(frontend): sync status popover in sidebar footer"
```

---

### Task 4: Spinner on the syncing account's sidebar header

**Files:**
- Modify: `frontend/src/components/mail/sidebar-folders.tsx` (`AccountSection`, header button ~lines 352-372)

- [ ] **Step 1: Add the spinner**

In `frontend/src/components/mail/sidebar-folders.tsx`:

1. Add `Loader2` to the `lucide-react` import block (lines 9-20, alphabetical between `Inbox` and `Send`):

```ts
import {
  Archive,
  ChevronDown,
  ChevronRight,
  File,
  Flag,
  Folder,
  Inbox,
  Loader2,
  Send,
  Trash2,
  type LucideIcon,
} from 'lucide-react';
```

2. Add the import with the other `@/lib` imports (after the `folder-tree` import, line 35):

```ts
import { useSyncProgress } from '@/lib/sync-progress';
```

3. Inside `AccountSection` (after the `expandedIds` line, ~line 339):

```ts
  const isSyncing = useSyncProgress().get(account.id)?.state === 'syncing';
```

4. In the header button, between the chevron and the name `<span>` (~line 364), render:

```tsx
            {isSyncing ? (
              <Loader2 className="size-3 shrink-0 animate-spin text-ter-foreground" />
            ) : null}
```

- [ ] **Step 2: Typecheck + lint**

Run: `cd frontend && npm run check`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add frontend/src/components/mail/sidebar-folders.tsx
git commit -m "feat(frontend): spinner on sidebar account header while syncing"
```

---

### Task 5: Full verification

- [ ] **Step 1: Full gate**

Run: `cd frontend && npm run check`
Expected: PASS — oxlint (only the 4 known pre-existing warnings), tsc clean, all vitest suites green (111 + 10 new = 121 tests).

- [ ] **Step 2: Live smoke test**

With the Vite dev server (`http://127.0.0.1:5173`, session `lyra-default-acct` tab): trigger "sync all", open the popover, confirm per-account progress lines update, and confirm the spinner appears on the syncing account's header. (Manual/agent-driven via WebBridge; skip if no live backend is reachable.)

- [ ] **Step 3: Done**

Report results. Do NOT push without explicit user approval.
