/**
 * Sender avatars via the backend resolver (`GET /api/v1/avatars/{email}`).
 * Authenticated binary → apiBlob (img src can't carry the bearer header,
 * see attachments.ts). Hits AND misses are memoized per session so lists
 * never refetch; object URLs are shared, not revoked per component.
 */

import { useEffect, useState } from 'react';

import { apiBlob } from '@/lib/api-client';

const cache = new Map<string, string | null>();

/** Synchronous read for render: undefined = unknown, null = no avatar. */
export function avatarState(email: string): string | null | undefined {
  return cache.get(email.trim().toLowerCase());
}

export async function loadAvatar(email: string): Promise<string | null> {
  const key = email.trim().toLowerCase();
  const known = cache.get(key);
  if (known !== undefined) return known;
  try {
    const blob = await apiBlob(`/avatars/${encodeURIComponent(key)}`);
    const url = URL.createObjectURL(blob);
    cache.set(key, url);
    return url;
  } catch {
    cache.set(key, null);
    return null;
  }
}

/** Component seam: current avatar URL for an address (null while loading/miss). */
export function useAvatar(email: string | undefined): string | null {
  const [url, setUrl] = useState<string | null>(() =>
    email ? (avatarState(email) ?? null) : null,
  );
  useEffect(() => {
    if (!email) return;
    let live = true;
    void loadAvatar(email).then((u) => {
      if (live && u) setUrl(u);
    });
    return () => {
      live = false;
    };
  }, [email]);
  return url;
}

/** Test-only: clear the session cache. */
export function resetAvatarCacheForTests(): void {
  cache.clear();
}
