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

/** Attachment metadata from the message detail payload. */
export interface MailAttachment {
  id: string;
  filename?: string;
  contentType?: string;
  sizeBytes?: number;
  isInline: boolean;
  /** CID for inline images (`src="cid:…"` in the HTML body). */
  contentId?: string;
}

export interface MailMessage {
  id: string;
  accountId: string;
  folderId: string;
  threadId?: string;
  /** RFC 5322 Message-ID — identifies cross-folder copies of one message. */
  messageIdHeader?: string;
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
  /** Attachment metadata; only the detail payload carries it. */
  attachments?: MailAttachment[];
  remoteContentBlocked?: boolean;
  /** OpenGPG decrypt/verify status from GET message (when present). */
  opengpg?: MailOpengpgStatus;
  /** Thread was replied to (list status glyph). */
  isReplied?: boolean;
  labels?: string[];
}

export interface MailOpengpgSignature {
  fingerprint: string;
  email?: string;
  valid: boolean;
  time?: string;
}

export interface MailOpengpgStatus {
  encrypted: boolean;
  decrypted: boolean;
  signatures: MailOpengpgSignature[];
  error?: string;
}

export interface MailFolder {
  id: string;
  accountId: string;
  name: string;
  /** Effective role: override wins over SPECIAL-USE / name inference. */
  role?: 'inbox' | 'sent' | 'drafts' | 'trash' | 'spam' | 'archive';
  /** Explicit local override when set. */
  roleOverride?: 'inbox' | 'sent' | 'drafts' | 'trash' | 'spam' | 'archive';
  parentId?: string;
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

// ── Dashboard stats ──────────────────────────────────────────────

export interface DailyVolume {
  date: string;
  received: number;
}

export interface TopSender {
  address: string;
  name: string | null;
  count: number;
}

/** Response for `GET /api/v1/messages/stats?days=`. */
export interface StatsResponse {
  days: number;
  daily: DailyVolume[];
  topSenders: TopSender[];
  totals: { received: number; sent: number; unread: number };
}

// ── UI state ───────────────────────────────────────────────────────

export type SupportedLocale = 'en' | 'zh';

/** When to mark a message read (Settings → Reading status). */
export type MarkReadPolicy = 'on_open' | 'on_scroll_end' | 'manual';
