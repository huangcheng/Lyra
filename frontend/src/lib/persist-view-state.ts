/**
 * Persist mail view-state (selected account/folder, sidebar folder
 * expansion, sidebar account order, default account, theme, notification
 * prefs) to the server so the sidebar restores identically after a reload —
 * and on any other device.
 *
 * Server-side store: `lyra_user.ui_state` JSON blob via
 * `PATCH /api/v1/auth/preferences` (debounced, fire-and-forget). A pending
 * debounced save is flushed on pagehide so a fast reload can't eat the
 * last change.
 */

import { api } from '@/lib/api-client';
import {
  readNotificationPrefs,
  subscribeNotificationPrefs,
  writeNotificationPrefs,
} from '@/lib/notifications';
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

function stringList(raw: unknown): string[] {
  return Array.isArray(raw) ? raw.filter((x): x is string => typeof x === 'string') : [];
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
    ui.setAccountOrder(stringList(uiState.accountOrder));
  }
  if (typeof uiState.defaultAccountId === 'string' && uiState.defaultAccountId) {
    ui.setDefaultAccount(uiState.defaultAccountId);
  }
  if (
    typeof uiState.listTab === 'string' &&
    (uiState.listTab === 'all' || uiState.listTab === 'unread')
  ) {
    ui.setListTab(uiState.listTab);
  }
  if (typeof uiState.favoritesAllInboxesExpanded === 'boolean') {
    ui.setFavoritesAllInboxesExpanded(uiState.favoritesAllInboxesExpanded);
  }
  if (uiState.theme === 'light' || uiState.theme === 'dark' || uiState.theme === 'system') {
    ui.setTheme(uiState.theme);
  }
  if (typeof uiState.notificationPrefs === 'object' && uiState.notificationPrefs !== null) {
    const o = uiState.notificationPrefs as Record<string, unknown>;
    writeNotificationPrefs({
      enabled: typeof o.enabled === 'boolean' ? o.enabled : false,
      mutedFolderIds: stringList(o.mutedFolderIds),
      mutedThreadIds: stringList(o.mutedThreadIds),
    });
  }
  // Message selection goes last — the account/folder setters above reset it.
  if (typeof uiState.selectedMessageId === 'string' && uiState.selectedMessageId) {
    ui.setSelectedMessage(uiState.selectedMessageId);
  }
}

/** Current persistable blob: UI-store fields + notification prefs. */
function currentUiState(): Record<string, unknown> {
  const s = useUIStore.getState();
  return {
    selectedAccountId: s.selectedAccountId,
    selectedFolderId: s.selectedFolderId,
    selectedFolderRole: s.selectedFolderRole,
    selectedMessageId: s.selectedMessageId,
    listTab: s.listTab,
    folderExpansion: s.folderExpansion,
    accountOrder: s.accountOrder,
    defaultAccountId: s.defaultAccountId,
    favoritesAllInboxesExpanded: s.favoritesAllInboxesExpanded,
    theme: s.theme,
    notificationPrefs: readNotificationPrefs(),
  };
}

/**
 * Subscribe once; writes are debounced and skipped while logged out.
 * Returns an unsubscribe that also detaches the pagehide flush.
 */
export function startViewStatePersistence(): () => void {
  let timer: number | undefined;

  const save = (keepalive = false) => {
    window.clearTimeout(timer);
    timer = undefined;
    if (!useAuthStore.getState().token) return;
    void api('/auth/preferences', {
      method: 'PATCH',
      body: JSON.stringify({ uiState: currentUiState() }),
      keepalive,
    }).catch(() => {
      // View state is best-effort; a failed save just means the next
      // reload restores the previous position.
    });
  };

  const schedule = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => save(), SAVE_DEBOUNCE_MS);
  };

  const unsubscribeStore = useUIStore.subscribe((state, prev) => {
    if (
      state.selectedAccountId === prev.selectedAccountId &&
      state.selectedFolderId === prev.selectedFolderId &&
      state.selectedFolderRole === prev.selectedFolderRole &&
      state.selectedMessageId === prev.selectedMessageId &&
      state.listTab === prev.listTab &&
      state.folderExpansion === prev.folderExpansion &&
      state.accountOrder === prev.accountOrder &&
      state.defaultAccountId === prev.defaultAccountId &&
      state.favoritesAllInboxesExpanded === prev.favoritesAllInboxesExpanded &&
      state.theme === prev.theme
    ) {
      return;
    }
    schedule();
  });
  // Notification prefs live outside the UI store; their writes funnel here.
  const unsubscribePrefs = subscribeNotificationPrefs(schedule);

  // A pending debounced save would be lost if the tab closes first.
  const onPageHide = () => {
    if (timer !== undefined) save(true);
  };
  window.addEventListener('pagehide', onPageHide);

  return () => {
    unsubscribeStore();
    unsubscribePrefs();
    window.removeEventListener('pagehide', onPageHide);
    window.clearTimeout(timer);
  };
}
