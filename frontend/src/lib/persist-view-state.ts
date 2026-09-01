/**
 * Persist mail view-state (selected account/folder, sidebar folder
 * expansion, sidebar account order) to the server so the sidebar restores
 * identically after a reload — and on any other device.
 *
 * Server-side store: `lyra_user.ui_state` JSON blob via
 * `PATCH /api/v1/auth/preferences` (debounced, fire-and-forget).
 */

import { api } from '@/lib/api-client';
import { useAuthStore } from '@/stores/auth';
import { useUIStore, type AccountExpansion } from '@/stores/ui';

const SAVE_DEBOUNCE_MS = 400;

/** Validate one restored expansion entry; drop malformed values. */
function parseExpansion(raw: unknown): { expanded: boolean; folderIds: string[] } | null {
  if (!raw || typeof raw !== 'object') return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.expanded !== 'boolean' || !Array.isArray(o.folderIds)) return null;
  return { expanded: o.expanded, folderIds: o.folderIds.filter((x) => typeof x === 'string') };
}

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
  if (uiState.folderExpansion && typeof uiState.folderExpansion === 'object') {
    const map: Record<string, AccountExpansion> = {};
    for (const [key, value] of Object.entries(uiState.folderExpansion)) {
      const parsed = parseExpansion(value);
      if (parsed) map[key] = parsed;
    }
    ui.setFolderExpansion(map);
  }
  if (Array.isArray(uiState.accountOrder)) {
    ui.setAccountOrder(uiState.accountOrder.filter((x): x is string => typeof x === 'string'));
  }
}

/** Subscribe once; writes are debounced and skipped while logged out. */
export function startViewStatePersistence(): () => void {
  let timer: number | undefined;
  return useUIStore.subscribe((state, prev) => {
    if (
      state.selectedAccountId === prev.selectedAccountId &&
      state.selectedFolderId === prev.selectedFolderId &&
      state.selectedFolderRole === prev.selectedFolderRole &&
      state.folderExpansion === prev.folderExpansion &&
      state.accountOrder === prev.accountOrder
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
            folderExpansion: s.folderExpansion,
            accountOrder: s.accountOrder,
          },
        }),
      }).catch(() => {
        // View state is best-effort; a failed save just means the next
        // reload restores the previous position.
      });
    }, SAVE_DEBOUNCE_MS);
  });
}
