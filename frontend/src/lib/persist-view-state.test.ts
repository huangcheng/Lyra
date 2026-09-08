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
  // jsdom has no matchMedia; theme changes go through it.
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: false,
    media: query,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })) as unknown as typeof window.matchMedia;
});

afterEach(() => {
  vi.useRealTimers();
  useAuthStore.getState().clearSession();
  useUIStore.setState({ accountOrder: [], defaultAccountId: null });
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

describe('applyViewState defaultAccountId', () => {
  it('restores a valid string', () => {
    applyViewState({ defaultAccountId: 'acc-1' });
    expect(useUIStore.getState().defaultAccountId).toBe('acc-1');
  });

  it('ignores non-string values', () => {
    applyViewState({ defaultAccountId: 42 });
    expect(useUIStore.getState().defaultAccountId).toBeNull();
  });

  it('leaves the current value when the key is absent', () => {
    useUIStore.setState({ defaultAccountId: 'keep' });
    applyViewState({ accountOrder: ['x'] });
    expect(useUIStore.getState().defaultAccountId).toBe('keep');
  });
});

describe('applyViewState theme + notification prefs', () => {
  afterEach(() => {
    localStorage.removeItem('lyra.notifications');
    localStorage.removeItem('lyra_theme');
  });

  it('restores a valid theme', () => {
    applyViewState({ theme: 'dark' });
    expect(useUIStore.getState().theme).toBe('dark');
    expect(localStorage.getItem('lyra_theme')).toBe('dark');
  });

  it('ignores an unknown theme', () => {
    useUIStore.getState().setTheme('light');
    applyViewState({ theme: 'solarized' });
    expect(useUIStore.getState().theme).toBe('light');
  });

  it('restores notification prefs into localStorage', () => {
    applyViewState({
      notificationPrefs: { enabled: true, mutedFolderIds: ['f1'], mutedThreadIds: ['t1'] },
    });
    expect(JSON.parse(localStorage.getItem('lyra.notifications') ?? '{}')).toEqual({
      enabled: true,
      mutedFolderIds: ['f1'],
      mutedThreadIds: ['t1'],
    });
  });

  it('ignores a malformed notificationPrefs blob', () => {
    applyViewState({ notificationPrefs: 'not-an-object' });
    expect(localStorage.getItem('lyra.notifications')).toBeNull();
  });
});

describe('startViewStatePersistence accountOrder', () => {
  it('includes accountOrder in the PATCH payload when it changes', async () => {
    const stop = startViewStatePersistence();
    useUIStore.getState().setAccountOrder(['b', 'a']);
    await vi.advanceTimersByTimeAsync(500);
    stop();
    expect(mockedApi).toHaveBeenCalledWith(
      '/auth/preferences',
      expect.objectContaining({
        method: 'PATCH',
        body: expect.stringContaining('"accountOrder":["b","a"]'),
      }),
    );
  });

  it('includes defaultAccountId in the PATCH payload when it changes', async () => {
    const stop = startViewStatePersistence();
    useUIStore.getState().setDefaultAccount('acc-1');
    await vi.advanceTimersByTimeAsync(500);
    stop();
    expect(mockedApi).toHaveBeenCalledWith(
      '/auth/preferences',
      expect.objectContaining({
        method: 'PATCH',
        body: expect.stringContaining('"defaultAccountId":"acc-1"'),
      }),
    );
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

  it('includes theme in the PATCH payload when it changes', async () => {
    const stop = startViewStatePersistence();
    useUIStore.getState().setTheme('dark');
    await vi.advanceTimersByTimeAsync(500);
    stop();
    expect(mockedApi).toHaveBeenCalledWith(
      '/auth/preferences',
      expect.objectContaining({
        method: 'PATCH',
        body: expect.stringContaining('"theme":"dark"'),
      }),
    );
    useUIStore.getState().setTheme('system');
  });

  it('PATCHes when notification prefs change outside the UI store', async () => {
    mockedApi.mockClear();
    const stop = startViewStatePersistence();
    const { writeNotificationPrefs } = await import('@/lib/notifications');
    writeNotificationPrefs({ enabled: true, mutedFolderIds: [], mutedThreadIds: [] });
    await vi.advanceTimersByTimeAsync(500);
    stop();
    expect(mockedApi).toHaveBeenCalledWith(
      '/auth/preferences',
      expect.objectContaining({
        method: 'PATCH',
        body: expect.stringContaining('"notificationPrefs":{"enabled":true'),
      }),
    );
    localStorage.removeItem('lyra.notifications');
  });
});
