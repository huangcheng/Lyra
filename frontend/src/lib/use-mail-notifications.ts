/**
 * Bridge the SSE sync stream to mail notifications.
 *
 * Mounted once in the root layout next to `useSyncEventSource`. The stream
 * already fans out through `syncEvents$`; this tap only observes — no store
 * writes (matching the rxjs module's ASYNC/RECOVERY-only role split).
 */

import { useEffect } from 'react';

import { syncEvents$ } from '@/rxjs/sync-events';
import { handleSyncEventForNotifications } from '@/lib/notifications';

export function useMailNotifications(): void {
  useEffect(() => {
    const sub = syncEvents$.subscribe({
      next: (ev) => {
        void handleSyncEventForNotifications(ev);
      },
    });
    return () => sub.unsubscribe();
  }, []);
}
