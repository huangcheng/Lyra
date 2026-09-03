import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { api } from '@/lib/api-client';
import {
  FOLDER_REFRESH_DEBOUNCE_MS,
  refreshFoldersNow,
  resetFolderRefreshForTests,
  scheduleFolderRefresh,
} from '@/lib/refresh-folders';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';

vi.mock('@/lib/api-client', () => ({
  api: vi.fn(),
}));

describe('refresh-folders', () => {
  beforeEach(() => {
    resetFolderRefreshForTests();
    vi.useFakeTimers();
    useAuthStore.setState({ token: 't' });
    useMailStore.setState({ folders: {} });
    vi.mocked(api).mockReset();
  });

  afterEach(() => {
    resetFolderRefreshForTests();
    vi.useRealTimers();
  });

  it('refreshFoldersNow maps API folders into the mail store', async () => {
    vi.mocked(api).mockResolvedValueOnce([
      {
        id: 'f1',
        accountId: 'a1',
        name: 'Spam',
        role: 'spam',
        unreadMessages: 0,
        totalMessages: 0,
        sortOrder: 0,
      },
    ]);

    await refreshFoldersNow();

    expect(api).toHaveBeenCalledWith('/folders');
    expect(useMailStore.getState().folders.f1?.unreadCount).toBe(0);
  });

  it('scheduleFolderRefresh debounces into a single GET', async () => {
    vi.mocked(api).mockResolvedValueOnce([]);

    scheduleFolderRefresh();
    scheduleFolderRefresh();
    expect(api).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(FOLDER_REFRESH_DEBOUNCE_MS);
    expect(api).toHaveBeenCalledTimes(1);
  });

  it('skips when unauthenticated', async () => {
    useAuthStore.setState({ token: null });
    await refreshFoldersNow();
    expect(api).not.toHaveBeenCalled();
  });
});
