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

/** Count starred messages visible for the current account switcher scope. */
export function starredCount(
  messages: Record<string, { accountId: string; isStarred: boolean }>,
  accountId: string,
  allAccountsSentinel: string,
): number {
  return Object.values(messages).filter((m) => {
    if (!m.isStarred) return false;
    if (accountId !== allAccountsSentinel && m.accountId !== accountId) return false;
    return true;
  }).length;
}
