/**
 * Zustand store for UI chrome state.
 */

import { create } from 'zustand';

import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { applyTheme, getStoredTheme, storeTheme, type ThemeMode } from '@/lib/theme';
import type { MarkReadPolicy, SupportedLocale } from '@/types';

export interface ComposeDraft {
  to: string;
  cc?: string;
  subject: string;
  body: string;
  mode: 'new' | 'reply' | 'forward' | 'draft';
  /** Local message id of the server draft being edited (autosave replaces it). */
  draftMessageId?: string;
  /** Initial rich-editor content (reply/forward quote, restored draft body). */
  initialHtml?: string;
  /** Forwarding carries the original's non-inline attachments (metadata). */
  forwardAttachments?: Array<{ id: string; filename?: string; contentType?: string }>;
}

/** Sidebar expansion for one account: section open + expanded folder ids. */
export interface AccountExpansion {
  expanded: boolean;
  folderIds: string[];
}

interface UIState {
  /** `all` = unified inbox across accounts. */
  selectedAccountId: string;
  selectedFolderId: string | null;
  /** Standard role used in unified view (`inbox`, `sent`, …). */
  selectedFolderRole: string | null;
  selectedMessageId: string | null;
  searchQuery: string;
  listTab: 'all' | 'unread';
  composeOpen: boolean;
  composeDraft: ComposeDraft | null;
  /** Local mute (session): hide these message ids from the list. */
  mutedMessageIds: string[];
  /** Sidebar tree expansion, keyed by account id (persisted server-side). */
  folderExpansion: Record<string, AccountExpansion>;
  /** Custom sidebar account order (account ids; persisted server-side). */
  accountOrder: string[];
  locale: SupportedLocale;
  markReadPolicy: MarkReadPolicy;
  theme: ThemeMode;

  setSelectedAccount: (id: string) => void;
  setSelectedFolder: (id: string | null) => void;
  setSelectedFolderRole: (role: string | null) => void;
  setSelectedMessage: (id: string | null) => void;
  setSearchQuery: (query: string) => void;
  setListTab: (tab: 'all' | 'unread') => void;
  setComposeOpen: (open: boolean) => void;
  openCompose: (draft?: Partial<ComposeDraft>) => void;
  clearComposeDraft: () => void;
  toggleMuteMessage: (id: string) => void;
  setAccountExpanded: (accountId: string, expanded: boolean) => void;
  toggleFolderExpanded: (accountId: string, folderId: string) => void;
  /** Bulk-restore from the server-persisted view-state blob. */
  setFolderExpansion: (map: Record<string, AccountExpansion>) => void;
  setAccountOrder: (ids: string[]) => void;
  setLocale: (locale: SupportedLocale) => void;
  setMarkReadPolicy: (policy: MarkReadPolicy) => void;
  setTheme: (theme: ThemeMode) => void;
}

export const useUIStore = create<UIState>((set) => ({
  selectedAccountId: ALL_ACCOUNTS,
  selectedFolderId: null,
  selectedFolderRole: 'inbox',
  selectedMessageId: null,
  searchQuery: '',
  listTab: 'all',
  composeOpen: false,
  composeDraft: null,
  mutedMessageIds: [],
  folderExpansion: {},
  accountOrder: [],
  locale: 'en',
  markReadPolicy: 'on_open' as MarkReadPolicy,
  theme: getStoredTheme(),

  setSelectedAccount: (id) =>
    set({
      selectedAccountId: id,
      selectedFolderId: null,
      selectedFolderRole: id === ALL_ACCOUNTS ? 'inbox' : null,
      selectedMessageId: null,
    }),

  setSelectedFolder: (id) =>
    set({ selectedFolderId: id, selectedFolderRole: null, selectedMessageId: null }),

  setSelectedFolderRole: (role) =>
    set({ selectedFolderRole: role, selectedFolderId: null, selectedMessageId: null }),

  setSelectedMessage: (id) => set({ selectedMessageId: id }),

  setSearchQuery: (query) => set({ searchQuery: query }),

  setListTab: (tab) => set({ listTab: tab }),

  setComposeOpen: (open) =>
    set((s) => ({ composeOpen: open, composeDraft: open ? s.composeDraft : null })),

  openCompose: (draft) =>
    set({
      composeOpen: true,
      composeDraft: {
        to: draft?.to ?? '',
        cc: draft?.cc,
        subject: draft?.subject ?? '',
        body: draft?.body ?? '',
        mode: draft?.mode ?? 'new',
        draftMessageId: draft?.draftMessageId,
        initialHtml: draft?.initialHtml,
        forwardAttachments: draft?.forwardAttachments,
      },
    }),

  clearComposeDraft: () => set({ composeDraft: null }),

  toggleMuteMessage: (id) =>
    set((s) => ({
      mutedMessageIds: s.mutedMessageIds.includes(id)
        ? s.mutedMessageIds.filter((x) => x !== id)
        : [...s.mutedMessageIds, id],
    })),

  setAccountExpanded: (accountId, expanded) =>
    set((s) => ({
      folderExpansion: {
        ...s.folderExpansion,
        [accountId]: { expanded, folderIds: s.folderExpansion[accountId]?.folderIds ?? [] },
      },
    })),

  toggleFolderExpanded: (accountId, folderId) =>
    set((s) => {
      const current = s.folderExpansion[accountId] ?? { expanded: true, folderIds: [] };
      const folderIds = current.folderIds.includes(folderId)
        ? current.folderIds.filter((id) => id !== folderId)
        : [...current.folderIds, folderId];
      return {
        folderExpansion: { ...s.folderExpansion, [accountId]: { ...current, folderIds } },
      };
    }),

  setFolderExpansion: (map) => set({ folderExpansion: map }),

  setAccountOrder: (ids) => set({ accountOrder: ids }),

  setLocale: (locale) => set({ locale }),

  setMarkReadPolicy: (policy) => set({ markReadPolicy: policy }),

  setTheme: (theme) => {
    storeTheme(theme);
    applyTheme(theme);
    set({ theme });
  },
}));
