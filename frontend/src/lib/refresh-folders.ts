/**
 * Debounced folder list refresh so sidebar unread badges stay in sync after
 * local mutations (trash / move / snooze / mark-read). Counts live on the
 * folder rows from GET /folders; message actions update the DB but the
 * client only re-fetched folders on sync_complete before this helper.
 */

import { api } from '@/lib/api-client';
import { mapApiFolder, type ApiFolder } from '@/lib/mail-api';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';

/** Match sync_complete coalescing in useMailData. */
export const FOLDER_REFRESH_DEBOUNCE_MS = 400;

let timer: ReturnType<typeof setTimeout> | undefined;
let inflight: Promise<void> | null = null;

/** Fetch folders now and replace the mail-store snapshot. */
export async function refreshFoldersNow(): Promise<void> {
  const token = useAuthStore.getState().token;
  if (!token) return;
  if (inflight) return inflight;
  inflight = (async () => {
    try {
      const data = await api<ApiFolder[]>('/folders');
      useMailStore.getState().setFolders(data.map(mapApiFolder));
    } catch {
      /* network or HTTP error — keep last good snapshot */
    } finally {
      inflight = null;
    }
  })();
  return inflight;
}

/** Coalesce rapid mutations into one folders GET. */
export function scheduleFolderRefresh(): void {
  clearTimeout(timer);
  timer = setTimeout(() => {
    void refreshFoldersNow();
  }, FOLDER_REFRESH_DEBOUNCE_MS);
}

/** Test helper: clear pending debounce / in-flight gate. */
export function resetFolderRefreshForTests(): void {
  clearTimeout(timer);
  timer = undefined;
  inflight = null;
}
