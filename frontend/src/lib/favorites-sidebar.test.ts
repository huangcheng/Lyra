import { describe, expect, it } from 'vitest';

import { accountInboxChildren, starredCount } from './favorites-sidebar';
import type { MailAccount, MailFolder } from '@/types';

function account(partial: Partial<MailAccount> & Pick<MailAccount, 'id'>): MailAccount {
  return {
    displayName: 'A',
    emailAddress: 'a@example.com',
    protocol: 'imap',
    isActive: true,
    syncEnabled: true,
    ...partial,
  };
}

function folder(
  partial: Partial<MailFolder> & Pick<MailFolder, 'id' | 'accountId' | 'name'>,
): MailFolder {
  return {
    sortOrder: 0,
    totalCount: 0,
    unreadCount: 0,
    ...partial,
  };
}

describe('accountInboxChildren', () => {
  it('returns one child per account that has an inbox role folder', () => {
    const accounts = [account({ id: 'a1', displayName: 'Personal' }), account({ id: 'a2' })];
    const folders = {
      i1: folder({ id: 'i1', accountId: 'a1', name: 'INBOX', role: 'inbox', unreadCount: 2 }),
      i2: folder({ id: 'i2', accountId: 'a2', name: 'INBOX', role: 'inbox', unreadCount: 0 }),
      d1: folder({ id: 'd1', accountId: 'a1', name: 'Drafts', role: 'drafts' }),
    };
    expect(accountInboxChildren(accounts, folders)).toEqual([
      { accountId: 'a1', accountLabel: 'Personal', folderId: 'i1', unreadCount: 2 },
      { accountId: 'a2', accountLabel: 'A', folderId: 'i2', unreadCount: 0 },
    ]);
  });
});

describe('starredCount', () => {
  it('counts starred in all-accounts or single-account scope', () => {
    const messages = {
      m1: { accountId: 'a1', isStarred: true },
      m2: { accountId: 'a2', isStarred: true },
      m3: { accountId: 'a1', isStarred: false },
    };
    expect(starredCount(messages, 'all', 'all')).toBe(2);
    expect(starredCount(messages, 'a1', 'all')).toBe(1);
  });
});
