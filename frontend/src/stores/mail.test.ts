import { beforeEach, describe, expect, it } from 'vitest';

import { useMailStore } from './mail';
import type { MailMessage } from '@/types';

function makeMessage(overrides: Partial<MailMessage>): MailMessage {
  return {
    id: 'm1',
    accountId: 'a1',
    folderId: 'f1',
    subject: 'Hello',
    from: { email: 'a@example.com' },
    to: [],
    date: '2026-08-26T00:00:00Z',
    snippet: 'Hello',
    isRead: false,
    isStarred: false,
    isDraft: false,
    hasAttachments: false,
    ...overrides,
  };
}

const listPayload = makeMessage({ id: 'm1' }); // list rows carry no body
const detailPayload = makeMessage({ id: 'm1', bodyText: 'full text', bodyHtml: '<p>full</p>' });

describe('mail store message merge', () => {
  beforeEach(() => {
    useMailStore.setState({ accounts: [], folders: {}, messages: {}, threads: {} });
  });

  it('upsertMessage keeps a fetched body when a list payload arrives later', () => {
    useMailStore.getState().upsertMessage(detailPayload);
    useMailStore.getState().upsertMessage(listPayload);
    const msg = useMailStore.getState().messages.m1;
    expect(msg.bodyHtml).toBe('<p>full</p>');
    expect(msg.bodyText).toBe('full text');
  });

  it('upsertMessage lets a fresh detail payload replace the body', () => {
    useMailStore.getState().upsertMessage(detailPayload);
    useMailStore.getState().upsertMessage(makeMessage({ id: 'm1', bodyText: 'new text' }));
    const msg = useMailStore.getState().messages.m1;
    expect(msg.bodyText).toBe('new text');
    expect(msg.bodyHtml).toBeUndefined();
  });

  it('replaceMessagesForView preserves bodies while refreshing list fields', () => {
    useMailStore.getState().upsertMessage(detailPayload);
    useMailStore
      .getState()
      .replaceMessagesForView({ accountId: 'a1', folderId: 'f1', folderRole: null }, [
        makeMessage({ id: 'm1', isRead: true }),
      ]);
    const msg = useMailStore.getState().messages.m1;
    expect(msg.isRead).toBe(true);
    expect(msg.bodyHtml).toBe('<p>full</p>');
  });

  it('replaceMessagesForView keeps snippet-only messages as-is', () => {
    useMailStore
      .getState()
      .replaceMessagesForView({ accountId: 'a1', folderId: 'f1', folderRole: null }, [listPayload]);
    expect(useMailStore.getState().messages.m1.bodyHtml).toBeUndefined();
  });
});
