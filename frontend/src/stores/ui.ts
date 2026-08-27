/**
 * Zustand store for UI chrome state.
 */

import { create } from 'zustand';

import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { applyTheme, getStoredTheme, storeTheme, type ThemeMode } from '@/lib/theme';
import type { MarkReadPolicy, SupportedLocale } from '@/types';

export interface ComposeDraft {
  to: string;
  subject: string;
  body: string;
  mode: 'new' | 'reply' | 'forward';
  /** Forwarding carries the original's non-inline attachments (metadata). */
  forwardAttachments?: Array<{ id: string; filename?: string; contentType?: string }>;
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
        subject: draft?.subject ?? '',
        body: draft?.body ?? '',
        mode: draft?.mode ?? 'new',
      },
    }),

  clearComposeDraft: () => set({ composeDraft: null }),

  toggleMuteMessage: (id) =>
    set((s) => ({
      mutedMessageIds: s.mutedMessageIds.includes(id)
        ? s.mutedMessageIds.filter((x) => x !== id)
        : [...s.mutedMessageIds, id],
    })),

  setLocale: (locale) => set({ locale }),

  setMarkReadPolicy: (policy) => set({ markReadPolicy: policy }),

  setTheme: (theme) => {
    storeTheme(theme);
    applyTheme(theme);
    set({ theme });
  },
}));
