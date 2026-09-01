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
