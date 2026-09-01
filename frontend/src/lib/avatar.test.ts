import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', () => ({ apiBlob: vi.fn() }));

import { apiBlob } from '@/lib/api-client';
import { avatarState, loadAvatar, resetAvatarCacheForTests } from '@/lib/avatar';

const mockedApiBlob = vi.mocked(apiBlob);

beforeEach(() => {
  resetAvatarCacheForTests();
  mockedApiBlob.mockReset();
});

describe('loadAvatar', () => {
  it('returns an object URL for a hit and memoizes it', async () => {
    mockedApiBlob.mockResolvedValue(new Blob(['x'], { type: 'image/png' }));
    const first = await loadAvatar('a@example.com');
    const second = await loadAvatar('a@example.com');
    expect(first).not.toBeNull();
    expect(second).toBe(first);
    expect(mockedApiBlob).toHaveBeenCalledTimes(1);
    expect(mockedApiBlob).toHaveBeenCalledWith('/avatars/a%40example.com');
  });

  it('memoizes misses as null', async () => {
    mockedApiBlob.mockRejectedValue(new Error('404'));
    expect(await loadAvatar('b@example.com')).toBeNull();
    expect(await loadAvatar('b@example.com')).toBeNull();
    expect(mockedApiBlob).toHaveBeenCalledTimes(1);
  });

  it('exposes state for components without async work', async () => {
    mockedApiBlob.mockResolvedValue(new Blob(['x']));
    await loadAvatar('c@example.com');
    expect(avatarState('c@example.com')).not.toBeNull();
    expect(avatarState('d@example.com')).toBeUndefined();
  });
});
