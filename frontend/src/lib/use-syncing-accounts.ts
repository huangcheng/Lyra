/**
 * Tracks which accounts currently have a sync running, from the SSE stream.
 * Replaces boolean "any syncing" flags that went false as soon as *one*
 * account finished while another was still running.
 */

import { useEffect, useState } from 'react';

import { syncEvents$ } from '@/rxjs/sync-events';
import type { SyncEvent } from '@/types';

/** Pure reducer: apply one sync event to the in-flight account id set. */
export function reduceSyncEvent(active: ReadonlySet<string>, ev: SyncEvent): Set<string> {
  const next = new Set(active);
  if (ev.type === 'sync_started') next.add(ev.accountId);
  if (ev.type === 'sync_complete' || ev.type === 'sync_error') next.delete(ev.accountId);
  return next;
}

/** Ids of accounts currently syncing (empty set = idle). */
export function useSyncingAccounts(): ReadonlySet<string> {
  const [active, setActive] = useState<ReadonlySet<string>>(new Set());
  useEffect(() => {
    const sub = syncEvents$.subscribe((ev) => setActive((prev) => reduceSyncEvent(prev, ev)));
    return () => sub.unsubscribe();
  }, []);
  return active;
}
