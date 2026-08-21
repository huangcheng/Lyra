/**
 * Zustand store for mail data.
 *
 * Owns normalised mail data: messages, folders, threads, accounts.
 * This is the single source of truth for domain data the views read.
 *
 * Role: DATA only. No UI chrome state (→ uiStore), no flow logic (→ XState),
 * no async orchestration (→ RxJS).
 */

import { create } from 'zustand';
import type { MailMessage, MailFolder, MailThread, MailAccount } from '../types';

interface MailState {
  // ── Data ─────────────────────────────────────────────────────
  accounts: MailAccount[];
  folders: Record<string, MailFolder>;
  messages: Record<string, MailMessage>;
  threads: Record<string, MailThread>;

  // ── Derived helpers (selectors) ──────────────────────────────
  getAccountById: (id: string) => MailAccount | undefined;
  getFoldersForAccount: (accountId: string) => MailFolder[];
  getMessagesForFolder: (folderId: string) => MailMessage[];
  getThreadById: (id: string) => MailThread | undefined;

  // ── Mutations ────────────────────────────────────────────────
  setAccounts: (accounts: MailAccount[]) => void;
  upsertFolder: (folder: MailFolder) => void;
  upsertMessage: (message: MailMessage) => void;
  upsertThread: (thread: MailThread) => void;
  markMessageRead: (id: string) => void;
  toggleStar: (id: string) => void;
  removeMessage: (id: string) => void;
}

export const useMailStore = create<MailState>((set, get) => ({
  // ── Initial state ────────────────────────────────────────────
  accounts: [],
  folders: {},
  messages: {},
  threads: {},

  // ── Selectors ────────────────────────────────────────────────
  getAccountById: (id) => get().accounts.find((a) => a.id === id),

  getFoldersForAccount: (accountId) =>
    Object.values(get().folders)
      .filter((f) => f.accountId === accountId)
      .sort((a, b) => a.sortOrder - b.sortOrder),

  getMessagesForFolder: (folderId) =>
    Object.values(get().messages)
      .filter((m) => m.folderId === folderId)
      .sort((a, b) => new Date(b.date).getTime() - new Date(a.date).getTime()),

  getThreadById: (id) => get().threads[id],

  // ── Mutations ────────────────────────────────────────────────
  setAccounts: (accounts) => set({ accounts }),

  upsertFolder: (folder) =>
    set((state) => ({
      folders: { ...state.folders, [folder.id]: folder },
    })),

  upsertMessage: (message) =>
    set((state) => ({
      messages: { ...state.messages, [message.id]: message },
    })),

  upsertThread: (thread) =>
    set((state) => ({
      threads: { ...state.threads, [thread.id]: thread },
    })),

  markMessageRead: (id) =>
    set((state) => {
      const msg = state.messages[id];
      if (!msg) return state;
      return {
        messages: {
          ...state.messages,
          [id]: { ...msg, isRead: true },
        },
      };
    }),

  toggleStar: (id) =>
    set((state) => {
      const msg = state.messages[id];
      if (!msg) return state;
      return {
        messages: {
          ...state.messages,
          [id]: { ...msg, isStarred: !msg.isStarred },
        },
      };
    }),

  removeMessage: (id) =>
    set((state) => {
      const { [id]: _removed, ...rest } = state.messages;
      return { messages: rest };
    }),
}));
