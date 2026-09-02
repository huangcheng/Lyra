import { describe, expect, it } from 'vitest';

import { reduceSyncEvent } from '@/lib/use-syncing-accounts';

describe('reduceSyncEvent', () => {
  it('adds on sync_started, removes on sync_complete', () => {
    let s = reduceSyncEvent(new Set(), { type: 'sync_started', accountId: 'a' });
    s = reduceSyncEvent(s, { type: 'sync_started', accountId: 'b' });
    expect([...s].sort()).toEqual(['a', 'b']);
    s = reduceSyncEvent(s, { type: 'sync_complete', accountId: 'a' });
    expect([...s]).toEqual(['b']);
  });

  it('removes on sync_error too', () => {
    const s = reduceSyncEvent(new Set(['a']), {
      type: 'sync_error',
      accountId: 'a',
      error: 'x',
    });
    expect(s.size).toBe(0);
  });

  it('does not mutate the input set', () => {
    const before = new Set(['a']);
    reduceSyncEvent(before, { type: 'sync_complete', accountId: 'a' });
    expect(before.has('a')).toBe(true);
  });
});
