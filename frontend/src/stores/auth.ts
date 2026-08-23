/**
 * Zustand store for auth session data.
 *
 * Owns: session token, current user info, auth state.
 *
 * Role: DATA only. No flow logic (→ XState), no UI state (→ uiStore).
 */

import { create } from 'zustand';

export interface User {
  id: string;
  username: string;
  displayName?: string;
  locale: string;
  totpEnabled: boolean;
  markReadPolicy?: string;
}

interface AuthState {
  // ── Session data ────────────────────────────────────────────
  token: string | null;
  user: User | null;
  isAuthenticated: boolean;

  // ── Bootstrap state ────────────────────────────────────────
  hasUser: boolean | null; // null = loading, false = needs bootstrap

  // ── Mutations ──────────────────────────────────────────────
  setToken: (token: string) => void;
  setUser: (user: User) => void;
  setAuthenticated: (authenticated: boolean) => void;
  setHasUser: (hasUser: boolean) => void;
  clearSession: () => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  // ── Initial state ──────────────────────────────────────────
  token: null,
  user: null,
  isAuthenticated: false,
  hasUser: null,

  // ── Mutations ──────────────────────────────────────────────
  setToken: (token) => set({ token }),

  setUser: (user) => set({ user, isAuthenticated: true }),

  setAuthenticated: (isAuthenticated) => set({ isAuthenticated }),

  setHasUser: (hasUser) => set({ hasUser }),

  clearSession: () =>
    set({
      token: null,
      user: null,
      isAuthenticated: false,
    }),
}));
