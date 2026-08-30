/**
 * Persist mail view-state (selected account/folder) to the server so the
 * sidebar restores identically after a reload — and on any other device.
 *
 * Server-side store: `lyra_user.ui_state` JSON blob via
 * `PATCH /api/v1/auth/preferences` (debounced, fire-and-forget).
 */

import { api } from '@/lib/api-client';
import { useAuthStore } from '@/stores/auth';
import { useUIStore } from '@/stores/ui';

const SAVE_DEBOUNCE_MS = 400;

/** Apply a server-restored view-state blob to the UI store. */
export function applyViewState(uiState: Record<string, unknown> | null | undefined): void {
  if (!uiState || typeof uiState !== 'object') return;
  const ui = useUIStore.getState();
  const accountId = uiState.selectedAccountId;
  const folderId = uiState.selectedFolderId;
  const folderRole = uiState.selectedFolderRole;
  if (typeof accountId === 'string' && accountId) {
    ui.setSelectedAccount(accountId);
  }
  if (typeof folderId === 'string' && folderId) {
    ui.setSelectedFolder(folderId);
  } else if (typeof folderRole === 'string' && folderRole) {
    ui.setSelectedFolderRole(folderRole);
  }
}

/** Subscribe once; writes are debounced and skipped while logged out. */
export function startViewStatePersistence(): () => void {
  let timer: number | undefined;
  return useUIStore.subscribe((state, prev) => {
    if (
      state.selectedAccountId === prev.selectedAccountId &&
      state.selectedFolderId === prev.selectedFolderId &&
      state.selectedFolderRole === prev.selectedFolderRole
    ) {
      return;
    }
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      if (!useAuthStore.getState().token) return;
      const s = useUIStore.getState();
      void api('/auth/preferences', {
        method: 'PATCH',
        body: JSON.stringify({
          uiState: {
            selectedAccountId: s.selectedAccountId,
            selectedFolderId: s.selectedFolderId,
            selectedFolderRole: s.selectedFolderRole,
          },
        }),
      }).catch(() => {
        // View state is best-effort; a failed save just means the next
        // reload restores the previous position.
      });
    }, SAVE_DEBOUNCE_MS);
  });
}
