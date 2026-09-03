/**
 * Map backend mail JSON onto frontend types.
 */

import type { DkimInfo, MailAddress, MailAttachment, MailFolder, MailMessage } from '@/types';

export interface ApiFolder {
  id: string;
  accountId: string;
  name: string;
  role?: string | null;
  roleOverride?: string | null;
  parentId?: string | null;
  sortOrder: number;
  totalMessages: number;
  unreadMessages: number;
}

export interface ApiAttachment {
  id: string;
  filename?: string;
  contentType?: string;
  sizeBytes?: number;
  isInline: boolean;
  contentId?: string;
}

export interface ApiMessage {
  id: string;
  accountId: string;
  folderId: string;
  messageIdHeader?: string;
  inReplyTo?: string;
  referencesHeaders?: string;
  labels?: string;
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
  isDraft?: boolean;
  hasAttachments: boolean;
  attachments?: ApiAttachment[];
  remoteContentBlocked?: boolean;
  dkim?: DkimInfo | null;
  opengpg?: {
    encrypted: boolean;
    decrypted: boolean;
    signatures: Array<{
      fingerprint: string;
      email?: string;
      valid: boolean;
      time?: string;
    }>;
    error?: string;
  };
}

export function parseAddress(json?: string): MailAddress {
  if (!json) return { email: 'unknown' };
  try {
    const parsed = JSON.parse(json) as unknown;
    if (typeof parsed === 'string') {
      return parseOneAddress(parsed);
    }
    if (parsed && typeof parsed === 'object') {
      const obj = parsed as {
        raw?: string;
        email?: string;
        name?: string;
      };
      const raw = obj.raw ?? obj.email;
      if (raw) {
        const addr = parseOneAddress(raw);
        if (obj.name && !addr.name) return { name: obj.name, email: addr.email };
        return addr;
      }
    }
    return { email: 'unknown' };
  } catch {
    return { email: 'unknown' };
  }
}

function parseOneAddress(raw: string): MailAddress {
  const match = raw.match(/^(.+?)\s*<(.+?)>$/);
  if (match) return { name: match[1].trim(), email: match[2].trim() };
  return { email: raw };
}

export /** Labels arrive as a JSON array text (`[\"work\"]`); never throws. */
function parseLabels(raw: string | undefined): string[] | undefined {
  if (!raw) return undefined;
  try {
    const v = JSON.parse(raw);
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : undefined;
  } catch {
    return undefined;
  }
}

function parseAddresses(json?: string): MailAddress[] {
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
  const roleOverride = folder.roleOverride as MailFolder['roleOverride'] | undefined;
  return {
    id: folder.id,
    accountId: folder.accountId,
    name: folder.name,
    role: role || undefined,
    roleOverride: roleOverride || undefined,
    parentId: folder.parentId ?? undefined,
    unreadCount: folder.unreadMessages,
    totalCount: folder.totalMessages,
    sortOrder: folder.sortOrder,
  };
}

export function mapApiAttachments(rows: ApiAttachment[] | undefined): MailAttachment[] | undefined {
  if (!rows) return undefined;
  return rows.map((a) => ({
    id: String(a.id),
    filename: a.filename,
    contentType: a.contentType,
    sizeBytes: a.sizeBytes,
    isInline: Boolean(a.isInline),
    contentId: a.contentId,
  }));
}

export function mapApiMessage(msg: ApiMessage | Record<string, unknown>): MailMessage {
  const row = msg as ApiMessage;
  return {
    id: String(row.id),
    accountId: String(row.accountId),
    folderId: String(row.folderId),
    messageIdHeader: row.messageIdHeader ?? undefined,
    inReplyTo: row.inReplyTo ?? undefined,
    referencesHeaders: row.referencesHeaders ?? undefined,
    labels: parseLabels(row.labels),
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
    isDraft: Boolean(row.isDraft),
    hasAttachments: Boolean(row.hasAttachments),
    attachments: mapApiAttachments(row.attachments),
    remoteContentBlocked: Boolean(row.remoteContentBlocked),
    dkim: row.dkim ?? null,
    opengpg: row.opengpg
      ? {
          encrypted: Boolean(row.opengpg.encrypted),
          decrypted: Boolean(row.opengpg.decrypted),
          signatures: (row.opengpg.signatures ?? []).map((s) => ({
            fingerprint: s.fingerprint ?? '',
            email: s.email,
            valid: Boolean(s.valid),
            time: s.time,
          })),
          error: row.opengpg.error,
        }
      : undefined,
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
