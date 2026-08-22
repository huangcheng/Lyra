/**
 * Map backend mail JSON onto frontend types.
 */

import type { MailAddress, MailFolder, MailMessage } from '@/types';

export interface ApiFolder {
  id: string;
  accountId: string;
  name: string;
  role?: string | null;
  sortOrder: number;
  totalMessages: number;
  unreadMessages: number;
}

export interface ApiMessage {
  id: string;
  accountId: string;
  folderId: string;
  subject?: string;
  fromAddress?: string;
  toAddresses?: string;
  ccAddresses?: string;
  date?: string;
  snippet?: string;
  bodyText?: string;
  bodyHtml?: string;
  isRead: boolean;
  isStarred: boolean;
  hasAttachments: boolean;
}

export function parseAddress(json?: string): MailAddress {
  if (!json) return { email: 'unknown' };
  try {
    const parsed = JSON.parse(json) as { raw?: string };
    if (parsed.raw) {
      const match = parsed.raw.match(/^(.+?)\s*<(.+?)>$/);
      if (match) return { name: match[1].trim(), email: match[2].trim() };
      return { email: parsed.raw };
    }
    return { email: 'unknown' };
  } catch {
    return { email: 'unknown' };
  }
}

export function parseAddresses(json?: string): MailAddress[] {
  if (!json) return [];
  try {
    const parsed = JSON.parse(json) as unknown;
    if (Array.isArray(parsed)) {
      return parsed.map((item: string) => {
        const match = item.match(/^(.+?)\s*<(.+?)>$/);
        if (match) return { name: match[1].trim(), email: match[2].trim() };
        return { email: item };
      });
    }
    return [];
  } catch {
    return [];
  }
}

export function mapApiFolder(folder: ApiFolder): MailFolder {
  const role = folder.role as MailFolder['role'] | undefined;
  return {
    id: folder.id,
    accountId: folder.accountId,
    name: folder.name,
    role: role || undefined,
    unreadCount: folder.unreadMessages,
    totalCount: folder.totalMessages,
    sortOrder: folder.sortOrder,
  };
}

export function mapApiMessage(msg: ApiMessage | Record<string, unknown>): MailMessage {
  const row = msg as ApiMessage;
  return {
    id: String(row.id),
    accountId: String(row.accountId),
    folderId: String(row.folderId),
    subject: row.subject ?? '(no subject)',
    from: parseAddress(row.fromAddress),
    to: parseAddresses(row.toAddresses),
    cc: parseAddresses(row.ccAddresses),
    date: row.date ?? new Date().toISOString(),
    snippet: row.snippet ?? '',
    bodyText: row.bodyText,
    bodyHtml: row.bodyHtml,
    isRead: Boolean(row.isRead),
    isStarred: Boolean(row.isStarred),
    isDraft: false,
    hasAttachments: Boolean(row.hasAttachments),
  };
}

export const STANDARD_FOLDER_ROLES = [
  'inbox',
  'drafts',
  'sent',
  'spam',
  'trash',
  'archive',
] as const;

export type StandardFolderRole = (typeof STANDARD_FOLDER_ROLES)[number];

/** Sentinel account id for the unified inbox (all accounts). */
export const ALL_ACCOUNTS = 'all';
