/**
 * Zustand store for mail data.
 *
 * Owns normalised mail data: messages, folders, threads, accounts.
 */

import { create } from 'zustand';

import { ALL_ACCOUNTS, STANDARD_FOLDER_ROLES, type StandardFolderRole } from '@/lib/mail-api';
import type { MailAccount, MailFolder, MailMessage, MailThread } from '@/types';

export interface UnifiedFolder {
  role: StandardFolderRole;
  unreadCount: number;
  totalCount: number;
}

interface MailState {
  accounts: MailAccount[];
  folders: Record<string, MailFolder>;
  messages: Record<string, MailMessage>;
  threads: Record<string, MailThread>;

  getAccountById: (id: string) => MailAccount | undefined;
  getFoldersForAccount: (accountId: string) => MailFolder[];
  getUnifiedFolders: () => UnifiedFolder[];
  getMessagesForFolder: (folderId: string) => MailMessage[];
  getMessagesForView: (opts: {
    accountId: string | typeof ALL_ACCOUNTS;
    folderId: string | null;
    folderRole: string | null;
  }) => MailMessage[];
  getThreadById: (id: string) => MailThread | undefined;

  setAccounts: (accounts: MailAccount[]) => void;
  setFolders: (folders: MailFolder[]) => void;
  upsertFolder: (folder: MailFolder) => void;
  upsertMessage: (message: MailMessage) => void;
  upsertThread: (thread: MailThread) => void;
  markMessageRead: (id: string) => void;
  toggleStar: (id: string) => void;
  removeMessage: (id: string) => void;
}

export const useMailStore = create<MailState>((set, get) => ({
  accounts: [],
  folders: {},
  messages: {},
  threads: {},

  getAccountById: (id) => get().accounts.find((a) => a.id === id),

  getFoldersForAccount: (accountId) =>
    Object.values(get().folders)
      .filter((f) => f.accountId === accountId)
      .sort((a, b) => a.sortOrder - b.sortOrder),

  getUnifiedFolders: () => {
    const folders = Object.values(get().folders);
    return STANDARD_FOLDER_ROLES.map((role) => {
      const matching = folders.filter((f) => f.role === role);
      return {
        role,
        unreadCount: matching.reduce((sum, f) => sum + f.unreadCount, 0),
        totalCount: matching.reduce((sum, f) => sum + f.totalCount, 0),
      };
    });
  },

  getMessagesForFolder: (folderId) =>
    Object.values(get().messages)
      .filter((m) => m.folderId === folderId)
      .sort((a, b) => new Date(b.date).getTime() - new Date(a.date).getTime()),

  getMessagesForView: ({ accountId, folderId, folderRole }) => {
    const messages = Object.values(get().messages);
    const folders = get().folders;
    const filtered = messages.filter((m) => {
      if (accountId !== ALL_ACCOUNTS && m.accountId !== accountId) return false;
      if (folderId) return m.folderId === folderId;
      if (folderRole) return folders[m.folderId]?.role === folderRole;
      return false;
    });
    return filtered.sort((a, b) => new Date(b.date).getTime() - new Date(a.date).getTime());
  },

  getThreadById: (id) => get().threads[id],

  setAccounts: (accounts) => set({ accounts }),

  setFolders: (folders) =>
    set({
      folders: Object.fromEntries(folders.map((f) => [f.id, f])),
    }),

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
