/**
 * Fetch messages for the current mail list view.
 */

import { api } from '@/lib/api-client';
import { ALL_ACCOUNTS, mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import type { MailMessage } from '@/types';

export function messagesUrlForView(opts: {
  accountId: string;
  folderId: string | null;
  folderRole: string | null;
}): string | null {
  if (opts.folderId) {
    return `/folders/${opts.folderId}/messages`;
  }
  if (opts.folderRole === 'starred') {
    const params = new URLSearchParams({ isStarred: 'true' });
    if (opts.accountId !== ALL_ACCOUNTS) params.set('accountId', opts.accountId);
    return `/messages?${params}`;
  }
  if (opts.folderRole) {
    const params = new URLSearchParams({ role: opts.folderRole });
    if (opts.accountId !== ALL_ACCOUNTS) params.set('accountId', opts.accountId);
    return `/messages?${params}`;
  }
  return null;
}

export async function fetchMessagesForView(opts: {
  accountId: string;
  folderId: string | null;
  folderRole: string | null;
}): Promise<MailMessage[]> {
  const url = messagesUrlForView(opts);
  if (!url) return [];
  const data = await api<ApiMessage[]>(url);
  return data.map(mapApiMessage);
}
