/**
 * Core type definitions for Lyra.
 *
 * These mirror the backend data model (see docs/specs/2026-08-20-lyra-data-model-spec.md).
 */

// ── Mail data ──────────────────────────────────────────────────────

export interface MailAddress {
  name?: string;
  email: string;
}

export interface MailMessage {
  id: string;
  accountId: string;
  folderId: string;
  threadId?: string;
  subject: string;
  from: MailAddress;
  to: MailAddress[];
  cc?: MailAddress[];
  date: string;
  snippet: string;
  bodyText?: string;
  bodyHtml?: string;
  isRead: boolean;
  isStarred: boolean;
  isDraft: boolean;
  hasAttachments: boolean;
  labels?: string[];
}

export interface MailFolder {
  id: string;
  accountId: string;
  name: string;
  role?: 'inbox' | 'sent' | 'drafts' | 'trash' | 'spam' | 'archive';
  unreadCount: number;
  totalCount: number;
  sortOrder: number;
}

export interface MailThread {
  id: string;
  accountId: string;
  subject: string;
  snippet: string;
  date: string;
  messageCount: number;
  unreadCount: number;
  isStarred: boolean;
  participants: MailAddress[];
}

export interface MailAccount {
  id: string;
  displayName: string;
  emailAddress: string;
  protocol: 'jmap' | 'imap';
  isActive: boolean;
  syncEnabled: boolean;
  lastSyncAt?: string;
}

// ── Sync events ────────────────────────────────────────────────────

export type SyncEvent =
  | { type: 'sync_started'; accountId: string }
  | {
      type: 'folder_progress';
      accountId: string;
      folderId: string;
      fetched: number;
      total: number;
    }
  | { type: 'folder_complete'; accountId: string; folderId: string }
  | {
      type: 'incremental_complete';
      accountId: string;
      folderId: string;
      changes: number;
    }
  | { type: 'sync_error'; accountId: string; error: string }
  | { type: 'sync_complete'; accountId: string };

// ── UI state ───────────────────────────────────────────────────────

export type SupportedLocale = 'en' | 'zh';
