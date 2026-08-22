/**
 * Zustand store for UI chrome state.
 */

import { create } from 'zustand';

import { ALL_ACCOUNTS } from '@/lib/mail-api';
import type { SupportedLocale } from '@/types';

export interface ComposeDraft {
  to: string;
  subject: string;
  body: string;
  mode: 'new' | 'reply' | 'forward';
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
  locale: SupportedLocale;

  setSelectedAccount: (id: string) => void;
  setSelectedFolder: (id: string | null) => void;
  setSelectedFolderRole: (role: string | null) => void;
  setSelectedMessage: (id: string | null) => void;
  setSearchQuery: (query: string) => void;
  setListTab: (tab: 'all' | 'unread') => void;
  setComposeOpen: (open: boolean) => void;
  openCompose: (draft?: Partial<ComposeDraft>) => void;
  clearComposeDraft: () => void;
  setLocale: (locale: SupportedLocale) => void;
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
  locale: 'en',

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
        subject: draft?.subject ?? '',
        body: draft?.body ?? '',
        mode: draft?.mode ?? 'new',
      },
    }),

  clearComposeDraft: () => set({ composeDraft: null }),

  setLocale: (locale) => set({ locale }),
}));
