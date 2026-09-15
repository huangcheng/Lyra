/**
 * Server-authoritative Starred sidebar badge. The web client's message
 * cache only holds folders visited this session, so counting `isStarred`
 * locally undercounts — the total has to come from the server's COUNT.
 */

import { useEffect } from 'react';

import { api } from '@/lib/api-client';
import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { syncEvents$ } from '@/rxjs/sync-events';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

export function useStarredCount(): number {
  const token = useAuthStore((s) => s.token);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const count = useMailStore((s) => s.starredCount);
  const setStarredCount = useMailStore((s) => s.setStarredCount);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    const fetchCount = () => {
      const params = new URLSearchParams();
      if (selectedAccountId !== ALL_ACCOUNTS) params.set('accountId', selectedAccountId);
      const qs = params.toString();
      api<{ count: number }>(`/messages/starred-count${qs ? `?${qs}` : ''}`)
        .then((res) => {
          if (!cancelled) setStarredCount(res.count);
        })
        .catch(() => {});
    };
    setStarredCount(null);
    fetchCount();
    // Remote flag changes arrive through sync; local flips adjust the
    // total optimistically inside the store's toggleStar.
    const sub = syncEvents$.subscribe((ev) => {
      if (ev.type === 'sync_complete') fetchCount();
    });
    return () => {
      cancelled = true;
      sub.unsubscribe();
    };
  }, [token, selectedAccountId, setStarredCount]);

  return count ?? 0;
}
