/**
 * Zustand store for UI chrome state.
 *
 * Owns: selected folder, selected message, compose state, search query,
 * language preference, sidebar collapse, etc.
 *
 * Role: UI VIEW STATE only. No domain data (→ mailStore), no flow logic
 * (→ XState machines), no async streams (→ RxJS).
 */

import { create } from 'zustand';
import type { SupportedLocale } from '../types';

interface UIState {
  // ── Selection ────────────────────────────────────────────────
  selectedAccountId: string | null;
  selectedFolderId: string | null;
  selectedMessageId: string | null;

  // ── Search ───────────────────────────────────────────────────
  searchQuery: string;

  // ── Layout ───────────────────────────────────────────────────
  sidebarCollapsed: boolean;

  // ── Compose ──────────────────────────────────────────────────
  composeOpen: boolean;

  // ── i18n ─────────────────────────────────────────────────────
  locale: SupportedLocale;

  // ── Mutations ────────────────────────────────────────────────
  setSelectedAccount: (id: string | null) => void;
  setSelectedFolder: (id: string | null) => void;
  setSelectedMessage: (id: string | null) => void;
  setSearchQuery: (query: string) => void;
  toggleSidebar: () => void;
  setComposeOpen: (open: boolean) => void;
  setLocale: (locale: SupportedLocale) => void;
}

export const useUIStore = create<UIState>((set) => ({
  // ── Initial state ────────────────────────────────────────────
  selectedAccountId: null,
  selectedFolderId: null,
  selectedMessageId: null,
  searchQuery: '',
  sidebarCollapsed: false,
  composeOpen: false,
  locale: 'en',

  // ── Mutations ────────────────────────────────────────────────
  setSelectedAccount: (id) =>
    set({ selectedAccountId: id, selectedFolderId: null, selectedMessageId: null }),

  setSelectedFolder: (id) => set({ selectedFolderId: id, selectedMessageId: null }),

  setSelectedMessage: (id) => set({ selectedMessageId: id }),

  setSearchQuery: (query) => set({ searchQuery: query }),

  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),

  setComposeOpen: (open) => set({ composeOpen: open }),

  setLocale: (locale) => set({ locale }),
}));
