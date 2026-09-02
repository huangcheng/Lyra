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
