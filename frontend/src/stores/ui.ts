/**
 * Zustand store for UI chrome state.
 *
 * Owns: selected folder, selected message, compose state, search query,
 * language preference, sidebar collapse, pane widths, etc.
 *
 * Role: UI VIEW STATE only. No domain data (→ mailStore), no flow logic
 * (→ XState machines), no async streams (→ RxJS).
 */

import { create } from 'zustand';
import type { SupportedLocale } from '../types';

const PANE_STORAGE_KEY = 'lyra.paneWidths';

export const DEFAULT_SIDEBAR_WIDTH = 232;
export const DEFAULT_LIST_WIDTH = 340;
export const MIN_SIDEBAR_WIDTH = 180;
export const MAX_SIDEBAR_WIDTH = 320;
export const MIN_LIST_WIDTH = 280;
export const MAX_LIST_WIDTH = 480;

export interface ComposeDraft {
  to: string;
  subject: string;
  body: string;
  mode: 'new' | 'reply' | 'forward';
}

interface PaneWidths {
  sidebarWidth: number;
  listWidth: number;
}

function loadPaneWidths(): PaneWidths {
  try {
    const raw = localStorage.getItem(PANE_STORAGE_KEY);
    if (!raw) {
      return { sidebarWidth: DEFAULT_SIDEBAR_WIDTH, listWidth: DEFAULT_LIST_WIDTH };
    }
    const parsed = JSON.parse(raw) as Partial<PaneWidths>;
    return {
      sidebarWidth: clamp(
        parsed.sidebarWidth ?? DEFAULT_SIDEBAR_WIDTH,
        MIN_SIDEBAR_WIDTH,
        MAX_SIDEBAR_WIDTH,
      ),
      listWidth: clamp(parsed.listWidth ?? DEFAULT_LIST_WIDTH, MIN_LIST_WIDTH, MAX_LIST_WIDTH),
    };
  } catch {
    return { sidebarWidth: DEFAULT_SIDEBAR_WIDTH, listWidth: DEFAULT_LIST_WIDTH };
  }
}

function clamp(n: number, min: number, max: number) {
  return Math.min(max, Math.max(min, n));
}

interface UIState {
  selectedAccountId: string | null;
  selectedFolderId: string | null;
  selectedMessageId: string | null;
  searchQuery: string;
  searchOpen: boolean;
  sidebarCollapsed: boolean;
  sidebarWidth: number;
  listWidth: number;
  composeOpen: boolean;
  composeDraft: ComposeDraft | null;
  locale: SupportedLocale;

  setSelectedAccount: (id: string | null) => void;
  setSelectedFolder: (id: string | null) => void;
  setSelectedMessage: (id: string | null) => void;
  setSearchQuery: (query: string) => void;
  setSearchOpen: (open: boolean) => void;
  toggleSidebar: () => void;
  setPaneWidths: (widths: Partial<PaneWidths>) => void;
  resetPaneWidths: () => void;
  setComposeOpen: (open: boolean) => void;
  openCompose: (draft?: Partial<ComposeDraft>) => void;
  clearComposeDraft: () => void;
  setLocale: (locale: SupportedLocale) => void;
}

const initialPanes = typeof window !== 'undefined' ? loadPaneWidths() : {
  sidebarWidth: DEFAULT_SIDEBAR_WIDTH,
  listWidth: DEFAULT_LIST_WIDTH,
};

export const useUIStore = create<UIState>((set, get) => ({
  selectedAccountId: null,
  selectedFolderId: null,
  selectedMessageId: null,
  searchQuery: '',
  searchOpen: false,
  sidebarCollapsed: false,
  sidebarWidth: initialPanes.sidebarWidth,
  listWidth: initialPanes.listWidth,
  composeOpen: false,
  composeDraft: null,
  locale: 'en',

  setSelectedAccount: (id) =>
    set({ selectedAccountId: id, selectedFolderId: null, selectedMessageId: null }),

  setSelectedFolder: (id) => set({ selectedFolderId: id, selectedMessageId: null }),

  setSelectedMessage: (id) => set({ selectedMessageId: id }),

  setSearchQuery: (query) => set({ searchQuery: query }),

  setSearchOpen: (open) => set({ searchOpen: open, searchQuery: open ? get().searchQuery : '' }),

  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),

  setPaneWidths: (widths) => {
    const next = {
      sidebarWidth: clamp(
        widths.sidebarWidth ?? get().sidebarWidth,
        MIN_SIDEBAR_WIDTH,
        MAX_SIDEBAR_WIDTH,
      ),
      listWidth: clamp(widths.listWidth ?? get().listWidth, MIN_LIST_WIDTH, MAX_LIST_WIDTH),
    };
    set(next);
    try {
      localStorage.setItem(PANE_STORAGE_KEY, JSON.stringify(next));
    } catch {
      /* ignore quota */
    }
  },

  resetPaneWidths: () => {
    const next = { sidebarWidth: DEFAULT_SIDEBAR_WIDTH, listWidth: DEFAULT_LIST_WIDTH };
    set(next);
    try {
      localStorage.setItem(PANE_STORAGE_KEY, JSON.stringify(next));
    } catch {
      /* ignore */
    }
  },

  setComposeOpen: (open) => set({ composeOpen: open, composeDraft: open ? get().composeDraft : null }),

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
