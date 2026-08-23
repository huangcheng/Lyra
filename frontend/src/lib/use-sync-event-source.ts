/**
 * Bind the SSE sync stream to a session token.
 */

import { useEffect } from 'react';

import { connectSyncSse, syncEventSubject } from '@/rxjs/sync-events';
import { useAuthStore } from '@/stores/auth';

export function useSyncEventSource(): void {
  const token = useAuthStore((s) => s.token);

  useEffect(() => {
    if (!token) return;
    const sub = connectSyncSse().subscribe({
      next: (ev) => syncEventSubject.next(ev),
    });
    return () => sub.unsubscribe();
  }, [token]);
}
