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

  it('folder_progress clears a prior error', () => {
    let map: SyncProgressMap = reduceSyncProgress(new Map(), {
      type: 'sync_error',
      accountId: 'a1',
      error: 'boom',
    });
    map = reduceSyncProgress(map, {
      type: 'folder_progress',
      accountId: 'a1',
      folderId: 'f1',
      fetched: 1,
      total: 2,
    });
    expect(map.get('a1')).toMatchObject({ state: 'syncing', error: null });
  });

  it('does not mutate the input map', () => {
    const input = new Map();
    reduceSyncProgress(input, started);
    expect(input.size).toBe(0);
  });
});
