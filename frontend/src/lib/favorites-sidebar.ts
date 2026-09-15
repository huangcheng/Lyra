/**
 * Helpers for unified mailbox sidebar rows (All Inboxes, Starred, …).
 */

import type { MailAccount, MailFolder } from '@/types';

export interface AccountInboxChild {
  accountId: string;
  accountLabel: string;
  folderId: string;
  unreadCount: number;
}

/** Per-account Inbox shortcuts under All Inboxes. */
export function accountInboxChildren(
  accounts: MailAccount[],
  folders: Record<string, MailFolder>,
): AccountInboxChild[] {
  const out: AccountInboxChild[] = [];
  for (const account of accounts) {
    const inbox = Object.values(folders).find(
      (f) => f.accountId === account.id && f.role === 'inbox',
    );
    if (!inbox) continue;
    out.push({
      accountId: account.id,
      accountLabel: account.displayName || account.emailAddress,
      folderId: inbox.id,
      unreadCount: inbox.unreadCount,
    });
  }
  return out;
}
