import { describe, expect, it } from 'vitest';

import { messagesUrlForView } from './load-mail-messages';
import { ALL_ACCOUNTS } from '@/lib/mail-api';

describe('messagesUrlForView', () => {
  it('uses folder path when folderId is set', () => {
    expect(
      messagesUrlForView({ accountId: ALL_ACCOUNTS, folderId: 'f1', folderRole: 'inbox' }),
    ).toBe('/folders/f1/messages');
  });

  it('uses role query for unified roles', () => {
    expect(
      messagesUrlForView({ accountId: ALL_ACCOUNTS, folderId: null, folderRole: 'drafts' }),
    ).toBe('/messages?role=drafts');
    expect(messagesUrlForView({ accountId: 'acc1', folderId: null, folderRole: 'sent' })).toBe(
      '/messages?role=sent&accountId=acc1',
    );
  });

  it('uses isStarred for Favorites Starred view', () => {
    expect(
      messagesUrlForView({ accountId: ALL_ACCOUNTS, folderId: null, folderRole: 'starred' }),
    ).toBe('/messages?isStarred=true');
    expect(messagesUrlForView({ accountId: 'acc1', folderId: null, folderRole: 'starred' })).toBe(
      '/messages?isStarred=true&accountId=acc1',
    );
  });
});
