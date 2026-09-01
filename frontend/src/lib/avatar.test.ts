import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', () => ({ apiBlob: vi.fn() }));

import { act, createElement } from 'react';
import { createRoot } from 'react-dom/client';

import { apiBlob } from '@/lib/api-client';
import { avatarState, loadAvatar, resetAvatarCacheForTests, useAvatar } from '@/lib/avatar';

const mockedApiBlob = vi.mocked(apiBlob);

(globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;

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

describe('useAvatar', () => {
  it('clears the previous sender photo when the new sender has none', async () => {
    mockedApiBlob.mockImplementation((path: string) =>
      path.includes('a%40example.com')
        ? Promise.resolve(new Blob(['x'], { type: 'image/png' }))
        : Promise.reject(new Error('404')),
    );

    const seen: (string | null)[] = [];
    function Probe({ email }: { email: string }) {
      seen.push(useAvatar(email));
      return null;
    }

    const container = document.createElement('div');
    document.body.appendChild(container);
    const root = createRoot(container);

    // Sender A resolves to a photo.
    await act(async () => {
      root.render(createElement(Probe, { email: 'a@example.com' }));
    });
    const urlA = seen.at(-1);
    expect(urlA).not.toBeNull();

    // Sender B has no avatar: the hook must clear, never keep A's photo.
    await act(async () => {
      root.render(createElement(Probe, { email: 'b@example.com' }));
    });
    expect(seen.at(-1)).toBeNull();
    expect(seen.at(-1)).not.toBe(urlA);

    await act(async () => {
      root.unmount();
    });
    container.remove();
  });
});
